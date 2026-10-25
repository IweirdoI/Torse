//! One owner enforces prepare, reserve, publish, accept, synchronize, then expose.

use std::time::{Duration, Instant};
use torse_diagnostics::{SerialProfiler, Stage};

use torse_storage::{CheckpointRetention, CommitJournal};
use torse_types::{
    Admission, Binding, CommitCursor, Durability, Fault, FaultCode, ModelRequest, OutputBudget,
    Publication, RejectCode, StageError, Tick,
};

use crate::{HostAuthority, HostCheckpointStore, InvalidationSink, PreparedChange, WorldDirectory};

#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    pub snapshot_budget: OutputBudget,
    pub max_invalidations: u32,
    pub shutdown_max_steps: u32,
    /// Keep this many already-published generations before writing the next one. At least
    /// one predecessor survives a failed publication; a successful write adds one file.
    pub checkpoint_predecessors: u32,
}

impl ServerConfig {
    pub fn validate(self) -> Result<(), Fault> {
        if self.snapshot_budget.max_items == 0
            || self.snapshot_budget.max_bytes == 0
            || self.shutdown_max_steps == 0
            || self.checkpoint_predecessors == 0
        {
            return Err(Fault::invalid_data(
                "server policy must allow snapshots, shutdown work, and a checkpoint predecessor",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerState {
    Running,
    Stopping,
    Faulted,
}

/// This value exists only after a validated synchronization barrier. A Queued admission
/// receipt is separate and never implies that a mutation has survived a process restart.
#[derive(Debug)]
pub struct DurablePublication {
    pub publication: Publication,
    pub durability: Durability,
}

#[derive(Debug)]
pub enum CommitStep {
    Idle,
    Blocked(StageError),
    Durable(DurablePublication),
}

/// Synchronization waiting is measured outside authority calls and world write ownership.
#[derive(Clone, Copy, Debug, Default)]
pub struct FlushMetrics {
    pub calls: u64,
    pub total_wait: Duration,
    pub last_wait: Duration,
}

#[derive(Debug)]
pub struct ShutdownReport {
    pub through: CommitCursor,
    pub durability: Durability,
    pub committed_during_shutdown: u32,
    pub flush: FlushMetrics,
}

/// Field declaration order is intentional: Rust drops the authority and storage handles
/// before releasing the directory lease. There is no mutable authority/backend escape hatch.
pub struct Server<A: HostAuthority, J, S> {
    pub(crate) authority: A,
    pub(crate) journal: J,
    pub(crate) checkpoints: S,
    binding: Binding,
    config: ServerConfig,
    state: ServerState,
    flush: FlushMetrics,
    pub(crate) profiler: SerialProfiler,
    _directory: WorldDirectory,
}

impl<A: HostAuthority, J: CommitJournal<A::Model>, S: HostCheckpointStore<A::Model>>
    Server<A, J, S>
{
    /// Initialize a new world under an already-acquired directory lease. Admission remains
    /// unavailable until the complete position-zero checkpoint has been published.
    pub fn initialize(
        directory: WorldDirectory,
        authority: A,
        journal: J,
        checkpoints: S,
        config: ServerConfig,
    ) -> Result<Self, Fault> {
        if authority.cursor().seq.0 != 0 || !authority.at_tick_boundary() {
            return Err(Fault::invalid_data(
                "new authority must be at the initial tick boundary",
            ));
        }
        let mut server = Self::assemble(directory, authority, journal, checkpoints, config)?;
        server.checkpoint()?;
        Ok(server)
    }

    /// Filesystem recovery calls this only after isolated replay and all storage barriers
    /// succeed. Custom backends must supply equally validated checkpoint/log baselines.
    pub(crate) fn assemble(
        directory: WorldDirectory,
        authority: A,
        journal: J,
        checkpoints: S,
        config: ServerConfig,
    ) -> Result<Self, Fault> {
        config.validate()?;
        let binding = authority.binding();
        let cursor = authority.cursor();
        let durable = journal.durability();
        if binding.world != directory.world()
            || binding.epoch != directory.epoch()
            || cursor.world != binding.world
            || durable.binding != binding
            || durable.through.0 != cursor.seq.0
        {
            return Err(Fault::invalid_data(
                "authority, directory, and journal baselines differ",
            ));
        }
        Ok(Self {
            authority,
            journal,
            checkpoints,
            binding,
            config,
            state: ServerState::Running,
            flush: FlushMetrics::default(),
            profiler: SerialProfiler::default(),
            _directory: directory,
        })
    }

    pub fn state(&self) -> ServerState {
        self.state
    }
    pub fn binding(&self) -> Binding {
        self.binding
    }
    pub fn durability(&self) -> Durability {
        self.journal.durability()
    }
    pub fn flush_metrics(&self) -> FlushMetrics {
        self.flush
    }

    /// Trusted transport/session validation belongs to B7. This boundary additionally
    /// rejects other worlds and previous runs before invoking the authority adapter.
    pub fn submit(&mut self, request: ModelRequest<A::Model>) -> Admission {
        if self.state != ServerState::Running {
            return Admission::Rejected(RejectCode::Stopping);
        }
        let binding = self.binding;
        if self.authority.binding() != binding {
            self.fail(Fault::new(
                FaultCode::InvalidState,
                "authority changed its live binding",
            ));
            return Admission::Rejected(RejectCode::Stopping);
        }
        if request.id.world != binding.world || request.id.epoch != binding.epoch {
            return Admission::Rejected(RejectCode::StaleSession);
        }
        self.authority.submit(request)
    }

    /// Read-only world access is gated just like committed action results. In particular,
    /// callers cannot inspect a published-but-unsynchronized state after a storage failure.
    pub fn authority(&self) -> Result<&A, Fault> {
        if self.state != ServerState::Running {
            return Err(stopped());
        }
        self.validate_durable(self.authority.cursor(), self.journal.durability())?;
        Ok(&self.authority)
    }

    pub fn drive_one(&mut self, sink: &mut impl InvalidationSink) -> Result<CommitStep, Fault> {
        if self.state != ServerState::Running {
            return Err(stopped());
        }
        self.commit_one(sink, None)
    }

    fn commit_one(
        &mut self,
        sink: &mut impl InvalidationSink,
        deadline: Option<Tick>,
    ) -> Result<CommitStep, Fault> {
        if let Err(error) =
            self.validate_durable(self.authority.cursor(), self.journal.durability())
        {
            return Err(self.fail(error));
        }
        let before = self.authority.cursor();
        let resume_before = self.authority.resume_point();
        let preparation = {
            let _span = self.profiler.span(Stage::Preparation);
            self.authority.prepare_next()
        };
        let prepared = match preparation {
            Ok(value) => value,
            Err(error) => return Err(self.fail(error)),
        };
        if self.authority.cursor() != before
            || self.authority.resume_point() != resume_before
            || self.authority.binding() != self.binding
        {
            return Err(self.fail(Fault::new(
                FaultCode::InvalidState,
                "preparation changed authoritative progress",
            )));
        }
        let Some(prepared) = prepared else {
            return Ok(CommitStep::Idle);
        };
        let meta = prepared.record().meta;
        if meta.cursor.world != before.world
            || meta.previous != before.seq
            || meta.cursor.seq.follows(before.seq).is_err()
            || meta.compatibility != self.authority.binding().compatibility
            || deadline.is_some_and(|end| {
                meta.resume_at.tick > end
                    || (meta.resume_at.tick == end && !self.authority.is_tick_start(meta.resume_at))
            })
        {
            return Err(self.fail(Fault::new(
                FaultCode::InvalidState,
                "invalid authority preparation",
            )));
        }
        let staged = match self.journal.try_stage(prepared.record()) {
            Ok(staged) => staged,
            Err(StageError::Stopped(error)) => return Err(self.fail(error)),
            Err(error) => return Ok(CommitStep::Blocked(error)),
        };
        let publication_result = {
            let _span = self.profiler.span(Stage::Publication);
            self.authority.publish(prepared)
        };
        let publication = match publication_result {
            Ok(value) => value,
            Err(error) => return Err(self.fail(error)),
        };
        // No user callback, output release, or I/O is inserted between publish and accept.
        if let Err(error) = self.journal.accept(staged) {
            return Err(self.fail(error));
        }
        if publication.at != meta.cursor
            || self.authority.cursor() != meta.cursor
            || self.authority.resume_point() != meta.resume_at
            || publication.invalidated_replicas.len() as u64
                > u64::from(self.config.max_invalidations)
        {
            return Err(self.fail(Fault::new(
                FaultCode::InvalidState,
                "authority publication mismatch",
            )));
        }
        if let Err(error) =
            sink.invalidate(self.authority.binding(), &publication.invalidated_replicas)
        {
            return Err(self.fail(error));
        }
        let durability = self.flush_to(publication.at)?;
        Ok(CommitStep::Durable(DurablePublication {
            publication,
            durability,
        }))
    }

    pub fn checkpoint(&mut self) -> Result<Durability, Fault> {
        if self.state != ServerState::Running {
            return Err(stopped());
        }
        self.save_checkpoint()
    }

    /// Maintenance remains serialized under the same directory lease as publication. The
    /// storage layer always revalidates the active snapshot and reports a conservative log
    /// floor; the host keeps the complete B2 journal regardless of that floor.
    pub fn prune_checkpoints(&mut self, retain: usize) -> Result<CheckpointRetention, Fault> {
        if self.state != ServerState::Running {
            return Err(stopped());
        }
        self.prune(retain)
    }

    fn prune(&mut self, retain: usize) -> Result<CheckpointRetention, Fault> {
        match self.checkpoints.prune_checkpoints(retain) {
            Ok(report) => Ok(report),
            Err(error)
                if !self.checkpoints.is_stopped()
                    && matches!(
                        error.code,
                        FaultCode::ResourceLimit | FaultCode::InvalidData
                    ) =>
            {
                Err(error)
            }
            Err(error) => Err(self.fail(error)),
        }
    }

    fn save_checkpoint(&mut self) -> Result<Durability, Fault> {
        if !self.authority.at_tick_boundary() {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "checkpoint requires a tick boundary",
            ));
        }
        let cursor = self.authority.cursor();
        let resume_before = self.authority.resume_point();
        let durable = self.flush_to(cursor)?;
        let snapshot = match self.authority.capture_snapshot(self.config.snapshot_budget) {
            Ok(value) => value,
            Err(error) => return Err(self.fail(error)),
        };
        if self.authority.cursor() != cursor
            || self.authority.resume_point() != resume_before
            || self.authority.binding() != self.binding
            || snapshot.meta.cursor != cursor
            || snapshot.meta.compatibility != self.authority.binding().compatibility
            || snapshot.meta.resume_at != resume_before
        {
            return Err(self.fail(Fault::new(
                FaultCode::InvalidState,
                "authority snapshot mismatch",
            )));
        }
        // Reclaim only older generations and orphans. The active checkpoint and the full
        // journal remain available throughout the following publication, including on error.
        self.prune(self.config.checkpoint_predecessors as usize)?;
        if let Err(error) = self.checkpoints.publish_checkpoint(&snapshot, durable) {
            if error.code == FaultCode::ResourceLimit && !self.checkpoints.is_stopped() {
                return Err(error);
            }
            return Err(self.fail(error));
        }
        Ok(durable)
    }

    /// Consume the server so logs/checkpoints close before the directory lease is released.
    /// The deadline is inclusive for admitted input. The adapter may advance to the next
    /// tick boundary, but a finite step limit prevents stalled or future-event-driven drains.
    pub fn shutdown(
        mut self,
        deadline: Tick,
        sink: &mut impl InvalidationSink,
    ) -> Result<ShutdownReport, Fault> {
        if self.state != ServerState::Running {
            return Err(stopped());
        }
        let boundary_tick = deadline
            .0
            .checked_add(1)
            .map(Tick)
            .filter(|_| deadline >= self.authority.resume_point().tick)
            .ok_or_else(|| Fault::invalid_data("shutdown deadline is invalid"))?;
        self.state = ServerState::Stopping;
        if let Err(error) = self.authority.stop_admission(deadline) {
            return Err(self.fail(error));
        }
        let mut committed = 0;
        let mut steps = 0;
        while !self.authority.shutdown_ready() {
            if steps >= self.config.shutdown_max_steps {
                return Err(self.fail(Fault::resource_limit("shutdown step budget exhausted")));
            }
            steps += 1;
            match self.commit_one(sink, Some(boundary_tick))? {
                CommitStep::Durable(_) => committed += 1,
                CommitStep::Blocked(StageError::Busy) => {
                    self.flush_to(self.authority.cursor())?;
                }
                CommitStep::Blocked(_) => {
                    return Err(self.fail(Fault::new(
                        FaultCode::InvalidState,
                        "shutdown preparation cannot be staged",
                    )));
                }
                CommitStep::Idle => {
                    return Err(self.fail(Fault::new(
                        FaultCode::InvalidState,
                        "authority stalled before shutdown boundary",
                    )));
                }
            }
        }
        if self.authority.resume_point().tick > boundary_tick {
            return Err(self.fail(Fault::new(
                FaultCode::InvalidState,
                "shutdown passed its cutoff",
            )));
        }
        let durability = self.save_checkpoint()?;
        Ok(ShutdownReport {
            through: self.authority.cursor(),
            durability,
            committed_during_shutdown: committed,
            flush: self.flush,
        })
    }

    fn flush_to(&mut self, cursor: CommitCursor) -> Result<Durability, Fault> {
        let start = Instant::now();
        let result = {
            let _span = self.profiler.span(Stage::Flush);
            self.journal.flush_through(cursor)
        };
        self.flush.calls = self.flush.calls.saturating_add(1);
        self.flush.last_wait = start.elapsed();
        self.flush.total_wait = self.flush.total_wait.saturating_add(self.flush.last_wait);
        let durable = match result {
            Ok(value) => value,
            Err(error) => return Err(self.fail(error)),
        };
        if let Err(error) = self.validate_durable(cursor, durable) {
            return Err(self.fail(error));
        }
        Ok(durable)
    }

    fn validate_durable(&self, cursor: CommitCursor, durable: Durability) -> Result<(), Fault> {
        if self.authority.binding() != self.binding
            || durable.binding != self.binding
            || cursor.world != self.binding.world
            || !durable.covers(cursor)
            || durable.through.0 > self.authority.cursor().seq.0
        {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "durability does not cover this authority",
            ));
        }
        Ok(())
    }

    pub(crate) fn fail(&mut self, fault: Fault) -> Fault {
        self.state = ServerState::Faulted;
        // Never attempt another world publication or claim saved state after a terminal error.
        let _ = self
            .authority
            .stop_admission(self.authority.resume_point().tick);
        fault
    }
}

fn stopped() -> Fault {
    Fault::new(FaultCode::InvalidState, "server is not running")
}
