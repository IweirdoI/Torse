//! Headless commit-arbitration scenario.
//!
//! Two producers read the same empty cell and both try to fill it. The
//! coordinator must let exactly one commit, reject the other with `Conflict`, and
//! leave the world in the winner's state. A third transaction on a different cell
//! must still succeed, which proves the conflict check is per cell and not per
//! section.
//!
//! This mirrors the M2 acceptance case "two machines compete for the last item"
//! at block granularity, before the inventory module exists.

use torse_content::{compile, m0_fixture_pack};
use torse_sim::{CommitCoordinator, OrderingKey, Phase, Transaction};
use torse_types::SectionPos;
use torse_types::{BlockPos, ContentKey, TransactionId, TransactionOutcome, WorldId};
use torse_world::{
    BlockStateId, BlockStateRegistry, Section, SectionStore, TerrainQuery, WorldBounds,
};

/// Result counters reported by the scenario.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CommitScenarioReport {
    /// Transactions that committed.
    pub committed: usize,
    /// Transactions rejected because their read set no longer held.
    pub conflicts: usize,
    /// Final commit sequence.
    pub commit_seq: u64,
    /// Commit position of the winning transaction.
    pub winner_seq: u64,
    /// State of the contested cell after arbitration.
    pub contested_state: u16,
    /// Whether the unrelated transaction also committed.
    pub unrelated_committed: bool,
}

/// Failure produced by the scenario.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitScenarioError {
    /// Human-readable explanation.
    pub detail: String,
}

impl CommitScenarioError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for CommitScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for CommitScenarioError {}

/// Run the arbitration scenario.
pub fn run() -> Result<CommitScenarioReport, CommitScenarioError> {
    let content = compile(m0_fixture_pack())
        .map_err(|errors| CommitScenarioError::new(format!("content failed: {errors:?}")))?;
    let registry = BlockStateRegistry::build(&content)
        .map_err(|error| CommitScenarioError::new(format!("registry failed: {error}")))?;
    let stone = registry
        .default_state(&key("base:stone"))
        .ok_or_else(|| CommitScenarioError::new("missing base:stone"))?;
    let glass = registry
        .default_state(&key("base:glass_slab"))
        .ok_or_else(|| CommitScenarioError::new("missing base:glass_slab"))?;

    let bounds = WorldBounds::new(0, 64)
        .map_err(|error| CommitScenarioError::new(format!("bounds failed: {error}")))?;
    let mut store = SectionStore::new(16, bounds)
        .map_err(|error| CommitScenarioError::new(format!("store failed: {error}")))?;
    for section in [SectionPos::ZERO, SectionPos::new(1, 0, 0)] {
        store
            .request(section)
            .map_err(|error| CommitScenarioError::new(format!("request failed: {error}")))?;
        store
            .install(
                section,
                Section::empty(16).map_err(|error| {
                    CommitScenarioError::new(format!("section failed: {error}"))
                })?,
            )
            .map_err(|error| CommitScenarioError::new(format!("install failed: {error}")))?;
        store
            .finish_load(section)
            .map_err(|error| CommitScenarioError::new(format!("finish failed: {error}")))?;
    }

    let contested = BlockPos::new(3, 1, 3);
    let unrelated = BlockPos::new(4, 1, 3);

    let mut first = transaction(1, 0);
    first
        .read_block(contested, BlockStateId::EMPTY)
        .write_block(contested, stone);
    let mut second = transaction(2, 1);
    second
        .read_block(contested, BlockStateId::EMPTY)
        .write_block(contested, glass);
    let mut third = transaction(3, 2);
    third
        .read_block(unrelated, BlockStateId::EMPTY)
        .write_block(unrelated, stone);

    let mut coordinator = CommitCoordinator::new(WorldId(42));
    let results = coordinator
        .commit_ordered(&mut store, vec![second, third, first])
        .map_err(|fault| CommitScenarioError::new(format!("coordinator fault: {fault}")))?;

    let mut report = CommitScenarioReport {
        commit_seq: coordinator.seq().0,
        contested_state: 0,
        ..CommitScenarioReport::default()
    };
    for result in &results {
        match &result.outcome {
            TransactionOutcome::Committed { at, .. } => {
                report.committed += 1;
                if result.transaction == TransactionId(1) {
                    report.winner_seq = at.seq.0;
                }
                if result.transaction == TransactionId(3) {
                    report.unrelated_committed = true;
                }
            }
            TransactionOutcome::Rejected(_) => report.conflicts += 1,
        }
    }

    match store.query(contested) {
        TerrainQuery::Ready(state) => report.contested_state = state.0,
        other => {
            return Err(CommitScenarioError::new(format!(
                "contested cell is not readable: {other:?}"
            )));
        }
    }

    // The assertions are part of the scenario, not a test-only wrapper: the
    // command exits nonzero when arbitration regresses.
    if report.committed != 2 || report.conflicts != 1 {
        return Err(CommitScenarioError::new(format!(
            "expected two commits and one conflict, got {report:?}"
        )));
    }
    if report.contested_state != stone.0 {
        return Err(CommitScenarioError::new(
            "the winning transaction did not own the contested cell",
        ));
    }
    if !report.unrelated_committed {
        return Err(CommitScenarioError::new(
            "an unrelated cell must not conflict with the contested one",
        ));
    }
    if report.commit_seq != 2 {
        return Err(CommitScenarioError::new(
            "a rejected transaction must not consume a commit sequence",
        ));
    }
    Ok(report)
}

fn transaction(id: u128, stable: u64) -> Transaction {
    let ordering = OrderingKey {
        phase: Phase::Interaction,
        rule_priority: 0,
        stable_task_key: stable,
        local_sequence: 0,
    };
    let source = key("base:scenario");
    Transaction::new(TransactionId(id), source, ordering)
}

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("scenario keys are valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contender_loses_without_consuming_a_sequence() {
        let report = run().expect("scenario completes");
        assert_eq!(report.committed, 2);
        assert_eq!(report.conflicts, 1);
        assert_eq!(report.commit_seq, 2);
        assert_eq!(report.winner_seq, 1);
        assert!(report.unrelated_committed);
    }
}
