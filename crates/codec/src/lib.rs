//! Storage encoding and decoding for authority data.
//!
//! This module provides explicit field-based encoding for snapshots and commit records.
//! Encoding must be deterministic, versioned, and respect budget constraints.

#![forbid(unsafe_code)]

pub mod frame;
pub mod journal;
pub mod registry;
pub mod snapshot;

use torse_types::{Fault, FaultCode, ModelCommit, ModelSnapshot, OutputBudget};

/// Result type for encoding operations.
pub type EncodeResult<T = ()> = Result<T, Fault>;

/// Storage codec trait for encoding and decoding authority data.
///
/// Implementations must:
/// - Use explicit field encoding (no derive macros for persistent formats)
/// - Check budget constraints during encoding
/// - Preserve unknown fields for forward compatibility
/// - Return errors on incomplete or corrupt data
pub trait StorageCodec<M: torse_types::AuthorityData> {
    /// Encode a snapshot into the output buffer.
    ///
    /// On success, appends the complete encoded snapshot to `out`.
    /// On failure, restores `out` to its original length without partial writes.
    fn encode_snapshot(
        &self,
        value: &ModelSnapshot<M>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> EncodeResult<()>;

    /// Decode a snapshot from input bytes.
    ///
    /// The entire input must be consumed. Budget applies to both the input size
    /// and the allocated output structures.
    fn decode_snapshot(&self, input: &[u8], budget: OutputBudget)
    -> EncodeResult<ModelSnapshot<M>>;

    /// Encode a commit record into the output buffer.
    ///
    /// On success, appends the complete encoded commit to `out`.
    /// On failure, restores `out` to its original length without partial writes.
    fn encode_commit(
        &self,
        value: &ModelCommit<M>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> EncodeResult<()>;

    /// Decode a commit record from input bytes.
    ///
    /// The entire input must be consumed. Budget applies to both the input size
    /// and the allocated output structures.
    fn decode_commit(&self, input: &[u8], budget: OutputBudget) -> EncodeResult<ModelCommit<M>>;
}

/// Budget guard to track and enforce output limits during encoding.
#[derive(Debug)]
pub struct BudgetGuard {
    max_items: u32,
    max_bytes: u64,
    used_items: u32,
    used_bytes: u64,
}

impl BudgetGuard {
    /// Create a new budget guard from an output budget.
    pub fn new(budget: OutputBudget) -> Self {
        Self {
            max_items: budget.max_items,
            max_bytes: budget.max_bytes,
            used_items: 0,
            used_bytes: 0,
        }
    }

    /// Check if adding an item with the given byte size would exceed the budget.
    pub fn can_add(&self, byte_size: u64) -> bool {
        self.used_items < self.max_items
            && self.used_bytes.saturating_add(byte_size) <= self.max_bytes
    }

    /// Reserve space for an item with the given byte size.
    ///
    /// Returns an error if the budget would be exceeded.
    pub fn reserve(&mut self, byte_size: u64) -> EncodeResult<()> {
        if !self.can_add(byte_size) {
            return Err(Fault::resource_limit(format!(
                "budget exceeded: items={}/{}, bytes={}/{}",
                self.used_items, self.max_items, self.used_bytes, self.max_bytes
            )));
        }
        self.used_items += 1;
        self.used_bytes = self.used_bytes.saturating_add(byte_size);
        Ok(())
    }

    /// Get the number of items used.
    pub fn used_items(&self) -> u32 {
        self.used_items
    }

    /// Get the number of bytes used.
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    /// Get remaining item capacity.
    pub fn remaining_items(&self) -> u32 {
        self.max_items.saturating_sub(self.used_items)
    }

    /// Get remaining byte capacity.
    pub fn remaining_bytes(&self) -> u64 {
        self.max_bytes.saturating_sub(self.used_bytes)
    }
}

/// Encoder state wrapping a buffer and budget guard.
pub struct Encoder<'a> {
    buffer: &'a mut Vec<u8>,
    guard: BudgetGuard,
    initial_len: usize,
}

impl<'a> Encoder<'a> {
    /// Create a new encoder with the given buffer and budget.
    pub fn new(buffer: &'a mut Vec<u8>, budget: OutputBudget) -> Self {
        let initial_len = buffer.len();
        Self {
            buffer,
            guard: BudgetGuard::new(budget),
            initial_len,
        }
    }

