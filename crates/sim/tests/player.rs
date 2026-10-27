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
fn persistent_step_matches_detached_prediction_and_guards_frozen_collision_reads() {
    use torse_sim::player::{plan_step, predict_step};
    use torse_world::{EntityCollider, PlayerMotion, collider_schema, player_motion_schema};
    let mut world = world();
    let handle = world
        .entities_mut()
        .spawn(Position::new(8.5, 1.900001, 8.5))
        .unwrap();
    let schemas = world.schemas().clone();
    let components = world.entities_mut().components_mut(handle).unwrap();
    components
        .insert(
            &schemas,
            &collider_schema(),
            EntityCollider::Box {
                half: [300_000, 900_000, 300_000],
            }
            .fields(),
        )
        .unwrap();
    let motion = PlayerMotion {
        vertical_micros: 0,
        grounded: true,
    };
    components
        .insert(&schemas, &player_motion_schema(), motion.fields())
        .unwrap();
    let input = MoveInput {
        seq: 1,
        wish: [127, 0, 127],
        jump: true,
    };
    let previous = world.entities().get(handle).unwrap().position;
    let predicted = predict_step(&world, previous, [0.3, 0.9, 0.3], motion, input).unwrap();
    let order = torse_sim::OrderingKey {
        phase: torse_sim::Phase::Simulation,
        rule_priority: 0,
        stable_task_key: 1,
        local_sequence: 0,
    };
    let tx = plan_step(&world, handle, input, TransactionId(2), order).unwrap();
    assert_eq!(world.entities().get(handle).unwrap().position, previous);
    let prepared = world.prepare(&tx).unwrap();
    world.publish(prepared).unwrap();
    let entity = world.entities().get(handle).unwrap();
    assert_eq!(entity.position, predicted.0);
    assert_eq!(PlayerMotion::of(entity).unwrap().unwrap(), predicted.1);

    let frozen = plan_step(
        &world,
        handle,
        MoveInput {
            seq: 2,
            wish: [127, 0, 0],
            jump: false,
        },
        TransactionId(3),
        order,
    )
    .unwrap();
    let read = frozen
        .preconditions
        .iter()
        .find(|read| read.expected == torse_world::BlockStateId::EMPTY)
        .unwrap();
    let mut edit = torse_sim::Transaction::new(TransactionId(4), key("test:terrain"), order);
    edit.write_block(
        read.pos,
        world.registry().default_state(&key("base:stone")).unwrap(),
    );
    world.commit(&edit).unwrap();
    assert!(matches!(
        world.prepare(&frozen),
        Err(torse_sim::PrepareError::Rejected(RejectCode::Conflict))
    ));
}

#[test]
fn zero_capacity_movement_and_invalid_binding_ids_fail_without_panicking() {
    let mut buffer = MoveInputBuffer::new(0);
    buffer.push(MoveInput {
        seq: 1,
        wish: [1, 0, 0],
        jump: false,
    });
    assert!(buffer.take_resync());
    assert!(buffer.settle(1).is_empty());
    let mut bindings = PlayerBindings::default();
    assert_eq!(
        bindings.bind(SessionId(0), EntityId(1)),
        Err(RejectCode::InvalidData)
    );
    assert_eq!(
        bindings.bind(SessionId(1), EntityId(0)),
        Err(RejectCode::InvalidData)
    );
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
fn unstuck_relocates_a_buried_walker() {
    use torse_sim::OrderingKey;
    use torse_sim::Transaction;
    use torse_sim::player::WalkerState;
    use torse_sim::player::unstuck;
    use torse_types::TransactionOutcome;
    use torse_world::Section;

    let content = compile(m0_fixture_pack()).expect("content");
    let mut world = WorldAuthority::new(
        WorldId(1),
        torse_world::BlockStateRegistry::build(&content).expect("registry"),
        torse_world::ComponentSchemas::build(&content),
        16,
        WorldBounds::new(0, 32).expect("bounds"),
        Compatibility {
            authority_schema: 1,
            rules_digest: Digest([1; 32]),
            content_digest: Digest([2; 32]),
        },
    )
    .expect("authority");
    for pos in [
        torse_types::SectionPos::ZERO,
        torse_types::SectionPos::new(1, 0, 0),
    ] {
        world.store_mut().request(pos).expect("request");
        world
            .store_mut()
            .install(pos, Section::empty(16).expect("section"))
            .expect("install");
        world.store_mut().finish_load(pos).expect("finish");
    }

    // Seal the walker in: floor at y=0, walls at the four sides up to y=3,
    // and a lid at y=3. Free space exists above the lid.
    let stone = world
        .registry()
        .default_state(&key("base:stone"))
        .expect("stone");
    let mut seal = Transaction::new(
        TransactionId(1),
        key("test:seal"),
        OrderingKey {
            phase: torse_sim::Phase::Simulation,
            rule_priority: 0,
            stable_task_key: 1,
            local_sequence: 0,
        },
    );
    for y in 0..=3i64 {
        for x in 3..=7i64 {
            for z in 3..=7i64 {
                let wall = y == 0 || y == 3 || x == 3 || x == 7 || z == 3 || z == 7;
                if wall {
                    seal.write_block(torse_types::BlockPos::new(x, y, z), stone);
                }
            }
        }
    }
    match world.commit(&seal).expect("no fault").outcome {
        TransactionOutcome::Committed { .. } => {}
        outcome => panic!("seal committed: {outcome:?}"),
    }

    // The walker starts inside the sealed box, its collider overlapping the
    // walls (embedding is the state being escaped).
    let handle = world
        .entities_mut()
        .spawn(Position::new(5.5, 2.5, 5.5))
        .expect("spawn");
    let mut state = WalkerState::new(handle);
    let source = key("test:motion");

    let outcome = unstuck(&mut world, &mut state, &source, TransactionId(200));
    let Some(position) = outcome.expect("no fault") else {
        panic!("a box with headroom above the lid is escapable");
    };
    // The escape moved the walker out of the sealed volume.
    let inside = position.x > 3.0
        && position.x < 7.0
        && position.z > 3.0
        && position.z < 7.0
        && position.y > 0.0
        && position.y < 4.0;
    assert!(!inside, "escaped to {position:?}");
    // The committed entity position matches.
    let committed = world.entities().get(handle).expect("entity").position;
    assert_eq!(committed, position);
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
    buffer.push(input(2));
    buffer.push(input(1));
    assert_eq!(buffer.watermark(), 0);
    let settled = buffer.settle(8);
    assert_eq!(
        settled.iter().map(|i| i.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(buffer.watermark(), 2);

    // Stale and duplicate sequences drop silently.
    buffer.push(input(2));
    buffer.push(input(1));
    assert!(buffer.settle(8).is_empty());

    // Overflow evicts the oldest pending input and raises the resync flag.
    for seq in 3..=6 {
        buffer.push(input(seq));
    }
    assert!(!buffer.take_resync());
    buffer.push(input(7));
    assert!(buffer.take_resync());
    // The evicted input is the oldest; the newest survives.
    let settled = buffer.settle(8);
    assert_eq!(
        settled.iter().map(|i| i.seq).collect::<Vec<_>>(),
        vec![4, 5, 6, 7]
    );
    // Per-tick settlement respects the limit.
    buffer.push(input(8));
    buffer.push(input(9));
    buffer.push(input(10));
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
    // The vertical component is ignored: network input cannot drift upward.
    let climbing = MoveInput {
        seq: 4,
        wish: [0, 1, 0],
        jump: false,
    };
    assert_eq!(climbing.direction(), [0.0, 0.0, 0.0]);
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
