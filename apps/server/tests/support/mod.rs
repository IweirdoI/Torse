//! A bounded counter authority. Its opaque final-value records never invoke gameplay on replay.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use torse_codec::{CodecLimits, Decoder, Encoder, StorageCodec, metadata};
use torse_server::*;
use torse_storage::*;
use torse_types::*;

pub const WORLD: WorldId = WorldId(7);
pub const LIMITS: CodecLimits = CodecLimits {
    max_frame_bytes: 512,
    max_field_bytes: 256,
    max_depth: 4,
};
pub fn compatibility() -> Compatibility {
    Compatibility {
        authority_schema: 1,
        rules_digest: Digest([2; 32]),
        content_digest: Digest([3; 32]),
    }
}
pub fn journal_config() -> JournalConfig {
    JournalConfig {
        max_pending_records: 2,
        max_pending_bytes: 2048,
        codec_budget: OutputBudget::new(8, 512),
        codec_limits: LIMITS,
    }
}
pub fn checkpoint_config() -> CheckpointConfig {
    CheckpointConfig {
        codec_limits: LIMITS,
        codec_budget: OutputBudget::new(8, 512),
        max_manifest_bytes: 8192,
        max_snapshot_files: 16,
    }
}
pub fn server_config() -> ServerConfig {
    ServerConfig {
        snapshot_budget: OutputBudget::new(8, 512),
        max_invalidations: 4,
        shutdown_max_steps: 8,
        checkpoint_predecessors: 1,
    }
}
pub fn file_config() -> FileServerConfig {
    FileServerConfig {
        server: server_config(),
        journal: journal_config(),
        checkpoint: checkpoint_config(),
    }
}
pub fn fault() -> Fault {
    Fault::new(FaultCode::StorageUnavailable, "injected host failure")
}
pub fn resume(tick: u64, phase: u16) -> ResumePoint {
    ResumePoint {
        tick: Tick(tick),
        next_phase: phase,
        next_step: 0,
    }
}

pub struct Model;
pub struct Input {
    pub delta: u64,
    pub target: Tick,
    pub closes_tick: bool,
}
impl AuthorityData for Model {
    type Snapshot = u64;
    type Changes = u64;
    type Input = Input;
    type Output = ();
    type ReplicaSnapshot = ();
    type ReplicaChanges = ();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    None,
    Prepare,
    Publish,
    Capture,
    StageBusy,
    StageLarge,
    StageInvalid,
    StageStopped,
    Accept,
    Append,
    Sync,
    BadBinding,
    ShortBarrier,
    Checkpoint,
    Invalidate,
    BadPublication,
    Stall,
    Restore,
    Replay,
    Finish,
    WrongFinish,
    AheadBarrier,
    BadSnapshot,
    IgnoreCutoff,
    ExitAfterPublish,
    MutatingPrepare,
    MutatingCapture,
    WrongResumeTick,
    WrongResumePhase,
    WrongResumeStep,
    BoundaryStep,
    CheckpointQuota,
    TerminalCheckpointQuota,
}

#[derive(Clone)]
pub struct Control {
    pub trace: Rc<RefCell<Vec<&'static str>>>,
    pub failure: Rc<Cell<Failure>>,
    pub invalidations: Rc<Cell<bool>>,
    pub value: Rc<Cell<u64>>,
    pub canceled: Rc<Cell<u32>>,
}
impl Control {
    pub fn new() -> Self {
        Self {
            trace: Rc::default(),
            failure: Rc::new(Cell::new(Failure::None)),
            invalidations: Rc::new(Cell::new(false)),
            value: Rc::default(),
            canceled: Rc::default(),
        }
    }
    pub fn event(&self, name: &'static str) {
        self.trace.borrow_mut().push(name);
    }
    pub fn reset(&self) {
        self.trace.borrow_mut().clear();
    }
}

