//! Revision and event identities used by validation and recovery.

use core::fmt;

use crate::{CommitCursor, RejectCode, Tick};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Revision(pub u64);

impl Revision {
    pub const INITIAL: Self = Self(0);

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct EventId(pub u128);

impl EventId {
    pub fn validate(self) -> Result<(), EventIdError> {
        if self.0 == 0 {
            Err(EventIdError::Zero)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventIdError {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct TransactionId(pub u128);

impl TransactionId {
    pub fn validate(self) -> Result<(), TransactionIdError> {
        if self.0 == 0 {
            Err(TransactionIdError::Zero)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionIdError {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionOutcome<T> {
    Committed { at: CommitCursor, value: T },
    Rejected(RejectCode),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionResult<T> {
    pub transaction: TransactionId,
    pub outcome: TransactionOutcome<T>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledEvent<T> {
    pub id: EventId,
    pub target_tick: Tick,
    pub input: T,
}
