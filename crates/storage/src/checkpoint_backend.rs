//! Checkpoint I/O primitives with explicit publication and synchronization boundaries.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use torse_types::{Fault, FaultCode};

use crate::journal::io_fault;
use crate::manifest::corrupt;

/// An exclusively owned checkpoint namespace. Successful writes must consume all bytes.
///
/// Failures may leave partial files or manifest suffixes. The store then stops; only a new
/// instance may inspect and repair the namespace. Implementations must enforce read/list
/// limits before allocating from untrusted lengths. Synchronization must cover the snapshot
/// and its name before a manifest reference can become durable.
pub trait CheckpointBackend {
    fn read_manifest(&mut self, limit: u64) -> Result<Vec<u8>, Fault>;
    fn truncate_manifest(&mut self, length: u64) -> Result<(), Fault>;
    fn append_manifest(&mut self, bytes: &[u8]) -> Result<(), Fault>;
    fn sync_manifest(&mut self) -> Result<(), Fault>;
    /// Atomically replace the complete history and persist its name before returning.
    /// Errors stop the caller; either the previous or replacement history must survive.
    /// Custom backends may reject compaction before performing any mutation.
    fn replace_manifest(&mut self, _bytes: &[u8]) -> Result<(), Fault> {
        Err(Fault::new(
            FaultCode::InvalidState,
            "manifest replacement is unsupported",
        ))
    }
    fn create_snapshot(&mut self, generation: u64, bytes: &[u8]) -> Result<(), Fault>;
    fn sync_snapshot(&mut self, generation: u64) -> Result<(), Fault>;
    fn read_snapshot(&mut self, generation: u64, limit: u64) -> Result<Vec<u8>, Fault>;
    fn snapshot_generations(&mut self, limit: u32) -> Result<Vec<u64>, Fault>;
    fn remove_snapshot(&mut self, generation: u64) -> Result<(), Fault>;
}

/// Bounded, retained in-process data. Synchronization does not survive a process exit.
#[derive(Debug)]
pub struct MemoryCheckpointBackend {
    manifest: Vec<u8>,
    snapshots: BTreeMap<u64, Vec<u8>>,
    used: u64,
    max_bytes: u64,
}

impl MemoryCheckpointBackend {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            manifest: Vec::new(),
            snapshots: BTreeMap::new(),
            used: 0,
            max_bytes,
        }
    }

    fn copy(&self, bytes: &[u8], limit: u64) -> Result<Vec<u8>, Fault> {
        if bytes.len() as u64 > limit {
            return Err(Fault::resource_limit("checkpoint read limit exceeded"));
        }
        let mut result = Vec::new();
        result
            .try_reserve_exact(bytes.len())
            .map_err(|_| Fault::resource_limit("checkpoint buffer allocation"))?;
        result.extend_from_slice(bytes);
        Ok(result)
    }

    fn reserve(&self, length: usize) -> Result<u64, Fault> {
        self.used
            .checked_add(length as u64)
            .filter(|total| *total <= self.max_bytes)
            .ok_or_else(|| Fault::resource_limit("memory checkpoint retention limit exceeded"))
    }
}

impl CheckpointBackend for MemoryCheckpointBackend {
    fn read_manifest(&mut self, limit: u64) -> Result<Vec<u8>, Fault> {
        self.copy(&self.manifest, limit)
    }

    fn truncate_manifest(&mut self, length: u64) -> Result<(), Fault> {
        let length = usize::try_from(length)
            .map_err(|_| Fault::resource_limit("manifest length is not addressable"))?;
        if length > self.manifest.len() {
            return Err(Fault::invalid_data("cannot extend manifest during repair"));
        }
        self.used -= (self.manifest.len() - length) as u64;
        self.manifest.truncate(length);
        Ok(())
    }