#[derive(Clone)]
pub struct Codec(pub Control);
impl StorageCodec<Model> for Codec {
    fn encode_snapshot(
        &self,
        value: &ModelSnapshot<Model>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> Result<(), Fault> {
        let mut encoder = Encoder::new(out, budget, LIMITS);
        encoder.count_items(1)?;
        metadata::write_snapshot_meta(&mut encoder, value.meta)?;
        encoder.write_u64(value.state)?;
        encoder.finish();
        Ok(())
    }
    fn decode_snapshot(
        &self,
        bytes: &[u8],
        budget: OutputBudget,
    ) -> Result<ModelSnapshot<Model>, Fault> {
        let mut decoder = Decoder::new(bytes, budget, LIMITS);
        decoder.charge_items(1)?;
        decoder.charge_bytes(std::mem::size_of::<ModelSnapshot<Model>>() as u64)?;
        let meta = metadata::read_snapshot_meta(&mut decoder)?;
        let state = decoder.read_u64()?;
        if !decoder.is_complete() {
            return Err(Fault::invalid_data("trailing snapshot data"));
        }
        Ok(WorldSnapshot { meta, state })
    }
    fn encode_commit(
        &self,
        value: &ModelCommit<Model>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> Result<(), Fault> {
        let mut encoder = Encoder::new(out, budget, LIMITS);
        encoder.count_items(1)?;
        metadata::write_commit_meta(&mut encoder, value.meta)?;
        encoder.write_u64(value.changes)?;
        encoder.finish();
        Ok(())
    }
    fn decode_commit(
        &self,
        bytes: &[u8],
        budget: OutputBudget,
    ) -> Result<ModelCommit<Model>, Fault> {
        let mut decoder = Decoder::new(bytes, budget, LIMITS);
        decoder.charge_items(1)?;
        decoder.charge_bytes(std::mem::size_of::<ModelCommit<Model>>() as u64)?;
        let meta = metadata::read_commit_meta(&mut decoder)?;
        let changes = decoder.read_u64()?;
        if !decoder.is_complete() {
            return Err(Fault::invalid_data("trailing commit data"));
        }
        Ok(CommitRecord { meta, changes })
    }
}

pub struct Prepared {
    record: ModelCommit<Model>,
    request: Option<RequestId>,
}
impl PreparedChange for Prepared {
    type Model = Model;
    fn record(&self) -> &ModelCommit<Model> {
        &self.record
    }
}

pub struct Authority {
    pub binding: Binding,
    pub sequence: CommitSeq,
    pub point: ResumePoint,
    pub value: u64,
    pub control: Control,
    queue: VecDeque<ModelRequest<Model>>,
    stopping: bool,
}
impl Authority {
    pub fn new(binding: Binding, control: Control) -> Self {
        Self {
            binding,
            sequence: CommitSeq(0),
            point: resume(0, 0),
            value: 0,
            control,
            queue: VecDeque::new(),
            stopping: false,
        }
    }
}
impl HostAuthority for Authority {
    type Model = Model;
    type Prepared = Prepared;
    fn binding(&self) -> Binding {
        self.binding
    }
    fn cursor(&self) -> CommitCursor {
        CommitCursor {
            world: self.binding.world,
            seq: self.sequence,
        }
    }
    fn resume_point(&self) -> ResumePoint {
        self.point
    }
    fn is_tick_start(&self, point: ResumePoint) -> bool {
        point.next_phase == 0 && point.next_step == 0
    }
    fn submit(&mut self, request: ModelRequest<Model>) -> Admission {
        self.control.event("submit");
        if self.stopping {
            return Admission::Rejected(RejectCode::Stopping);
        }
        if self.queue.len() >= 4 {
            return Admission::Rejected(RejectCode::QuotaExceeded);
        }
        let target_tick = request.input.target;
        self.queue.push_back(request);
        Admission::Queued { target_tick }
    }
    fn prepare_next(&mut self) -> Result<Option<Prepared>, Fault> {
        self.control.event("prepare");
        if self.control.failure.get() == Failure::MutatingPrepare {
            self.sequence = self.sequence.next().unwrap();
            return Ok(None);
        }
        if self.control.failure.get() == Failure::Prepare {
            return Err(fault());
        }
        if self.control.failure.get() == Failure::Stall {
            return Ok(None);
        }
        let (value, mut point, request) = if let Some(request) = self.queue.front() {
            let point = if request.input.closes_tick {
                resume(request.input.target.0 + 1, 0)
            } else {
                resume(request.input.target.0, 1)
            };
            (self.value + request.input.delta, point, Some(request.id))
        } else if self.stopping && !self.at_tick_boundary() {
            (self.value, resume(self.point.tick.0 + 1, 0), None)
        } else {
            return Ok(None);
        };
        if self.control.failure.get() == Failure::BoundaryStep {
            point.next_step = 1;
        }
        Ok(Some(Prepared {
            record: CommitRecord {
                meta: CommitMeta {
                    cursor: CommitCursor {
                        world: self.binding.world,
                        seq: self
                            .sequence
                            .next()
                            .ok_or_else(|| Fault::resource_limit("counter sequence exhausted"))?,
                    },
                    previous: self.sequence,
                    compatibility: self.binding.compatibility,
                    resume_at: point,
                },
                changes: value,
            },
            request,
        }))
    }
    fn publish(&mut self, prepared: Prepared) -> Result<Publication, Fault> {
        self.control.event("publish");
        if self.control.failure.get() == Failure::Publish {
            return Err(fault());
        }
        if let Some(request) = prepared.request {
            assert_eq!(self.queue.pop_front().unwrap().id, request);
        }
        self.value = prepared.record.changes;
        self.control.value.set(self.value);
        self.sequence = prepared.record.meta.cursor.seq;
        self.point = prepared.record.meta.resume_at;
        if self.control.failure.get() == Failure::ExitAfterPublish {
            std::process::exit(86);
        }
        let mut at = self.cursor();
        if self.control.failure.get() == Failure::BadPublication {
            at.seq = CommitSeq(99);
        }
        Ok(Publication {
            at,
            invalidated_replicas: if self.control.invalidations.get() {
                vec![SubscriptionId(1)]
            } else {
                Vec::new()
            },
        })
    }
    fn capture_snapshot(&mut self, budget: OutputBudget) -> Result<ModelSnapshot<Model>, Fault> {
        self.control.event("capture");
        if self.control.failure.get() == Failure::MutatingCapture {
            self.point.tick.0 += 1;
        }
        if self.control.failure.get() == Failure::Capture {
            return Err(fault());
        }
        if budget.max_items == 0 || budget.max_bytes < 8 {
            return Err(Fault::resource_limit("counter snapshot budget"));
        }
        let mut cursor = self.cursor();
        if self.control.failure.get() == Failure::BadSnapshot {
            cursor.seq = CommitSeq(999);
        }
        Ok(WorldSnapshot {
            meta: SnapshotMeta {
                cursor,
                compatibility: self.binding.compatibility,
                resume_at: self.point,
            },
            state: self.value,
        })
    }
    fn stop_admission(&mut self, deadline: Tick) -> Result<(), Fault> {
        self.control.event("stop-admission");
        self.stopping = true;
        let before = self.queue.len();
        if self.control.failure.get() != Failure::IgnoreCutoff {
            self.queue
                .retain(|request| request.input.target <= deadline);
        }
        self.control
            .canceled
            .set(self.control.canceled.get() + (before - self.queue.len()) as u32);
        Ok(())
    }
    fn shutdown_ready(&self) -> bool {
        self.stopping && self.queue.is_empty() && self.at_tick_boundary()
    }
}

pub struct Recovery {
    authority: Authority,
    snapshot: bool,
}
impl Recovery {
    pub fn new(binding: Binding, control: Control) -> Self {
        Self {
            authority: Authority::new(binding, control),
            snapshot: false,
        }
    }
}
impl RecoveryTarget for Recovery {
    type Model = Model;
    type Authority = Authority;
    fn restore_snapshot(&mut self, snapshot: ModelSnapshot<Model>) -> Result<(), Fault> {
        self.authority.control.event("restore");
        if self.authority.control.failure.get() == Failure::Restore {
            return Err(fault());
        }
        assert!(!self.snapshot);
        self.snapshot = true;
        self.authority.sequence = snapshot.meta.cursor.seq;
        self.authority.point = snapshot.meta.resume_at;
        self.authority.value = snapshot.state;
        Ok(())
    }
    fn replay(&mut self, record: ModelCommit<Model>) -> Result<(), Fault> {
        self.authority.control.event("replay");
        if self.authority.control.failure.get() == Failure::Replay {
            return Err(fault());
        }
        assert!(self.snapshot);
        assert_eq!(record.meta.previous, self.authority.sequence);
        self.authority.sequence = record.meta.cursor.seq;
        self.authority.point = record.meta.resume_at;
        self.authority.value = record.changes;
        Ok(())
    }
    fn finish(mut self, expected: CommitCursor) -> Result<Authority, Fault> {
        self.authority.control.event("finish");
        if self.authority.control.failure.get() == Failure::Finish {
            return Err(fault());
        }
        assert!(self.snapshot);
        assert_eq!(self.authority.cursor(), expected);
        if self.authority.control.failure.get() == Failure::WrongFinish {
            self.authority.sequence = CommitSeq(99);
        }
        match self.authority.control.failure.get() {
            Failure::WrongResumeTick => self.authority.point.tick = Tick(0),
            Failure::WrongResumePhase => self.authority.point.next_phase = 1,
            Failure::WrongResumeStep => self.authority.point.next_step = 1,
            _ => {}
        }
        self.authority.control.value.set(self.authority.value);
        Ok(self.authority)
    }
}

pub fn request(
    binding: Binding,
    sequence: u64,
    target: u64,
    closes_tick: bool,
) -> ModelRequest<Model> {
    WorldRequest {
        id: RequestId {
            world: binding.world,
            epoch: binding.epoch,
            session: SessionId(1),
            seq: RequestSeq(sequence),
        },
        actor: ActorContextId(1),
        input: Input {
            delta: 1,
            target: Tick(target),
            closes_tick,
        },
    }
}

pub struct Sink(pub Control);
impl InvalidationSink for Sink {
    fn invalidate(&mut self, _: Binding, ids: &[SubscriptionId]) -> Result<(), Fault> {
        self.0.event("invalidate");
        if self.0.invalidations.get() {
            assert_eq!(ids, &[SubscriptionId(1)]);
        }
        if self.0.failure.get() == Failure::Invalidate {
            return Err(fault());
        }
        Ok(())
    }
}

pub struct Backend {
    pub bytes: Vec<u8>,
    pub control: Control,
}
impl JournalBackend for Backend {
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.control.event("append");
        if self.control.failure.get() == Failure::Append {
            return Err(std::io::Error::other("injected append"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.control.event("sync");
        if self.control.failure.get() == Failure::Sync {
            return Err(std::io::Error::other("injected sync"));
        }
        Ok(())
    }
}
pub struct JournalSpy {
    pub inner: Journal<Model, Codec, Backend>,
    pub control: Control,
}
impl CommitJournal<Model> for JournalSpy {
    type Staged = Staged;
    fn try_stage(&mut self, record: &ModelCommit<Model>) -> Result<Staged, StageError> {
        self.control.event("stage");
        match self.control.failure.get() {
            Failure::StageBusy => return Err(StageError::Busy),
            Failure::StageLarge => return Err(StageError::TooLarge),
            Failure::StageInvalid => return Err(StageError::InvalidRecord(fault())),
            Failure::StageStopped => return Err(StageError::Stopped(fault())),
            _ => {}
        }
        self.inner.try_stage(record)
    }
    fn accept(&mut self, staged: Staged) -> Result<(), Fault> {
        self.control.event("accept");
        if self.control.failure.get() == Failure::Accept {
            return Err(fault());
        }
        self.inner.accept(staged)
    }
    fn flush_through(&mut self, cursor: CommitCursor) -> Result<Durability, Fault> {
        self.control.event("flush");
        let mut durable = self.inner.flush_through(cursor)?;
        if cursor.seq.0 > 0 {
            match self.control.failure.get() {
                Failure::BadBinding => durable.binding.epoch = WorldEpoch(999),
                Failure::ShortBarrier => durable.through = DurableSeq(cursor.seq.0 - 1),
                Failure::AheadBarrier => durable.through = DurableSeq(cursor.seq.0 + 1),
                _ => {}
            }
        }
        Ok(durable)
    }
    fn durability(&self) -> Durability {
        self.inner.durability()
    }
}
pub struct CheckpointSpy {
    pub inner: MemoryCheckpoints<Model, Codec>,
    pub control: Control,
}
impl CheckpointStore<Model> for CheckpointSpy {
    fn publish_checkpoint(
        &mut self,
        snapshot: &ModelSnapshot<Model>,
        durable: Durability,
    ) -> Result<(), Fault> {
        self.control.event("checkpoint");
        if self.control.failure.get() == Failure::Checkpoint {
            return Err(fault());
        }
        if matches!(
            self.control.failure.get(),
            Failure::CheckpointQuota | Failure::TerminalCheckpointQuota
        ) {
            return Err(Fault::resource_limit("injected checkpoint quota"));
        }
        self.inner.publish_checkpoint(snapshot, durable)
    }
}

impl HostCheckpointStore<Model> for CheckpointSpy {
    fn prune_checkpoints(&mut self, retain: usize) -> Result<CheckpointRetention, Fault> {
        self.control.event("prune");
        self.inner.prune(retain)
    }

    fn is_stopped(&self) -> bool {
        self.inner.is_stopped() || self.control.failure.get() == Failure::TerminalCheckpointQuota
    }
}

pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.work/server-tests");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub fn acquire(&self) -> WorldDirectory {
        WorldDirectory::acquire(&self.0, WORLD, 8192).unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.0).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                assert!(matches!(
                    entry.file_name().to_str(),
                    Some("checkpoints" | "sections")
                ));
                for file in fs::read_dir(entry.path()).unwrap() {
                    fs::remove_file(file.unwrap().path()).unwrap();
                }
                fs::remove_dir(entry.path()).unwrap();
            } else {
                fs::remove_file(entry.path()).unwrap();
            }
        }
        fs::remove_dir(&self.0).unwrap();
    }
}

