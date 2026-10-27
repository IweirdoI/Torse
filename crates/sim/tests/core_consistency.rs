//! Persistence policies and block instances share the ordinary commit boundary.

use torse_content::{
    ComponentField, ComponentFieldType, ComponentSchema, PersistencePolicy, compile,
    m0_fixture_pack,
};
use torse_sim::action::{
    ActionOrigin, ActionOutput, ActionRequest, ActionService, ActionTarget, ActorContext,
    register_action_module,
};
use torse_sim::replica::{ReplicaLimits, ReplicaScope, ReplicaSource};
use torse_sim::{
    OrderingKey, Phase, PrepareError, Recovery, Transaction, TransactionGroup, WorldAuthority,
    block_actions, changes_from_prepared, persistent_changes_from_prepared,
};
use torse_types::*;
use torse_world::*;

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).unwrap()
}

fn body(half: [u32; 3]) -> EntityCollider {
    EntityCollider::Box { half }
}

fn declare_body(world: &mut WorldAuthority, handle: EntityHandle, body: EntityCollider) {
    let schemas = world.schemas().clone();
    world
        .entities_mut()
        .components_mut(handle)
        .unwrap()
        .insert(&schemas, &collider_schema(), body.fields())
        .unwrap();
}

fn resize(
    world: &WorldAuthority,
    handle: EntityHandle,
    body: Option<EntityCollider>,
) -> Transaction {
    let entity = world.entities().get(handle).unwrap();
    let mut change = tx(90, "core:module/physics");
    change.components.push(ComponentEdit {
        id: entity.id,
        schema: collider_schema(),
        previous: entity.components.component(&collider_schema()).cloned(),
        fields: body.map(EntityCollider::fields),
    });
    change
}

fn translate(
    world: &WorldAuthority,
    handle: EntityHandle,
    delta: [f64; 3],
    hint: [f64; 3],
) -> Transaction {
    let entity = world.entities().get(handle).unwrap();
    let mut change = tx(91, "test:module");
    change.move_entity(handle, entity.id, entity.position, delta, hint);
    change
}

#[test]
fn declared_bodies_control_planned_and_prepared_placement() {
    for (collider, blocked) in [
        (body([100_000; 3]), false),
        (body([800_000; 3]), true),
        (EntityCollider::Disabled, false),
    ] {
        let (mut world, service) = setup(false, false);
        let pos = BlockPos::new(4, 4, 4);
        let planned = action(&world, &service, pos, Some("base:stone"), 0).unwrap();
        let h = world
            .entities_mut()
            .spawn(Position::new(3.5, 4.5, 4.5))
            .unwrap();
        declare_body(&mut world, h, collider);
        assert_eq!(
            action(&world, &service, pos, Some("base:stone"), 0).is_err(),
            blocked
        );
        assert_eq!(world.prepare(&planned).is_err(), blocked);
        if !blocked {
            apply(&mut world, &planned);
        }
    }
}

#[test]
fn movement_uses_declared_dimensions_instead_of_request_hint() {
    for (half, end) in [(100_000, 3.9), (800_000, 3.2)] {
        let (mut world, _) = setup(false, false);
        let stone = world.registry().default_state(&key("base:stone")).unwrap();
        world
            .store_mut()
            .edit(BlockPos::new(4, 4, 4), stone)
            .unwrap();
        let h = world
            .entities_mut()
            .spawn(Position::new(2.5, 4.5, 4.5))
            .unwrap();
        declare_body(&mut world, h, body([half, 100_000, 100_000]));
        let movement = translate(&world, h, [4.0, 0.0, 0.0], [0.01; 3]);
        apply(&mut world, &movement);
        let position = world.entities().get(h).unwrap().position;
        assert!((position.x - end).abs() < 0.001, "{position:?}");
    }
}

#[test]
fn disabled_body_crosses_solids_but_refuses_absent_and_out_of_bounds_cells() {
    let (mut world, _) = setup(false, false);
    let stone = world.registry().default_state(&key("base:stone")).unwrap();
    world
        .store_mut()
        .edit(BlockPos::new(4, 4, 4), stone)
        .unwrap();
    let h = world
        .entities_mut()
        .spawn(Position::new(2.5, 4.5, 4.5))
        .unwrap();
    declare_body(&mut world, h, EntityCollider::Disabled);
    let movement = translate(&world, h, [4.0, 0.0, 0.0], [0.3; 3]);
    apply(&mut world, &movement);
    assert_eq!(world.entities().get(h).unwrap().position.x, 6.5);
    for delta in [[10.0, 0.0, 0.0], [0.0, -6.0, 0.0]] {
        let movement = translate(&world, h, delta, [0.3; 3]);
        assert_eq!(
            world.prepare(&movement),
            Err(PrepareError::Rejected(RejectCode::NotLoaded))
        );
    }
}

