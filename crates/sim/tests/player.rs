//! Player movement domain: shared physics, input sequencing, reconciliation,
//! and session bindings, per the movement contract.

use torse_content::{compile, m0_fixture_pack};
use torse_sim::WorldAuthority;
use torse_sim::player::{
    Correction, MoveInput, MoveInputBuffer, PlayerBindings, WalkerState, reconcile, walker_step,
};
use torse_types::{
    Compatibility, ContentKey, Digest, EntityId, RejectCode, SectionPos, SessionId, TransactionId,
    WorldId,
};
use torse_world::{Position, Section, WorldBounds};

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("key")
}

fn world() -> WorldAuthority {
    let content = compile(m0_fixture_pack()).expect("content");
    let registry = torse_world::BlockStateRegistry::build(&content).expect("registry");
    let schemas = torse_world::ComponentSchemas::build(&content);
    let mut world = WorldAuthority::new(
        WorldId(1),
        registry,
        schemas,
        16,
        WorldBounds::new(0, 32).expect("bounds"),
        Compatibility {
            authority_schema: 1,
            rules_digest: Digest([1; 32]),
            content_digest: content.presentation_digest,
        },
    )
    .expect("authority");
    for pos in [SectionPos::ZERO, SectionPos::new(1, 0, 0)] {
        world.store_mut().request(pos).expect("request");
        world
            .store_mut()
            .install(pos, Section::empty(16).expect("section"))
            .expect("install");
        world.store_mut().finish_load(pos).expect("finish");
    }
    let stone = world
        .registry()
        .default_state(&key("base:stone"))
        .expect("stone");
    // A stone floor at y = 0 inside the first section.
    let mut tx = torse_sim::Transaction::new(
        TransactionId(1),
        key("test:floor"),
        torse_sim::OrderingKey {
            phase: torse_sim::Phase::Simulation,
            rule_priority: 0,
            stable_task_key: 1,
            local_sequence: 0,
        },
    );
    for x in 0..16i64 {
        for z in 0..16i64 {
            tx.write_block(torse_types::BlockPos::new(x, 0, z), stone);
        }
    }
    match world.commit(&tx).expect("no fault").outcome {
        torse_types::TransactionOutcome::Committed { .. } => {}
        outcome => panic!("floor committed: {outcome:?}"),
    }
    world
}

#[test]
fn walker_falls_lands_and_jumps() {
    let mut world = world();
    let handle = world
        .entities_mut()
        .spawn(Position::new(8.5, 6.0, 8.5))
        .expect("spawn");
    let id = world.entities().get(handle).expect("entity").id;
    let mut state = WalkerState::new(handle);
    let source = key("test:motion");

    let mut grounded = false;
    for tick in 1..=60u64 {
        let (_, landed) = walker_step(
            &mut world,
            &mut state,
            [0.0, 0.0, 0.0],
            false,
            0.05,
            &source,
            TransactionId(u128::from(tick)),
        )
        .expect("step");
        grounded = landed;
        if grounded {
            break;
        }
    }
    assert!(grounded, "must land on the floor");
    let floor_level = world.entities().get(handle).expect("entity").position.y;
    // The floor cell spans y = 0..1, so the resting centre is one half
    // height above its top.
    assert!(
        (floor_level - 1.9).abs() < 1e-6,
        "resting centre {floor_level}"
    );

    // A jump rises on the next tick and is airborne.
    let (risen, landed) = walker_step(
        &mut world,
        &mut state,
        [0.0, 0.0, 0.0],
        true,
        0.05,
        &source,
        TransactionId(100),
    )
    .expect("jump");
    assert!(!landed);
    assert!(risen.y > floor_level);
    assert_eq!(world.entities().get(handle).expect("entity").id, id);
}

#[test]
fn input_buffer_orders_dedupes_and_caps() {
    let mut buffer = MoveInputBuffer::new(4);
    let input = |seq: u64| MoveInput {
        seq,
        wish: [0, 0, -1],
        jump: false,
    };
    // Out-of-order arrival still settles in sequence order.
    buffer.push(input(2)).expect("push");
    buffer.push(input(1)).expect("push");
    assert_eq!(buffer.watermark(), 0);
    let settled = buffer.settle(8);
    assert_eq!(
        settled.iter().map(|i| i.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(buffer.watermark(), 2);

    // Stale and duplicate sequences drop silently.
    buffer.push(input(2)).expect("push");
    buffer.push(input(1)).expect("push");
    assert!(buffer.settle(8).is_empty());

    // Capacity refuses further input.
    for seq in 3..=6 {
        buffer.push(input(seq)).expect("push");
    }
    assert_eq!(buffer.push(input(7)), Err(RejectCode::QuotaExceeded));
    // Per-tick settlement respects the limit.
    assert_eq!(buffer.settle(2).len(), 2);
}

#[test]
fn quantized_directions_normalize() {
    let straight = MoveInput {
        seq: 1,
        wish: [0, 0, -1],
        jump: false,
    };
    assert_eq!(straight.direction(), [0.0, 0.0, -1.0]);
    let diagonal = MoveInput {
        seq: 2,
        wish: [1, 0, -1],
        jump: false,
    };
    let direction = diagonal.direction();
    assert!((direction[0] - direction[2].abs()).abs() < 1e-9);
    assert!((direction[0] - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9);
    let idle = MoveInput {
        seq: 3,
        wish: [0, 0, 0],
        jump: false,
    };
    assert_eq!(idle.direction(), [0.0, 0.0, 0.0]);
}

#[test]
fn reconciliation_snaps_beyond_tolerance() {
    let predicted = Position::new(0.0, 0.0, 0.0);
    let close = Position::new(0.02, 0.01, 0.0);
    match reconcile(predicted, close, torse_sim::player::RECONCILE_TOLERANCE) {
        Correction::Keep { base } => assert_eq!(base, close),
        Correction::Snap(_) => panic!("small drift must keep the prediction"),
    }
    let far = Position::new(0.5, 0.0, 0.0);
    match reconcile(predicted, far, torse_sim::player::RECONCILE_TOLERANCE) {
        Correction::Snap(position) => assert_eq!(position, far),
        Correction::Keep { .. } => panic!("large drift must snap"),
    }
}

#[test]
fn bindings_are_one_to_one_and_revocable() {
    let mut bindings = PlayerBindings::default();
    let first = SessionId(1);
    let second = SessionId(2);
    let entity = EntityId(10);
    bindings.bind(first, entity).expect("bind");
    // Either side being taken refuses the other.
    assert_eq!(
        bindings.bind(second, entity),
        Err(RejectCode::Conflict),
        "entity already owned"
    );
    assert_eq!(
        bindings.bind(first, EntityId(11)),
        Err(RejectCode::Conflict),
        "session already bound"
    );
    assert_eq!(bindings.entity_of(first), Some(entity));
    assert_eq!(bindings.session_of(entity), Some(first));
    bindings.unbind(first);
    assert_eq!(bindings.entity_of(first), None);
    assert_eq!(bindings.session_of(entity), None);
    bindings.unbind(first);
    // The freed pair can rebind.
    bindings.bind(second, entity).expect("rebind");
    assert_eq!(bindings.entity_of(second), Some(entity));
}
