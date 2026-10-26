//! Real world integration for trusted, synchronous local transactions and phase progress.
//!
//! This model has no network inputs, results, scheduled events, or replica subscriptions.
//! A caller plans a transaction from a durable read, then submits it through the server.
//! Optimistic preconditions are rechecked before journal reservation. Transaction IDs are
//! diagnostic identities, not persisted request deduplication keys.

use std::convert::Infallible;

use torse_sim::{
    Phase, PrepareError, Recovery, Transaction, WorldAuthority, next_resume_point,
    persistent_changes_from_prepared,
};
use torse_storage::CommitJournal;
use torse_types::{
    Admission, AuthorityData, Binding, CommitCursor, CommitRecord, Fault, FaultCode, ModelCommit,
    ModelRequest, ModelSnapshot, OutputBudget, Publication, RejectCode, ResumePoint, SnapshotMeta,
    StageError, Tick, WorldSnapshot,
};
use torse_world::{BlockStateRegistry, ChangeSet, ComponentSchemas, Snapshot};

use crate::{
    CommitStep, DurablePublication, HostAuthority, HostCheckpointStore, NoReplicas, PreparedChange,
    RecoveryTarget, Server,
};

/// The existing world payload format, with transport entry points intentionally uninhabited.
pub struct LocalWorldModel;

impl AuthorityData for LocalWorldModel {
    type Snapshot = Snapshot;
    type Changes = ChangeSet;
    type Input = Infallible;
    type Output = Infallible;
    type ReplicaSnapshot = Infallible;
    type ReplicaChanges = Infallible;
}

/// Staging owns the complete world record and the corresponding opaque publication plan.
pub struct WorldPrepared {
    change: torse_sim::PreparedChange,
    record: ModelCommit<LocalWorldModel>,
}

impl PreparedChange for WorldPrepared {
    type Model = LocalWorldModel;

    fn record(&self) -> &ModelCommit<Self::Model> {
        &self.record
    }
}

/// Real world authority owned exclusively by the durable server.
pub struct WorldHost {
    binding: Binding,
    world: WorldAuthority,
    prepared: Option<WorldPrepared>,
    stopping: bool,
}

impl WorldHost {
    /// Consume a finite, fully resident world at a whole-phase boundary.
    /// Initial server creation additionally requires a tick boundary.
    pub fn new(binding: Binding, world: WorldAuthority) -> Result<Self, Fault> {
        if binding.world != world.world()
            || binding.compatibility != world.coordinator().compatibility()
            || !phase_boundary(world.coordinator().resume_point())
        {
            return Err(Fault::invalid_data(
                "local world binding or phase boundary mismatch",
            ));
        }
        Ok(Self {
            binding,
            world,
            prepared: None,
            stopping: false,
        })
    }

    /// The server releases this read only after the current position is durable.
    pub fn world(&self) -> &WorldAuthority {
        &self.world
    }

    fn wrap(&self, change: torse_sim::PreparedChange) -> Result<WorldPrepared, Fault> {
        let changes =
            persistent_changes_from_prepared(self.world.registry(), self.world.schemas(), &change)
                .map_err(|error| invariant(error.to_string()))?;
        Ok(WorldPrepared {
            record: CommitRecord {
                meta: changes.meta,
                changes,
            },
            change,
        })
    }
}

impl HostAuthority for WorldHost {
    type Model = LocalWorldModel;
    type Prepared = WorldPrepared;

    fn binding(&self) -> Binding {
        self.binding
    }

    fn cursor(&self) -> CommitCursor {
        CommitCursor {
            world: self.world.world(),
            seq: self.world.seq(),
        }
    }

    fn resume_point(&self) -> ResumePoint {
        self.world.coordinator().resume_point()
    }

    fn is_tick_start(&self, point: ResumePoint) -> bool {
        tick_start(point)
    }

    fn submit(&mut self, request: ModelRequest<Self::Model>) -> Admission {
        match request.input {}
    }

