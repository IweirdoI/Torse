//! Check owned snapshot output before cloning any world payload.

use torse_types::{ContentKey, Fault, OutputBudget, SnapshotMeta};

use crate::{
    BlockStateRegistry, ComponentValue, EntityStore, SectionStorage, SectionStore, Snapshot,
    SnapshotComponent, SnapshotEntity, SnapshotSection, SnapshotValue, StateEntry,
};

impl Snapshot {
    /// Capture a complete world only when its owned payload fits both limits.
    ///
    /// Items count the root and each state, section, entity, component, field, list entry,
    /// and record entry. Bytes include inline structs, collection elements, key strings,
    /// text, and dense cells. Allocator overhead and temporary capture bookkeeping are
    /// excluded. The preflight borrows world data and uses a fixed state bitmap.
    pub fn capture_with_budget(
        meta: SnapshotMeta,
        store: &SectionStore,
        entities: &EntityStore,
        registry: &BlockStateRegistry,
        budget: OutputBudget,
    ) -> Result<Self, Fault> {
        Self::capture_selected_with_budget(meta, store, entities, registry, budget, None)
    }

    /// Preflight and capture only the persistent projection of the world.
    pub fn capture_persistent_with_budget(
        meta: SnapshotMeta,
        store: &SectionStore,
        entities: &EntityStore,
        registry: &BlockStateRegistry,
        schemas: &crate::ComponentSchemas,
        budget: OutputBudget,
    ) -> Result<Self, Fault> {
        Self::capture_selected_with_budget(meta, store, entities, registry, budget, Some(schemas))
    }

    fn capture_selected_with_budget(
        meta: SnapshotMeta,
        store: &SectionStore,
        entities: &EntityStore,
        registry: &BlockStateRegistry,
        budget: OutputBudget,
        schemas: Option<&crate::ComponentSchemas>,
    ) -> Result<Self, Fault> {
        let mut remaining = Remaining(budget);
        remaining.entries::<Self>(1)?;
        remaining.entries::<SnapshotSection>(store.len())?;
        let mut used = [false; u16::MAX as usize + 1];
        for pos in store.positions() {
            let section = store
                .section(pos)
                .ok_or_else(|| Fault::invalid_data("snapshot section is not readable"))?;
            match section.storage() {
                SectionStorage::Uniform(state) => used[state.0 as usize] = true,
                SectionStorage::Dense(cells) => {
                    remaining.bytes((cells.len() * size_of::<u16>()) as u64)?;
                    for state in cells {
                        used[state.0 as usize] = true;
                    }
                }
            }
        }
        for (id, _) in used.iter().enumerate().skip(1).filter(|(_, used)| **used) {
            let state = crate::BlockStateId(id as u16);
            remaining.entries::<StateEntry>(1)?;
            remaining.key(
                registry
                    .definition_key(state)
                    .ok_or_else(|| Fault::invalid_data("snapshot state is not registered"))?,
            )?;
            remaining
                .bytes(std::mem::size_of_val(registry.state_values(state).unwrap_or(&[])) as u64)?;
        }
        for (_, entity) in entities.iter() {
            remaining.entries::<SnapshotEntity>(1)?;
            for schema in entity.components.ids() {
                if let Some(schemas) = schemas
                    && !schemas
                        .persists(schema)
                        .map_err(|e| Fault::invalid_data(e.to_string()))?
                {
                    continue;
                }
                remaining.entries::<SnapshotComponent>(1)?;
                remaining.key(schema)?;
                if let Some(fields) = entity.components.component(schema) {
                    for (key, value) in fields {
                        remaining.entries::<(ContentKey, SnapshotValue)>(1)?;
                        remaining.key(key)?;
                        remaining.value(value)?;
                    }
                }
            }
        }
        Self::capture_selected(meta, store, entities, registry, schemas)
            .map_err(|error| Fault::invalid_data(error.to_string()))
    }
}

struct Remaining(OutputBudget);

impl Remaining {
    fn bytes(&mut self, bytes: u64) -> Result<(), Fault> {
        self.0.max_bytes = self
            .0
            .max_bytes
            .checked_sub(bytes)
            .ok_or_else(|| Fault::resource_limit("snapshot byte budget exhausted"))?;
        Ok(())
    }

    fn entries<T>(&mut self, count: usize) -> Result<(), Fault> {
        let count = u32::try_from(count)
            .map_err(|_| Fault::resource_limit("snapshot item count overflow"))?;
        self.0.max_items = self
            .0
            .max_items
            .checked_sub(count)
            .ok_or_else(|| Fault::resource_limit("snapshot item budget exhausted"))?;
        self.bytes(u64::from(count) * size_of::<T>() as u64)
    }

    fn key(&mut self, key: &ContentKey) -> Result<(), Fault> {
        self.bytes((key.namespace().len() + key.path().len()) as u64)
    }

    fn value(&mut self, value: &ComponentValue) -> Result<(), Fault> {
        match value {
            ComponentValue::Content(key) => self.key(key),
            ComponentValue::Text(text) => self.bytes(text.len() as u64),
            ComponentValue::List(items) => {
                self.entries::<SnapshotValue>(items.len())?;
                for value in items {
                    self.value(value)?;
                }
                Ok(())
            }
            ComponentValue::Record(fields) => {
                self.entries::<(ContentKey, SnapshotValue)>(fields.len())?;
                for (key, value) in fields {
                    self.key(key)?;
                    self.value(value)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
