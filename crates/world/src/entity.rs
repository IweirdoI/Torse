//! Entity storage with a per-section spatial index.
//!
//! Entities have two identities. The persistent [`EntityId`] is what saves and
//! network messages may reference; the runtime [`EntityHandle`] is a slot plus
//! generation and becomes invalid the moment the slot is reused. This split is
//! what lets a delayed task or an old packet fail cleanly instead of reaching a
//! different entity.
//!
//! The spatial index maps a section to the entities whose position falls inside
//! it. It is maintained by this store rather than by callers: moving an entity
//! through the API updates the index in the same call, so the index cannot drift
//! when a system forgets a step.

use std::collections::BTreeMap;

use torse_types::{BlockPos, EntityHandle, EntityId, SectionPos};

use crate::component::{ComponentFields, ComponentSchemas, ComponentSet, MAX_COMPONENT_INSTANCES};
use crate::section::MAX_SECTION_SIDE;

/// Fractional world position of an entity.
///
/// Coordinates are absolute block units. The client converts to camera-relative
/// floats when it submits geometry; the authority keeps absolute values so
/// collision, distance checks, and replication share one meaning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    /// World x in block units.
    pub x: f64,
    /// World y in block units.
    pub y: f64,
    /// World z in block units.
    pub z: f64,
}

impl Position {
    /// Preserve the exact IEEE-754 representation in exchange records.
    pub fn to_bits(self) -> [u64; 3] {
        [self.x.to_bits(), self.y.to_bits(), self.z.to_bits()]
    }

    /// Rebuild a position; callers must validate it before storing it.
    pub fn from_bits(bits: [u64; 3]) -> Self {
        Self::new(
            f64::from_bits(bits[0]),
            f64::from_bits(bits[1]),
            f64::from_bits(bits[2]),
        )
    }

    /// Construct a position without validation.
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// Return whether every component is finite.
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    /// Translate by a delta.
    pub fn translated(self, delta: [f64; 3]) -> Self {
        Self::new(self.x + delta[0], self.y + delta[1], self.z + delta[2])
    }

    /// Floor position to the containing block cell.
    pub fn block(self) -> BlockPos {
        BlockPos::new(
            self.x.floor() as i64,
            self.y.floor() as i64,
            self.z.floor() as i64,
        )
    }

    /// Section that contains this position, given a section side.
    pub fn section(self, side: u32) -> Option<SectionPos> {
        let block = self.block();
        block.to_section(side).ok().map(|(section, _)| section)
    }
}

/// One authoritative entity translation, with exact optimistic concurrency data.
///
/// Persistent identities, rather than runtime slots, let the same record replay
/// after a restart. The previous position prevents a misplaced or duplicate
/// record from moving an entity from an unrelated state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityMove {
    /// Persistent entity identity.
    pub id: EntityId,
    /// Position observed during preparation.
    pub previous_bits: [u64; 3],
    /// Collision-resolved position to publish.
    pub position_bits: [u64; 3],
}

/// One stable component replacement or removal with an exact read precondition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentEdit {
    /// Persistent owner identity; runtime handles never enter a saved record.
    pub id: EntityId,
    /// Component schema and ownership declaration.
    pub schema: torse_types::ContentKey,
    /// Observed component, or absence when creating a new instance.
    pub previous: Option<ComponentFields>,
    /// Replacement fields, or absence when removing an existing instance.
    pub fields: Option<ComponentFields>,
}

/// Opaque component replacements validated before terrain or entity publication.
///
/// Keeping validated replacements private prevents the publishing caller from
/// injecting unchecked fields after the journal has reserved the record.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComponentBatch {
    updates: Vec<ComponentUpdate>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ComponentUpdate {
    handle: EntityHandle,
    edit: ComponentEdit,
}

/// Maximum protected component targets in one atomic transaction.
pub const MAX_COMPONENT_EDITS: usize = 256;

impl ComponentBatch {
    /// Recheck observed fields and final object capacity before the first write.
    pub fn validate(&self, entities: &EntityStore) -> Result<(), EntityError> {
        let mut counts = BTreeMap::new();
        for ComponentUpdate { handle, edit } in &self.updates {
            let current = entities.get(*handle).ok_or(EntityError::StaleHandle)?;
            if current.id != edit.id
                || current.components.component(&edit.schema) != edit.previous.as_ref()
            {
                return Err(EntityError::ComponentConflict);
            }
            let count = counts.entry(edit.id).or_insert(current.components.len());
            if edit.previous.is_none() {
                *count += 1;
            }
            if edit.fields.is_none() {
                *count -= 1;
            }
        }
        if counts
            .values()
            .any(|count| *count > MAX_COMPONENT_INSTANCES)
        {
            return Err(EntityError::InvalidComponents);
        }
        Ok(())
    }
}