    fn prepare_next(&mut self) -> Result<Option<Self::Prepared>, Fault> {
        if let Some(prepared) = self.prepared.take() {
            return Ok(Some(prepared));
        }
        if self.stopping && !self.at_tick_boundary() {
            let change = self
                .world
                .prepare_phase_advance()
                .map_err(|error| invariant(error.to_string()))?;
            return self.wrap(change).map(Some);
        }
        Ok(None)
    }

    fn publish(&mut self, prepared: Self::Prepared) -> Result<Publication, Fault> {
        let publication = self
            .world
            .publish(prepared.change)
            .map_err(|error| invariant(error.to_string()))?;
        Ok(Publication {
            at: publication.cursor,
            invalidated_replicas: Vec::new(),
        })
    }

    fn capture_snapshot(
        &mut self,
        budget: OutputBudget,
    ) -> Result<ModelSnapshot<Self::Model>, Fault> {
        let meta = SnapshotMeta {
            cursor: self.cursor(),
            compatibility: self.binding.compatibility,
            resume_at: self.resume_point(),
        };
        // The bounded world capture counts Snapshot itself; additionally charge the host
        // wrapper's duplicate metadata and alignment before cloning any payload.
        let extra = size_of::<ModelSnapshot<Self::Model>>() - size_of::<Snapshot>();
        let budget = OutputBudget {
            max_items: budget.max_items,
            max_bytes: budget
                .max_bytes
                .checked_sub(extra as u64)
                .ok_or_else(|| Fault::resource_limit("snapshot wrapper exceeds budget"))?,
        };
        let state = self.world.capture_snapshot_with_budget(meta, budget)?;
        Ok(WorldSnapshot { meta, state })
    }

    fn stop_admission(&mut self, _: Tick) -> Result<(), Fault> {
        self.stopping = true;
        Ok(())
    }

    fn shutdown_ready(&self) -> bool {
        self.stopping && self.prepared.is_none() && self.at_tick_boundary()
    }
}

/// A local attempt either rejects without mutation, blocks before publication, or is durable.
#[derive(Debug)]
pub enum LocalCommit {
    Rejected(RejectCode),
    Blocked(StageError),
    Durable(DurablePublication),
}

impl<J: CommitJournal<LocalWorldModel>, S: HostCheckpointStore<LocalWorldModel>>
    Server<WorldHost, J, S>
{
    /// Commit one trusted local plan through the same staging and durability gates as B4.
    /// A blocked attempt leaves the borrowed input and world unchanged for caller retry.
    /// There is no request deduplication or crash-safe queued-input promise in this API.
    pub fn commit_transaction(&mut self, transaction: &Transaction) -> Result<LocalCommit, Fault> {
        self.authority()?;
        let change = match self.authority.world.prepare(transaction) {
            Ok(change) => change,
            Err(PrepareError::Rejected(code)) => return Ok(LocalCommit::Rejected(code)),
            Err(PrepareError::SequenceExhausted) => {
                return Err(self.fail(invariant("world commit sequence exhausted")));
            }
        };
        self.commit_local(change)
    }

    /// Declare the current phase settled and durably advance one whole phase.
    /// This primitive does not run systems, schedule events, or persist queued requests.
    pub fn advance_phase(&mut self) -> Result<LocalCommit, Fault> {
        self.authority()?;
        let change = match self.authority.world.prepare_phase_advance() {
            Ok(change) => change,
            Err(error) => return Err(self.fail(invariant(error.to_string()))),
        };
        self.commit_local(change)
    }

    fn commit_local(&mut self, change: torse_sim::PreparedChange) -> Result<LocalCommit, Fault> {
        self.authority.prepared = Some(match self.authority.wrap(change) {
            Ok(prepared) => prepared,
            Err(error) => return Err(self.fail(error)),
        });
        match self.drive_one(&mut NoReplicas)? {
            CommitStep::Durable(value) => Ok(LocalCommit::Durable(value)),
            CommitStep::Blocked(error) => Ok(LocalCommit::Blocked(error)),
            CommitStep::Idle => Err(self.fail(invariant("local preparation disappeared"))),
        }
    }
}