#[test]
fn resize_checks_final_terrain_and_grouped_placement_checks_final_body() {
    let (mut world, service) = setup(false, false);
    let pos = BlockPos::new(4, 4, 4);
    let h = world
        .entities_mut()
        .spawn(Position::new(3.5, 4.5, 4.5))
        .unwrap();
    declare_body(&mut world, h, body([100_000; 3]));
    let place = action(&world, &service, pos, Some("base:stone"), 0).unwrap();
    apply(&mut world, &place);
    let expand = resize(&world, h, Some(body([800_000; 3])));
    assert_eq!(
        world.prepare(&expand),
        Err(PrepareError::Rejected(RejectCode::Conflict))
    );
    // Expansion is valid at the final destination, swept with the original body.
    let movement = translate(&world, h, [-1.0, 0.0, 0.0], [0.01; 3]);
    let group =
        TransactionGroup::new(TransactionId(100), expand.ordering, vec![expand, movement]).unwrap();
    let prepared = world.prepare_group(&group).unwrap();
    world.publish(prepared).unwrap();
    assert_eq!(
        EntityCollider::of(world.entities().get(h).unwrap()).unwrap(),
        Some(body([800_000; 3]))
    );
    // The next placement may use the body's final disabled state.
    let mut place = tx(101, "world:module");
    let pos = BlockPos::new(2, 4, 4);
    let stone = world.registry().default_state(&key("base:stone")).unwrap();
    place
        .write_block(pos, stone)
        .require_block_clear(pos, stone);
    assert!(world.prepare(&place).is_err());
    let disable = resize(&world, h, Some(EntityCollider::Disabled));
    let group =
        TransactionGroup::new(TransactionId(102), disable.ordering, vec![place, disable]).unwrap();
    world.publish(world.prepare_group(&group).unwrap()).unwrap();
}

#[test]
fn collider_spawn_checks_initial_fit_and_module_may_create_and_remove_its_body() {
    let (mut world, _) = setup(false, false);
    let stone = world.registry().default_state(&key("base:stone")).unwrap();
    world
        .store_mut()
        .edit(BlockPos::new(4, 4, 4), stone)
        .unwrap();
    let mut create = tx(1, "test:module");
    create.spawn_entity(
        Position::new(4.5, 4.5, 4.5),
        [(collider_schema(), body([100_000; 3]).fields())].into(),
    );
    assert_eq!(
        world.prepare(&create),
        Err(PrepareError::Rejected(RejectCode::Conflict))
    );
    assert_eq!(world.entities().next_entity_id(), 1);
    create.spawns[0]
        .components
        .insert(collider_schema(), EntityCollider::Disabled.fields());
    apply(&mut world, &create);
    let h = world.entities().handle_of(EntityId(1)).unwrap();
    let mut unauthorized = resize(&world, h, Some(body([100_000; 3])));
    unauthorized.source = key("test:module");
    assert_eq!(
        world.prepare(&unauthorized),
        Err(PrepareError::Rejected(RejectCode::PermissionDenied))
    );
    let mut remove = tx(2, "test:module");
    remove.despawn_entity(h, world.entities().get(h).unwrap());
    apply(&mut world, &remove);
    assert!(world.entities().is_empty());
}

#[test]
fn collider_resize_and_movement_reject_stale_prepared_observations() {
    let (mut world, _) = setup(false, false);
    let h = world
        .entities_mut()
        .spawn(Position::new(4.5, 4.5, 4.5))
        .unwrap();
    declare_body(&mut world, h, body([100_000; 3]));
    let movement = translate(&world, h, [1.0, 0.0, 0.0], [0.3; 3]);
    let group =
        TransactionGroup::new(TransactionId(5), movement.ordering, vec![movement.clone()]).unwrap();
    let token = world.prevalidate_movement(&group).unwrap();
    let prepared = world.prepare(&movement).unwrap();
    declare_body(&mut world, h, EntityCollider::Disabled);
    assert!(world.refresh_movement(&token).is_err());
    assert!(world.publish(prepared).is_err());
    let change = resize(&world, h, Some(body([100_000; 3])));
    let prepared = world.prepare(&change).unwrap();
    world
        .entities_mut()
        .move_to(h, Position::new(5.5, 4.5, 4.5))
        .unwrap();
    assert!(world.publish(prepared).is_err());
    assert_eq!(world.seq(), CommitSeq::INITIAL);
    let prepared = world.prepare(&change).unwrap();
    let stone = world.registry().default_state(&key("base:stone")).unwrap();
    world
        .store_mut()
        .edit(BlockPos::new(5, 4, 4), stone)
        .unwrap();
    assert!(world.publish(prepared).is_err());
}

#[test]
fn tiny_body_work_is_bounded_without_partial_translation() {
    let (mut world, _) = setup(false, false);
    let h = world
        .entities_mut()
        .spawn(Position::new(4.5, 4.5, 4.5))
        .unwrap();
    declare_body(&mut world, h, body([1; 3]));
    let movement = translate(&world, h, [1.0, 0.0, 0.0], [0.3; 3]);
    assert_eq!(
        world.prepare(&movement),
        Err(PrepareError::Rejected(RejectCode::QuotaExceeded))
    );
    assert_eq!(world.entities().get(h).unwrap().position.x, 4.5);
    assert_eq!(world.seq(), CommitSeq::INITIAL);
}

#[test]
fn disabled_body_checks_short_diagonal_intersections_and_world_bounds() {
    let (mut world, _) = setup(false, false);
    for pos in [SectionPos::new(1, 0, 1), SectionPos::new(0, -1, 0)] {
        world.store_mut().request(pos).unwrap();
        world
            .store_mut()
            .install(pos, Section::empty(16).unwrap())
            .unwrap();
        world.store_mut().finish_load(pos).unwrap();
    }
    let h = world
        .entities_mut()
        .spawn(Position::new(15.99, 4.5, 15.98))
        .unwrap();
    declare_body(&mut world, h, EntityCollider::Disabled);
    let movement = translate(&world, h, [0.2, 0.0, 0.2], [0.3; 3]);
    assert_eq!(
        world.prepare(&movement),
        Err(PrepareError::Rejected(RejectCode::NotLoaded))
    );
    world
        .entities_mut()
        .move_to(h, Position::new(6.5, -15.5, 4.5))
        .unwrap();
    let movement = translate(&world, h, [0.0, -1.0, 0.0], [0.3; 3]);
    assert_eq!(
        world.prepare(&movement),
        Err(PrepareError::Rejected(RejectCode::NotLoaded))
    );
}