/// One stored entity.
#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    /// Persistent identity stable across save and reload.
    pub id: EntityId,
    /// Current world position.
    pub position: Position,
    /// Bounded components owned by modules.
    pub components: ComponentSet,
}

#[derive(Debug)]
struct Slot {
    generation: u32,
    entity: Option<Entity>,
}

/// Entity table with a section index.
#[derive(Debug)]
pub struct EntityStore {
    side: u32,
    slots: Vec<Slot>,
    free: Vec<u32>,
    index: BTreeMap<SectionPos, Vec<u32>>,
    by_id: BTreeMap<EntityId, u32>,
    next_id: u128,
    live: usize,
}

impl EntityStore {
    pub(crate) fn validate_lifecycle_capacity(
        &self,
        removed: &[EntityHandle],
        spawned: usize,
    ) -> Result<(), EntityError> {
        let reusable = self.free.len()
            + removed
                .iter()
                .filter(|handle| handle.generation < u32::MAX)
                .count();
        let needed = spawned.saturating_sub(reusable) as u64;
        if self.slots.len() as u64 + needed > u64::from(u32::MAX) + 1 {
            return Err(EntityError::IdExhausted);
        }
        Ok(())
    }
    /// Resolve and validate a bounded component group without changing storage.
    pub fn prepare_components(
        &self,
        edits: &[ComponentEdit],
        schemas: &ComponentSchemas,
    ) -> Result<ComponentBatch, EntityError> {
        if edits.len() > MAX_COMPONENT_EDITS {
            return Err(EntityError::InvalidComponents);
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut updates = Vec::with_capacity(edits.len());
        for edit in edits {
            if !seen.insert((edit.id, edit.schema.clone())) {
                return Err(EntityError::InvalidComponents);
            }
            let handle = self.handle_of(edit.id).ok_or(EntityError::StaleHandle)?;
            let entity = self.get(handle).ok_or(EntityError::StaleHandle)?;
            if entity.components.component(&edit.schema) != edit.previous.as_ref() {
                return Err(EntityError::ComponentConflict);
            }
            if schemas.get(&edit.schema).is_none()
                || (edit.previous.is_none() && edit.fields.is_none())
            {
                return Err(EntityError::InvalidComponents);
            }
            if let Some(fields) = &edit.fields {
                ComponentSet::validate(schemas, &edit.schema, fields)
                    .map_err(|_| EntityError::InvalidComponents)?;
            }
            updates.push(ComponentUpdate {
                handle,
                edit: edit.clone(),
            });
        }
        let batch = ComponentBatch { updates };
        batch.validate(self)?;
        Ok(batch)
    }

    /// Publish an opaque, prevalidated group under exclusive storage access.
    pub fn publish_components(&mut self, batch: ComponentBatch) -> Result<(), EntityError> {
        batch.validate(self)?;
        for ComponentUpdate { handle, edit } in batch.updates {
            // No callback or lifecycle change can occur after validation. Each
            // handle remains live while only its component set is replaced.
            self.components_mut(handle)
                .expect("validated component owner remains live")
                .apply_validated(edit.schema, edit.fields);
        }
        Ok(())
    }

    /// Create an empty store for a section side.
    pub fn new(side: u32) -> Result<Self, EntityError> {
        if side == 0 || side > MAX_SECTION_SIDE {
            return Err(EntityError::InvalidSide { side });
        }
        Ok(Self {
            side,
            slots: Vec::new(),
            free: Vec::new(),
            index: BTreeMap::new(),
            by_id: BTreeMap::new(),
            next_id: 1,
            live: 0,
        })
    }

    /// Section side used by the spatial index.
    pub const fn side(&self) -> u32 {
        self.side
    }

    /// Allocation watermark, including identities of entities already removed.
    /// `u128::MAX` denotes exhaustion; spawning never wraps or reuses an identity.
    pub const fn next_entity_id(&self) -> u128 {
        self.next_id
    }

    /// Restore a persisted watermark after rebuilding the live entity table.
    /// A checkpoint may advance the derived watermark but must never rewind it.
    pub fn restore_next_entity_id(&mut self, next: u128) -> Result<(), EntityError> {
        if next == 0 || next < self.next_id {
            return Err(EntityError::InvalidId);
        }
        self.next_id = next;
        Ok(())
    }

    /// Spawn an entity at a position.
    pub fn spawn(&mut self, position: Position) -> Result<EntityHandle, EntityError> {
        if !position.is_finite() {
            return Err(EntityError::InvalidPosition);
        }
        let id = EntityId(self.next_id);
        id.validate().map_err(|_| EntityError::IdExhausted)?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(EntityError::IdExhausted)?;

        let section = position
            .section(self.side)
            .ok_or(EntityError::InvalidPosition)?;
        let entity = Entity {
            id,
            position,
            components: ComponentSet::new(),
        };
        let (slot_index, generation) = match self.free.pop() {
            Some(index) => {
                let slot = &mut self.slots[index as usize];
                debug_assert!(slot.entity.is_none());
                slot.entity = Some(entity);
                (index, slot.generation)
            }
            None => {
                let index =
                    u32::try_from(self.slots.len()).map_err(|_| EntityError::IdExhausted)?;
                self.slots.push(Slot {
                    generation: 1,
                    entity: Some(entity),
                });
                (index, 1)
            }
        };
        self.index.entry(section).or_default().push(slot_index);
        if let Some(list) = self.index.get_mut(&section) {
            list.sort_unstable();
        }
        self.live += 1;
        self.by_id.insert(id, slot_index);
        Ok(EntityHandle {
            slot: slot_index,
            generation,
        })
    }

    /// Insert an entity with a given persistent identity.
    ///
    /// Used by restore. The identity must be valid, unique, and not zero; the
    /// store advances its own allocator past it so later spawns cannot collide.
    pub fn insert_restored(
        &mut self,
        id: EntityId,
        position: Position,
        components: ComponentSet,
    ) -> Result<EntityHandle, EntityError> {
        id.validate().map_err(|_| EntityError::InvalidId)?;
        if !position.is_finite() {
            return Err(EntityError::InvalidPosition);
        }
        if self.by_id.contains_key(&id) {
            return Err(EntityError::DuplicateId { id });
        }
        let section = position
            .section(self.side)
            .ok_or(EntityError::InvalidPosition)?;
        let entity = Entity {
            id,
            position,
            components,
        };
        let (slot_index, generation) = match self.free.pop() {
            Some(index) => {
                let slot = &mut self.slots[index as usize];
                slot.entity = Some(entity);
                (index, slot.generation)
            }
            None => {
                let index =
                    u32::try_from(self.slots.len()).map_err(|_| EntityError::IdExhausted)?;
                self.slots.push(Slot {
                    generation: 1,
                    entity: Some(entity),
                });
                (index, 1)
            }
        };
        self.index.entry(section).or_default().push(slot_index);
        if let Some(list) = self.index.get_mut(&section) {
            list.sort_unstable();
        }
        self.live += 1;
        self.by_id.insert(id, slot_index);
        self.next_id = self.next_id.max(id.0.saturating_add(1));
        Ok(EntityHandle {
            slot: slot_index,
            generation,
        })
    }

    /// Remove an entity and invalidate its handle.
    pub fn despawn(&mut self, handle: EntityHandle) -> Option<Entity> {
        let slot = self.slots.get_mut(handle.slot as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let entity = slot.entity.take()?;
        self.by_id.remove(&entity.id);
        if let Some(section) = entity.position.section(self.side) {
            let empty = match self.index.get_mut(&section) {
                Some(list) => {
                    list.retain(|index| *index != handle.slot);
                    list.is_empty()
                }
                None => false,
            };
            if empty {
                self.index.remove(&section);
            }
        }
        if slot.generation < u32::MAX {
            slot.generation += 1;
            self.free.push(handle.slot);
        }
        self.live -= 1;
        Some(entity)
    }

    /// Borrow an entity by handle.
    pub fn get(&self, handle: EntityHandle) -> Option<&Entity> {
        let slot = self.slots.get(handle.slot as usize)?;
        (slot.generation == handle.generation)
            .then_some(slot.entity.as_ref())
            .flatten()
    }

    /// Mutate the component set of an entity.
    pub fn components_mut(&mut self, handle: EntityHandle) -> Option<&mut ComponentSet> {
        let slot = self.slots.get_mut(handle.slot as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.entity.as_mut().map(|entity| &mut entity.components)
    }

    /// Move an entity and update the index.
    pub fn move_to(&mut self, handle: EntityHandle, position: Position) -> Result<(), EntityError> {
        if !position.is_finite() {
            return Err(EntityError::InvalidPosition);
        }
        let new_section = position
            .section(self.side)
            .ok_or(EntityError::InvalidPosition)?;
        let old_section = {
            let slot = self
                .slots
                .get_mut(handle.slot as usize)
                .ok_or(EntityError::StaleHandle)?;
            if slot.generation != handle.generation {
                return Err(EntityError::StaleHandle);
            }
            let entity = slot.entity.as_mut().ok_or(EntityError::StaleHandle)?;
            let old = entity.position.section(self.side);
            entity.position = position;
            old
        };
        if old_section == Some(new_section) {
            return Ok(());
        }
        if let Some(section) = old_section {
            let empty = match self.index.get_mut(&section) {
                Some(list) => {
                    list.retain(|index| *index != handle.slot);
                    list.is_empty()
                }
                None => false,
            };
            if empty {
                self.index.remove(&section);
            }
        }
        let list = self.index.entry(new_section).or_default();
        list.push(handle.slot);
        list.sort_unstable();
        Ok(())
    }

    /// Resolve a persistent identity without scanning every live entity.
    pub fn handle_of(&self, id: EntityId) -> Option<EntityHandle> {
        let index = *self.by_id.get(&id)?;
        let slot = self.slots.get(index as usize)?;
        Some(EntityHandle {
            slot: index,
            generation: slot.generation,
        })
    }

    /// Validate an entire movement group before publishing any member.
    pub fn validate_moves(&self, moves: &[EntityMove]) -> Result<(), EntityError> {
        let mut seen = std::collections::BTreeSet::new();
        for movement in moves {
            if !seen.insert(movement.id) {
                return Err(EntityError::DuplicateId { id: movement.id });
            }
            let handle = self
                .handle_of(movement.id)
                .ok_or(EntityError::StaleHandle)?;
            let entity = self.get(handle).ok_or(EntityError::StaleHandle)?;
            if entity.position.to_bits() != movement.previous_bits {
                return Err(EntityError::PositionConflict);
            }
            let position = Position::from_bits(movement.position_bits);
            if !position.is_finite() || position.section(self.side).is_none() {
                return Err(EntityError::InvalidPosition);
            }
        }
        Ok(())
    }

    /// Apply prevalidated translations and keep both spatial indexes coherent.
    ///
    /// No callbacks run between validation and application. Each identity and
    /// destination is already checked before the first entity is changed.
    pub fn apply_moves(&mut self, moves: &[EntityMove]) -> Result<(), EntityError> {
        self.validate_moves(moves)?;
        for movement in moves {
            let handle = self
                .handle_of(movement.id)
                .ok_or(EntityError::StaleHandle)?;
            self.move_to(handle, Position::from_bits(movement.position_bits))?;
        }
        Ok(())
    }

    /// Handles of entities whose position falls inside a section.
    pub fn entities_in_section(
        &self,
        section: SectionPos,
    ) -> impl Iterator<Item = EntityHandle> + '_ {
        let slots = &self.slots;
        self.index
            .get(&section)
            .into_iter()
            .flat_map(move |indices| indices.iter())
            .filter_map(move |index| {
                let slot = slots.get(*index as usize)?;
                slot.entity.as_ref().map(|_| EntityHandle {
                    slot: *index,
                    generation: slot.generation,
                })
            })
    }

    /// Number of live entities.
    pub fn len(&self) -> usize {
        self.live
    }

    /// Return whether no entity is live.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Iterate live entities with their handles, ordered by slot.
    pub fn iter(&self) -> impl Iterator<Item = (EntityHandle, &Entity)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.entity.as_ref().map(|entity| {
                (
                    EntityHandle {
                        slot: index as u32,
                        generation: slot.generation,
                    },
                    entity,
                )
            })
        })
    }

    /// Number of entries in the spatial index, used by consistency checks.
    pub fn indexed_count(&self) -> usize {
        self.index.values().map(Vec::len).sum()
    }
}

