//! Validation utilities for identity and sequence types.

use core::fmt;

/// Validation error types for checking identity and sequence constraints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    /// Request sequence must start at 1, not 0.
    RequestSeqZero,
    /// Replica sequence must start at 1, not 0.
    ReplicaSeqZero,
    /// CommitSeq cursor does not follow previous sequence.
    NonConsecutiveCommit { previous: u64, current: u64 },
    /// No CommitSeq can follow u64::MAX; commits must stop.
    CommitSeqOverflow,
    /// DurableSeq exceeds CommitSeq.
    DurableExceedsCommit { durable: u64, commit: u64 },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestSeqZero => write!(f, "request sequence must start at 1, not 0"),
            Self::ReplicaSeqZero => write!(f, "replica sequence must start at 1, not 0"),
            Self::NonConsecutiveCommit { previous, current } => {
                write!(
                    f,
                    "non-consecutive commit: previous={}, current={}",
                    previous, current
                )
            }
            Self::CommitSeqOverflow => write!(f, "commit sequence overflow after u64::MAX"),
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