pub type TestServer = Server<Authority, JournalSpy, CheckpointSpy>;
pub struct Fixture {
    pub server: TestServer,
    pub control: Control,
    pub directory: Directory,
}
pub fn fixture() -> Fixture {
    fixture_with(server_config(), Control::new()).unwrap()
}
pub fn fixture_with(config: ServerConfig, control: Control) -> Result<Fixture, Fault> {
    let directory = Directory::new();
    let lease = directory.acquire();
    let binding = Binding {
        world: WORLD,
        epoch: lease.epoch(),
        compatibility: compatibility(),
    };
    let codec = Codec(control.clone());
    let authority = Authority::new(binding, control.clone());
    let inner = Journal::with_backend(
        binding,
        CommitCursor::initial(WORLD),
        codec.clone(),
        journal_config(),
        Backend {
            bytes: Vec::new(),
            control: control.clone(),
        },
    )
    .unwrap();
    let journal = JournalSpy {
        inner,
        control: control.clone(),
    };
    let checkpoints = CheckpointSpy {
        inner: Checkpoints::open(
            binding,
            codec,
            checkpoint_config(),
            MemoryCheckpointBackend::new(16384),
        )
        .unwrap(),
        control: control.clone(),
    };
    let server = Server::initialize(lease, authority, journal, checkpoints, config)?;
    Ok(Fixture {
        server,
        control,
        directory,
    })
}

pub type TestFileServer = FileServer<Authority, Codec>;
pub fn create_file(directory: &Directory, control: Control) -> TestFileServer {
    FileServer::create_file(
        directory.acquire(),
        compatibility(),
        Codec(control.clone()),
        file_config(),
        |binding| Ok(Authority::new(binding, control)),
    )
    .unwrap()
}
pub fn open_file(directory: &Directory, control: Control) -> Result<TestFileServer, Fault> {
    FileServer::open_file(
        directory.acquire(),
        compatibility(),
        Codec(control.clone()),
        file_config(),
        |binding| Ok(Recovery::new(binding, control)),
    )
}
