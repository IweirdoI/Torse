//! Generation-checked runtime handles.

use torse_types::EntityHandle;

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// Slot table that rejects stale handles after a slot is reused.
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> Default for HandleTable<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> HandleTable<T> {
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

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

    pub fn get(&self, handle: EntityHandle) -> Option<&T> {
        let slot = self.slots.get(handle.slot as usize)?;
        (slot.generation == handle.generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    pub fn remove(&mut self, handle: EntityHandle) -> Option<T> {
        let slot = self.slots.get_mut(handle.slot as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let value = slot.value.take()?;
        if slot.generation < u32::MAX {
            slot.generation += 1;
            self.free.push(handle.slot);
        }
        Some(value)
    }

    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_slot_invalidates_old_handle() {
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
