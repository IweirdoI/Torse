//! A deliberately small domain model; it does not register an authority world format.

#![allow(dead_code)]

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use torse_codec::{CodecLimits, Decoder, Encoder, StorageCodec, metadata, record};
use torse_storage::{JournalBackend, JournalConfig};
use torse_types::*;

pub struct TestModel;

impl AuthorityData for TestModel {
    type Snapshot = Vec<u8>;
    type Changes = Vec<u8>;
    type Input = ();
    type Output = ();
    type ReplicaSnapshot = ();
    type ReplicaChanges = ();
}

pub const LIMITS: CodecLimits = CodecLimits {
    max_frame_bytes: 2048,
    max_field_bytes: 1024,
    max_depth: 4,
};

pub fn binding() -> Binding {
    Binding {
        world: WorldId(7),
        epoch: WorldEpoch(11),
        compatibility: Compatibility {
            authority_schema: 1,
            rules_digest: Digest([2; 32]),
            content_digest: Digest([3; 32]),
        },
    }
}

pub fn baseline() -> CommitCursor {
    CommitCursor::initial(binding().world)
}

pub fn config() -> JournalConfig {
    JournalConfig {
        max_pending_records: 3,
        max_pending_bytes: 4096,
        codec_budget: OutputBudget::new(8, 2048),
        codec_limits: LIMITS,
    }
}

pub fn commit(sequence: u64) -> ModelCommit<TestModel> {
    CommitRecord {
        meta: CommitMeta {
            cursor: CommitCursor {
                world: binding().world,
                seq: CommitSeq(sequence),
            },
            previous: CommitSeq(sequence - 1),
            compatibility: binding().compatibility,
            resume_at: ResumePoint {
                tick: Tick(sequence / 6),
                next_phase: (sequence % 6) as u16,
                next_step: 0,
            },
        },
        changes: vec![sequence as u8, 0, 0xff],
    }
}

pub fn cursor(sequence: u64) -> CommitCursor {
    CommitCursor {
        world: binding().world,
        seq: CommitSeq(sequence),
    }
}

/// Explicit metadata plus one opaque, length-prefixed field. Unknown bytes stay unchanged.
pub struct TestCodec {
    pub fail_encode: bool,
}

impl TestCodec {
    pub fn new() -> Self {
        Self { fail_encode: false }
    }
}

impl StorageCodec<TestModel> for TestCodec {
    fn encode_snapshot(
        &self,
        value: &ModelSnapshot<TestModel>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> Result<(), Fault> {
        let mut encoder = Encoder::new(out, budget, LIMITS);
        encoder.count_items(1)?;
        metadata::write_snapshot_meta(&mut encoder, value.meta)?;
        encoder.write_field(&value.state)?;
        encoder.finish();
        Ok(())
    }

    fn decode_snapshot(
        &self,
        bytes: &[u8],
        budget: OutputBudget,
    ) -> Result<ModelSnapshot<TestModel>, Fault> {
        let mut input = Decoder::new(bytes, budget, LIMITS);
        input.charge_items(1)?;
        input.charge_bytes(std::mem::size_of::<ModelSnapshot<TestModel>>() as u64)?;
        let meta = metadata::read_snapshot_meta(&mut input)?;
        let field = input.read_field()?;
        input.charge_bytes(field.len() as u64)?;
        let state = field.to_vec();
        if !input.is_complete() {
            return Err(Fault::invalid_data("trailing snapshot bytes"));
        }
        Ok(WorldSnapshot { meta, state })
    }

    fn encode_commit(
        &self,
        value: &ModelCommit<TestModel>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> Result<(), Fault> {
        let mut encoder = Encoder::new(out, budget, LIMITS);
        encoder.count_items(1)?;
        metadata::write_commit_meta(&mut encoder, value.meta)?;
        if self.fail_encode {
            return Err(Fault::invalid_data("injected codec failure after metadata"));
        }
        encoder.write_field(&value.changes)?;
        encoder.finish();
        Ok(())
    }

    fn decode_commit(
        &self,
        bytes: &[u8],
        budget: OutputBudget,
    ) -> Result<ModelCommit<TestModel>, Fault> {
        let mut input = Decoder::new(bytes, budget, LIMITS);
        input.charge_items(1)?;
        input.charge_bytes(std::mem::size_of::<ModelCommit<TestModel>>() as u64)?;
        let meta = metadata::read_commit_meta(&mut input)?;
        let field = input.read_field()?;
        input.charge_bytes(field.len() as u64)?;
        let changes = field.to_vec();
        if !input.is_complete() {
            return Err(Fault::invalid_data("trailing commit bytes"));
        }
        Ok(CommitRecord { meta, changes })
    }
}

pub fn encoded(value: &ModelCommit<TestModel>) -> Vec<u8> {
    let mut payload = Vec::new();
    TestCodec::new()
        .encode_commit(value, &mut payload, config().codec_budget)
        .unwrap();
    framed(&payload, record::RecordKind::Commit)
}

pub fn framed(payload: &[u8], kind: record::RecordKind) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut out = Encoder::new(&mut bytes, OutputBudget::new(1, 4096), LIMITS);
    record::write_record(&mut out, kind, payload).unwrap();
    out.finish();
    bytes
}

/// Partial appends and failed durability barriers exercise the actual journal state machine.
#[derive(Default)]
pub struct FaultyBackend {
    pub bytes: Vec<u8>,
    pub fail_after_bytes: Option<usize>,
    pub fail_sync_call: Option<usize>,
    pub append_calls: usize,
    pub sync_calls: usize,
}

impl JournalBackend for FaultyBackend {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.append_calls += 1;
        if let Some(limit) = self.fail_after_bytes {
            let available = limit.saturating_sub(self.bytes.len());
            self.bytes
                .extend_from_slice(&bytes[..bytes.len().min(available)]);
            if available < bytes.len() {
                return Err(io::Error::other("injected partial append"));
            }
        } else {
            self.bytes.extend_from_slice(bytes);
        }
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        self.sync_calls += 1;
        if self.fail_sync_call == Some(self.sync_calls) {
            return Err(io::Error::other("injected sync failure"));
        }
        Ok(())
    }
}

/// Every test owns one file under the workspace scratch directory, never a system temp path.
pub struct TestFile(pub PathBuf);

impl TestFile {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.work/storage-tests");
        std::fs::create_dir_all(&parent).unwrap();
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(parent.join(format!("{}-{index}.journal", std::process::id())))
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove test journal: {error}"),
        }
    }
}
