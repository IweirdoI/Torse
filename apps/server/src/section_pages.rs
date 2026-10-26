//! Immutable, content-addressed section pages. Publish pages before referencing them in a journal.
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use torse_codec::{CodecLimits, wire};
use torse_types::{Digest, Fault, FaultCode, OutputBudget};
use torse_world::Snapshot;

static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct SectionPages {
    root: PathBuf,
    budget: OutputBudget,
    codec: CodecLimits,
    max_files: usize,
}

impl SectionPages {
    /// The caller owns the world directory lock and creates the page directory.
    pub fn new(
        root: impl AsRef<Path>,
        budget: OutputBudget,
        codec: CodecLimits,
        max_files: usize,
    ) -> Result<Self, Fault> {
        if !root.as_ref().is_dir()
            || max_files == 0
            || budget.max_bytes == 0
            || budget.max_items == 0
        {
            return Err(Fault::invalid_data(
                "invalid section page directory or limits",
            ));
        }
        Ok(Self {
            root: root.as_ref().to_owned(),
            budget,
            codec,
            max_files,
        })
    }
    pub fn digest(&self, page: &Snapshot) -> Result<Digest, Fault> {
        Ok(hash(&self.encode(page)?))
    }
    fn encode(&self, page: &Snapshot) -> Result<Vec<u8>, Fault> {
        if page.sections.len() != 1 {
            return Err(Fault::invalid_data("page requires exactly one section"));
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"TORSEPG1");
        wire::encode(page, &mut bytes, self.budget, self.codec)?;
        if bytes.len() as u64 > self.budget.max_bytes {
            return Err(Fault::resource_limit("page exceeds file budget"));
        }
        Ok(bytes)
    }
    pub fn put(&self, page: &Snapshot) -> Result<Digest, Fault> {
        let bytes = self.encode(page)?;
        let digest = hash(&bytes);
        let path = self.path(digest);
        if path.exists() {
            if self.get(digest)? != *page {
                return Err(Fault::invalid_data("page digest collision"));
            }
            return Ok(digest);
        }
        // Include interrupted publications in the directory capacity limit.
        if fs::read_dir(&self.root).map_err(io)?.count() >= self.max_files {
            return Err(Fault::resource_limit(
                "section page file capacity exhausted",
            ));
        }
        let temporary = self.root.join(format!(
            "page-{}-{}.part",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(io)?;
            file.write_all(&bytes).map_err(io)?;
            file.sync_all().map_err(io)?;
            drop(file);
            fs::rename(&temporary, &path).map_err(io)?;
            #[cfg(unix)]
            fs::File::open(&self.root)
                .and_then(|dir| dir.sync_all())
                .map_err(io)?;
            Ok(digest)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    pub fn get(&self, digest: Digest) -> Result<Snapshot, Fault> {
        let file = fs::File::open(self.path(digest)).map_err(io)?;
        let limit = self
            .budget
            .max_bytes
            .min(u64::from(self.codec.max_frame_bytes).saturating_add(8));
        if file.metadata().map_err(io)?.len() > limit {
            return Err(Fault::resource_limit("section page too large"));
        }
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() as u64 > limit || hash(&bytes) != digest || !bytes.starts_with(b"TORSEPG1") {
            return Err(Fault::invalid_data("section page integrity check failed"));
        }
        let page: Snapshot = wire::decode(&bytes[8..], self.budget, self.codec)?;
        if page.sections.len() != 1 {
            return Err(Fault::invalid_data("invalid section page shape"));
        }
        Ok(page)
    }
    pub fn path(&self, digest: Digest) -> PathBuf {
        let name: String = digest.0.iter().map(|b| format!("{b:02x}")).collect();
        self.root.join(format!("{name}.page"))
    }
}

fn hash(bytes: &[u8]) -> Digest {
    Digest(Sha256::digest(bytes).into())
}
fn io(error: std::io::Error) -> Fault {
    Fault::new(FaultCode::StorageUnavailable, error.to_string())
}
