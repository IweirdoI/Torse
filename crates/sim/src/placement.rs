//! Placement occupancy conditions survive planning and are sealed before staging.

use std::collections::BTreeSet;

use torse_types::RejectCode;
use torse_world::{BlockStateRegistry, EntityLifecycle, EntityMove, EntityStore, Position};
use torse_world_services::Aabb;

use crate::transaction::{MAX_CLEARANCE_CHECKS, Mutation};

/// Resolved world bounds use exact bits so prepared changes retain value equality.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Volume {
    min: [u64; 3],
    max: [u64; 3],
}

pub(crate) fn prepare(
    registry: &BlockStateRegistry,
    writes: &[Mutation],
    checks: &[Mutation],
) -> Result<Vec<Volume>, RejectCode> {
    if checks.len() > MAX_CLEARANCE_CHECKS {
        return Err(RejectCode::QuotaExceeded);
    }
    let mut seen = BTreeSet::new();
    let mut volumes = Vec::new();
    for check in checks {
        if check.state.is_empty()
            || !seen.insert(check.pos)
            || writes.iter().rev().find(|w| w.pos == check.pos) != Some(check)
            || registry.definition(check.state).is_none()
        {
            return Err(RejectCode::InvalidData);
        }
        if let Some(shape) = registry.collision_shape(check.state) {
            volumes.extend(crate::movement::boxes(&shape).into_iter().map(|local| {
                let bounds = local.at(check.pos);
                Volume {
                    min: bounds.min.map(f64::to_bits),
                    max: bounds.max.map(f64::to_bits),
                }
            }));
        }
    }
    Ok(volumes)
}

/// Test the complete resulting entity set, including sibling moves and spawns.
/// Attached block instances have no implied collider. Ordinary entities
/// currently use the walker bounds until generic declarations are available.
pub(crate) fn clear(
    volumes: &[Volume],
    entities: &EntityStore,
    moves: &[EntityMove],
    lifecycle: Option<&EntityLifecycle>,
) -> bool {
    if volumes.is_empty() {
        return true;
    }
    let binding = torse_world::block_instance_schema();
    let overlaps = |position: Position| {
        let collider = Aabb::from_center(
            [position.x, position.y, position.z],
            crate::player::WALKER_HALF,
        );
        volumes.iter().any(|volume| {
            Aabb {
                min: volume.min.map(f64::from_bits),
                max: volume.max.map(f64::from_bits),
            }
            .overlaps(&collider)
        })
    };
    for (_, entity) in entities.iter() {
        if entity.components.component(&binding).is_some()
            || lifecycle.is_some_and(|l| l.despawned.iter().any(|e| e.id == entity.id))
        {
            continue;
        }
        let position = moves
            .iter()
            .find(|m| m.id == entity.id)
            .map_or(entity.position, |m| Position::from_bits(m.position_bits));
        if overlaps(position) {
            return false;
        }
    }
    !lifecycle.is_some_and(|l| {
        l.spawned.iter().any(|e| {
            !e.components.contains_key(&binding) && overlaps(Position::from_bits(e.position_bits))
        })
    })
}