    fn append_manifest(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        let total = self.reserve(bytes.len())?;
        self.manifest
            .try_reserve_exact(bytes.len())
            .map_err(|_| Fault::resource_limit("manifest buffer allocation"))?;
        self.manifest.extend_from_slice(bytes);
        self.used = total;
        Ok(())
    }

    fn sync_manifest(&mut self) -> Result<(), Fault> {
        Ok(())
    }

    fn replace_manifest(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        let total = self.used - self.manifest.len() as u64;
        let total = total
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= self.max_bytes)
            .ok_or_else(|| Fault::resource_limit("memory checkpoint retention limit exceeded"))?;
        let replacement = self.copy(bytes, bytes.len() as u64)?;
        self.manifest = replacement;
        self.used = total;
        Ok(())
    }

    fn create_snapshot(&mut self, generation: u64, bytes: &[u8]) -> Result<(), Fault> {
        if self.snapshots.contains_key(&generation) {
            return Err(corrupt("checkpoint generation already exists"));
        }
        let total = self.reserve(bytes.len())?;
        let data = self.copy(bytes, bytes.len() as u64)?;
        self.snapshots.insert(generation, data);
        self.used = total;
        Ok(())
    }

    fn sync_snapshot(&mut self, generation: u64) -> Result<(), Fault> {
        if !self.snapshots.contains_key(&generation) {
            return Err(corrupt("checkpoint snapshot is missing"));
        }
        Ok(())
    }

    fn read_snapshot(&mut self, generation: u64, limit: u64) -> Result<Vec<u8>, Fault> {
        let bytes = self
            .snapshots
            .get(&generation)
            .ok_or_else(|| corrupt("checkpoint snapshot is missing"))?;
        self.copy(bytes, limit)
    }

    fn snapshot_generations(&mut self, limit: u32) -> Result<Vec<u64>, Fault> {
        if self.snapshots.len() as u64 > u64::from(limit) {
            return Err(Fault::resource_limit(
                "checkpoint file count limit exceeded",
            ));
        }
        Ok(self.snapshots.keys().copied().collect())
    }

    fn remove_snapshot(&mut self, generation: u64) -> Result<(), Fault> {
        if let Some(bytes) = self.snapshots.remove(&generation) {
            self.used -= bytes.len() as u64;
        }
        Ok(())
    }
}

/// An existing, dedicated directory containing `manifest.log` and immutable snapshots.
///
/// The manifest lock is held for this backend's lifetime. The caller owns world-directory
/// creation and the broader world lock. Unix synchronizes directory entries as well as files.
/// Windows uses `sync_all` on files; abrupt-process recovery is tested, but device power-loss
/// guarantees require platform/filesystem qualification before production deployment.
#[derive(Debug)]
pub struct FileCheckpointBackend {
    root: PathBuf,
    manifest: File,
    lock: Option<File>,
}

impl Drop for FileCheckpointBackend {
    fn drop(&mut self) {
        // A forked child or duplicate descriptor can outlive this owner. Closing only
        // our descriptor would leave its lock alive until every duplicate closes.
        let _ = self.manifest.unlock();
        if let Some(lock) = &self.lock {
            let _ = lock.unlock();
        }
    }
}

impl FileCheckpointBackend {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, Fault> {
        Self::open_mode(root.as_ref(), true)
    }

    /// Acquire existing locks and inspect an existing namespace without creating paths.
    /// Synchronization during activation creates the stable namespace lock for legacy data.
    pub fn inspect(root: impl AsRef<Path>) -> Result<Self, Fault> {
        Self::open_mode(root.as_ref(), false)
    }