#[test]
fn collider_checkpoint_journal_and_public_replica_preserve_the_same_fields() {
    let (mut world, _) = setup(false, false);
    let h = world
        .entities_mut()
        .spawn(Position::new(4.5, 4.5, 4.5))
        .unwrap();
    declare_body(&mut world, h, body([100_000, 300_000, 200_000]));
    let snapshot = world.capture_snapshot(meta(&world)).unwrap();
    let change = resize(&world, h, Some(EntityCollider::Disabled));
    let changes = apply(&mut world, &change);
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    recovery.restore_snapshot(&snapshot).unwrap();
    recovery.replay(&changes).unwrap();
    let restored = recovery.finish().unwrap();
    assert_eq!(
        EntityCollider::of(restored.entities().iter().next().unwrap().1).unwrap(),
        Some(EntityCollider::Disabled)
    );
    let checkpoint = world.capture_snapshot(meta(&world)).unwrap();
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    recovery.restore_snapshot(&checkpoint).unwrap();
    assert_eq!(
        EntityCollider::of(
            recovery
                .finish()
                .unwrap()
                .entities()
                .iter()
                .next()
                .unwrap()
                .1
        )
        .unwrap(),
        Some(EntityCollider::Disabled)
    );
    let binding = Binding {
        world: world.world(),
        epoch: WorldEpoch(1),
        compatibility: world.coordinator().compatibility(),
    };
    let budget = OutputBudget::new(100_000, 8_000_000);
    let mut source = ReplicaSource::new(
        world,
        binding,
        ReplicaLimits {
            max_subscriptions: 1,
            max_sections: 1,
            max_queued_batches: 8,
            snapshot: budget,
            queue: budget,
        },
    )
    .unwrap();
    let seed = source
        .open_replica(
            Viewer {
                session: SessionId(1),
                actor: ActorContextId(1),
            },
            actor(),
            ReplicaScope {
                sections: [SectionPos::ZERO].into(),
            },
            budget,
        )
        .unwrap();
    assert_eq!(
        seed.state.entities[0].components.get(&collider_schema()),
        Some(&EntityCollider::Disabled.fields())
    );
    let change = resize(source.world(), h, Some(body([200_000; 3])));
    source.publish(source.prepare(&change).unwrap()).unwrap();
    let batches = source
        .take_replica_batches(binding, seed.subscription, budget)
        .unwrap();
    assert_eq!(
        batches[0].changes.entities[0]
            .components
            .get(&collider_schema()),
        Some(&body([200_000; 3]).fields())
    );
}

fn fields(value: u32) -> ComponentFields {
    [(key("test:value"), ComponentValue::U32(value))].into()
}

fn setup(instances: bool, defaults: bool) -> (WorldAuthority, ActionService) {
    let mut pack = m0_fixture_pack();
    block_actions::register_content(&mut pack);
    let components = [
        ("test:saved", PersistencePolicy::Always),
        ("test:cache", PersistencePolicy::Never),
        ("test:optional", PersistencePolicy::OwnerDefined),
    ]
    .map(|(id, persistence)| ComponentSchema {
        id: key(id),
        schema_version: 1,
        owner_module: key("test:module"),
        fields: vec![ComponentField {
            id: key("test:value"),
            field_type: ComponentFieldType::Unsigned32,
            required: true,
        }],
        persistence,
        replication: ReplicationDeclaration {
            visibility: if persistence == PersistencePolicy::Never {
                ReplicationVisibility::Public
            } else {
                ReplicationVisibility::Private
            },
            ..ReplicationDeclaration::SERVER_PRIVATE
        },
        migration_binding: None,
    });
    register_action_module(
        &mut pack,
        key("test:module"),
        key("test:state"),
        components.into(),
        vec![],
        vec![],
    );
    if instances {
        for block in &mut pack.blocks {
            if ["base:glass_slab", "base:glass"].contains(&block.value.id.to_string().as_str()) {
                block.value.component_defaults = vec![key("test:saved"), key("test:cache")];
            }
        }
    }
    let content = compile(pack).unwrap();
    let mut schemas = ComponentSchemas::build(&content);
    if defaults {
        for block in ["base:glass_slab", "base:glass"] {
            for schema in ["test:saved", "test:cache"] {
                schemas
                    .set_block_default(&key("test:module"), key(block), key(schema), fields(7))
                    .unwrap();
            }
        }
    }
    let service = ActionService::bind(
        block_actions::descriptors(),
        block_actions::handlers(),
        &schemas,
    )
    .unwrap();
    let mut world = WorldAuthority::new(
        WorldId(1),
        BlockStateRegistry::build(&content).unwrap(),
        schemas,
        16,
        WorldBounds::new(-16, 32).unwrap(),
        Compatibility {
            authority_schema: 1,
            rules_digest: Digest([1; 32]),
            content_digest: content.authority_digest,
        },
    )
    .unwrap();
    for pos in [SectionPos::ZERO, SectionPos::new(-1, 0, 0)] {
        world.store_mut().request(pos).unwrap();
        world
            .store_mut()
            .install(pos, Section::empty(16).unwrap())
            .unwrap();
        world.store_mut().finish_load(pos).unwrap();
    }
    (world, service)
}

fn tx(id: u128, source: &str) -> Transaction {
    Transaction::new(
        TransactionId(id),
        key(source),
        OrderingKey {
            phase: Phase::Interaction,
            rule_priority: 0,
            stable_task_key: id as u64,
            local_sequence: 0,
        },
    )
}

fn meta(world: &WorldAuthority) -> SnapshotMeta {
    SnapshotMeta {
        cursor: CommitCursor {
            world: world.world(),
            seq: world.seq(),
        },
        compatibility: world.coordinator().compatibility(),
        resume_at: world.coordinator().resume_point(),
    }
}

