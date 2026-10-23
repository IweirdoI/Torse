//! Single-coordinator commit protocol.
//!
//! One coordinator owns the authoritative commit sequence. Processing a
//! transaction is a three-step protocol:
//!
//! 1. Revalidate every recorded read against the current authority.
//! 2. Ask storage to apply all writes as one validated group.
//! 3. Only then allocate the next commit sequence and return the record.
//!
//! A rejected transaction consumes no sequence number and changes nothing. A
//! sequence overflow is a terminal condition rather than a rejection: continuing
//! would reuse numbers that recovery and replication rely on being unique.

use torse_types::{
    BlockPos, CommitCursor, CommitSeq, RejectCode, TransactionId, TransactionOutcome,
    TransactionResult, WorldId,
};
use torse_types::{Revision, SectionPos};
use torse_world::{BlockEdit, SectionStore, StoreError, TerrainQuery};

use crate::transaction::Transaction;

/// Changes produced by one committed transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Committed {
    /// Commit position assigned to this transaction.
    pub cursor: CommitCursor,
    /// Ordered writes that were applied.
    pub edits: Vec<BlockEdit>,
    /// Sections touched by the commit, in coordinate order.
    pub touched: Vec<(SectionPos, Revision)>,
}

/// Terminal failure that stops the world instead of rejecting one transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitFault {
    /// The commit sequence cannot advance without wrapping.
    SequenceExhausted,
}

impl std::fmt::Display for CommitFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SequenceExhausted => write!(f, "commit sequence exhausted"),
        }
    }
}

impl std::error::Error for CommitFault {}

/// Owner of the world's commit order.
#[derive(Clone, Copy, Debug)]
pub struct CommitCoordinator {
    world: WorldId,
    seq: CommitSeq,
}

impl CommitCoordinator {
    /// Create a coordinator at the baseline commit position.
    pub const fn new(world: WorldId) -> Self {
        Self {
            world,
            seq: CommitSeq::INITIAL,
        }
    }

    /// Last committed position.
    pub const fn seq(&self) -> CommitSeq {
        self.seq
    }

    /// World identity used in commit cursors.
    pub const fn world(&self) -> WorldId {
        self.world
    }

    /// Commit one transaction at the current position.
    ///
    /// A rejected transaction returns `Rejected` and consumes no sequence
    /// number. Only a fault stops the coordinator.
    pub fn commit(
        &mut self,
        store: &mut SectionStore,
        transaction: &Transaction,
    ) -> Result<TransactionResult<Committed>, CommitFault> {
        self.try_commit(store, transaction)
    }

    /// Commit transactions in deterministic order.
    ///
    /// The caller supplies unsorted work; ordering happens here so that every
    /// host derives the same sequence from the same transactions.
    pub fn commit_ordered(
        &mut self,
        store: &mut SectionStore,
        transactions: Vec<Transaction>,
    ) -> Result<Vec<TransactionResult<Committed>>, CommitFault> {
        let mut results = Vec::with_capacity(transactions.len());
        for transaction in Transaction::order(transactions) {
            results.push(self.commit(store, &transaction)?);
        }
        Ok(results)
    }

    fn try_commit(
        &mut self,
        store: &mut SectionStore,
        transaction: &Transaction,
    ) -> Result<TransactionResult<Committed>, CommitFault> {
        if transaction.mutations.is_empty() {
            return Ok(rejected(transaction.id, RejectCode::InvalidData));
        }
        if has_duplicate_targets(transaction) {
            return Ok(rejected(transaction.id, RejectCode::InvalidData));
        }

        // Step 1: revalidate the read set.
        for precondition in &transaction.preconditions {
            match store.query(precondition.pos) {
                TerrainQuery::Ready(current) if current == precondition.expected => {}
                TerrainQuery::Ready(_) => {
                    return Ok(rejected(transaction.id, RejectCode::Conflict));
                }
                TerrainQuery::NotLoaded => {
                    return Ok(rejected(transaction.id, RejectCode::NotLoaded));
                }
                TerrainQuery::OutOfBounds => {
                    return Ok(rejected(transaction.id, RejectCode::MissingTarget));
                }
            }
        }

        // Step 2: apply the write set as one validated group.
        let edits: Vec<BlockEdit> = transaction
            .mutations
            .iter()
            .map(|mutation| BlockEdit {
                pos: mutation.pos,
                state: mutation.state,
            })
            .collect();
        let touched = match store.apply_edits(&edits) {
            Ok(touched) => touched,
            Err(error) => {
                return Ok(rejected(transaction.id, code_for(&error)));
            }
        };

        // Step 3: allocate the commit position.
        let Some(next) = self.seq.next() else {
            return Err(CommitFault::SequenceExhausted);
        };
        self.seq = next;
        let cursor = CommitCursor {
            world: self.world,
            seq: next,
        };
        Ok(TransactionResult {
            transaction: transaction.id,
            outcome: TransactionOutcome::Committed {
                at: cursor,
                value: Committed {
                    cursor,
                    edits,
                    touched,
                },
            },
        })
    }
}

fn rejected<T>(transaction: TransactionId, code: RejectCode) -> TransactionResult<T> {
    TransactionResult {
        transaction,
        outcome: TransactionOutcome::Rejected(code),
    }
}

fn has_duplicate_targets(transaction: &Transaction) -> bool {
    let mut targets: Vec<BlockPos> = Vec::with_capacity(transaction.mutations.len());
    for mutation in &transaction.mutations {
        if targets.contains(&mutation.pos) {
            return true;
        }
        targets.push(mutation.pos);
    }
    false
}

