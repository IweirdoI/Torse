//! Bounded entity creation and removal with allocator and full-state preconditions.

use std::collections::{BTreeMap, BTreeSet};

use torse_types::{ContentKey, EntityHandle, EntityId};

use crate::{
    ComponentFields, ComponentSchemas, ComponentSet, Entity, EntityError, EntityStore, Position,
    SectionStore, TerrainQuery, component_size,
};

/// Maximum total creations and removals in one transaction.
pub const MAX_ENTITY_LIFECYCLE: usize = 256;
/// Maximum conservative lifecycle payload, including observed removal components.
pub const MAX_ENTITY_LIFECYCLE_BYTES: u64 = 8_388_608;
/// Prototype coordinate bound shared by lifecycle admission and replay.
pub const MAX_SPAWN_COORDINATE: f64 = 1_048_576.0;

/// Stable full state used to create an entity or compare a removal observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityState {
    pub id: EntityId,
    pub position_bits: [u64; 3],
    pub components: BTreeMap<ContentKey, ComponentFields>,
}

impl EntityState {
    /// Capture exact bits and every protected component from a read-only entity.
    pub fn capture(entity: &Entity) -> Self {
        Self {
            id: entity.id,
            position_bits: entity.position.to_bits(),
            components: entity
                .components
                .ids()
                .map(|schema| {
                    (
                        schema.clone(),
                        entity.components.component(schema).unwrap().clone(),
                    )
                })
                .collect(),
        }
    }

    fn matches(&self, entity: &Entity) -> bool {
        self.id == entity.id
            && self.position_bits == entity.position.to_bits()
            && self.components.len() == entity.components.len()
            && self
                .components
                .iter()
                .all(|(schema, fields)| entity.components.component(schema) == Some(fields))
    }
}

/// Final lifecycle effects. Created IDs follow request order from the observed watermark.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityLifecycle {
    pub previous_next_id: u128,
    pub next_id: u128,
    pub spawned: Vec<EntityState>,
    pub despawned: Vec<EntityState>,
}

impl EntityLifecycle {
    /// Check bounded structure and allocator arithmetic without consulting world state.
    pub fn validate_structure(&self) -> Result<(), EntityError> {
        if self.spawned.len() + self.despawned.len() > MAX_ENTITY_LIFECYCLE
            || (self.spawned.is_empty() && self.despawned.is_empty())
            || self.previous_next_id == 0
            || self
                .previous_next_id
                .checked_add(self.spawned.len() as u128)
                != Some(self.next_id)
        {
            return Err(EntityError::InvalidLifecycle);
        }
        let mut seen = BTreeSet::new();
        for (index, state) in self.spawned.iter().enumerate() {
            if state.id.0 != self.previous_next_id + index as u128 {
                return Err(EntityError::InvalidLifecycle);
            }
        }
        for state in self.spawned.iter().chain(&self.despawned) {
            state.id.validate().map_err(|_| EntityError::InvalidId)?;
            if !seen.insert(state.id) {
                return Err(EntityError::InvalidLifecycle);
            }
        }
        Self::validate_payload(
            self.spawned.len() + self.despawned.len(),
            self.spawned
                .iter()
                .chain(&self.despawned)
                .map(|state| &state.components),
        )
    }