fn actor() -> ActorContext {
    ActorContext::new(key("test:editor"))
        .permission(key("world:permission/edit"))
        .section(SectionPos::ZERO)
        .section(SectionPos::new(-1, 0, 0))
}

fn action(
    world: &WorldAuthority,
    service: &ActionService,
    pos: BlockPos,
    block: Option<&str>,
    half: u32,
) -> Result<Transaction, RejectCode> {
    let mut parameters = ComponentFields::new();
    if let Some(block) = block {
        parameters.insert(
            key("world:field/block"),
            ComponentValue::Content(key(block)),
        );
        parameters.insert(key("world:field/half"), ComponentValue::U32(half));
    }
    let request = ActionRequest {
        action: key(if block.is_some() {
            "world:action/place"
        } else {
            "world:action/break"
        }),
        api_version: 1,
        origin: ActionOrigin::Network,
        target: ActionTarget::Block(pos),
        parameters,
        transaction: TransactionId(u128::from(world.seq().0) + 1),
        ordering: tx(1, "world:module").ordering,
    };
    match service.plan(world, &actor(), Phase::Interaction, &request)? {
        ActionOutput::Mutation(tx) => Ok(*tx),
        _ => panic!("expected mutation"),
    }
}

fn apply(world: &mut WorldAuthority, tx: &Transaction) -> ChangeSet {
    let prepared = world.prepare(tx).unwrap();
    let changes =
        persistent_changes_from_prepared(world.registry(), world.schemas(), &prepared).unwrap();
    world.publish(prepared).unwrap();
    changes
}

#[test]
fn placement_uses_the_resolved_state_collision_shape() {
    let (mut world, service) = setup(false, false);
    // The walker reaches y=4.4, overlapping only a lower slab at y=4.
    world
        .entities_mut()
        .spawn(Position::new(4.5, 3.5, 4.5))
        .unwrap();
    let pos = BlockPos::new(4, 4, 4);
    assert_eq!(
        action(&world, &service, pos, Some("base:glass_slab"), 0),
        Err(RejectCode::Conflict)
    );
    let upper = action(&world, &service, pos, Some("base:glass_slab"), 1).unwrap();
    apply(&mut world, &upper);
    // Completion must check the full cube, even though the upper half was free.
    assert_eq!(
        action(&world, &service, pos, Some("base:glass_slab"), 0),
        Err(RejectCode::Conflict)
    );
    let remove = action(&world, &service, pos, None, 0).unwrap();
    apply(&mut world, &remove);
    let handle = world.entities().iter().next().unwrap().0;
    world
        .entities_mut()
        .move_to(handle, Position::new(4.5, 5.5, 4.5))
        .unwrap();
    // Now the walker overlaps only the upper slab. The definition's lower shape
    // must not allow an upper placement to bury it.
    assert!(action(&world, &service, pos, Some("base:glass_slab"), 0).is_ok());
    assert_eq!(
        action(&world, &service, pos, Some("base:glass_slab"), 1),
        Err(RejectCode::Conflict)
    );
    assert!(
        actor()
            .view(&world)
            .cell_blockers(pos, BlockStateId(u16::MAX))
            .is_err()
    );
}

#[test]
fn placement_rechecks_arrivals_after_action_planning_and_frozen_restore() {
    use torse_sim::durable::StoredTransactionGroup;
    let (mut world, service) = setup(false, false);
    let pos = BlockPos::new(4, 4, 4);
    let planned = action(&world, &service, pos, Some("base:stone"), 0).unwrap();
    let frozen = StoredTransactionGroup::single(&world, &planned).unwrap();
    assert_eq!(frozen.members[0].clearance.len(), 1);
    let handle = world
        .entities_mut()
        .spawn(Position::new(1.5, 4.5, 4.5))
        .unwrap();
    let entity = world.entities().get(handle).unwrap();
    let mut arrival = tx(50, "test:module");
    arrival.move_entity(
        handle,
        entity.id,
        entity.position,
        [3.0, 0.0, 0.0],
        [0.3, 0.9, 0.3],
    );
    world.commit(&arrival).unwrap();
    let seq = world.seq();
    let next = world.entities().next_entity_id();
    for attempt in [&planned, &frozen.restore(&world).unwrap().members[0]] {
        assert_eq!(
            world.prepare(attempt),
            Err(PrepareError::Rejected(RejectCode::Conflict))
        );
    }
    assert_eq!(world.seq(), seq);
    assert_eq!(world.entities().next_entity_id(), next);
    assert_eq!(
        world.store().query(pos),
        TerrainQuery::Ready(BlockStateId::EMPTY)
    );
    world
        .entities_mut()
        .move_to(handle, Position::new(1.5, 4.5, 4.5))
        .unwrap();
    let committed = world.commit(&planned).unwrap();
    assert!(matches!(
        committed.outcome,
        TransactionOutcome::Committed { .. }
    ));
}

#[test]
fn placement_publication_rejects_new_occupants_before_any_write() {
    let (mut world, service) = setup(true, true);
    let pos = BlockPos::new(4, 4, 4);
    let planned = action(&world, &service, pos, Some("base:glass"), 0).unwrap();
    let prepared = world.prepare(&planned).unwrap();
    world
        .entities_mut()
        .spawn(Position::new(4.5, 4.5, 4.5))
        .unwrap();
    assert!(world.publish(prepared).is_err());
    assert_eq!(world.seq(), CommitSeq(0));
    assert_eq!(world.entities().len(), 1);
    assert_eq!(world.entities().next_entity_id(), 2);
    assert_eq!(
        world.store().query(pos),
        TerrainQuery::Ready(BlockStateId::EMPTY)
    );
    assert!(world.entities().block_instance_at(pos).unwrap().is_none());
}

