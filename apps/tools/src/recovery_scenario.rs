//! Recovery scenario: snapshot plus journal tail rebuilds the same authority.
//!
//! This is the A-side path a server takes on startup: capture a snapshot at a
//! commit position, keep applying commits, then restore from the snapshot and
//! replay the recorded changes. The scenario asserts that the recovered world
//! matches the live one and that commit numbering continues.

use torse_content::{compile, m0_fixture_pack};
use torse_sim::{
    OrderingKey, Phase, Recovery, Transaction, WorldAuthority, changes_from_committed,
};
use torse_types::{
    BlockPos, CommitCursor, Compatibility, ContentKey, Digest, ResumePoint, SectionPos,
    SnapshotMeta, Tick, TransactionId, TransactionOutcome, WorldId,
};
use torse_world::{
    BlockStateId, BlockStateRegistry, ComponentSchemas, Section, TerrainQuery, WorldBounds,
};

/// Result counters reported by the scenario.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RecoveryReport {
    /// Commit position of the captured snapshot.
    pub snapshot_seq: u64,
    /// Commit position after the journal tail was replayed.
    pub recovered_seq: u64,
    /// Live position before recovery started.
    pub live_seq: u64,
    /// Cells compared between the live and recovered worlds.
    pub cells_checked: usize,
    /// Whether every cell matched.
    pub restored_equal: bool,
    /// Commit position after one more commit on the recovered authority.
    pub continued_seq: u64,
}

/// Failure produced by the scenario.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryScenarioError {
    /// Human-readable explanation.
    pub detail: String,
}

impl RecoveryScenarioError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for RecoveryScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for RecoveryScenarioError {}

