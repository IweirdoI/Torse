//! Explicit lifecycle records with full removal observations and bounded schema maps.

use std::collections::BTreeMap;

use torse_types::ContentKey;
use torse_world::{ComponentFields, EntityLifecycle, EntityState, MAX_ENTITY_LIFECYCLE};

use crate::value::{
    ascending, charge_vec, invalid, read_component_fields, read_entity, read_items, read_key,
    read_position, write_component_fields, write_entity, write_items, write_key, write_position,
};
use crate::{Decoder, EncodeResult, Encoder};

pub(crate) fn write_lifecycle(
    out: &mut Encoder<'_>,
    change: Option<&EntityLifecycle>,
) -> EncodeResult<()> {
    let Some(change) = change else {
        return out.write_u8(0);
    };
    change
        .validate_structure()
        .map_err(|error| invalid(error.to_string()))?;
    out.write_u8(1)?;
    out.write_u128(change.previous_next_id)?;
    out.write_u128(change.next_id)?;
    write_states(out, &change.spawned)?;
    write_states(out, &change.despawned)
}

pub(crate) fn read_lifecycle(input: &mut Decoder<'_>) -> EncodeResult<Option<EntityLifecycle>> {
    match input.read_u8()? {
        0 => return Ok(None),
        1 => {}
        _ => return Err(invalid("unknown lifecycle presence tag")),
    }
    let change = EntityLifecycle {
        previous_next_id: input.read_u128()?,
        next_id: input.read_u128()?,
        spawned: read_states(input)?,
        despawned: read_states(input)?,
    };
    change
        .validate_structure()
        .map_err(|error| invalid(error.to_string()))?;
    Ok(Some(change))
}

fn write_states(out: &mut Encoder<'_>, states: &[EntityState]) -> EncodeResult<()> {
    write_items(out, states.len(), "lifecycle entity")?;
    for state in states {
        write_entity(out, state.id)?;
        write_position(out, state.position_bits)?;
        write_items(out, state.components.len(), "lifecycle component")?;
        for (schema, fields) in &state.components {
            write_key(out, schema)?;
            write_component_fields(out, fields, 0)?;
        }
    }
    Ok(())
}

fn read_states(input: &mut Decoder<'_>) -> EncodeResult<Vec<EntityState>> {
    let count = read_items(input, 16 + 24 + 4, "lifecycle entity")?;
    if count > MAX_ENTITY_LIFECYCLE {
        return Err(invalid("lifecycle entity count exceeds limit"));
    }
    charge_vec::<EntityState>(input, count)?;
    let mut states = Vec::with_capacity(count);
    for _ in 0..count {
        let id = read_entity(input)?;
        let position_bits = read_position(input)?;
        let count = read_items(input, 1 + 4, "lifecycle component")?;
        if count > torse_world::MAX_COMPONENT_INSTANCES {
            return Err(invalid("lifecycle component count exceeds limit"));
        }
        charge_vec::<(ContentKey, ComponentFields)>(input, count)?;
        let mut components = BTreeMap::new();
        for _ in 0..count {
            let schema = read_key(input)?;
            ascending(
                components.last_key_value().map(|(key, _)| key),
                &schema,
                "component",
            )?;
            let fields = read_component_fields(input)?;
            components.insert(schema, fields);
        }
        states.push(EntityState {
            id,
            position_bits,
            components,
        });
    }
    Ok(states)
}
