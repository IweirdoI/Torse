//! Server-authoritative player movement domain, per the movement contract.
//!
//! The same integration drives both sides: the server settles inputs through
//! [`walker_step`] on its own authority, and a client predicts locally with
//! the identical function so divergence stays explainable. Input sequencing,
//! reconciliation, and session-to-player bindings round out the pieces the
//! transport and execution layers wire up.

use std::collections::BTreeMap;

use torse_types::{
    EntityHandle, EntityId, RejectCode, SessionId, TransactionId, TransactionOutcome,
};
use torse_world::Position;

use crate::coordinator::CommitFault;
use crate::{OrderingKey, Phase, Transaction, WorldAuthority};

/// Walk speed in blocks per second.
pub const WALK_SPEED: f64 = 4.3;
/// Gravity in blocks per second squared.
pub const GRAVITY: f64 = 28.0;
/// Jump launch speed in blocks per second.
pub const JUMP_SPEED: f64 = 8.4;
/// Player collider half extents: 0.6 wide, 1.8 tall, centre at body middle.
pub const WALKER_HALF: [f64; 3] = [0.3, 0.9, 0.3];
/// Default reconciliation tolerance in blocks; beyond it the client snaps.
pub const RECONCILE_TOLERANCE: f64 = 0.05;

/// Client-side integration state for one walking entity.
#[derive(Clone, Copy, Debug)]
pub struct WalkerState {
    /// Runtime handle of the player entity.
    pub handle: EntityHandle,
    /// Velocity in blocks per second; vertical persists between ticks.
    pub velocity: [f64; 3],
    /// Whether the last step rested on ground or a ceiling stopped the rise.
    pub grounded: bool,
}

impl WalkerState {
    /// Bind integration state to a freshly spawned entity at rest.
    pub const fn new(handle: EntityHandle) -> Self {
        Self {
            handle,
            velocity: [0.0; 3],
            grounded: false,
        }
    }
}

/// Why one walker step could not run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepError {
    /// The observed entity no longer exists.
    EntityGone,
    /// The request was rejected; the entity stays where it is.
    Rejected(RejectCode),
    /// Publication failed; the world stopped by invariant.
    Fault(CommitFault),
}

/// Integrate one walk tick and commit the displacement as a movement
/// transaction; the authority resolves terrain collision in `prepare`.
///
/// `wish` is a normalized horizontal direction, `jump` requests a jump while
/// grounded, and `source` names the settling system in the record. Rejections
/// (a stale observation, for instance) drop the tick and keep the entity at
/// its committed position.
pub fn walker_step(
    world: &mut WorldAuthority,
    state: &mut WalkerState,
    wish: [f64; 3],
    jump: bool,
    dt: f32,
    source: &torse_types::ContentKey,
    transaction: TransactionId,
) -> Result<(Position, bool), StepError> {
    let Some(entity) = world.entities().get(state.handle) else {
        return Err(StepError::EntityGone);
    };
    let (id, previous) = (entity.id, entity.position);
    let half = crate::body::of(entity, &[], WALKER_HALF)
        .map_err(StepError::Rejected)?
        .unwrap_or(WALKER_HALF);

    state.velocity[0] = wish[0] * WALK_SPEED;
    state.velocity[2] = wish[2] * WALK_SPEED;
    state.velocity[1] -= GRAVITY * f64::from(dt);
    if state.grounded && jump {
        state.velocity[1] = JUMP_SPEED;
    }
    let dt = f64::from(dt);
    let requested = previous.translated([
        state.velocity[0] * dt,
        state.velocity[1] * dt,
        state.velocity[2] * dt,
    ]);

    let mut tx = Transaction::new(
        transaction,
        source.clone(),
        OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: u64::try_from(transaction.0).unwrap_or(u64::MAX),
            local_sequence: 0,
        },
    );
    tx.move_entity(
        state.handle,
        id,
        previous,
        [
            requested.x - previous.x,
            requested.y - previous.y,
            requested.z - previous.z,
        ],
        half,
    );
    let result = world.commit(&tx);

    // A rejection with the entity embedded in terrain is the wedged state:
    // movement refuses colliders that already overlap. Try to escape to the
    // nearest free position; when no neighbour is free the walker stays put
    // by contract. The wedged check runs while the entity borrow is alive,
    // then the escape takes the world mutably.
    let wedged_now = match &result {
        Ok(outcome) => {
            matches!(outcome.outcome, TransactionOutcome::Rejected(_))
                && wedged(world, state.handle, id) == Ok(true)
        }
        Err(_) => false,
    };

    if wedged_now
        && let Ok(Some(escaped)) = unstuck(
            world,
            state,
            source,
            TransactionId(transaction.0.wrapping_add(1)),
        )
    {
        return Ok((escaped, false));
    }

    let Some(entity) = world.entities().get(state.handle) else {
        return Err(StepError::EntityGone);
    };
    match result {
        Ok(outcome) => {
            if let TransactionOutcome::Rejected(code) = outcome.outcome {
                // Drop this tick: the entity stays at its committed position.
                let _ = code;
                state.velocity[1] = 0.0;
                return Ok((entity.position, state.grounded));
            }
        }
        Err(fault) => return Err(StepError::Fault(fault)),
    }

    // Resting above the request means the descent was blocked: that is the
    // floor. Ending below the request means a ceiling stopped the rise.
    let settled = entity.position;
    if settled.y - requested.y > 1e-9 {
        state.grounded = true;
        state.velocity[1] = 0.0;
    } else if requested.y - settled.y > 1e-9 {
        state.velocity[1] = 0.0;
        state.grounded = false;
    } else {
        state.grounded = false;
    }
    Ok((settled, state.grounded))
}

