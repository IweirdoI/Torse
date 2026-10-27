//! Exchange scenario: capture a world, restore it, and replay committed changes.
//!
//! The scenario checks the A-side payload contract end to end without any
//! encoding: a snapshot must reproduce every cell and entity, and a change set
//! converted from a committed transaction must reproduce the same cells when
//! applied to the restored world.

use torse_content::{compile, m0_fixture_pack};
use torse_sim::{CommitCoordinator, OrderingKey, Phase, Transaction, changes_from_committed};
use torse_types::{
    BlockPos, CommitCursor, CommitSeq, Compatibility, ContentKey, Digest, ResumePoint, SectionPos,
    SnapshotMeta, Tick, TransactionId, TransactionOutcome, WorldId,
};
use torse_world::{
    BlockStateId, BlockStateRegistry, CellContent, ComponentSchemas, EntityStore, Position,
    Section, SectionStore, Snapshot, TerrainQuery, WorldBounds,
};

/// Result counters reported by the scenario.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ExchangeReport {
    /// Sections in the snapshot.
    pub sections: usize,
    /// Stable state entries in the snapshot.
    pub states: usize,
    /// Entities in the snapshot.
    pub entities: usize,
    /// Cells compared after restore.
    pub cells_checked: usize,
    /// Committed changes replayed onto the restored world.
    pub changes: usize,
    /// Final commit sequence.
    pub commit_seq: u64,
    /// Whether every restored section matched the source.
    pub restored_equal: bool,
    /// Whether every replayed change matched the source.
    pub replayed_equal: bool,
}

/// Failure produced by the scenario.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExchangeScenarioError {
    /// Human-readable explanation.
    pub detail: String,
}

impl ExchangeScenarioError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ExchangeScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for ExchangeScenarioError {}

