//! Append and durability adapters shared by production journals and fault-injection tests.

use crate::{
    JournalConfig,
    segments::{self, Anchor, SegmentWriter},
};
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use torse_types::{CommitCursor, Fault};

/// The journal stops permanently after either operation fails.
///
/// `append` must write the entire slice or return an error. An error may leave a partial
/// suffix; recovery validates the file before it can be used again. `sync` is the durability
/// barrier, and ordinary successful writes never advance the public durable position.
pub trait JournalBackend {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn sync(&mut self) -> io::Result<()>;
    fn reclaim(
        &mut self,
        _floor: CommitCursor,
        _durable: CommitCursor,
        _config: JournalConfig,
    ) -> Result<(), Fault> {
        Ok(())
    }
}

/// A bounded in-process store for deterministic tests and local adapters.
///
/// Its durable prefix survives destruction of a journal only if the caller retains these
/// bytes. It deliberately makes no process-crash or device-persistence guarantee.
#[derive(Debug)]
pub struct MemoryBackend {
    bytes: Vec<u8>,
    max_bytes: u64,
}

impl MemoryBackend {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl JournalBackend for MemoryBackend {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let total = (self.bytes.len() as u64)
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= self.max_bytes)
            .ok_or_else(|| io::Error::other("memory journal retention limit exceeded"))?;
        let total = usize::try_from(total)
            .map_err(|_| io::Error::other("memory journal length is not addressable"))?;
        if total > self.bytes.capacity() {
            let ceiling = self.max_bytes.min(usize::MAX as u64) as usize;
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(4096)
                .max(total)
                .min(ceiling);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Owns an exclusively locked file for the entire journal lifetime.
///
/// The per-file lock prevents two journal instances from writing the same file. The broader
/// application owns the broader world-directory lease; checkpoints use a separate backend.
#[derive(Debug)]
pub struct FileBackend {
    pub(crate) file: File,
    pub(crate) path: PathBuf,
    pub(crate) baseline: CommitCursor,
    pub(crate) segments: Option<SegmentWriter>,
}

impl Drop for FileBackend {
    fn drop(&mut self) {
        // Release ownership even if a child or duplicate still holds the file description.
        let _ = self.file.unlock();
    }
}

impl JournalBackend for FileBackend {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.segments {
            Some(writer) => writer.append(bytes),
            None => self.file.write_all(bytes),
        }
    }

    fn sync(&mut self) -> io::Result<()> {
        match &mut self.segments {
            Some(writer) => writer.sync(),
            None => self.file.sync_all(),
        }
    }

    fn reclaim(
        &mut self,
        floor: CommitCursor,
        durable: CommitCursor,
        config: JournalConfig,
    ) -> Result<(), Fault> {
        use crate::journal::io_fault;
        use std::fs;
        let root = segments::root(&self.path);
        let mut files = segments::list(&root, config)?;
        let mut anchor = self.segments.as_ref().map(|s| s.anchor).unwrap_or(Anchor {
            base: self.baseline,
            legacy_end: durable.seq,
            floor: self.baseline.seq,
            legacy_bytes: self
                .file
                .metadata()
                .map_err(|e| io_fault("stat legacy journal", e))?
                .len(),
        });
        if floor.seq < anchor.floor {
            return Err(Fault::invalid_data(
                "journal retention floor cannot regress",
            ));
        }
        if self.segments.is_none() {
            if !files.is_empty() {
                return Err(Fault::invalid_data("unpublished journal segments exist"));
            }
            if !root.exists() {
                fs::create_dir(&root).map_err(|e| io_fault("create segment directory", e))?;
                segments::sync_directory(root.parent().expect("journal has a parent"))?;
            }
        }
        anchor.floor = floor.seq;
        // The durable root is published before touching any previously reachable byte.
        segments::publish_anchor(&root, anchor)?;
        if let Some(writer) = &mut self.segments {
            writer.active = None;
        }
        let mut retained = Vec::new();
        for index in 0..files.len() {
            let end = files
                .get(index + 1)
                .map(|next| next.first - 1)
                .unwrap_or(durable.seq.0);
            if end <= floor.seq.0 {
                fs::remove_file(&files[index].path)
                    .map_err(|e| io_fault("remove covered segment", e))?;
            } else {
                retained.push(index);
            }
        }
        segments::install_marker(&mut self.file, anchor)?;
        segments::sync_directory(&root)?;
        let active = if let Some(index) = retained.last() {
            let segment = &files[*index];
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&segment.path)
                .map_err(|e| io_fault("reopen active segment", e))?;
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::End(0))
                .map_err(|e| io_fault("seek active segment", e))?;
            Some((file, segment.length))
        } else {
            None
        };
        self.segments = Some(SegmentWriter {
            root,
            anchor,
            active,
            files: retained.len() as u32,
            next: durable
                .seq
                .0
                .checked_add(1)
                .ok_or_else(|| Fault::resource_limit("journal sequence exhausted"))?,
            config,
        });
        files.clear();
        Ok(())
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::path::PathBuf;

    #[test]
    fn memory_growth_preserves_bytes_and_never_reserves_past_retention_limit() {
        let mut backend = MemoryBackend::new(10003);
        for _ in 0..10003 {
            backend.append(&[7]).unwrap();
            assert!(backend.bytes.capacity() <= 10003);
        }
        assert!(backend.append(&[8]).is_err());
        assert_eq!(backend.bytes(), vec![7; 10003]);
        backend.sync().unwrap();
    }

    #[test]
    fn dropping_journal_owner_releases_lock_with_a_duplicate_descriptor_alive() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.work/lock-tests");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(format!("journal-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.try_lock().unwrap();
        let owner = FileBackend {
            file,
            path: path.clone(),
            baseline: CommitCursor::initial(torse_types::WorldId(1)),
            segments: None,
        };
        let duplicate = owner.file.try_clone().unwrap();
        drop(owner);
        let reopened = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let acquired = reopened.try_lock();
        drop(duplicate);
        acquired.expect("owner drop must unlock despite a duplicate descriptor");
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(contender.try_lock().is_err());
        drop(contender);
        reopened.unlock().unwrap();
        drop(reopened);
        fs::remove_file(path).unwrap();
    }
}
