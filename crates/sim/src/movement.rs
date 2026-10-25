//! Bounded, read-only preparation of authoritative entity translations.
//!
//! Collision queries are recorded before applying the transaction's terrain
//! overlay. Publication checks those observations again before any write. This
//! catches a host mutating terrain between prepare and publish, including a
//! previously absent section becoming loaded. A journal can reject or drop the
//! prepared record without moving an entity or changing an index.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use torse_content::Shape;
use torse_types::{BlockPos, RejectCode};
use torse_world::{
    BlockEdit, BlockStateRegistry, EntityMove, EntityStore, Position, SectionStore, TerrainQuery,
};
use torse_world_services::{Aabb, CollisionCell, CollisionSource, LocalBox, collides, sweep};

use crate::transaction::Movement;

/// Prototype limit for each absolute center coordinate, in block units.
pub const MAX_MOVEMENT_COORDINATE: f64 = 1_048_576.0;
/// Maximum requested translation along one axis in one transaction.
pub const MAX_MOVEMENT_DELTA: f64 = 16.0;
/// Largest collider half extent along one axis.
pub const MAX_COLLIDER_HALF: f64 = 4.0;
/// Maximum entity translations in a single atomic transaction.
pub const MAX_TRANSACTION_MOVEMENTS: usize = 256;

pub(crate) struct ResolvedMovements {
    pub moves: Vec<EntityMove>,
    pub reads: Vec<(BlockPos, TerrainQuery)>,
}

pub(crate) fn prepare_movements(
    store: &SectionStore,
    entities: &EntityStore,
    registry: &BlockStateRegistry,
    edits: &[BlockEdit],
    requests: &[Movement],
) -> Result<ResolvedMovements, RejectCode> {
    if requests.len() > MAX_TRANSACTION_MOVEMENTS {
        return Err(RejectCode::QuotaExceeded);
    }
    if entities.side() != store.side() {
        return Err(RejectCode::InvalidData);
    }
    let view = CollisionView {
        store,
        registry,
        overlay: edits.iter().map(|edit| (edit.pos, edit.state)).collect(),
        reads: RefCell::new(BTreeMap::new()),
        unknown: RefCell::new(false),
    };
    let mut seen = BTreeSet::new();
    let mut moves = Vec::with_capacity(requests.len());
    for request in requests {
        if !seen.insert(request.id) {
            return Err(RejectCode::InvalidData);
        }
        let entity = entities
            .get(request.handle)
            .ok_or(RejectCode::MissingTarget)?;
        if entity.id != request.id {
            return Err(RejectCode::MissingTarget);
        }
        if entity.position.to_bits() != request.previous_bits {
            return Err(RejectCode::Conflict);
        }
        let delta = request.delta_bits.map(f64::from_bits);
        let half = request.half_bits.map(f64::from_bits);
        let position = entity.position;
        if !bounded_position(position)
            || delta
                .iter()
                .any(|v| !v.is_finite() || v.abs() > MAX_MOVEMENT_DELTA)
            || half
                .iter()
                .any(|v| !v.is_finite() || *v <= 0.0 || *v > MAX_COLLIDER_HALF)
            || !bounded_position(position.translated(delta))
        {
            return Err(RejectCode::InvalidData);
        }
        // A collider thinner than a substep can tunnel through thin geometry.
        // Require a minimum width greater than the collision substep. The
        // existing sweep remains the shared prototype for authority/prediction.
        if half.iter().any(|v| *v < torse_world_services::MAX_SUBSTEP) {
            return Err(RejectCode::InvalidData);
        }
        let collider = Aabb::from_center([position.x, position.y, position.z], half);
        let overlapping = collides(&view, &collider);
        if *view.unknown.borrow() {
            return Err(RejectCode::InvalidData);
        }
        if overlapping {
            return Err(RejectCode::Conflict);
        }
        let result = sweep(&view, collider, delta);
        if *view.unknown.borrow() {
            return Err(RejectCode::InvalidData);
        }
        let center = result.aabb.center();
        moves.push(EntityMove {
            id: request.id,
            previous_bits: request.previous_bits,
            position_bits: Position::new(center[0], center[1], center[2]).to_bits(),
        });
    }
    Ok(ResolvedMovements {
        moves,
        reads: view.reads.into_inner().into_iter().collect(),
    })
}

fn bounded_position(position: Position) -> bool {
    position.is_finite()
        && [position.x, position.y, position.z]
            .iter()
            .all(|value| value.abs() <= MAX_MOVEMENT_COORDINATE)
}

struct CollisionView<'a> {
    store: &'a SectionStore,
    registry: &'a BlockStateRegistry,
    overlay: BTreeMap<BlockPos, torse_world::BlockStateId>,
    reads: RefCell<BTreeMap<BlockPos, TerrainQuery>>,
    unknown: RefCell<bool>,
}

impl CollisionSource for CollisionView<'_> {
    fn collision(&self, pos: BlockPos) -> CollisionCell {
        let observed = self.store.query(pos);
        self.reads.borrow_mut().insert(pos, observed);
        match observed {
            TerrainQuery::Ready(original) => {
                let state = self.overlay.get(&pos).copied().unwrap_or(original);
                if state.is_empty() {
                    return CollisionCell::Empty;
                }
                let Some(definition) = self.registry.definition(state) else {
                    *self.unknown.borrow_mut() = true;
                    return CollisionCell::NotLoaded;
                };
                match &definition.collision_profile.shape {
                    None => CollisionCell::Empty,
                    Some(shape) => CollisionCell::Solid(boxes(shape)),
                }
            }
            TerrainQuery::NotLoaded => CollisionCell::NotLoaded,
            TerrainQuery::OutOfBounds => CollisionCell::OutOfBounds,
        }
    }
}

fn boxes(shape: &Shape) -> Vec<LocalBox> {
    // Corner/stair collision uses the same conservative enclosing cube as the
    // M1 reference scenario. State-dependent multi-box geometry is deferred.
    vec![match shape {
        Shape::FullCube | Shape::Corner | Shape::Stair | Shape::Cross => LocalBox::FULL_CUBE,
        Shape::LowerSlab => LocalBox {
            min: [0.0; 3],
            max: [1.0, 0.5, 1.0],
        },
        Shape::Panel => LocalBox {
            min: [0.45, 0.0, 0.0],
            max: [0.55, 1.0, 1.0],
        },
        Shape::Box(bounds) => LocalBox {
            min: bounds.min.map(f64::from),
            max: bounds.max.map(f64::from),
        },
    }]
}
