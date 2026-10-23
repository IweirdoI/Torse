//! Typed transactions and their deterministic arbitration order.
//!
//! A transaction records what it read and what it wants to change. The
//! coordinator checks the read set against the current authority before applying
//! anything, which is what prevents two systems from spending the same resource.
//!
//! Ordering is part of the contract, not an implementation detail. Transactions
//! are applied by `(phase, rule_priority, stable_task_key, local_sequence)` and,
//! for full totality, by transaction identity. None of those fields depends on a
//! thread number or on the order in which results happened to be produced.

use torse_types::{BlockPos, ContentKey, EntityHandle, EntityId, TransactionId};
use torse_world::{ComponentEdit, Position};

use crate::phase::Phase;

/// One cell a transaction read, with the value it observed.
///
/// The expectation is the block state rather than a global revision, so two
/// transactions touching different cells of the same section do not conflict.
/// Section-level revisions are coarser and can be added later for reads that
/// genuinely depend on a whole section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockPrecondition {
    /// Cell that was read.
    pub pos: BlockPos,
    /// State observed at read time.
    pub expected: torse_world::BlockStateId,
}

/// One cell the transaction wants to write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mutation {
    /// Target cell.
    pub pos: BlockPos,
    /// State to write.
    pub state: torse_world::BlockStateId,
}

/// A requested translation whose final position is resolved during preparation.
///
/// The caller supplies an observed identity and position, never an unchecked
/// final position. Exact bits make signed zero and stale observations explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Movement {
    /// Runtime handle, including its generation.
    pub handle: EntityHandle,
    /// Persistent identity observed through that handle.
    pub id: EntityId,
    /// Source position observed by the producing system.
    pub previous_bits: [u64; 3],
    /// Requested translation, in world block units.
    pub delta_bits: [u64; 3],
    /// Positive collider half extents; the entity position is its center.
    pub half_bits: [u64; 3],
}

/// Stable ordering key of one transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct OrderingKey {
    /// Phase that produced the transaction.
    pub phase: Phase,
    /// Rule priority inside the phase; lower commits first.
    pub rule_priority: i32,
    /// Caller-provided key derived from content and object identity.
    pub stable_task_key: u64,
    /// Sequence inside one task invocation.
    pub local_sequence: u32,
}

/// A prepared change that has not been committed yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transaction {
    /// Stable transaction identity used for diagnostics and deduplication.
    pub id: TransactionId,
    /// Module or system that produced the changes.
    pub source: ContentKey,
    /// Arbitration order.
    pub ordering: OrderingKey,
    /// Reads that must still hold for the transaction to commit.
    pub preconditions: Vec<BlockPrecondition>,
    /// Writes applied atomically when every precondition holds.
    pub mutations: Vec<Mutation>,
    /// Translations resolved against authoritative collision before publication.
    pub movements: Vec<Movement>,
    /// Schema-validated protected fields; only the schema owner may write them.
    pub components: Vec<ComponentEdit>,
}

impl Transaction {
    /// Start an empty transaction.
    pub fn new(id: TransactionId, source: ContentKey, ordering: OrderingKey) -> Self {
        Self {
            id,
            source,
            ordering,
            preconditions: Vec::new(),
            mutations: Vec::new(),
            movements: Vec::new(),
            components: Vec::new(),
        }
    }

    /// Record a read that the commit must revalidate.
    pub fn read_block(&mut self, pos: BlockPos, expected: torse_world::BlockStateId) -> &mut Self {
        self.preconditions.push(BlockPrecondition { pos, expected });
        self
    }

    /// Record a write.
    pub fn write_block(&mut self, pos: BlockPos, state: torse_world::BlockStateId) -> &mut Self {
        self.mutations.push(Mutation { pos, state });
        self
    }

    /// Request one collision-resolved move with an optimistic source position.
    pub fn move_entity(
        &mut self,
        handle: EntityHandle,
        id: EntityId,
        previous: Position,
        delta: [f64; 3],
        half: [f64; 3],
    ) -> &mut Self {
        self.movements.push(Movement {
            handle,
            id,
            previous_bits: previous.to_bits(),
            delta_bits: delta.map(f64::to_bits),
            half_bits: half.map(f64::to_bits),
        });
        self
    }

    /// Sort transactions into commit order.
    pub fn order(mut transactions: Vec<Self>) -> Vec<Self> {
        transactions.sort_by(|left, right| {
            left.ordering
                .cmp(&right.ordering)
                .then(left.id.0.cmp(&right.id.0))
        });
        transactions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key is valid")
    }

    fn tx(id: u128, priority: i32, stable: u64, local: u32) -> Transaction {
        Transaction::new(
            TransactionId(id),
            key("base:test"),
            OrderingKey {
                phase: Phase::Simulation,
                rule_priority: priority,
                stable_task_key: stable,
                local_sequence: local,
            },
        )
    }

    #[test]
    fn order_follows_priority_key_and_sequence() {
        let ordered = Transaction::order(vec![
            tx(2, 5, 1, 0),
            tx(3, 1, 9, 0),
            tx(1, 5, 0, 2),
            tx(4, 5, 0, 1),
        ]);
        let ids: Vec<u128> = ordered.iter().map(|value| value.id.0).collect();
        assert_eq!(ids, vec![3, 4, 1, 2]);
    }

    #[test]
    fn identical_keys_fall_back_to_transaction_identity() {
        let ordered = Transaction::order(vec![tx(9, 0, 0, 0), tx(4, 0, 0, 0)]);
        let ids: Vec<u128> = ordered.iter().map(|value| value.id.0).collect();
        assert_eq!(ids, vec![4, 9]);
    }

    #[test]
    fn phase_sorts_before_priority() {
        let mut first = tx(1, 9, 0, 0);
        first.ordering.phase = Phase::Input;
        let mut second = tx(2, 0, 0, 0);
        second.ordering.phase = Phase::Publish;
        let ordered = Transaction::order(vec![second, first]);
        assert_eq!(ordered[0].ordering.phase, Phase::Input);
    }
}
