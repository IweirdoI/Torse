//! Generation-checked runtime handles.
//!
//! Persistent object identity and runtime addressing are intentionally separate.
//! A slot may be reused after an object is removed, so a raw slot index is not a
//! safe reference. Every slot keeps a generation counter; a handle is valid only
//! while both fields match. This prevents delayed tasks, stale commands, and old
//! network callbacks from accidentally reaching a newly created object.

use torse_types::EntityHandle;

/// Runtime slot metadata hidden behind the handle table.
struct Slot<T> {
    /// Current generation expected by live and future handles.
    generation: u32,
    /// `None` means the slot is empty and may be reused.
    value: Option<T>,
}

/// Slot table that rejects stale handles after a slot is reused.
///
/// The table is intentionally small and self-contained for M0. It does not
/// provide persistence, thread synchronization, or stable IDs; higher layers
/// must combine it with the authority lifecycle and lock discipline appropriate
/// to their execution model.
pub struct HandleTable<T> {
    /// Dense slot storage. Empty slots remain allocated so indices stay stable.
    slots: Vec<Slot<T>>,
    /// Reusable slot indices. The table never shrinks solely because a value is removed.
    free: Vec<u32>,
}

impl<T> Default for HandleTable<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> HandleTable<T> {
    /// Create an empty table without allocating any slots.
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    /// Insert a value and return its generation-stamped runtime handle.
    ///
    /// Reusing a free slot keeps the slot index but preserves the generation
    /// advanced during removal, so handles issued for the previous occupant
    /// remain invalid.
    pub fn insert(&mut self, value: T) -> EntityHandle {
        if let Some(slot) = self.free.pop() {
            let entry = &mut self.slots[slot as usize];
            debug_assert!(entry.value.is_none());
            entry.value = Some(value);
            return EntityHandle {
                slot,
                generation: entry.generation,
            };
        }

        let slot = u32::try_from(self.slots.len()).expect("handle table exhausted");
        self.slots.push(Slot {
            generation: 1,
            value: Some(value),
        });
        EntityHandle {
            slot,
            generation: 1,
        }
    }

    /// Borrow a value only when the handle generation matches the live slot.
    ///
    /// A stale generation and an empty slot both return `None`. Callers should
    /// treat both as "this handle no longer refers to a usable object".
    pub fn get(&self, handle: EntityHandle) -> Option<&T> {
        let slot = self.slots.get(handle.slot as usize)?;
        (slot.generation == handle.generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    /// Remove and return a value if the handle is current.
    ///
    /// Success advances the slot generation before the slot is made available
    /// for reuse. If the generation is already `u32::MAX`, the slot is retired
    /// rather than wrapped, because wrapping would make an old handle valid again.
    pub fn remove(&mut self, handle: EntityHandle) -> Option<T> {
        let slot = self.slots.get_mut(handle.slot as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let value = slot.value.take()?;
        // Retire exhausted slots instead of wrapping their generation. The
        // small amount of address space is preferable to an alias bug.
        if slot.generation < u32::MAX {
            slot.generation += 1;
            self.free.push(handle.slot);
        }
        Some(value)
    }

    /// Return the number of occupied slots, not the number of allocated slots.
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count()
    }

    /// Return whether the table currently contains no live values.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_slot_invalidates_old_handle() {
        // This is the central lifecycle guarantee: releasing an object must not
        // permit a delayed owner of the old handle to reach its replacement.
        let mut table = HandleTable::new();
        let old = table.insert("first");
        assert_eq!(table.remove(old), Some("first"));
        let new = table.insert("second");
        assert_eq!(new.slot, old.slot);
        assert_ne!(new.generation, old.generation);
        assert_eq!(table.get(old), None);
        assert_eq!(table.get(new), Some(&"second"));
    }

    #[test]
    fn mismatched_generation_cannot_remove_current_value() {
        // A forged or stale generation must not be able to remove a current
        // object merely because the slot index still exists.
        let mut table = HandleTable::new();
        let handle = table.insert(7);
        let stale = EntityHandle {
            slot: handle.slot,
            generation: handle.generation + 1,
        };
        assert_eq!(table.remove(stale), None);
        assert_eq!(table.get(handle), Some(&7));
    }
}