/// Run the exchange scenario.
pub fn run() -> Result<ExchangeReport, ExchangeScenarioError> {
    let content = compile(m0_fixture_pack())
        .map_err(|errors| ExchangeScenarioError::new(format!("content failed: {errors:?}")))?;
    let registry = BlockStateRegistry::build(&content)
        .map_err(|error| ExchangeScenarioError::new(format!("registry failed: {error}")))?;
    let schemas = ComponentSchemas::build(&content);
    let stone = registry
        .default_state(&key("base:stone"))
        .ok_or_else(|| ExchangeScenarioError::new("missing base:stone"))?;
    let glass = registry
        .default_state(&key("base:glass_slab"))
        .ok_or_else(|| ExchangeScenarioError::new("missing base:glass_slab"))?;

    let bounds = WorldBounds::new(0, 64)
        .map_err(|error| ExchangeScenarioError::new(format!("bounds failed: {error}")))?;
    let mut store = SectionStore::new(16, bounds)
        .map_err(|error| ExchangeScenarioError::new(format!("store failed: {error}")))?;
    for pos in [
        SectionPos::new(1, 0, 0),
        SectionPos::ZERO,
        SectionPos::new(-1, 0, 0),
    ] {
        store
            .request(pos)
            .map_err(|error| ExchangeScenarioError::new(format!("request failed: {error}")))?;
        store
            .install(pos, Section::empty(16).map_err(section_error)?)
            .map_err(|error| ExchangeScenarioError::new(format!("install failed: {error}")))?;
        store
            .finish_load(pos)
            .map_err(|error| ExchangeScenarioError::new(format!("finish failed: {error}")))?;
    }
    for (pos, state) in [
        (BlockPos::new(1, 1, 1), stone),
        (BlockPos::new(2, 1, 1), glass),
        (BlockPos::new(17, 1, 1), stone),
        (BlockPos::new(-3, 1, 2), glass),
        (BlockPos::new(5, 2, 5), stone),
    ] {
        store
            .edit(pos, state)
            .map_err(|error| ExchangeScenarioError::new(format!("edit failed: {error}")))?;
    }

    let mut entities = EntityStore::new(16)
        .map_err(|error| ExchangeScenarioError::new(format!("entity store failed: {error}")))?;
    for (index, center) in [
        Position::new(0.5, 1.5, 0.5),
        Position::new(17.5, 1.5, 1.5),
        Position::new(-2.5, 1.5, 3.5),
    ]
    .iter()
    .enumerate()
    {
        let handle = entities
            .spawn(*center)
            .map_err(|error| ExchangeScenarioError::new(format!("spawn failed: {error}")))?;
        if index == 0 {
            entities
                .components_mut(handle)
                .expect("entity exists")
                .insert(
                    &schemas,
                    &key("base:schema/inventory_content"),
                    Default::default(),
                )
                .map_err(|error| {
                    ExchangeScenarioError::new(format!("component failed: {error}"))
                })?;
        }
    }

    let snapshot_meta = SnapshotMeta {
        cursor: CommitCursor {
            world: WorldId(11),
            seq: CommitSeq::INITIAL,
        },
        compatibility: Compatibility {
            authority_schema: 1,
            rules_digest: Digest([3; 32]),
            content_digest: content_digest(&content),
        },
        resume_at: ResumePoint {
            tick: Tick(0),
            next_phase: 0,
            next_step: 0,
        },
    };
    let snapshot = Snapshot::capture(snapshot_meta, &store, &entities, &registry)
        .map_err(|error| ExchangeScenarioError::new(format!("capture failed: {error}")))?;

    let mut report = ExchangeReport {
        sections: snapshot.sections.len(),
        states: snapshot.states.len(),
        entities: snapshot.entities.len(),
        ..ExchangeReport::default()
    };

    let mut restored = snapshot
        .restore(&registry, &schemas)
        .map_err(|error| ExchangeScenarioError::new(format!("restore failed: {error}")))?;

    // Compare every section's cell layout directly; revisions are run-local and
    // intentionally restart at zero.
    let mut equal = true;
    for pos in store.positions() {
        let source = store
            .section(pos)
            .ok_or_else(|| ExchangeScenarioError::new("source section vanished"))?;
        let target = restored
            .store
            .section(pos)
            .ok_or_else(|| ExchangeScenarioError::new("restored section missing"))?;
        if !source.values().eq(target.values()) {
            equal = false;
        }
        report.cells_checked += source.cell_count();
    }
    report.restored_equal = equal;
    if !equal {
        return Err(ExchangeScenarioError::new(
            "restored sections do not match the source",
        ));
    }
    if restored.entities.len() != entities.len() {
        return Err(ExchangeScenarioError::new(
            "entity count changed across restore",
        ));
    }

    // Commit two edits and replay their stable payload on the restored world.
    let mut coordinator = CommitCoordinator::new(WorldId(11));
    let mut replayed_equal = true;
    for (id, pos, state) in [
        (1u128, BlockPos::new(3, 1, 3), stone),
        (2, BlockPos::new(18, 1, 1), glass),
    ] {
        let mut transaction = Transaction::new(
            TransactionId(id),
            key("base:scenario"),
            OrderingKey {
                phase: Phase::Interaction,
                rule_priority: 0,
                stable_task_key: id as u64,
                local_sequence: 0,
            },
        );
        transaction
            .read_block(pos, BlockStateId::EMPTY)
            .write_block(pos, state);
        let result = coordinator
            .commit(&mut store, &transaction)
            .map_err(|fault| ExchangeScenarioError::new(format!("commit failed: {fault}")))?;
        let committed = match &result.outcome {
            TransactionOutcome::Committed { value, .. } => value,
            TransactionOutcome::Rejected(code) => {
                return Err(ExchangeScenarioError::new(format!(
                    "transaction was rejected: {code:?}"
                )));
            }
        };
        let changes = changes_from_committed(&registry, committed)
            .map_err(|error| ExchangeScenarioError::new(format!("conversion failed: {error}")))?;
        report.changes += 1;
        changes
            .apply(&mut restored.store, &registry)
            .map_err(|error| ExchangeScenarioError::new(format!("apply failed: {error}")))?;
        if changes.edits.iter().any(|edit| {
            !matches!(
                &edit.content,
                CellContent::Block { block, .. } if block == &key("base:stone") || block == &key("base:glass_slab")
            )
        }) {
            return Err(ExchangeScenarioError::new(
                "change payload contains unresolved content",
            ));
        }
        if restored.store.query(pos) == TerrainQuery::Ready(BlockStateId::EMPTY) {
            replayed_equal = false;
        }
    }
    report.replayed_equal = replayed_equal;
    report.commit_seq = coordinator.seq().0;
    if !replayed_equal {
        return Err(ExchangeScenarioError::new("replayed changes did not apply"));
    }

    // The restored world must match the source for the replayed cells too.
    for pos in [BlockPos::new(3, 1, 3), BlockPos::new(18, 1, 1)] {
        if restored.store.query(pos) != store.query(pos) {
            return Err(ExchangeScenarioError::new(
                "restored world diverged from the source after replay",
            ));
        }
    }

    Ok(report)
}

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("scenario keys are valid")
}

fn section_error(error: torse_world::SectionError) -> ExchangeScenarioError {
    ExchangeScenarioError::new(format!("section failed: {error}"))
}

fn content_digest(content: &torse_content::CompiledContent) -> Digest {
    content.presentation_digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_scenario_roundtrips_and_replays() {
        let report = run().expect("scenario completes");
        assert_eq!(report.sections, 3);
        assert!(report.states >= 2, "both block kinds must be registered");
        assert_eq!(report.entities, 3);
        assert_eq!(report.changes, 2);
        assert_eq!(report.commit_seq, 2);
        assert!(report.restored_equal);
        assert!(report.replayed_equal);
    }
}