#[test]
fn delayed_placement_retains_partial_shape_and_removal_semantics() {
    let (mut world, service) = setup(false, false);
    let pos = BlockPos::new(4, 4, 4);
    let lower = action(&world, &service, pos, Some("base:glass_slab"), 0).unwrap();
    let upper = action(&world, &service, pos, Some("base:glass_slab"), 1).unwrap();
    let handle = world
        .entities_mut()
        .spawn(Position::new(4.5, 3.5, 4.5))
        .unwrap();
    assert_eq!(
        world.prepare(&lower),
        Err(PrepareError::Rejected(RejectCode::Conflict))
    );
    apply(&mut world, &upper);
    world
        .entities_mut()
        .move_to(handle, Position::new(1.5, 4.5, 4.5))
        .unwrap();
    let complete = action(&world, &service, pos, Some("base:glass_slab"), 0).unwrap();
    let remove = action(&world, &service, pos, None, 0).unwrap();
    assert!(remove.clearance.is_empty());
    world
        .entities_mut()
        .move_to(handle, Position::new(4.5, 3.5, 4.5))
        .unwrap();
    assert_eq!(
        world.prepare(&complete),
        Err(PrepareError::Rejected(RejectCode::Conflict))
    );
    apply(&mut world, &remove);
}

#[test]
fn sibling_spawns_and_moves_cannot_enter_a_guarded_placement() {
    let (mut world, service) = setup(true, true);
    let pos = BlockPos::new(4, 4, 4);
    let place = action(&world, &service, pos, Some("base:glass"), 0).unwrap();
    let mut spawn = tx(2, "test:module");
    spawn.spawn_entity(Position::new(4.5, 4.5, 4.5), Default::default());
    let group = TransactionGroup::new(
        TransactionId(3),
        place.ordering,
        vec![place.clone(), spawn.clone()],
    )
    .unwrap();
    assert_eq!(
        world.prepare_group(&group),
        Err(PrepareError::Rejected(RejectCode::Conflict))
    );
    assert_eq!(world.seq(), CommitSeq(0));
    assert!(world.entities().is_empty());
    assert_eq!(world.entities().next_entity_id(), 1);
    let handle = world
        .entities_mut()
        .spawn(Position::new(1.5, 4.5, 4.5))
        .unwrap();
    let entity = world.entities().get(handle).unwrap();
    let mut movement = tx(4, "test:module");
    movement.move_entity(
        handle,
        entity.id,
        entity.position,
        [3.0, 0.0, 0.0],
        [0.3, 0.9, 0.3],
    );
    let group = TransactionGroup::new(
        TransactionId(5),
        place.ordering,
        vec![place.clone(), movement],
    )
    .unwrap();
    assert_eq!(
        world.prepare_group(&group),
        Err(PrepareError::Rejected(RejectCode::Conflict))
    );
    assert_eq!(
        world.entities().get(handle).unwrap().position,
        Position::new(1.5, 4.5, 4.5)
    );
    spawn.spawns[0].position_bits = Position::new(12.5, 4.5, 4.5).to_bits();
    let group =
        TransactionGroup::new(TransactionId(6), place.ordering, vec![place, spawn]).unwrap();
    let prepared = world.prepare_group(&group).unwrap();
    assert_eq!(prepared.entity_lifecycle().unwrap().spawned.len(), 2);
    world.publish(prepared).unwrap();
    assert_eq!(world.entities().len(), 3);
    assert!(world.entities().block_instance_at(pos).unwrap().is_some());
}

#[test]
fn placement_conditions_reject_mismatch_duplicates_and_excess_before_publication() {
    let (world, service) = setup(false, false);
    let pos = BlockPos::new(4, 4, 4);
    let planned = action(&world, &service, pos, Some("base:stone"), 0).unwrap();
    let mut invalid = planned.clone();
    invalid.clearance[0].pos.x += 1;
    assert_eq!(
        world.prepare(&invalid),
        Err(PrepareError::Rejected(RejectCode::InvalidData))
    );
    invalid = planned.clone();
    invalid.clearance.push(invalid.clearance[0]);
    assert_eq!(
        world.prepare(&invalid),
        Err(PrepareError::Rejected(RejectCode::InvalidData))
    );
    invalid.clearance.resize(257, planned.clearance[0]);
    assert_eq!(
        world.prepare(&invalid),
        Err(PrepareError::Rejected(RejectCode::QuotaExceeded))
    );
    assert!(
        world
            .coordinator()
            .prepare(world.store(), &planned)
            .is_err()
    );
    let mut replacement = tx(2, "test:module");
    replacement.write_block(pos, BlockStateId::EMPTY);
    let unchecked = TransactionGroup {
        id: TransactionId(3),
        ordering: planned.ordering,
        members: vec![planned, replacement],
    };
    assert_eq!(
        world.prepare_group(&unchecked),
        Err(PrepareError::Rejected(RejectCode::InvalidData))
    );
}