/// Failure produced by entity storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityError {
    /// Lifecycle structure, allocation arithmetic, or target overlap is invalid.
    InvalidLifecycle,
    /// The allocation watermark or a complete removal observation has changed.
    LifecycleConflict,
    /// A component observed by the transaction has changed.
    ComponentConflict,
    /// Component fields, schema, duplicates, or group size are invalid.
    InvalidComponents,
    /// A recorded source position no longer matches the authority.
    PositionConflict,
    /// The section side is outside the supported range.
    InvalidSide {
        /// Requested side.
        side: u32,
    },
    /// A position was not finite or could not be converted to a section.
    InvalidPosition,
    /// The handle is stale or was never issued.
    StaleHandle,
    /// The persistent identifier space is exhausted.
    IdExhausted,
    /// A persisted identifier was zero or otherwise invalid.
    InvalidId,
    /// Restore tried to insert an identity that is already live.
    DuplicateId {
        /// Conflicting identity.
        id: EntityId,
    },
}

impl std::fmt::Display for EntityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLifecycle => write!(f, "entity lifecycle group is invalid"),
            Self::LifecycleConflict => write!(f, "entity lifecycle observation changed"),
            Self::ComponentConflict => write!(f, "entity component no longer matches the record"),
            Self::InvalidComponents => write!(f, "entity component group is invalid"),
            Self::PositionConflict => write!(f, "entity position no longer matches the record"),
            Self::InvalidSide { side } => write!(f, "section side {side} is not supported"),
            Self::InvalidPosition => write!(f, "entity position is not finite"),
            Self::StaleHandle => write!(f, "entity handle is stale"),
            Self::IdExhausted => write!(f, "entity id space exhausted"),
            Self::InvalidId => write!(f, "entity id is invalid"),
            Self::DuplicateId { id } => write!(f, "entity {} already exists", id.0),
        }
    }
}