    /// Check borrowed request payloads before constructing owned lifecycle records.
    pub fn validate_payload<'a>(
        count: usize,
        components: impl Iterator<Item = &'a BTreeMap<ContentKey, ComponentFields>>,
    ) -> Result<(), EntityError> {
        if count > MAX_ENTITY_LIFECYCLE {
            return Err(EntityError::InvalidLifecycle);
        }
        let mut bytes = (size_of::<Self>() + count * size_of::<EntityState>()) as u64;
        for set in components {
            if set.len() > crate::MAX_COMPONENT_INSTANCES {
                return Err(EntityError::InvalidComponents);
            }
            for (schema, fields) in set {
                bytes += (size_of::<(ContentKey, ComponentFields)>()
                    + schema.namespace().len()
                    + schema.path().len()) as u64;
                bytes += component_size(fields).map_err(|_| EntityError::InvalidComponents)?;
                if bytes > MAX_ENTITY_LIFECYCLE_BYTES {
                    return Err(EntityError::InvalidLifecycle);
                }
            }
        }
        Ok(())
    }

    /// Creations require a finite, bounded position in readable terrain. No collider or
    /// physics shape is implied by an entity record, so this does not run collision rules.
    pub fn validate_world(&self, store: &SectionStore) -> Result<(), EntityError> {
        for state in &self.spawned {
            let position = Position::from_bits(state.position_bits);
            if [position.x, position.y, position.z]
                .iter()
                .any(|value| !value.is_finite() || value.abs() > MAX_SPAWN_COORDINATE)
                || !matches!(store.query(position.block()), TerrainQuery::Ready(_))
            {
                return Err(EntityError::InvalidPosition);
            }
        }
        Ok(())
    }

    /// No entity may be created/removed and independently moved or edited in one record.
    pub fn conflicts_with(&self, id: EntityId) -> bool {
        self.spawned
            .iter()
            .chain(&self.despawned)
            .any(|state| state.id == id)
    }
}

/// Opaque, schema-validated lifecycle plan with runtime removal handles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleBatch {
    change: EntityLifecycle,
    handles: Vec<EntityHandle>,
    components: Vec<ComponentSet>,
}

impl LifecycleBatch {
    pub fn change(&self) -> &EntityLifecycle {
        &self.change
    }

    /// Validate all observations before any terrain, movement, component or lifecycle write.
    pub fn validate(&self, entities: &EntityStore) -> Result<(), EntityError> {
        entities.validate_lifecycle_capacity(&self.handles, self.change.spawned.len())?;
        if entities.next_entity_id() != self.change.previous_next_id {
            return Err(EntityError::LifecycleConflict);
        }
        for (handle, state) in self.handles.iter().zip(&self.change.despawned) {
            if !entities
                .get(*handle)
                .is_some_and(|entity| state.matches(entity))
            {
                return Err(EntityError::LifecycleConflict);
            }
        }
        Ok(())
    }
}

impl EntityStore {
    /// Resolve removals, validate complete creation schemas, and retain exact observations.
    pub fn prepare_lifecycle(
        &self,
        change: &EntityLifecycle,
        schemas: &ComponentSchemas,
    ) -> Result<LifecycleBatch, EntityError> {
        change.validate_structure()?;
        let mut handles = Vec::with_capacity(change.despawned.len());
        for state in &change.despawned {
            handles.push(self.handle_of(state.id).ok_or(EntityError::StaleHandle)?);
        }
        self.validate_lifecycle_capacity(&handles, change.spawned.len())?;
        let mut components = Vec::with_capacity(change.spawned.len());
        for state in &change.spawned {
            let position = Position::from_bits(state.position_bits);
            if !position.is_finite() || position.section(self.side()).is_none() {
                return Err(EntityError::InvalidPosition);
            }
            if self.handle_of(state.id).is_some() {
                return Err(EntityError::DuplicateId { id: state.id });
            }
            let mut set = ComponentSet::new();
            for (schema, fields) in &state.components {
                set.insert(schemas, schema, fields.clone())
                    .map_err(|_| EntityError::InvalidComponents)?;
            }
            components.push(set);
        }
        let batch = LifecycleBatch {
            change: change.clone(),
            handles,
            components,
        };
        batch.validate(self)?;
        Ok(batch)
    }

    /// Publish without callbacks after all other transaction targets have been validated.
    pub fn publish_lifecycle(&mut self, batch: LifecycleBatch) -> Result<(), EntityError> {
        batch.validate(self)?;
        for handle in batch.handles {
            self.despawn(handle)
                .expect("validated removal remains live");
        }
        for (state, components) in batch.change.spawned.into_iter().zip(batch.components) {
            self.insert_restored(
                state.id,
                Position::from_bits(state.position_bits),
                components,
            )?;
        }
        self.restore_next_entity_id(batch.change.next_id)
    }
}