/// Run the recovery scenario.
pub fn run() -> Result<RecoveryReport, RecoveryScenarioError> {
    let content = compile(m0_fixture_pack())
        .map_err(|errors| RecoveryScenarioError::new(format!("content failed: {errors:?}")))?;
    let registry = BlockStateRegistry::build(&content)
        .map_err(|error| RecoveryScenarioError::new(format!("registry failed: {error}")))?;
    let schemas = ComponentSchemas::build(&content);
    let stone = registry
        .default_state(&key("base:stone"))
        .ok_or_else(|| RecoveryScenarioError::new("missing base:stone"))?;
    let glass = registry
        .default_state(&key("base:glass_slab"))
        .ok_or_else(|| RecoveryScenarioError::new("missing base:glass_slab"))?;

    let compatibility = Compatibility {
        authority_schema: 1,
        rules_digest: Digest([4; 32]),
        content_digest: content.presentation_digest,
    };
    let mut authority = WorldAuthority::new(
        WorldId(21),
        registry,
        schemas,
        16,
        WorldBounds::new(0, 64)
            .map_err(|error| RecoveryScenarioError::new(format!("bounds failed: {error}")))?,
        compatibility,
    )
    .map_err(|error| RecoveryScenarioError::new(format!("authority failed: {error}")))?;

    for pos in [
        SectionPos::new(-1, 0, 0),
        SectionPos::ZERO,
        SectionPos::new(1, 0, 0),
    ] {
        authority
            .store_mut()
            .request(pos)
            .map_err(|error| RecoveryScenarioError::new(format!("request failed: {error}")))?;
        authority
            .store_mut()
            .install(pos, Section::empty(16).map_err(section_error)?)
            .map_err(|error| RecoveryScenarioError::new(format!("install failed: {error}")))?;
        authority
            .store_mut()
            .finish_load(pos)
            .map_err(|error| RecoveryScenarioError::new(format!("finish failed: {error}")))?;
    }

    let first = transaction(1, BlockPos::new(1, 1, 1), stone);
    authority
        .commit(&first)
        .map_err(|fault| RecoveryScenarioError::new(format!("commit failed: {fault}")))?;

    let snapshot = authority
        .capture_snapshot(SnapshotMeta {
            cursor: CommitCursor {
                world: authority.world(),
                seq: authority.seq(),
            },
            compatibility,
            resume_at: ResumePoint {
                tick: Tick(3),
                next_phase: Phase::Simulation.index_u16(),
                next_step: 0,
            },
        })
        .map_err(|error| RecoveryScenarioError::new(format!("snapshot failed: {error}")))?;

    let mut report = RecoveryReport {
        snapshot_seq: snapshot.meta.cursor.seq.0,
        ..RecoveryReport::default()
    };

    // Two more commits after the snapshot; only their payloads enter the tail.
    let mut tail = Vec::new();
    for (id, pos, state) in [
        (2u128, BlockPos::new(2, 1, 1), glass),
        (3, BlockPos::new(-3, 1, 2), stone),
    ] {
        let committed = authority
            .commit(&transaction(id, pos, state))
            .map_err(|fault| RecoveryScenarioError::new(format!("commit failed: {fault}")))?;
        match &committed.outcome {
            TransactionOutcome::Committed { value, .. } => tail.push(
                changes_from_committed(authority.registry(), value).map_err(|error| {
                    RecoveryScenarioError::new(format!("payload failed: {error}"))
                })?,
            ),
            TransactionOutcome::Rejected(code) => {
                return Err(RecoveryScenarioError::new(format!(
                    "transaction was rejected: {code:?}"
                )));
            }
        }
    }
    report.live_seq = authority.seq().0;

    let mut recovery: Recovery<'_> = authority.begin_restore();
    recovery
        .restore_snapshot(&snapshot)
        .map_err(|error| RecoveryScenarioError::new(format!("restore failed: {error}")))?;
    for change in &tail {
        recovery
            .replay(change)
            .map_err(|error| RecoveryScenarioError::new(format!("replay failed: {error}")))?;
    }
    report.recovered_seq = recovery.seq().0;
    let mut recovered = recovery
        .finish()
        .map_err(|error| RecoveryScenarioError::new(format!("finish failed: {error}")))?;

    // Compare every section that the live authority holds.
    let mut equal = true;
    for pos in authority.store().positions() {
        let live = authority
            .store()
            .section(pos)
            .ok_or_else(|| RecoveryScenarioError::new("live section missing"))?;
        let restored = recovered
            .store()
            .section(pos)
            .ok_or_else(|| RecoveryScenarioError::new("recovered section missing"))?;
        if !live.values().eq(restored.values()) {
            equal = false;
        }
        report.cells_checked += live.cell_count();
    }
    report.restored_equal = equal;
    if !equal {
        return Err(RecoveryScenarioError::new(
            "recovered world does not match the live world",
        ));
    }
    if report.recovered_seq != report.live_seq {
        return Err(RecoveryScenarioError::new(
            "recovered position does not match the live position",
        ));
    }

    // A recovered authority must keep numbering from the journal tail.
    recovered
        .commit(&transaction(4, BlockPos::new(4, 1, 1), stone))
        .map_err(|fault| RecoveryScenarioError::new(format!("continued commit failed: {fault}")))?;
    report.continued_seq = recovered.seq().0;
    if report.continued_seq != report.live_seq + 1 {
        return Err(RecoveryScenarioError::new(
            "recovered authority did not continue the sequence",
        ));
    }

    // A rejected transaction on the recovered world changes nothing.
    let mut stale = transaction(5, BlockPos::new(1, 1, 1), glass);
    stale
        .read_block(BlockPos::new(1, 1, 1), BlockStateId::EMPTY)
        .write_block(BlockPos::new(1, 1, 1), glass);
    let rejected = recovered
        .commit(&stale)
        .map_err(|fault| RecoveryScenarioError::new(format!("commit failed: {fault}")))?;
    if !matches!(rejected.outcome, TransactionOutcome::Rejected(_)) {
        return Err(RecoveryScenarioError::new(
            "a stale read must be rejected after recovery",
        ));
    }
    if recovered.store().query(BlockPos::new(1, 1, 1)) != TerrainQuery::Ready(stone) {
        return Err(RecoveryScenarioError::new(
            "rejected transaction changed the recovered world",
        ));
    }
    Ok(report)
}

fn transaction(id: u128, pos: BlockPos, state: BlockStateId) -> Transaction {
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
    transaction
}

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("scenario keys are valid")
}

fn section_error(error: torse_world::SectionError) -> RecoveryScenarioError {
    RecoveryScenarioError::new(format!("section failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_scenario_matches_the_live_world() {
        let report = run().expect("scenario completes");
        assert_eq!(report.snapshot_seq, 1);
        assert_eq!(report.live_seq, 3);
        assert_eq!(report.recovered_seq, 3);
        assert_eq!(report.continued_seq, 4);
        assert!(report.restored_equal);
        assert!(report.cells_checked > 0);
    }
}
