//! Append and durability adapters shared by production journals and fault-injection tests.

use std::fs::File;
use std::io::{self, Write};

/// The journal stops permanently after either operation fails.
///
/// `append` must write the entire slice or return an error. An error may leave a partial
/// suffix; recovery validates the file before it can be used again. `sync` is the durability
/// barrier, and ordinary successful writes never advance the public durable position.
pub trait JournalBackend {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn sync(&mut self) -> io::Result<()>;
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
        usize::try_from(total)
            .map_err(|_| io::Error::other("memory journal length is not addressable"))?;
        self.bytes
            .try_reserve_exact(bytes.len())
            .map_err(io::Error::other)?;
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
}

impl JournalBackend for FileBackend {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}
