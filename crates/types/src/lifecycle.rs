//! Revision and event identities used by validation and recovery.
//!
//! Revisions detect stale reads and derived-cache writes. Transaction and event
//! identifiers make retries, replay, and scheduled work observable across the
//! authority/storage/network boundaries. The identifiers are opaque values:
//! their numeric ordering is useful for diagnostics, but allocation policy
//! belongs to the authority implementation.

use core::fmt;

use crate::{CommitCursor, RejectCode, Tick};

/// Monotonic revision for a field, object, or derived resource.
///
/// Revision values are local to the thing they describe. Comparing revisions
/// from two unrelated objects is not meaningful.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Revision(pub u64);

impl Revision {
    /// Initial revision assigned when an item is created.
    pub const INITIAL: Self = Self(0);

    /// Return the next revision, or `None` instead of wrapping past `u64::MAX`.
    ///
    /// Exhaustion is treated as a terminal condition; reusing zero would make
    /// stale caches appear current again.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Stable identity for scheduled or queued authority work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct EventId(pub u128);

impl EventId {
    /// Validate that the event identifier is not the reserved zero sentinel.
    pub fn validate(self) -> Result<(), EventIdError> {
        if self.0 == 0 {
            Err(EventIdError::Zero)
        } else {
            Ok(())
        }
    }
}

/// Failure produced by an invalid event identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventIdError {
    /// Event identifiers start at one.
    Zero,
}

impl fmt::Display for EventIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => write!(f, "event ids start at 1"),
        }
    }
}

impl std::error::Error for EventIdError {}

/// Stable identity assigned to an authority transaction before commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct TransactionId(pub u128);

impl TransactionId {
    /// Validate that the transaction identifier is not the reserved zero sentinel.
    pub fn validate(self) -> Result<(), TransactionIdError> {
        if self.0 == 0 {
            Err(TransactionIdError::Zero)
        } else {
            Ok(())
        }
    }
}

/// Failure produced by an invalid transaction identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionIdError {
    /// Transaction identifiers start at one.
    Zero,
}

impl fmt::Display for TransactionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => write!(f, "transaction ids start at 1"),
        }
    }
}

impl std::error::Error for TransactionIdError {}

/// Result of attempting to commit a transaction.
///
/// `Committed` carries the exact commit cursor assigned by the authority. The
/// cursor is part of the result because callers must not infer durability from
/// the fact that a value was returned; persistence is reported separately by
/// `DurableSeq`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionOutcome<T> {
    /// The transaction published at the cursor and produced the attached value.
    Committed {
        /// Exact authority commit position assigned to the transaction.
        at: CommitCursor,
        /// Domain-specific result returned after commit.
        value: T,
    },
    /// The transaction made no authoritative change and returned a stable reason.
    Rejected(RejectCode),
}

/// Transaction identity paired with its terminal outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionResult<T> {
    /// Stable transaction identity assigned by the authority.
    pub transaction: TransactionId,
    /// Terminal result of the transaction attempt.
    pub outcome: TransactionOutcome<T>,
}

/// Event payload assigned to a future simulation tick.
///
/// The target tick is recorded explicitly so replay and recovery do not depend
/// on wall-clock time or the process in which the event was created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledEvent<T> {
    /// Stable event identity used by deduplication and recovery.
    pub id: EventId,
    /// Authority tick at which the event becomes eligible to execute.
    pub target_tick: Tick,
    /// Event payload interpreted by the owning module or system.
    pub input: T,
}
