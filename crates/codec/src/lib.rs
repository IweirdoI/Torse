//! Storage encoding and decoding for authority data.
//!
//! This module provides explicit field-based encoding for snapshots and commit records.
//! Encoding must be deterministic, versioned, and respect budget constraints.

#![forbid(unsafe_code)]

mod budget;
mod decoder;
mod encoder;
pub mod frame;
mod journal;
mod registry;
mod snapshot;

pub use budget::{BudgetGuard, CodecLimits};
pub use decoder::Decoder;
pub use encoder::Encoder;

use torse_types::{AuthorityData, Fault, ModelCommit, ModelSnapshot, OutputBudget};

/// Result type for encoding operations.
pub type EncodeResult<T = ()> = Result<T, Fault>;

/// Storage codec trait for encoding and decoding authority data.
///
/// Implementations must:
/// - Use explicit field encoding (no derive macros for persistent formats)
/// - Check budget constraints during encoding
/// - Preserve unknown fields for forward compatibility
/// - Return errors on incomplete or corrupt data
///
/// Implementations hold their [`CodecLimits`] from local configuration.
pub trait StorageCodec<M: AuthorityData> {
    /// Encode a snapshot into the output buffer.
    ///
    /// On success, appends the complete encoded snapshot to `out`. On failure, `out` keeps
    /// its original length. `budget.max_bytes` bounds the newly appended length.
    fn encode_snapshot(
        &self,
        value: &ModelSnapshot<M>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> EncodeResult<()>;

    /// Decode a snapshot from input bytes.
    ///
    /// The entire input must be consumed. The budget bounds owned payloads and container
    /// allocations; input lengths are bounded by the codec limits.
    fn decode_snapshot(&self, input: &[u8], budget: OutputBudget)
    -> EncodeResult<ModelSnapshot<M>>;

    /// Encode a commit record into the output buffer.
    ///
    /// On success, appends the complete encoded commit to `out`. On failure, `out` keeps
    /// its original length. `budget.max_bytes` bounds the newly appended length.
    fn encode_commit(
        &self,
        value: &ModelCommit<M>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> EncodeResult<()>;

    /// Decode a commit record from input bytes.
    ///
    /// The entire input must be consumed. The budget bounds owned payloads and container
    /// allocations; input lengths are bounded by the codec limits.
    fn decode_commit(&self, input: &[u8], budget: OutputBudget) -> EncodeResult<ModelCommit<M>>;
}

#[cfg(test)]
mod tests;