#[test]
fn instances_follow_cells_atomically_and_keep_identity_across_state_changes() {
    let (mut world, service) = setup(true, true);
    let pos = BlockPos::new(-1, 4, 3);
    let place = action(&world, &service, pos, Some("base:glass_slab"), 0).unwrap();
    let prepared = world.prepare(&place).unwrap();
    let proposed = prepared.entity_lifecycle().unwrap().spawned[0].id;
    assert!(world.entities().is_empty());
    assert_eq!(world.entities().next_entity_id(), proposed.0);
    drop(prepared); // A failed journal reservation consumes neither cells nor IDs.
    apply(&mut world, &place);
    let (handle, entity) = world.entities().block_instance_at(pos).unwrap().unwrap();
    assert_eq!(entity.id, proposed);
    assert_eq!(
        entity.components.component(&key("test:saved")),
        Some(&fields(7))
    );
    assert_eq!(BlockInstance::of(entity).unwrap().unwrap().pos, pos);
    let mut toggle = tx(2, "test:module");
    toggle.write_block(
        pos,
        world
            .registry()
            .state_of(&key("base:glass_slab"), &[1])
            .unwrap(),
    );
    apply(&mut world, &toggle);
    assert_eq!(
        world
            .entities()
            .block_instance_at(pos)
            .unwrap()
            .unwrap()
            .1
            .id,
        proposed
    );
    // The attached entity itself does not block completing the slab.
    let complete = action(&world, &service, pos, Some("base:glass_slab"), 0).unwrap();
    let replacement = apply(&mut world, &complete);
    let lifecycle = replacement.entity_lifecycle.unwrap();
    assert_eq!(lifecycle.despawned[0].id, proposed);
    assert_eq!(lifecycle.spawned.len(), 1);
    assert!(world.entities().get(handle).is_none());
    let new_id = world
        .entities()
        .block_instance_at(pos)
        .unwrap()
        .unwrap()
        .1
        .id;
    assert_ne!(new_id, proposed);
    let remove = action(&world, &service, pos, None, 0).unwrap();
    apply(&mut world, &remove);
    assert!(world.entities().is_empty());
    assert_eq!(world.entities().next_entity_id(), new_id.0 + 1);
}

#[test]
fn missing_defaults_and_unauthorized_initialization_change_nothing() {
    let (mut world, _) = setup(true, false);
    let pos = BlockPos::new(4, 4, 4);
    let mut place = tx(1, "world:module");
    place.write_block(
        pos,
        world.registry().default_state(&key("base:glass")).unwrap(),
    );
    assert!(world.prepare(&place).is_err());
    assert_eq!(world.seq(), CommitSeq::INITIAL);
    assert_eq!(world.entities().next_entity_id(), 1);
    assert_eq!(
        world.store().query(pos),
        TerrainQuery::Ready(BlockStateId::EMPTY)
    );
    let mut schemas = world.schemas().clone();
    assert!(
        schemas
            .set_block_default(
                &key("other:module"),
                key("base:glass"),
                key("test:saved"),
                fields(1)
            )
            .is_err()
    );
    // Unpaired cells from trusted bootstrap/load paths cannot be checkpointed.
    world
        .store_mut()
        .apply_edits(&[BlockEdit {
            pos,
            state: place.mutations[0].state,
        }])
        .unwrap();
    assert!(world.capture_snapshot(meta(&world)).is_err());
}

#[test]
fn block_destruction_conflicts_with_component_work_and_binding_cannot_be_forged() {
    let (mut world, service) = setup(true, true);
    let pos = BlockPos::new(4, 4, 4);
    let place = action(&world, &service, pos, Some("base:glass"), 0).unwrap();
    apply(&mut world, &place);
    let (handle, entity) = world.entities().block_instance_at(pos).unwrap().unwrap();
    let mut edit = tx(2, "test:module");
    edit.components.push(ComponentEdit {
        id: entity.id,
        schema: key("test:saved"),
        previous: Some(fields(7)),
        fields: Some(fields(8)),
    });
    let remove = action(&world, &service, pos, None, 0).unwrap();
    let group =
        TransactionGroup::new(TransactionId(9), edit.ordering, vec![edit.clone(), remove]).unwrap();
    assert_eq!(
        world.prepare_group(&group),
        Err(PrepareError::Rejected(RejectCode::Conflict))
    );
    let mut forged = tx(2, "core:module");
    forged.components.push(ComponentEdit {
        id: entity.id,
        schema: block_instance_schema(),
        previous: entity
            .components
            .component(&block_instance_schema())
            .cloned(),
        fields: None,
    });
    assert!(world.prepare(&forged).is_err());
    let mut remove_entity = tx(2, "test:module");
    remove_entity.despawn_entity(handle, entity);
    assert!(world.prepare(&remove_entity).is_err());
    let mut move_entity = tx(2, "test:module");
    move_entity.move_entity(
        handle,
        entity.id,
        entity.position,
        [1.0, 0.0, 0.0],
        [0.3, 0.9, 0.3],
    );
    assert_eq!(
        world.prepare(&move_entity),
        Err(PrepareError::Rejected(RejectCode::PermissionDenied))
    );
    apply(&mut world, &edit); // Component ownership still works for the attached object.
}