/// Escape a wedged entity: relocate it to the nearest free position and
/// commit the move.
///
/// The start may overlap terrain -- that is the condition being escaped.
/// `None` means no free neighbour was reachable within the escape budget;
/// the entity stays where it is.
pub fn unstuck(
    world: &mut WorldAuthority,
    state: &mut WalkerState,
    source: &torse_types::ContentKey,
    transaction: TransactionId,
) -> Result<Option<Position>, StepError> {
    let Some(entity) = world.entities().get(state.handle) else {
        return Err(StepError::EntityGone);
    };
    let (id, previous) = (entity.id, entity.position);
    let half = crate::body::of(entity, &[], WALKER_HALF)
        .map_err(StepError::Rejected)?
        .unwrap_or(WALKER_HALF);
    let move_result = crate::movement::prepare_escape(
        world.store(),
        world.entities(),
        world.registry(),
        state.handle,
        id,
        half,
    );
    let Some(entity_move) = move_result.map_err(StepError::Rejected)? else {
        return Ok(None);
    };
    let target = Position::from_bits(entity_move.position_bits);
    let delta = [
        target.x - previous.x,
        target.y - previous.y,
        target.z - previous.z,
    ];
    let mut tx = Transaction::new(
        transaction,
        source.clone(),
        OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: u64::try_from(transaction.0).unwrap_or(u64::MAX),
            local_sequence: 0,
        },
    );
    tx.movements.push(crate::Movement {
        handle: state.handle,
        id,
        previous_bits: previous.to_bits(),
        delta_bits: delta.map(f64::to_bits),
        half_bits: half.map(f64::to_bits),
    });
    let result = world.commit_escape(&tx);
    let Some(entity) = world.entities().get(state.handle) else {
        return Err(StepError::EntityGone);
    };
    match result {
        Ok(committed) => {
            if !committed {
                return Ok(None);
            }
            state.velocity = [0.0; 3];
            state.grounded = false;
            Ok(Some(entity.position))
        }
        Err(fault) => Err(StepError::Fault(fault)),
    }
}

/// Whether the entity's collider currently overlaps terrain.
fn wedged(world: &WorldAuthority, handle: EntityHandle, id: EntityId) -> Result<bool, RejectCode> {
    let entity = world
        .entities()
        .get(handle)
        .ok_or(RejectCode::MissingTarget)?;
    if entity.id != id {
        return Err(RejectCode::MissingTarget);
    }
    use torse_world_services::{Aabb, CollisionCell, CollisionSource, collides};
    struct View<'a> {
        world: &'a WorldAuthority,
    }
    impl CollisionSource for View<'_> {
        fn shape_reach(&self) -> ([u8; 3], [u8; 3]) {
            self.world.registry().collision_reach()
        }
        fn collision(&self, pos: torse_types::BlockPos) -> CollisionCell {
            match self.world.store().query(pos) {
                torse_world::TerrainQuery::Ready(state) => {
                    match self.world.registry().collision_shape_ref(state) {
                        None => CollisionCell::Empty,
                        Some(shape) => CollisionCell::Solid(crate::movement::boxes(shape)),
                    }
                }
                torse_world::TerrainQuery::NotLoaded => CollisionCell::NotLoaded,
                torse_world::TerrainQuery::OutOfBounds => CollisionCell::OutOfBounds,
            }
        }
    }
    let Some(half) = crate::body::of(entity, &[], WALKER_HALF)? else {
        return Ok(false);
    };
    let collider = Aabb::from_center(
        [entity.position.x, entity.position.y, entity.position.z],
        half,
    );
    Ok(collides(&View { world }, &collider))
}

/// One tick of aggregated movement intent.
///
/// The wish direction is quantized to unit axes, matching key input; the
/// settling side normalizes it before integration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MoveInput {
    /// Session-scoped sequence, monotonic from one.
    pub seq: u64,
    /// Horizontal intent in world axes, each component -1, 0, or 1.
    pub wish: [i8; 3],
    /// Whether the jump key is held this tick.
    pub jump: bool,
}