    fn open_mode(root: &Path, create: bool) -> Result<Self, Fault> {
        let root =
            fs::canonicalize(root).map_err(|e| io_fault("resolve checkpoint directory", &e))?;
        if !root.is_dir() {
            return Err(Fault::invalid_data(
                "checkpoint root must be an existing directory",
            ));
        }
        reject_symlink(&root.join("manifest.lock"), true)?;
        let lock = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .open(root.join("manifest.lock"))
        {
            Ok(lock) => {
                lock.try_lock()
                    .map_err(|e| io_fault("lock checkpoint namespace", e))?;
                Some(lock)
            }
            Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_fault("open checkpoint namespace lock", e)),
        };
        reject_symlink(&root.join("manifest.log"), true)?;
        let manifest = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .open(root.join("manifest.log"))
            .map_err(|e| io_fault("open checkpoint manifest", &e))?;
        manifest
            .try_lock()
            .map_err(|e| io_fault("lock checkpoint manifest", &e))?;
        let backend = Self {
            root,
            manifest,
            lock,
        };
        if create {
            backend.sync_directory()?;
        }
        Ok(backend)
    }

    fn path(&self, generation: u64) -> PathBuf {
        self.root.join(format!("checkpoint-{generation:016x}.bin"))
    }

    fn sync_directory(&self) -> Result<(), Fault> {
        #[cfg(unix)]
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| io_fault("sync checkpoint directory", &e))?;
        Ok(())
    }
}

impl CheckpointBackend for FileCheckpointBackend {
    fn read_manifest(&mut self, limit: u64) -> Result<Vec<u8>, Fault> {
        read_bounded(&mut self.manifest, limit)
    }

    fn truncate_manifest(&mut self, length: u64) -> Result<(), Fault> {
        self.manifest
            .set_len(length)
            .map_err(|e| io_fault("truncate checkpoint manifest", &e))
    }

    fn append_manifest(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        self.manifest
            .seek(SeekFrom::End(0))
            .and_then(|_| self.manifest.write_all(bytes))
            .map_err(|e| io_fault("append checkpoint manifest", &e))
    }

    fn sync_manifest(&mut self) -> Result<(), Fault> {
        if self.lock.is_none() {
            reject_symlink(&self.root.join("manifest.lock"), true)?;
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.root.join("manifest.lock"))
                .map_err(|e| io_fault("create checkpoint namespace lock", e))?;
            lock.try_lock()
                .map_err(|e| io_fault("lock checkpoint namespace", e))?;
            self.lock = Some(lock);
        }
        self.manifest
            .sync_all()
            .map_err(|e| io_fault("sync checkpoint manifest", &e))?;
        self.sync_directory()
    }

    fn replace_manifest(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        let path = self.root.join("manifest.next");
        reject_symlink(&path, true)?;
        let mut replacement = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| io_fault("create replacement manifest", e))?;
        replacement
            .try_lock()
            .map_err(|e| io_fault("lock replacement manifest", e))?;
        replacement
            .write_all(bytes)
            .and_then(|_| replacement.sync_all())
            .map_err(|e| io_fault("persist replacement manifest", e))?;
        fs::rename(&path, self.root.join("manifest.log"))
            .map_err(|e| io_fault("publish replacement manifest", e))?;
        self.sync_directory()?;
        let _ = self.manifest.unlock();
        self.manifest = replacement;
        Ok(())
    }

    fn create_snapshot(&mut self, generation: u64, bytes: &[u8]) -> Result<(), Fault> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path(generation))
            .map_err(|e| io_fault("create checkpoint snapshot", &e))?;
        file.write_all(bytes)
            .map_err(|e| io_fault("write checkpoint snapshot", &e))
    }

    fn sync_snapshot(&mut self, generation: u64) -> Result<(), Fault> {
        let path = self.path(generation);
        reject_symlink(&path, false)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|e| io_fault("sync checkpoint snapshot", &e))?;
        self.sync_directory()
    }

    fn read_snapshot(&mut self, generation: u64, limit: u64) -> Result<Vec<u8>, Fault> {
        let path = self.path(generation);
        reject_symlink(&path, false)?;
        let mut file = File::open(path).map_err(|e| io_fault("open checkpoint snapshot", &e))?;
        read_bounded(&mut file, limit)
    }

    fn snapshot_generations(&mut self, limit: u32) -> Result<Vec<u64>, Fault> {
        let mut result = Vec::new();
        let entries = fs::read_dir(&self.root).map_err(|e| io_fault("list checkpoints", &e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_fault("read checkpoint directory entry", &e))?;
            let kind = entry
                .file_type()
                .map_err(|e| io_fault("inspect checkpoint file", &e))?;
            if !kind.is_file() {
                return Err(corrupt("checkpoint directory contains a non-regular file"));
            }
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| corrupt("invalid checkpoint filename"))?;
            if matches!(name, "manifest.log" | "manifest.lock" | "manifest.next") {
                continue;
            }
            let number = name
                .strip_prefix("checkpoint-")
                .and_then(|s| s.strip_suffix(".bin"))
                .filter(|s| {
                    s.len() == 16
                        && s.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
                .ok_or_else(|| corrupt("unexpected file in checkpoint directory"))?;
            let generation = u64::from_str_radix(number, 16)
                .map_err(|_| corrupt("invalid checkpoint generation filename"))?;
            if result.len() as u64 >= u64::from(limit) {
                return Err(Fault::resource_limit(
                    "checkpoint file count limit exceeded",
                ));
            }
            result
                .try_reserve(1)
                .map_err(|_| Fault::resource_limit("checkpoint list allocation"))?;
            result.push(generation);
        }
        result.sort_unstable();
        Ok(result)
    }

    fn remove_snapshot(&mut self, generation: u64) -> Result<(), Fault> {
        let path = self.path(generation);
        reject_symlink(&path, false)?;
        fs::remove_file(path).map_err(|e| io_fault("remove old checkpoint snapshot", &e))?;
        self.sync_directory()
    }
}