#[test]
fn checkpoint_and_journal_projection_agree_for_updates_and_despawns() {
    let (mut world, _) = setup(false, false);
    let schemas = world.schemas().clone();
    let handle = world
        .entities_mut()
        .spawn(Position::new(4.5, 4.5, 4.5))
        .unwrap();
    for schema in ["test:saved", "test:cache", "test:optional"] {
        world
            .entities_mut()
            .components_mut(handle)
            .unwrap()
            .insert(&schemas, &key(schema), fields(1))
            .unwrap();
    }
    let snapshot = world.capture_snapshot(meta(&world)).unwrap();
    assert_eq!(snapshot.entities[0].components.len(), 2);
    let full = Snapshot::capture(
        meta(&world),
        world.store(),
        world.entities(),
        world.registry(),
    )
    .unwrap();
    assert_eq!(full.entities[0].components.len(), 3);
    let budget = OutputBudget::new(8, 1_048_576);
    assert!(
        Snapshot::capture_with_budget(
            meta(&world),
            world.store(),
            world.entities(),
            world.registry(),
            budget
        )
        .is_err()
    );
    assert_eq!(
        world
            .capture_snapshot_with_budget(meta(&world), budget)
            .unwrap(),
        snapshot
    );
    let id = world.entities().get(handle).unwrap().id;
    let mut update = tx(1, "test:module");
    for schema in ["test:saved", "test:cache"] {
        update.components.push(ComponentEdit {
            id,
            schema: key(schema),
            previous: Some(fields(1)),
            fields: Some(fields(2)),
        });
    }
    let prepared = world.prepare(&update).unwrap();
    let legacy = changes_from_prepared(world.registry(), &prepared).unwrap();
    let changes =
        persistent_changes_from_prepared(world.registry(), world.schemas(), &prepared).unwrap();
    assert_eq!(changes.component_edits.len(), 1);
    world.publish(prepared).unwrap();
    let mut remove = tx(2, "test:module");
    remove.despawn_entity(handle, world.entities().get(handle).unwrap());
    let removal = apply(&mut world, &remove);
    assert!(
        !removal.entity_lifecycle.as_ref().unwrap().despawned[0]
            .components
            .contains_key(&key("test:cache"))
    );
    // New projections and old complete records restore through the same builder.
    for (base, update) in [(&snapshot, &changes), (&full, &legacy)] {
        let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
        recovery.restore_snapshot(base).unwrap();
        recovery.replay(update).unwrap();
        recovery.replay(&removal).unwrap();
        let restored = recovery.finish().unwrap();
        assert!(restored.entities().is_empty());
        assert_eq!(restored.entities().next_entity_id(), 2);
        assert_eq!(restored.seq(), world.seq());
    }
}

#[test]
fn owner_defined_policy_requires_the_owner_and_never_changes_fixed_policies() {
    let (world, _) = setup(false, false);
    let mut pack = m0_fixture_pack();
    register_action_module(
        &mut pack,
        key("test:module"),
        key("test:state"),
        ["test:optional", "test:saved"]
            .into_iter()
            .map(|id| ComponentSchema {
                id: key(id),
                schema_version: 1,
                owner_module: key("test:module"),
                fields: vec![],
                persistence: if id == "test:optional" {
                    PersistencePolicy::OwnerDefined
                } else {
                    PersistencePolicy::Always
                },
                replication: ReplicationDeclaration::SERVER_PRIVATE,
                migration_binding: None,
            })
            .collect(),
        vec![],
        vec![],
    );
    let mut schemas = ComponentSchemas::build(&compile(pack).unwrap());
    assert!(schemas.persists(&key("test:optional")).unwrap());
    assert!(
        schemas
            .resolve_persistence(&key("other:module"), &key("test:optional"), false)
            .is_err()
    );
    assert!(
        schemas
            .resolve_persistence(&key("test:module"), &key("test:saved"), false)
            .is_err()
    );
    schemas
        .resolve_persistence(&key("test:module"), &key("test:optional"), false)
        .unwrap();
    assert!(!schemas.persists(&key("test:optional")).unwrap());
    assert!(schemas.persists(&key("test:unknown")).is_err());
    assert_eq!(
        world.schemas().clone().resolve_persistence(
            &key("test:module"),
            &key("test:optional"),
            false
        ),
        Err(torse_world::ComponentError::ConfigurationFrozen)
    );
}

#[test]
fn instance_recovery_and_replication_keep_bindings_but_hide_private_fields() {
    let (mut world, service) = setup(true, true);
    let snapshot = world.capture_snapshot(meta(&world)).unwrap();
    let pos = BlockPos::new(4, 4, 4);
    let place = action(&world, &service, pos, Some("base:glass"), 0).unwrap();
    let changes = apply(&mut world, &place);
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    recovery.restore_snapshot(&snapshot).unwrap();
    recovery.replay(&changes).unwrap();
    let restored = recovery.finish().unwrap();
    let entity = restored
        .entities()
        .block_instance_at(pos)
        .unwrap()
        .unwrap()
        .1;
    assert!(entity.components.component(&key("test:saved")).is_some());
    assert!(entity.components.component(&key("test:cache")).is_none());
    let binding = Binding {
        world: world.world(),
        epoch: WorldEpoch(1),
        compatibility: world.coordinator().compatibility(),
    };
    let budget = OutputBudget::new(100_000, 8_000_000);
    let mut source = ReplicaSource::new(
        world,
        binding,
        ReplicaLimits {
            max_subscriptions: 1,
            max_sections: 1,
            max_queued_batches: 8,
            snapshot: budget,
            queue: budget,
        },
    )
    .unwrap();
    let viewer = Viewer {
        session: SessionId(1),
        actor: ActorContextId(1),
    };
    let seed = source
        .open_replica(
            viewer,
            actor(),
            ReplicaScope {
                sections: [SectionPos::ZERO].into(),
            },
            budget,
        )
        .unwrap();
    assert_eq!(seed.state.entities[0].components.len(), 2);
    assert!(
        seed.state.entities[0]
            .components
            .contains_key(&key("test:cache"))
    );
    assert!(
        !seed.state.entities[0]
            .components
            .contains_key(&key("test:saved"))
    );
    assert!(
        seed.state.entities[0]
            .components
            .contains_key(&block_instance_schema())
    );
    let mut remove = tx(2, "world:module");
    remove.write_block(pos, BlockStateId::EMPTY);
    let prepared = source.prepare(&remove).unwrap();
    source.publish(prepared).unwrap();
    let batches = source
        .take_replica_batches(binding, seed.subscription, budget)
        .unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].changes.sections.len(), 1);
    assert_eq!(batches[0].changes.removed_entities.len(), 1);
}