fn code_for(error: &StoreError) -> RejectCode {
    match error {
        StoreError::NotLoaded { .. }
        | StoreError::UnknownSection { .. }
        | StoreError::NotEditable { .. } => RejectCode::NotLoaded,
        StoreError::OutOfBounds { .. } => RejectCode::InvalidData,
        StoreError::Occupied { .. } | StoreError::EmptyCell { .. } => RejectCode::Conflict,
        _ => RejectCode::InvalidData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torse_types::ContentKey;
    use torse_world::{BlockStateId, Section, WorldBounds};

    use crate::phase::Phase;
    use crate::transaction::OrderingKey;

    fn store() -> SectionStore {
        let bounds = WorldBounds::new(0, 64).expect("bounds are valid");
        let mut store = SectionStore::new(16, bounds).expect("side is valid");
        for section in [SectionPos::ZERO, SectionPos::new(1, 0, 0)] {
            store.request(section).expect("request is accepted");
            store
                .install(section, Section::empty(16).expect("side is valid"))
                .expect("install is accepted");
            store.finish_load(section).expect("load finishes");
        }
        store
    }

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key is valid")
    }

    fn tx(id: u128) -> Transaction {
        Transaction::new(
            TransactionId(id),
            key("base:test"),
            OrderingKey {
                phase: Phase::Simulation,
                rule_priority: 0,
                stable_task_key: 0,
                local_sequence: 0,
            },
        )
    }

    fn committed(result: &TransactionResult<Committed>) -> &Committed {
        match &result.outcome {
            TransactionOutcome::Committed { value, .. } => value,
            TransactionOutcome::Rejected(code) => panic!("expected commit, got {code:?}"),
        }
    }

    fn rejection(result: &TransactionResult<Committed>) -> RejectCode {
        match result.outcome {
            TransactionOutcome::Rejected(code) => code,
            TransactionOutcome::Committed { .. } => panic!("expected rejection"),
        }
    }

    #[test]
    fn commit_applies_writes_and_advances_the_sequence() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(7));
        let mut transaction = tx(1);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(5));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        let record = committed(&result);
        assert_eq!(record.cursor.seq, CommitSeq(1));
        assert_eq!(coordinator.seq(), CommitSeq(1));
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId(5))
        );
    }

    #[test]
    fn stale_read_is_a_conflict_and_consumes_nothing() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        store
            .edit(BlockPos::new(1, 1, 1), BlockStateId(9))
            .expect("setup edit");

        let mut transaction = tx(2);
        transaction
            .read_block(BlockPos::new(1, 1, 1), BlockStateId::EMPTY)
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(rejection(&result), RejectCode::Conflict);
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId(9)),
            "a rejected transaction must not write"
        );
    }

    #[test]
    fn two_readers_of_one_cell_commit_in_order_and_the_second_conflicts() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut first = tx(10);
        first
            .read_block(BlockPos::new(2, 1, 2), BlockStateId::EMPTY)
            .write_block(BlockPos::new(2, 1, 2), BlockStateId(4));
        let mut second = tx(11);
        second
            .read_block(BlockPos::new(2, 1, 2), BlockStateId::EMPTY)
            .write_block(BlockPos::new(2, 1, 2), BlockStateId(6));

        let results = coordinator
            .commit_ordered(&mut store, vec![second, first])
            .expect("commits run");
        // Identical ordering keys fall back to transaction identity, so the
        // lower identity wins the cell and the later one reports a conflict.
        assert_eq!(committed(&results[0]).cursor.seq, CommitSeq(1));
        assert_eq!(rejection(&results[1]), RejectCode::Conflict);
        assert_eq!(
            store.query(BlockPos::new(2, 1, 2)),
            TerrainQuery::Ready(BlockStateId(4)),
            "the lower transaction identity wins the tie"
        );
    }

    #[test]
    fn different_cells_in_one_section_do_not_conflict() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut first = tx(20);
        first
            .read_block(BlockPos::new(1, 1, 1), BlockStateId::EMPTY)
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
        let mut second = tx(21);
        second
            .read_block(BlockPos::new(2, 1, 1), BlockStateId::EMPTY)
            .write_block(BlockPos::new(2, 1, 1), BlockStateId(4));
        let results = coordinator
            .commit_ordered(&mut store, vec![first, second])
            .expect("commits run");
        assert_eq!(committed(&results[0]).cursor.seq, CommitSeq(1));
        assert_eq!(committed(&results[1]).cursor.seq, CommitSeq(2));
    }

    #[test]
    fn write_into_an_unloaded_section_changes_nothing() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(30);
        transaction
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(3))
            .write_block(BlockPos::new(40, 1, 1), BlockStateId(3));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(rejection(&result), RejectCode::NotLoaded);
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId::EMPTY)
        );
    }

    #[test]
    fn read_only_transaction_is_rejected_without_a_sequence() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(40);
        transaction.read_block(BlockPos::ZERO, BlockStateId::EMPTY);
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(rejection(&result), RejectCode::InvalidData);
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
    }

    #[test]
    fn duplicate_targets_are_rejected() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(50);
        transaction
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(3))
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(4));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(rejection(&result), RejectCode::InvalidData);
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
    }

    #[test]
    fn cursor_carries_world_and_sequence() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(99));
        let mut transaction = tx(60);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(2));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(committed(&result).cursor.world, WorldId(99));
        assert_eq!(committed(&result).cursor.seq, CommitSeq(1));
    }

    #[test]
    fn touched_sections_are_reported_for_publishing() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(70);
        transaction
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(2))
            .write_block(BlockPos::new(17, 1, 1), BlockStateId(2));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        let record = committed(&result);
        assert_eq!(record.touched.len(), 2);
        assert_eq!(record.touched[0].0, SectionPos::ZERO);
        assert_eq!(record.touched[1].0, SectionPos::new(1, 0, 0));
    }
}