/// Isolated real-world recovery using the current process's compiled content.
pub struct WorldHostRecovery<'a> {
    binding: Binding,
    recovery: Recovery<'a>,
    resume: Option<ResumePoint>,
}

impl<'a> WorldHostRecovery<'a> {
    pub fn new(
        binding: Binding,
        registry: &'a BlockStateRegistry,
        schemas: &'a ComponentSchemas,
    ) -> Self {
        Self {
            binding,
            recovery: Recovery::new(binding.world, registry, schemas),
            resume: None,
        }
    }

    fn validate(
        &self,
        compatibility: torse_types::Compatibility,
        point: ResumePoint,
    ) -> Result<(), Fault> {
        if compatibility != self.binding.compatibility {
            return Err(Fault::incompatible("local recovery compatibility mismatch"));
        }
        if !phase_boundary(point) {
            return Err(Fault::invalid_data(
                "local recovery requires a whole-phase boundary",
            ));
        }
        Ok(())
    }
}

impl RecoveryTarget for WorldHostRecovery<'_> {
    type Model = LocalWorldModel;
    type Authority = WorldHost;

    fn restore_snapshot(&mut self, snapshot: ModelSnapshot<Self::Model>) -> Result<(), Fault> {
        if snapshot.meta != snapshot.state.meta {
            return Err(Fault::invalid_data("snapshot metadata mismatch"));
        }
        self.validate(snapshot.meta.compatibility, snapshot.meta.resume_at)?;
        if !tick_start(snapshot.meta.resume_at) {
            return Err(Fault::invalid_data(
                "local checkpoint requires a tick boundary",
            ));
        }
        self.recovery
            .restore_snapshot(&snapshot.state)
            .map_err(|error| Fault::invalid_data(error.to_string()))?;
        self.resume = Some(snapshot.meta.resume_at);
        Ok(())
    }

    fn replay(&mut self, record: ModelCommit<Self::Model>) -> Result<(), Fault> {
        if record.meta != record.changes.meta {
            return Err(Fault::invalid_data("commit metadata mismatch"));
        }
        self.validate(record.meta.compatibility, record.meta.resume_at)?;
        let previous = self
            .resume
            .ok_or_else(|| Fault::invalid_data("missing local snapshot"))?;
        let point = record.meta.resume_at;
        if previous != point {
            let changes = &record.changes;
            if next_resume_point(previous) != Some(point)
                || !changes.edits.is_empty()
                || !changes.entity_moves.is_empty()
                || !changes.component_edits.is_empty()
                || changes.entity_lifecycle.is_some()
            {
                return Err(Fault::invalid_data("invalid local phase progress record"));
            }
        }
        self.recovery
            .replay(&record.changes)
            .map_err(|error| Fault::invalid_data(error.to_string()))?;
        self.resume = Some(point);
        Ok(())
    }

    fn finish(self, expected: CommitCursor) -> Result<Self::Authority, Fault> {
        if self.recovery.cursor() != expected {
            return Err(Fault::invalid_data("local recovery cursor mismatch"));
        }
        let world = self
            .recovery
            .finish()
            .map_err(|error| Fault::invalid_data(error.to_string()))?;
        WorldHost::new(self.binding, world)
    }
}

fn tick_start(point: ResumePoint) -> bool {
    point.next_phase == 0 && point.next_step == 0
}

fn phase_boundary(point: ResumePoint) -> bool {
    point.next_step == 0 && Phase::from_index(point.next_phase).is_some()
}

fn invariant(context: impl Into<String>) -> Fault {
    Fault::new(FaultCode::InvalidState, context)
}
