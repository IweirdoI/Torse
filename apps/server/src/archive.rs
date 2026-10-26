//! Explicit checkpoint archive migration. Active directories are never modified here.

use torse_codec::{CodecLimits, wire};
use torse_content::CompiledContent;
use torse_sim::{
    SystemSpec,
    durable::{ExecutionArchive, ExecutionSnapshot},
};
use torse_types::{CommitCursor, Fault, OutputBudget, WorldId, WorldSnapshot};
use torse_world::{MigrationPlan, SnapshotArchive};

use crate::{
    HostAuthority, HostCheckpointStore, RecoveryTarget, Server,
    execution::{ExecutionHost, ExecutionModel, ExecutionRecovery},
};

impl<J: torse_storage::CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>
    Server<ExecutionHost, J, S>
{
    /// Export only a healthy, fully durable tick boundary, without writing files.
    pub fn capture_archive(
        &mut self,
        content: &CompiledContent,
        budget: OutputBudget,
    ) -> Result<ExecutionArchive, Fault> {
        self.authority()?;
        if self.durability().through.0 != self.authority.cursor().seq.0 {
            return Err(Fault::invalid_data(
                "archive export requires a durable boundary",
            ));
        }
        self.authority.capture_archive(content, budget)
    }
}

pub(crate) fn capture(
    snapshot: ExecutionSnapshot,
    source: &CompiledContent,
    budget: OutputBudget,
    limits: CodecLimits,
) -> Result<ExecutionArchive, Fault> {
    if !snapshot.parked.is_empty() {
        return Err(Fault::incompatible(
            "portable archive requires all parked sections to be loaded",
        ));
    }
    let archive = ExecutionArchive {
        world: SnapshotArchive::capture(&snapshot.world, source, budget)?,
        runtime: snapshot.runtime,
    };
    wire::measure_inner(&archive, budget, limits)?;
    Ok(archive)
}

/// Migrate a quiescent checkpoint into a separate candidate. Pending events or
/// frozen work may encode old schema semantics and cannot be silently carried
/// across a compatibility transition. They remain preserved in the source.
pub fn migrate(
    source: &ExecutionArchive,
    target: &CompiledContent,
    target_rules: &[SystemSpec],
    plan: &MigrationPlan,
    budget: OutputBudget,
    limits: CodecLimits,
) -> Result<ExecutionArchive, Fault> {
    wire::measure_inner(source, budget, limits)?;
    if source.runtime.rules.as_ref() != target_rules
        || (plan.source != plan.target
            && (!source.runtime.events.is_empty() || source.runtime.work.is_some()))
    {
        return Err(Fault::incompatible(
            "archive execution requires explicit rule/event migration or quiescence",
        ));
    }
    let candidate = ExecutionArchive {
        world: source.world.migrate(target, plan, budget)?,
        runtime: source.runtime.clone(),
    };
    wire::measure_inner(&candidate, budget, limits)?;
    Ok(candidate)
}

/// Prepare an explicit new-world import, preserving entity identity allocation
/// and tick while starting a new commit/session namespace. The original archive
/// retains its receipts and history. Never reuse the original world identity.
pub fn fork_for_import(
    source: &ExecutionArchive,
    new_world: WorldId,
    target: &CompiledContent,
    budget: OutputBudget,
    limits: CodecLimits,
) -> Result<ExecutionArchive, Fault> {
    wire::measure_inner(source, budget, limits)?;
    source.world.validate_for_restore(target, budget)?;
    if new_world == source.world.snapshot.meta.cursor.world
        || source.world.snapshot.meta.resume_at.next_phase != 0
        || source.world.snapshot.meta.resume_at.next_step != 0
        || !source.runtime.events.is_empty()
        || source.runtime.work.is_some()
    {
        return Err(Fault::incompatible(
            "archive import requires a new world identity and quiescent tick boundary",
        ));
    }
    let mut candidate = source.clone();
    candidate.world.snapshot.meta.cursor = CommitCursor::initial(new_world);
    candidate.runtime.windows.clear();
    wire::measure_inner(&candidate, budget, limits)?;
    Ok(candidate)
}

impl ExecutionRecovery<'_> {
    /// Restore an available, fully migrated archive through the normal isolated
    /// recovery builder. No archive can bypass execution or domain validation.
    pub fn restore_archive(
        &mut self,
        archive: &ExecutionArchive,
        target: &CompiledContent,
        budget: OutputBudget,
        limits: CodecLimits,
    ) -> Result<(), Fault> {
        wire::measure_inner(archive, budget, limits)?;
        archive.world.validate_for_restore(target, budget)?;
        self.restore_snapshot(WorldSnapshot {
            meta: archive.world.snapshot.meta,
            state: ExecutionSnapshot {
                world: archive.world.snapshot.clone(),
                runtime: archive.runtime.clone(),
                parked: Default::default(),
            },
        })
    }
}