/// Bound the actual file length before allocation and detect concurrent growth. The lock is
/// cooperative, so callers must still protect this directory from unrelated external writers.
fn read_bounded(file: &mut File, limit: u64) -> Result<Vec<u8>, Fault> {
    let length = file
        .metadata()
        .map_err(|e| io_fault("stat checkpoint file", &e))?
        .len();
    if length > limit {
        return Err(Fault::resource_limit(
            "checkpoint file exceeds configured read limit",
        ));
    }
    let length = usize::try_from(length)
        .map_err(|_| Fault::resource_limit("checkpoint file length is not addressable"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| Fault::resource_limit("checkpoint read allocation"))?;
    bytes.resize(length, 0);
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut bytes))
        .map_err(|e| io_fault("read checkpoint file", &e))?;
    let mut extra = [0];
    if file
        .read(&mut extra)
        .map_err(|e| io_fault("check checkpoint file end", &e))?
        != 0
    {
        return Err(corrupt("checkpoint file changed during read"));
    }
    Ok(bytes)
}

fn reject_symlink(path: &Path, allow_missing: bool) -> Result<(), Fault> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        Ok(_) => Err(corrupt("checkpoint path is not a regular file")),
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Fault::new(
            FaultCode::StorageUnavailable,
            format!("inspect checkpoint: {error}"),
        )),
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn dropping_checkpoint_owner_releases_lock_with_a_duplicate_descriptor_alive() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.work/lock-tests")
            .join(format!("checkpoint-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let owner = FileCheckpointBackend::open(&root).unwrap();
        let duplicate = owner.manifest.try_clone().unwrap();
        assert!(FileCheckpointBackend::open(&root).is_err());
        drop(owner);
        let reopened = FileCheckpointBackend::open(&root);
        drop(duplicate);
        let reopened = reopened.expect("owner drop must unlock despite a duplicate descriptor");
        assert!(FileCheckpointBackend::open(&root).is_err());
        drop(reopened);
        fs::remove_file(root.join("manifest.log")).unwrap();
        fs::remove_file(root.join("manifest.lock")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
