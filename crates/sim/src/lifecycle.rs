//! Propose entity identities and enforce component ownership before staging.

use torse_types::RejectCode;
use torse_world::{
    ComponentSchemas, EntityError, EntityLifecycle, EntityState, EntityStore, LifecycleBatch,
    MAX_ENTITY_LIFECYCLE, SectionStore,
};

use crate::Transaction;

pub(crate) fn prepare_lifecycle(
    store: &SectionStore,
    entities: &EntityStore,
    schemas: &ComponentSchemas,
    transaction: &Transaction,
) -> Result<Option<LifecycleBatch>, RejectCode> {
    if transaction.spawns.len() + transaction.despawns.len() > MAX_ENTITY_LIFECYCLE {
        return Err(RejectCode::QuotaExceeded);
    }
    if transaction.spawns.is_empty() && transaction.despawns.is_empty() {
        return Ok(None);
    }
    EntityLifecycle::validate_payload(
        transaction.spawns.len() + transaction.despawns.len(),
        transaction
            .spawns
            .iter()
            .map(|spawn| &spawn.components)
            .chain(
                transaction
                    .despawns
                    .iter()
                    .map(|despawn| &despawn.previous.components),
            ),
    )
    .map_err(|_| RejectCode::QuotaExceeded)?;
    for request in &transaction.despawns {
        if entities
            .get(request.handle)
            .is_none_or(|entity| entity.id != request.previous.id)
        {
            return Err(RejectCode::MissingTarget);
        }
    }
    let previous_next_id = entities.next_entity_id();
    let next_id = previous_next_id
        .checked_add(transaction.spawns.len() as u128)
        .ok_or(RejectCode::QuotaExceeded)?;
    for components in transaction
        .spawns
        .iter()
        .map(|spawn| &spawn.components)
        .chain(
            transaction
                .despawns
                .iter()
                .map(|despawn| &despawn.previous.components),
        )
    {
        if components.contains_key(&torse_world::block_instance_schema()) {
            return Err(RejectCode::PermissionDenied);
        }
        for schema in components.keys() {
            let schema = schemas.get(schema).ok_or(RejectCode::InvalidData)?;
            if schema.owner_module != transaction.source {
                return Err(RejectCode::PermissionDenied);
            }
        }
    }
    let change = EntityLifecycle {
        previous_next_id,
        next_id,
        spawned: transaction
            .spawns
            .iter()
            .enumerate()
            .map(|(index, spawn)| EntityState {
                id: torse_types::EntityId(previous_next_id + index as u128),
                position_bits: spawn.position_bits,
                components: spawn.components.clone(),
            })
            .collect(),
        despawned: transaction
            .despawns
            .iter()
            .map(|despawn| despawn.previous.clone())
            .collect(),
    };
    if transaction
        .movements
        .iter()
        .any(|movement| change.conflicts_with(movement.id))
        || transaction
            .components
            .iter()
            .any(|edit| change.conflicts_with(edit.id))
    {
        return Err(RejectCode::InvalidData);
    }
    change
        .validate_world(store)
        .map_err(|_| RejectCode::InvalidData)?;
    entities
        .prepare_lifecycle(&change, schemas)
        .map(Some)
        .map_err(|error| match error {
            EntityError::LifecycleConflict => RejectCode::Conflict,
            EntityError::StaleHandle => RejectCode::MissingTarget,
            EntityError::IdExhausted => RejectCode::QuotaExceeded,
            _ => RejectCode::InvalidData,
        })
}
