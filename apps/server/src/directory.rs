//! A stable, exclusively locked file also records monotonically allocated run epochs.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use torse_types::{Fault, FaultCode, WorldEpoch, WorldId};

const MAGIC: &[u8; 8] = b"TORSEEP1";
const ENTRY_BYTES: usize = 48;

/// Hold this guard until every authority, journal, and checkpoint handle has been dropped.
/// Never delete or replace `world.lock`: another opener must lock the same file identity.
/// The root must already exist; creating/publishing the parent namespace belongs to the host.
pub struct WorldDirectory {
    root: PathBuf,
    world: WorldId,
    epoch: WorldEpoch,
    _lock: DirectoryLock,
}

/// Construct immediately after acquisition so every validation failure unlocks explicitly.
struct DirectoryLock(File);

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl WorldDirectory {
    /// Allocate an epoch while holding the directory's cooperative single-writer lease.
    /// The finite ledger limit fails explicitly rather than recycling old run identities.
    /// A partial final entry was never exposed by a successful acquisition and may be
    /// repaired. Complete corruption is fatal; system clocks are never used as identity.
    pub fn acquire(
        root: impl AsRef<Path>,
        world: WorldId,
        max_epoch_bytes: u64,
    ) -> Result<Self, Fault> {
        if max_epoch_bytes < ENTRY_BYTES as u64 {
            return Err(Fault::invalid_data(
                "epoch ledger limit cannot hold one entry",
            ));
        }
        let root = fs::canonicalize(root).map_err(|e| io_fault("resolve world directory", e))?;
        if !root.is_dir() {
            return Err(Fault::invalid_data("world root must be a directory"));
        }
        let path = root.join("world.lock");
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_file() => {}
            Ok(_) => return Err(corrupt("world lock must be a regular file")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_fault("inspect world lock", error)),
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| io_fault("open world lock", e))?;
        file.try_lock()
            .map_err(|e| io_fault("lock world directory", e))?;
        let mut lock = DirectoryLock(file);
        let file = &mut lock.0;
        let length = file
            .metadata()
            .map_err(|e| io_fault("stat epoch ledger", e))?
            .len();
        if length > max_epoch_bytes {
            return Err(Fault::resource_limit(
                "epoch ledger exceeds configured capacity",
            ));
        }
        let mut previous = 0u128;
        let mut valid_bytes = 0u64;
        let mut remaining = length;
        while remaining != 0 {
            let count = remaining.min(ENTRY_BYTES as u64) as usize;
            let mut bytes = [0; ENTRY_BYTES];
            file.read_exact(&mut bytes[..count])
                .map_err(|e| io_fault("read epoch ledger", e))?;
            let checked = (|| {
                if bytes[..count.min(8)] != MAGIC[..count.min(8)] {
                    return Err(corrupt("epoch ledger magic mismatch"));
                }
                if count < ENTRY_BYTES {
                    return Ok(None);
                }
                let crc = u32::from_le_bytes(bytes[40..44].try_into().expect("fixed entry width"));
                if crc32c::crc32c(&bytes[..40]) != crc || &bytes[44..] != b"EEND" {
                    return Err(corrupt("epoch ledger checksum or end marker mismatch"));
                }
                let recorded_world =
                    u128::from_le_bytes(bytes[8..24].try_into().expect("fixed world width"));
                let epoch =
                    u128::from_le_bytes(bytes[24..40].try_into().expect("fixed epoch width"));
                if recorded_world != world.0 {
                    return Err(Fault::invalid_data(
                        "world directory belongs to another world",
                    ));
                }
                if previous.checked_add(1) != Some(epoch) {
                    return Err(corrupt("epoch ledger sequence is not consecutive"));
                }
                Ok(Some(epoch))
            })()
            .map_err(|fault: Fault| {
                Fault::new(
                    fault.code,
                    format!("epoch ledger byte {valid_bytes}: {}", fault.context),
                )
            })?;
            let Some(epoch) = checked else { break };
            previous = epoch;
            valid_bytes += ENTRY_BYTES as u64;
            remaining -= ENTRY_BYTES as u64;
        }
        let epoch = previous
            .checked_add(1)
            .ok_or_else(|| Fault::resource_limit("world epochs exhausted"))?;
        if valid_bytes
            .checked_add(ENTRY_BYTES as u64)
            .is_none_or(|n| n > max_epoch_bytes)
        {
            return Err(Fault::resource_limit("epoch ledger capacity exhausted"));
        }
        let mut bytes = [0; ENTRY_BYTES];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..24].copy_from_slice(&world.0.to_le_bytes());
        bytes[24..40].copy_from_slice(&epoch.to_le_bytes());
        let crc = crc32c::crc32c(&bytes[..40]);
        bytes[40..44].copy_from_slice(&crc.to_le_bytes());
        bytes[44..].copy_from_slice(b"EEND");
        // Validate and reserve the next identity before repairing anything. A failed write
        // may leave a suffix, but that epoch is never returned to the application.
        file.set_len(valid_bytes)
            .and_then(|_| file.seek(SeekFrom::End(0)))
            .and_then(|_| file.write_all(&bytes))
            .and_then(|_| file.sync_all())
            .map_err(|e| io_fault("persist new world epoch", e))?;
        sync_directory(&root)?;
        Ok(Self {
            root,
            world,
            epoch: WorldEpoch(epoch),
            _lock: lock,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn world(&self) -> WorldId {
        self.world
    }
    pub fn epoch(&self) -> WorldEpoch {
        self.epoch
    }
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), Fault> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| io_fault("sync world directory", e))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn io_fault(operation: &str, error: impl std::fmt::Display) -> Fault {
    Fault::new(
        FaultCode::StorageUnavailable,
        format!("{operation}: {error}"),
    )
}

fn corrupt(context: &str) -> Fault {
    Fault::new(FaultCode::CorruptData, context)
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn dropping_directory_owner_releases_lock_with_a_duplicate_descriptor_alive() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.work/lock-tests")
            .join(format!("world-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let owner = WorldDirectory::acquire(&root, WorldId(7), 8192).unwrap();
        let duplicate = owner._lock.0.try_clone().unwrap();
        assert!(WorldDirectory::acquire(&root, WorldId(7), 8192).is_err());
        drop(owner);
        let reopened = WorldDirectory::acquire(&root, WorldId(7), 8192);
        drop(duplicate);
        let reopened = reopened.expect("owner drop must unlock despite a duplicate descriptor");
        assert_eq!(reopened.epoch(), WorldEpoch(2));
        assert!(WorldDirectory::acquire(&root, WorldId(7), 8192).is_err());
        drop(reopened);
        fs::remove_file(root.join("world.lock")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