    /// Write bytes to the buffer after checking budget.
    pub fn write(&mut self, data: &[u8]) -> EncodeResult<()> {
        let len = data.len() as u64;
        self.guard.reserve(len)?;
        self.buffer.extend_from_slice(data);
        Ok(())
    }

    /// Write a u8 value.
    pub fn write_u8(&mut self, value: u8) -> EncodeResult<()> {
        self.write(&[value])
    }

    /// Write a u16 value in little-endian format.
    pub fn write_u16(&mut self, value: u16) -> EncodeResult<()> {
        self.write(&value.to_le_bytes())
    }

    /// Write a u32 value in little-endian format.
    pub fn write_u32(&mut self, value: u32) -> EncodeResult<()> {
        self.write(&value.to_le_bytes())
    }

    /// Write a u64 value in little-endian format.
    pub fn write_u64(&mut self, value: u64) -> EncodeResult<()> {
        self.write(&value.to_le_bytes())
    }

    /// Write a u128 value in little-endian format.
    pub fn write_u128(&mut self, value: u128) -> EncodeResult<()> {
        self.write(&value.to_le_bytes())
    }

    /// Restore buffer to initial length on error.
    pub fn restore_on_error(&mut self) {
        self.buffer.truncate(self.initial_len);
    }

    /// Get the budget guard.
    pub fn guard(&self) -> &BudgetGuard {
        &self.guard
    }
}

impl<'a> Drop for Encoder<'a> {
    fn drop(&mut self) {
        // If we're unwinding due to panic and the buffer has grown,
        // truncate it back to the initial length to maintain transactional semantics.
        if std::thread::panicking() && self.buffer.len() > self.initial_len {
            self.buffer.truncate(self.initial_len);
        }
    }
}

/// Decoder state wrapping input bytes and budget guard.
pub struct Decoder<'a> {
    input: &'a [u8],
    position: usize,
    guard: BudgetGuard,
}

impl<'a> Decoder<'a> {
    /// Create a new decoder with the given input and budget.
    pub fn new(input: &'a [u8], budget: OutputBudget) -> Self {
        Self {
            input,
            position: 0,
            guard: BudgetGuard::new(budget),
        }
    }

    /// Check if all input has been consumed.
    pub fn is_complete(&self) -> bool {
        self.position == self.input.len()
    }

    /// Get remaining unread bytes.
    pub fn remaining(&self) -> usize {
        self.input.len().saturating_sub(self.position)
    }

    /// Read exactly `len` bytes from input.
    pub fn read(&mut self, len: usize) -> EncodeResult<&'a [u8]> {
        if self.position + len > self.input.len() {
            return Err(Fault::new(
                FaultCode::InvalidData,
                format!(
                    "unexpected end of input: need {} bytes, have {}",
                    len,
                    self.remaining()
                ),
            ));
        }
        self.guard.reserve(len as u64)?;
        let slice = &self.input[self.position..self.position + len];
        self.position += len;
        Ok(slice)
    }

    /// Read a u8 value.
    pub fn read_u8(&mut self) -> EncodeResult<u8> {
        let bytes = self.read(1)?;
        Ok(bytes[0])
    }

    /// Read a u16 value in little-endian format.
    pub fn read_u16(&mut self) -> EncodeResult<u16> {
        let bytes = self.read(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Read a u32 value in little-endian format.
    pub fn read_u32(&mut self) -> EncodeResult<u32> {
        let bytes = self.read(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Read a u64 value in little-endian format.
    pub fn read_u64(&mut self) -> EncodeResult<u64> {
        let bytes = self.read(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Read a u128 value in little-endian format.
    pub fn read_u128(&mut self) -> EncodeResult<u128> {
        let bytes = self.read(16)?;
        Ok(u128::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]))
    }

    /// Get the budget guard.
    pub fn guard(&self) -> &BudgetGuard {
        &self.guard
    }
}

#[cfg(test)]
mod tests;