impl std::error::Error for EntityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentSchemas;
    use torse_content::{compile, m0_fixture_pack};
    use torse_types::ContentKey;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key is valid")
    }

    fn store() -> EntityStore {
        EntityStore::new(16).expect("side is valid")
    }

    #[test]
    fn component_capacity_uses_the_final_group_and_is_rechecked_before_publication() {
        use torse_content::{ComponentSchema, PersistencePolicy};
        use torse_types::ReplicationDeclaration;
        let mut pack = m0_fixture_pack();
        let ids: Vec<_> = (0..=MAX_COMPONENT_INSTANCES)
            .map(|n| key(&format!("test:component{n}")))
            .collect();
        pack.state_schemas[0]
            .value
            .component_schemas
            .extend(ids.iter().map(|id| ComponentSchema {
                id: id.clone(),
                schema_version: 1,
                owner_module: key("base:inventory"),
                fields: Vec::new(),
                persistence: PersistencePolicy::Always,
                replication: ReplicationDeclaration::SERVER_PRIVATE,
                migration_binding: None,
            }));
        let schemas = ComponentSchemas::build(&compile(pack).expect("capacity fixture"));
        let mut store = store();
        let handle = store.spawn(Position::new(2.0, 2.0, 2.0)).expect("spawn");
        let id = store.get(handle).expect("entity").id;
        for schema in &ids[..MAX_COMPONENT_INSTANCES] {
            store
                .components_mut(handle)
                .expect("components")
                .insert(&schemas, schema, BTreeMap::new())
                .expect("initial capacity");
        }
        let add = ComponentEdit {
            id,
            schema: ids[MAX_COMPONENT_INSTANCES].clone(),
            previous: None,
            fields: Some(BTreeMap::new()),
        };
        assert!(
            store
                .prepare_components(std::slice::from_ref(&add), &schemas)
                .is_err()
        );
        let remove = ComponentEdit {
            id,
            schema: ids[0].clone(),
            previous: Some(BTreeMap::new()),
            fields: None,
        };
        // The record intentionally adds first. Only its final component count
        // matters, and no observer can see the intermediate private mutation.
        let batch = store
            .prepare_components(&[add, remove], &schemas)
            .expect("capacity-neutral group");
        store.publish_components(batch).expect("publish");
        assert_eq!(
            store.get(handle).expect("entity").components.len(),
            MAX_COMPONENT_INSTANCES
        );
        store
            .components_mut(handle)
            .expect("components")
            .remove(&ids[1]);
        let add = ComponentEdit {
            id,
            schema: ids[0].clone(),
            previous: None,
            fields: Some(BTreeMap::new()),
        };
        let batch = store
            .prepare_components(std::slice::from_ref(&add), &schemas)
            .expect("prepare");
        store
            .components_mut(handle)
            .expect("components")
            .insert(&schemas, &ids[1], BTreeMap::new())
            .expect("external insertion");
        assert_eq!(
            store.publish_components(batch),
            Err(EntityError::InvalidComponents)
        );
        assert!(
            store
                .get(handle)
                .expect("entity")
                .components
                .component(&ids[0])
                .is_none()
        );
        assert!(
            store
                .prepare_components(&vec![add; MAX_COMPONENT_EDITS + 1], &schemas)
                .is_err()
        );
    }

    #[test]
    fn spawn_assigns_persistent_ids_and_handles() {
        let mut store = store();
        let first = store
            .spawn(Position::new(0.5, 0.5, 0.5))
            .expect("spawn works");
        let second = store
            .spawn(Position::new(1.5, 0.5, 0.5))
            .expect("spawn works");
        assert_eq!(store.get(first).map(|entity| entity.id), Some(EntityId(1)));
        assert_eq!(store.get(second).map(|entity| entity.id), Some(EntityId(2)));
        assert_eq!(store.len(), 2);
        assert_eq!(store.indexed_count(), 2);
    }

    #[test]
    fn despawn_invalidates_the_handle_and_reuse_bumps_generation() {
        let mut store = store();
        let old = store
            .spawn(Position::new(0.5, 0.5, 0.5))
            .expect("spawn works");
        assert!(store.despawn(old).is_some());
        assert!(store.get(old).is_none());
        let reused = store
            .spawn(Position::new(2.5, 0.5, 0.5))
            .expect("spawn works");
        assert_eq!(reused.slot, old.slot);
        assert_ne!(reused.generation, old.generation);
        assert!(store.get(old).is_none());
        assert!(store.get(reused).is_some());
        assert_eq!(store.indexed_count(), 1);
    }

    #[test]
    fn moving_across_a_section_boundary_updates_the_index() {
        let mut store = store();
        let handle = store
            .spawn(Position::new(15.5, 0.5, 0.5))
            .expect("spawn works");
        assert_eq!(store.entities_in_section(SectionPos::ZERO).count(), 1);
        store
            .move_to(handle, Position::new(16.5, 0.5, 0.5))
            .expect("move works");
        assert_eq!(store.entities_in_section(SectionPos::ZERO).count(), 0);
        assert_eq!(
            store.entities_in_section(SectionPos::new(1, 0, 0)).count(),
            1
        );
        assert_eq!(store.indexed_count(), 1);
    }

    #[test]
    fn negative_positions_use_negative_sections() {
        let mut store = store();
        let handle = store
            .spawn(Position::new(-0.5, 0.5, -17.5))
            .expect("spawn works");
        assert_eq!(
            store
                .entities_in_section(SectionPos::new(-1, 0, -2))
                .count(),
            1
        );
        assert_eq!(
            store.get(handle).map(|entity| entity.position.block()),
            Some(BlockPos::new(-1, 0, -18))
        );
    }

    #[test]
    fn invalid_positions_are_rejected() {
        let mut store = store();
        assert_eq!(
            store.spawn(Position::new(f64::NAN, 0.0, 0.0)),
            Err(EntityError::InvalidPosition)
        );
        let handle = store
            .spawn(Position::new(0.5, 0.5, 0.5))
            .expect("spawn works");
        assert_eq!(
            store.move_to(handle, Position::new(0.0, f64::INFINITY, 0.0)),
            Err(EntityError::InvalidPosition)
        );
    }

    #[test]
    fn components_follow_the_entity_and_survive_moves() {
        let mut store = store();
        let handle = store
            .spawn(Position::new(0.5, 0.5, 0.5))
            .expect("spawn works");
        let content = compile(m0_fixture_pack()).expect("fixture content compiles");
        let schemas = ComponentSchemas::build(&content);
        let schema = key("base:schema/inventory_content");
        store
            .components_mut(handle)
            .expect("entity exists")
            .insert(&schemas, &schema, Default::default())
            .expect("component is valid");
        store
            .move_to(handle, Position::new(40.5, 0.5, 0.5))
            .expect("move works");
        let entity = store.get(handle).expect("entity exists");
        assert_eq!(entity.components.len(), 1);
        assert_eq!(entity.position.block(), BlockPos::new(40, 0, 0));
    }

    #[test]
    fn stale_handle_cannot_move_or_mutate() {
        let mut store = store();
        let handle = store
            .spawn(Position::new(0.5, 0.5, 0.5))
            .expect("spawn works");
        store.despawn(handle);
        assert_eq!(
            store.move_to(handle, Position::new(1.5, 0.5, 0.5)),
            Err(EntityError::StaleHandle)
        );
        assert!(store.components_mut(handle).is_none());
    }
}
