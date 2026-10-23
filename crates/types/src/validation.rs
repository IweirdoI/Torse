//! Validation utilities for identity and sequence types.

use core::fmt;

/// Validation error types for checking identity and sequence constraints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    /// Sequence number overflow or invalid progression.
    InvalidSequence { field: &'static str, value: u64 },
    /// Request sequence must start at 1, not 0.
    RequestSeqZero,
    /// Replica sequence must start at 1, not 0.
    ReplicaSeqZero,
    /// CommitSeq cursor does not follow previous sequence.
    NonConsecutiveCommit { previous: u64, current: u64 },
    /// DurableSeq exceeds CommitSeq.
    DurableExceedsCommit { durable: u64, commit: u64 },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSequence { field, value } => {
                write!(f, "invalid sequence in field {}: {}", field, value)
            }
            Self::RequestSeqZero => write!(f, "request sequence must start at 1, not 0"),
            Self::ReplicaSeqZero => write!(f, "replica sequence must start at 1, not 0"),
            Self::NonConsecutiveCommit { previous, current } => {
                write!(
                    f,
                    "non-consecutive commit: previous={}, current={}",
                    previous, current
                )
            }
            Self::DurableExceedsCommit { durable, commit } => {
                write!(
                    f,
                    "durable sequence {} exceeds commit sequence {}",
                    durable, commit
                )
            }
        }
    }
}

impl std::error::Error for ValidationError {}