impl MoveInput {
    /// Normalized world-space wish direction for [`walker_step`].
    pub fn direction(&self) -> [f64; 3] {
        // Horizontal only: vertical motion comes from gravity and jumps,
        // never from network input. A client sending a nonzero wish y gains
        // nothing.
        let raw = [f64::from(self.wish[0]), 0.0, f64::from(self.wish[2])];
        let length = (raw[0] * raw[0] + raw[2] * raw[2]).sqrt();
        if length == 0.0 {
            return [0.0; 3];
        }
        [raw[0] / length, 0.0, raw[2] / length]
    }
}

/// Bounded server-side buffer of pending movement inputs.
///
/// Sequence numbers are monotonic per session: duplicates and inputs at or
/// below the executed watermark are dropped on arrival, never re-executed.
/// A full buffer evicts the oldest pending input and raises the resync flag,
/// per the movement contract: the client replays its unconfirmed inputs
/// after the reset instead of the server guessing which intent to keep.
#[derive(Debug)]
pub struct MoveInputBuffer {
    capacity: usize,
    watermark: u64,
    pending: BTreeMap<u64, MoveInput>,
    resync_required: bool,
}

impl MoveInputBuffer {
    /// Buffer holding at most `capacity` unexecuted inputs.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            watermark: 0,
            pending: BTreeMap::new(),
            resync_required: false,
        }
    }

    /// Highest sequence already handed to execution.
    pub const fn watermark(&self) -> u64 {
        self.watermark
    }

    /// Queue one input; stale and duplicate sequences are dropped, and a full
    /// buffer evicts the oldest pending input and raises the resync flag.
    pub fn push(&mut self, input: MoveInput) {
        if input.seq <= self.watermark || self.pending.contains_key(&input.seq) {
            return;
        }
        while self.pending.len() >= self.capacity {
            let oldest = *self.pending.keys().next().expect("non-empty map");
            self.pending.remove(&oldest);
            self.resync_required = true;
        }
        self.pending.insert(input.seq, input);
    }

    /// Whether an overflow evicted pending inputs since the last check.
    /// The host sends a resync demand (ResetRequired) when this is set.
    pub fn take_resync(&mut self) -> bool {
        std::mem::take(&mut self.resync_required)
    }

    /// Take up to `limit` inputs in sequence order for this tick's
    /// settlement, advancing the watermark past them.
    pub fn settle(&mut self, limit: usize) -> Vec<MoveInput> {
        let taken: Vec<u64> = self.pending.keys().copied().take(limit).collect();
        taken
            .into_iter()
            .map(|seq| {
                self.watermark = seq;
                self.pending.remove(&seq).expect("key came from the map")
            })
            .collect()
    }
}

/// What a client should do with its prediction after an authoritative result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Correction {
    /// Prediction is close enough: keep the local position and rebase future
    /// integration onto the authoritative one.
    Keep { base: Position },
    /// Diverged beyond the tolerance: jump to the authoritative position.
    Snap(Position),
}

/// Compare a predicted position against the authoritative one.
pub fn reconcile(predicted: Position, authoritative: Position, tolerance: f64) -> Correction {
    let drift = (predicted.x - authoritative.x).abs()
        + (predicted.y - authoritative.y).abs()
        + (predicted.z - authoritative.z).abs();
    if drift <= tolerance {
        Correction::Keep {
            base: authoritative,
        }
    } else {
        Correction::Snap(authoritative)
    }
}

/// Session-to-player-entity bindings: one each way, per the contract.
#[derive(Debug, Default)]
pub struct PlayerBindings {
    by_session: BTreeMap<u128, EntityId>,
    by_entity: BTreeMap<EntityId, u128>,
}

impl PlayerBindings {
    /// Bind a session to a player entity; both sides must be free.
    pub fn bind(&mut self, session: SessionId, entity: EntityId) -> Result<(), RejectCode> {
        if self.by_session.contains_key(&session.0) || self.by_entity.contains_key(&entity) {
            return Err(RejectCode::Conflict);
        }
        self.by_session.insert(session.0, entity);
        self.by_entity.insert(entity, session.0);
        Ok(())
    }

    /// Drop a binding, if present; revocation and disconnect both land here.
    pub fn unbind(&mut self, session: SessionId) {
        if let Some(entity) = self.by_session.remove(&session.0) {
            self.by_entity.remove(&entity);
        }
    }

    /// The player entity bound to a session.
    pub fn entity_of(&self, session: SessionId) -> Option<EntityId> {
        self.by_session.get(&session.0).copied()
    }

    /// The session that owns a player entity.
    pub fn session_of(&self, entity: EntityId) -> Option<SessionId> {
        self.by_entity.get(&entity).copied().map(SessionId)
    }
}
