//! Budget and limit tracking shared by encoders and decoders.

use torse_types::{Fault, OutputBudget};

use crate::EncodeResult;

/// Hard input limits taken from local configuration.
///
/// There is no default value. Callers supply explicit limits so that no encoder or decoder
/// runs with unbounded lengths or nesting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodecLimits {
    /// Maximum payload length of a single frame.
    pub max_frame_bytes: u32,
    /// Maximum length of a single length-prefixed field.
    pub max_field_bytes: u32,
    /// Maximum nesting depth of composite values.
    pub max_depth: u16,
}

/// Tracks item and byte usage against an [`OutputBudget`].
///
/// Items count logical entries such as records, objects, or collection elements, and are
/// charged explicitly by codec implementations. Bytes count newly appended output when
/// encoding, and owned payload or container allocation when decoding.
#[derive(Debug)]
pub struct BudgetGuard {
    budget: OutputBudget,
    used_items: u32,
    used_bytes: u64,
}

impl BudgetGuard {
    pub fn new(budget: OutputBudget) -> Self {
        Self {
            budget,
            used_items: 0,
            used_bytes: 0,
        }
    }

    /// Fail unless `bytes` more bytes fit. Charges nothing.
    pub fn check_bytes(&self, bytes: u64) -> EncodeResult<()> {
        match self.used_bytes.checked_add(bytes) {
            Some(total) if self.budget.allows(self.used_items, total) => Ok(()),
            _ => Err(Fault::resource_limit(format!(
                "byte budget exceeded: used {}, requested {}, limit {}",
                self.used_bytes, bytes, self.budget.max_bytes
            ))),
        }
    }

    pub fn charge_bytes(&mut self, bytes: u64) -> EncodeResult<()> {
        self.check_bytes(bytes)?;
        self.used_bytes += bytes;
        Ok(())
    }

    pub fn charge_items(&mut self, items: u32) -> EncodeResult<()> {
        match self.used_items.checked_add(items) {
            Some(total) if self.budget.allows(total, self.used_bytes) => {
                self.used_items = total;
                Ok(())
            }
            _ => Err(Fault::resource_limit(format!(
                "item budget exceeded: used {}, requested {}, limit {}",
                self.used_items, items, self.budget.max_items
            ))),
        }
    }

    pub fn used_items(&self) -> u32 {
        self.used_items
    }

    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub fn remaining_items(&self) -> u32 {
        self.budget.max_items.saturating_sub(self.used_items)
    }

    pub fn remaining_bytes(&self) -> u64 {
        self.budget.max_bytes.saturating_sub(self.used_bytes)
    }
}