#[test]
fn recovery_refuses_changed_configuration_or_journal_metadata_before_apply() {
    let (mut world, _) = setup(true, true);
    let snapshot = world.capture_snapshot(meta(&world)).unwrap();
    let mut placement = tx(1, "world:module");
    let pos = BlockPos::new(3, 3, 3);
    placement.write_block(
        pos,
        world.registry().default_state(&key("base:glass")).unwrap(),
    );
    let mut changes = apply(&mut world, &placement);
    let original = changes.meta.compatibility;
    changes.meta.compatibility.rules_digest = Digest([99; 32]);
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    recovery.restore_snapshot(&snapshot).unwrap();
    assert!(recovery.replay(&changes).is_err());
    assert_eq!(recovery.seq(), CommitSeq::INITIAL);
    let restored = recovery.finish().unwrap();
    assert_eq!(
        restored.store().query(pos),
        TerrainQuery::Ready(BlockStateId::EMPTY)
    );
    let mut bad = snapshot.clone();
    bad.meta.compatibility = changes.meta.compatibility;
    assert!(bad.restore(world.registry(), world.schemas()).is_err());
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    assert!(recovery.restore_snapshot(&bad).is_err());
    assert_eq!(recovery.seq(), CommitSeq::INITIAL);
    assert_eq!(snapshot.meta.compatibility, original);
}

#[test]
fn recovery_validates_runtime_only_components_before_projection_and_can_retry() {
    let (mut world, _) = setup(false, false);
    let handle = world
        .entities_mut()
        .spawn(Position::new(2.5, 2.5, 2.5))
        .unwrap();
    let schemas = world.schemas().clone();
    world
        .entities_mut()
        .components_mut(handle)
        .unwrap()
        .insert(&schemas, &key("test:cache"), fields(7))
        .unwrap();
    let snapshot = Snapshot::capture(
        meta(&world),
        world.store(),
        world.entities(),
        world.registry(),
    )
    .unwrap();
    let mut malformed = snapshot.clone();
    let fields = &mut malformed.entities[0].components[0].fields;
    fields.push(fields[0].clone());
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    assert!(recovery.restore_snapshot(&malformed).is_err());
    assert_eq!(recovery.seq(), CommitSeq::INITIAL);
    recovery.restore_snapshot(&snapshot).unwrap();
    let restored = recovery.finish().unwrap();
    assert_eq!(restored.entities().len(), 1);
    assert!(
        restored
            .entities()
            .get(restored.entities().handle_of(EntityId(1)).unwrap())
            .unwrap()
            .components
            .is_empty()
    );
    assert_eq!(
        world
            .entities()
            .get(handle)
            .unwrap()
            .components
            .component(&key("test:cache")),
        Some(&self::fields(7))
    );
}

#[test]
fn explicit_and_automatic_lifecycle_share_one_allocator_and_replay_validation() {
    let (mut world, _) = setup(true, true);
    let snapshot = world.capture_snapshot(meta(&world)).unwrap();
    let pos = BlockPos::new(4, 4, 4);
    let mut create = tx(1, "test:module");
    create.spawn_entity(Position::new(12.5, 4.5, 4.5), Default::default());
    create.write_block(
        pos,
        world.registry().default_state(&key("base:glass")).unwrap(),
    );
    let changes = apply(&mut world, &create);
    let life = changes.entity_lifecycle.as_ref().unwrap();
    assert_eq!(
        life.spawned.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![EntityId(1), EntityId(2)]
    );
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    recovery.restore_snapshot(&snapshot).unwrap();
    recovery.validate_derived_lifecycle(&changes, 1, 0).unwrap();
    let mut forged = changes.clone();
    forged.entity_lifecycle.as_mut().unwrap().spawned[1]
        .components
        .insert(key("test:saved"), fields(99));
    assert!(recovery.validate_derived_lifecycle(&forged, 1, 0).is_err());
    recovery.replay(&changes).unwrap();
    drop(recovery);
    let snapshot = world.capture_snapshot(meta(&world)).unwrap();
    let mut remove = tx(2, "test:module");
    let handle = world.entities().handle_of(EntityId(1)).unwrap();
    remove.despawn_entity(handle, world.entities().get(handle).unwrap());
    remove.write_block(pos, BlockStateId::EMPTY);
    let changes = apply(&mut world, &remove);
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    recovery.restore_snapshot(&snapshot).unwrap();
    recovery.validate_derived_lifecycle(&changes, 0, 1).unwrap();
    recovery.replay(&changes).unwrap();
    let restored = recovery.finish().unwrap();
    assert!(restored.entities().is_empty());
    assert_eq!(restored.entities().next_entity_id(), 3);
}

#[test]
fn automatic_lifecycle_budget_and_missing_recovered_bindings_fail_without_publication() {
    let (world, _) = setup(true, true);
    let state = world.registry().default_state(&key("base:glass")).unwrap();
    let mut create = tx(1, "test:module");
    for index in 0..257 {
        create.write_block(
            BlockPos::new(index % 16, index / 16 % 16, index / 256),
            state,
        );
    }
    assert_eq!(
        world.prepare(&create),
        Err(PrepareError::Rejected(RejectCode::QuotaExceeded))
    );
    assert!(world.entities().is_empty());
    assert_eq!(world.entities().next_entity_id(), 1);
    assert_eq!(world.seq(), CommitSeq::INITIAL);
    let (mut world, service) = setup(true, true);
    let place = action(
        &world,
        &service,
        BlockPos::new(4, 4, 4),
        Some("base:glass"),
        0,
    )
    .unwrap();
    apply(&mut world, &place);
    let mut snapshot = world.capture_snapshot(meta(&world)).unwrap();
    snapshot.entities.clear();
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    recovery.restore_snapshot(&snapshot).unwrap();
    assert!(recovery.finish().is_err());
}
