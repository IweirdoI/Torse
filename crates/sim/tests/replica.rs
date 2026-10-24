use std::collections::BTreeMap;

use torse_content::{
    ComponentField, ComponentFieldType, ComponentSchema, PersistencePolicy, compile,
    m0_fixture_pack,
};
use torse_sim::action::{ActionTarget, ActorContext, register_action_module};
use torse_sim::replica::*;
use torse_sim::{OrderingKey, Phase, Transaction, WorldAuthority};
use torse_types::*;
use torse_world::*;

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).unwrap()
}
fn budget() -> OutputBudget {
    OutputBudget {
        max_items: 10_000,
        max_bytes: 4_000_000,
    }
}
fn limits() -> ReplicaLimits {
    ReplicaLimits {
        max_subscriptions: 4,
        max_sections: 4,
        max_queued_batches: 8,
        snapshot: budget(),
        queue: budget(),
    }
}
fn viewer(id: u128) -> Viewer {
    Viewer {
        session: SessionId(id),
        actor: ActorContextId(id),
    }
}
fn scope() -> ReplicaScope {
    ReplicaScope {
        sections: [SectionPos::ZERO].into(),
    }
}
fn fields(value: u32) -> ComponentFields {
    [(key("test:value"), ComponentValue::U32(value))].into()
}
fn tx(id: u128) -> Transaction {
    Transaction::new(
        TransactionId(id),
        key("test:owner"),
        OrderingKey {
            phase: Phase::Interaction,
            rule_priority: 0,
            stable_task_key: id as u64,
            local_sequence: 0,
        },
    )
}

fn world() -> WorldAuthority {
    let mut pack = m0_fixture_pack();
    let schemas = [
        (
            "test:public",
            ReplicationVisibility::Public,
            AuthorityMode::Server,
        ),
        (
            "test:authorized",
            ReplicationVisibility::Authorized,
            AuthorityMode::Server,
        ),
        (
            "test:private",
            ReplicationVisibility::Private,
            AuthorityMode::Server,
        ),
        (
            "test:client",
            ReplicationVisibility::Public,
            AuthorityMode::ClientPresentation,
        ),
    ]
    .map(|(id, visibility, authority)| ComponentSchema {
        id: key(id),
        schema_version: 1,
        owner_module: key("test:owner"),
        fields: vec![ComponentField {
            id: key("test:value"),
            field_type: ComponentFieldType::Unsigned32,
            required: true,
        }],
        persistence: PersistencePolicy::Always,
        replication: ReplicationDeclaration {
            authority,
            visibility,
            update_mode: UpdateMode::CompleteTransaction,
            presentation_hint: PresentationHint::Discrete,
        },
        migration_binding: None,
    })
    .into();
    register_action_module(
        &mut pack,
        key("test:owner"),
        key("test:state"),
        schemas,
        vec![],
        vec![],
    );
    let content = compile(pack).unwrap();
    let registry = BlockStateRegistry::build(&content).unwrap();
    let schemas = ComponentSchemas::build(&content);
    let mut world = WorldAuthority::new(
        WorldId(1),
        registry,
        schemas,
        16,
        WorldBounds::new(0, 32).unwrap(),
        Compatibility {
            authority_schema: 1,
            rules_digest: Digest([1; 32]),
            content_digest: Digest([2; 32]),
        },
    )
    .unwrap();
    for pos in [SectionPos::ZERO, SectionPos::new(1, 0, 0)] {
        world.store_mut().request(pos).unwrap();
        world
            .store_mut()
            .install(pos, Section::empty(16).unwrap())
            .unwrap();
        world.store_mut().finish_load(pos).unwrap();
    }
    for position in [Position::new(4.0, 4.0, 4.0), Position::new(20.0, 4.0, 4.0)] {
        let handle = world.entities_mut().spawn(position).unwrap();
        for name in [
            "test:public",
            "test:authorized",
            "test:private",
            "test:client",
        ] {
            // Set up via a trusted transaction to exercise the same owner validation.
            let entity = world.entities().get(handle).unwrap();
            let mut edit = tx(world.seq().0 as u128 + 1);
            edit.components.push(ComponentEdit {
                id: entity.id,
                schema: key(name),
                previous: None,
                fields: Some(fields(7)),
            });
            world.commit(&edit).unwrap();
        }
    }
    world
}

fn source(limits: ReplicaLimits) -> (ReplicaSource, Binding) {
    let world = world();
    let binding = Binding {
        world: world.world(),
        epoch: WorldEpoch(1),
        compatibility: world.coordinator().compatibility(),
    };
    (ReplicaSource::new(world, binding, limits).unwrap(), binding)
}
fn open(source: &mut ReplicaSource, id: u128, authorized: bool) -> WorldReplicaSeed {
    let actor = ActorContext::new(key("test:player"));
    let actor = if authorized {
        actor.target(ActionTarget::Entity(EntityId(1)))
    } else {
        actor
    };
    source
        .open_replica(viewer(id), actor, scope(), budget())
        .unwrap()
}
fn publish(source: &mut ReplicaSource, tx: &Transaction) -> torse_sim::Publication {
    let prepared = source.prepare(tx).unwrap();
    source.publish(prepared).unwrap()
}
fn edit(source: &ReplicaSource, schema: &str, value: u32, id: u128) -> Transaction {
    let entity = source
        .world()
        .entities()
        .get(source.world().entities().handle_of(EntityId(1)).unwrap())
        .unwrap();
    let mut tx = tx(id);
    tx.components.push(ComponentEdit {
        id: entity.id,
        schema: key(schema),
        previous: entity.components.component(&key(schema)).cloned(),
        fields: Some(fields(value)),
    });
    tx
}
fn block(source: &ReplicaSource, id: u128, x: i64) -> Transaction {
    let mut tx = tx(id);
    tx.write_block(
        BlockPos::new(x, 1, 1),
        source
            .world()
            .registry()
            .default_state(&key("base:stone"))
            .unwrap(),
    );
    tx
}

#[test]
fn baseline_and_actions_share_target_grants_without_private_or_out_of_scope_data() {
    let (mut source, _) = source(limits());
    let owner = open(&mut source, 1, true);
    let observer = open(&mut source, 2, false);
    assert_eq!(owner.state.entities.len(), 1);
    assert_eq!(owner.state.sections.len(), 1);
    assert_eq!(owner.state.entities[0].components.len(), 2);
    assert_eq!(
        observer.state.entities[0]
            .components
            .keys()
            .collect::<Vec<_>>(),
        vec![&key("test:public")]
    );
    for seed in [&owner, &observer] {
        assert!(!format!("{seed:?}").contains("test:private"));
        assert!(!format!("{seed:?}").contains("test:client"));
    }
    let actor = ActorContext::new(key("test:player"));
    assert_eq!(
        actor
            .view(source.world())
            .component(EntityId(1), &key("test:authorized")),
        Err(RejectCode::PermissionDenied)
    );
    let actor = actor.target(ActionTarget::Entity(EntityId(1)));
    assert_eq!(
        actor
            .view(source.world())
            .component(EntityId(1), &key("test:authorized"))
            .unwrap(),
        &fields(7)
    );
}

#[test]
fn seed_has_no_gap_prepared_dropped_and_invisible_commits_do_not_consume_replica_sequence() {
    let (mut source, binding) = source(limits());
    let seed = open(&mut source, 1, false);
    let dropped = source.prepare(&block(&source, 100, 1)).unwrap();
    drop(dropped);
    assert!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap()
            .is_empty()
    );
    let private = edit(&source, "test:private", 9, 101);
    publish(&mut source, &private);
    let authorized = edit(&source, "test:authorized", 9, 102);
    publish(&mut source, &authorized);
    assert!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap()
            .is_empty()
    );
    let tx = block(&source, 103, 1);
    let publication = publish(&mut source, &tx);
    let batches = source
        .take_replica_batches(binding, seed.subscription, budget())
        .unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].seq, ReplicaSeq(1));
    assert_eq!(batches[0].through, publication.cursor);
    assert_eq!(batches[0].through.seq.0, seed.baseline.seq.0 + 3);
    let fresh = open(&mut source, 2, false);
    assert_eq!(batches[0].changes.sections, fresh.state.sections);
    assert_eq!(batches[0].changes.states, fresh.state.states);
}

#[test]
fn mixed_transaction_is_one_filtered_batch_and_replaces_removed_components() {
    let (mut source, binding) = source(limits());
    let owner = open(&mut source, 1, true);
    let observer = open(&mut source, 2, false);
    let mut tx = edit(&source, "test:authorized", 8, 100);
    tx.write_block(
        BlockPos::new(1, 1, 1),
        source
            .world()
            .registry()
            .default_state(&key("base:stone"))
            .unwrap(),
    );
    tx.components
        .extend(edit(&source, "test:public", 9, 101).components);
    tx.components
        .extend(edit(&source, "test:private", 10, 102).components);
    publish(&mut source, &tx);
    for (seed, count) in [(&owner, 2), (&observer, 1)] {
        let batches = source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].changes.sections.len(), 1);
        assert_eq!(batches[0].changes.entities[0].components.len(), count);
        assert!(!format!("{batches:?}").contains("test:private"));
        assert!(!format!("{batches:?}").contains("previous"));
    }
    let mut remove = edit(&source, "test:authorized", 8, 104);
    remove.components[0].fields = None;
    publish(&mut source, &remove);
    let owner = source
        .take_replica_batches(binding, owner.subscription, budget())
        .unwrap();
    assert_eq!(owner[0].changes.entities[0].components.len(), 1);
    assert!(
        source
            .take_replica_batches(binding, observer.subscription, budget())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn too_small_output_retries_without_splitting_or_consuming_batch() {
    let (mut source, binding) = source(limits());
    let seed = open(&mut source, 1, false);
    let tx = block(&source, 100, 1);
    publish(&mut source, &tx);
    for budget in [
        OutputBudget {
            max_items: 0,
            max_bytes: 1_000_000,
        },
        OutputBudget {
            max_items: 1000,
            max_bytes: 1,
        },
    ] {
        assert_eq!(
            source
                .take_replica_batches(binding, seed.subscription, budget)
                .unwrap_err()
                .code,
            FaultCode::ResourceLimit
        );
    }
    let batch = source
        .take_replica_batches(binding, seed.subscription, budget())
        .unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].seq, ReplicaSeq(1));
    assert!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn slow_subscription_resets_without_blocking_world_or_healthy_viewer() {
    let mut limits = limits();
    limits.max_queued_batches = 1;
    let (mut source, binding) = source(limits);
    let slow = open(&mut source, 1, false);
    let healthy = open(&mut source, 2, false);
    let tx = block(&source, 100, 1);
    publish(&mut source, &tx);
    source
        .take_replica_batches(binding, healthy.subscription, budget())
        .unwrap();
    let tx = block(&source, 101, 2);
    let result = publish(&mut source, &tx);
    assert_eq!(result.invalidated_replicas, vec![slow.subscription]);
    assert_eq!(source.world().seq(), result.cursor.seq);
    assert_eq!(
        source
            .take_replica_batches(binding, slow.subscription, budget())
            .unwrap_err()
            .code,
        FaultCode::ResetRequired
    );
    assert_eq!(
        source
            .take_replica_batches(binding, healthy.subscription, budget())
            .unwrap()[0]
            .seq,
        ReplicaSeq(2)
    );
    source.close_replica(binding, slow.subscription).unwrap();
    let rebuilt = open(&mut source, 1, false);
    assert_ne!(rebuilt.subscription, slow.subscription);
    assert_eq!(rebuilt.baseline, result.cursor);
}

#[test]
fn revocation_clears_queued_data_and_signals_b_even_if_it_already_drained() {
    let (mut source, binding) = source(limits());
    let seed = open(&mut source, 1, true);
    let tx = edit(&source, "test:authorized", 8, 100);
    publish(&mut source, &tx);
    // B now holds a previously authorized batch; invalidation is a separate signal.
    assert_eq!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap()
            .len(),
        1
    );
    let tx = edit(&source, "test:authorized", 9, 101);
    publish(&mut source, &tx);
    assert_eq!(source.invalidate_viewer(viewer(1)), vec![seed.subscription]);
    assert_eq!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap_err()
            .code,
        FaultCode::ResetRequired
    );
    let phase = source.prepare_phase_advance().unwrap();
    assert_eq!(
        source.publish(phase).unwrap().invalidated_replicas,
        vec![seed.subscription]
    );
    source.close_replica(binding, seed.subscription).unwrap();
    let seed = open(&mut source, 1, false);
    assert!(
        !seed.state.entities[0]
            .components
            .contains_key(&key("test:authorized"))
    );
}

#[test]
fn stale_binding_and_closed_subscription_cannot_drain_or_close_new_data() {
    let (mut source, binding) = source(limits());
    let seed = open(&mut source, 1, false);
    let stale = Binding {
        epoch: WorldEpoch(2),
        ..binding
    };
    assert_eq!(
        source
            .close_replica(stale, seed.subscription)
            .unwrap_err()
            .code,
        FaultCode::ResetRequired
    );
    assert_eq!(
        source
            .take_replica_batches(stale, seed.subscription, budget())
            .unwrap_err()
            .code,
        FaultCode::ResetRequired
    );
    let tx = block(&source, 100, 1);
    publish(&mut source, &tx);
    assert_eq!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap()
            .len(),
        1
    );
    source.close_replica(binding, seed.subscription).unwrap();
    assert_eq!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap_err()
            .code,
        FaultCode::ResetRequired
    );
}

#[test]
fn seed_budget_scope_and_subscription_cap_fail_without_consuming_identity() {
    let mut limits = limits();
    limits.max_subscriptions = 1;
    let (mut source, binding) = source(limits);
    let result = source.open_replica(
        viewer(1),
        ActorContext::new(key("test:player")),
        scope(),
        OutputBudget {
            max_items: 1,
            max_bytes: 1,
        },
    );
    assert_eq!(result.unwrap_err().code, FaultCode::ResourceLimit);
    let seed = open(&mut source, 1, false);
    assert_eq!(seed.subscription, SubscriptionId(1));
    assert_eq!(
        source
            .open_replica(
                viewer(2),
                ActorContext::new(key("test:player")),
                scope(),
                budget()
            )
            .unwrap_err()
            .code,
        FaultCode::ResourceLimit
    );
    source.close_replica(binding, seed.subscription).unwrap();
    let rebuilt = open(&mut source, 2, false);
    assert_eq!(rebuilt.subscription, SubscriptionId(2));
}

#[test]
fn unready_and_absent_sections_are_omitted_not_fabricated_as_air() {
    let mut world = world();
    let pos = SectionPos::new(-1, 0, 0);
    world.store_mut().request(pos).unwrap();
    world
        .store_mut()
        .install(pos, Section::empty(16).unwrap())
        .unwrap();
    let binding = Binding {
        world: world.world(),
        epoch: WorldEpoch(1),
        compatibility: world.coordinator().compatibility(),
    };
    let mut source = ReplicaSource::new(world, binding, limits()).unwrap();
    let seed = source
        .open_replica(
            viewer(1),
            ActorContext::new(key("test:player")),
            ReplicaScope {
                sections: [pos, SectionPos::new(-2, 0, 0)].into(),
            },
            budget(),
        )
        .unwrap();
    assert!(seed.state.sections.is_empty());
    assert!(seed.state.states.is_empty());
    assert!(seed.state.entities.is_empty());
}

#[test]
fn entity_crossing_scope_despawn_and_spawn_emit_final_replacements_and_tombstones() {
    let (mut source, binding) = source(limits());
    let seed = open(&mut source, 1, false);
    let handle = source.world().entities().handle_of(EntityId(1)).unwrap();
    let mut movement = tx(100);
    movement.move_entity(
        handle,
        EntityId(1),
        Position::new(4.0, 4.0, 4.0),
        [13.0, 0.0, 0.0],
        [0.25; 3],
    );
    publish(&mut source, &movement);
    let batch = source
        .take_replica_batches(binding, seed.subscription, budget())
        .unwrap();
    assert_eq!(batch[0].changes.removed_entities, vec![EntityId(1)]);
    let entity = source.world().entities().get(handle).unwrap();
    let mut movement = tx(101);
    movement.move_entity(
        handle,
        entity.id,
        entity.position,
        [-13.0, 0.0, 0.0],
        [0.25; 3],
    );
    publish(&mut source, &movement);
    assert_eq!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap()[0]
            .changes
            .entities[0]
            .id,
        EntityId(1)
    );
    let mut despawn = tx(102);
    despawn.despawn_entity(handle, source.world().entities().get(handle).unwrap());
    publish(&mut source, &despawn);
    assert_eq!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap()[0]
            .changes
            .removed_entities,
        vec![EntityId(1)]
    );
    let mut spawn = tx(103);
    spawn.spawn_entity(Position::new(4.0, 4.0, 4.0), BTreeMap::new());
    publish(&mut source, &spawn);
    let batch = source
        .take_replica_batches(binding, seed.subscription, budget())
        .unwrap();
    assert_eq!(batch[0].changes.entities[0].id, EntityId(3));
    assert!(batch[0].changes.entities[0].components.is_empty());
}

#[test]
fn queue_item_and_byte_caps_signal_reset_on_first_oversize_atomic_batch() {
    for cap in [
        OutputBudget {
            max_items: 1,
            max_bytes: 4_000_000,
        },
        OutputBudget {
            max_items: 10_000,
            max_bytes: 1,
        },
    ] {
        let mut limits = limits();
        limits.queue = cap;
        let (mut source, binding) = source(limits);
        let seed = open(&mut source, 1, false);
        let tx = block(&source, 100, 1);
        let publication = publish(&mut source, &tx);
        assert_eq!(publication.invalidated_replicas, vec![seed.subscription]);
        assert_eq!(
            source
                .take_replica_batches(binding, seed.subscription, budget())
                .unwrap_err()
                .code,
            FaultCode::ResetRequired
        );
    }
}

#[test]
fn scope_limit_and_wrong_binding_are_rejected() {
    let world = world();
    let binding = Binding {
        world: WorldId(99),
        epoch: WorldEpoch(1),
        compatibility: world.coordinator().compatibility(),
    };
    assert!(ReplicaSource::new(world, binding, limits()).is_err());
    let mut limits = limits();
    limits.max_sections = 1;
    let (mut source, _) = source(limits);
    let scope = ReplicaScope {
        sections: [SectionPos::ZERO, SectionPos::new(1, 0, 0)].into(),
    };
    assert_eq!(
        source
            .open_replica(
                viewer(1),
                ActorContext::new(key("test:player")),
                scope,
                budget()
            )
            .unwrap_err()
            .code,
        FaultCode::ResourceLimit
    );
}

#[test]
fn output_budget_returns_only_complete_prefixes_in_replica_sequence_order() {
    let (mut source, binding) = source(limits());
    let seed = open(&mut source, 1, false);
    for (id, value) in [(100, 8), (101, 9)] {
        let tx = edit(&source, "test:public", value, id);
        publish(&mut source, &tx);
    }
    // Batch + entity + component + field = four counted items.
    let one = OutputBudget {
        max_items: 4,
        ..budget()
    };
    for seq in [1, 2] {
        let batches = source
            .take_replica_batches(binding, seed.subscription, one)
            .unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].seq, ReplicaSeq(seq));
    }
    assert!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dense_sections_are_budgeted_and_state_table_does_not_reveal_other_sections() {
    let mut world = world();
    let stone = world.registry().default_state(&key("base:stone")).unwrap();
    world
        .store_mut()
        .edit(BlockPos::new(17, 1, 1), stone)
        .unwrap();
    let binding = Binding {
        world: world.world(),
        epoch: WorldEpoch(1),
        compatibility: world.coordinator().compatibility(),
    };
    let mut source = ReplicaSource::new(world, binding, limits()).unwrap();
    let seed = open(&mut source, 1, false);
    assert!(seed.state.states.is_empty());
    let dense_scope = ReplicaScope {
        sections: [SectionPos::new(1, 0, 0)].into(),
    };
    let small = OutputBudget {
        max_bytes: 8_000,
        ..budget()
    };
    assert_eq!(
        source
            .open_replica(
                viewer(2),
                ActorContext::new(key("test:player")),
                dense_scope.clone(),
                small
            )
            .unwrap_err()
            .code,
        FaultCode::ResourceLimit
    );
    let dense = source
        .open_replica(
            viewer(2),
            ActorContext::new(key("test:player")),
            dense_scope,
            budget(),
        )
        .unwrap();
    assert_eq!(dense.state.states[0].block, key("base:stone"));
    assert!(matches!(
        dense.state.sections[0].cells,
        SnapshotCells::Dense(_)
    ));
}

#[test]
fn growing_projection_resets_subscription_after_publication_without_partial_output() {
    let mut limits = limits();
    limits.snapshot.max_bytes = 8_000; // Uniform seed fits, one dense section cannot.
    let (mut source, binding) = source(limits);
    let seed = open(&mut source, 1, false);
    let tx = block(&source, 100, 1);
    let result = publish(&mut source, &tx);
    assert_eq!(result.invalidated_replicas, vec![seed.subscription]);
    assert!(
        matches!(source.world().store().query(BlockPos::new(1, 1, 1)), TerrainQuery::Ready(state) if state != BlockStateId::EMPTY)
    );
    assert_eq!(
        source
            .take_replica_batches(binding, seed.subscription, budget())
            .unwrap_err()
            .code,
        FaultCode::ResetRequired
    );
}

#[test]
fn applying_atomic_deltas_reconstructs_fresh_authorized_seed() {
    let (mut source, binding) = source(limits());
    let seed = open(&mut source, 1, true);
    let subscription = seed.subscription;
    let mut mirror = seed.state;
    let mut next_seq = 1;
    for index in 0..4 {
        let tx = if index % 2 == 0 {
            block(&source, 100 + index, index as i64 + 1)
        } else {
            edit(&source, "test:authorized", 10 + index as u32, 100 + index)
        };
        publish(&mut source, &tx);
        for batch in source
            .take_replica_batches(binding, subscription, budget())
            .unwrap()
        {
            assert_eq!(batch.seq, ReplicaSeq(next_seq));
            next_seq += 1;
            let change = batch.changes;
            mirror.states = change.states;
            mirror
                .sections
                .retain(|s| !change.removed_sections.contains(&s.pos));
            mirror
                .entities
                .retain(|e| !change.removed_entities.contains(&e.id));
            for section in change.sections {
                mirror.sections.retain(|s| s.pos != section.pos);
                mirror.sections.push(section);
            }
            for entity in change.entities {
                mirror.entities.retain(|e| e.id != entity.id);
                mirror.entities.push(entity);
            }
            mirror.sections.sort_by_key(|s| s.pos);
            mirror.entities.sort_by_key(|e| e.id);
        }
        let fresh = open(&mut source, 2, true);
        assert_eq!(mirror, fresh.state);
        source.close_replica(binding, fresh.subscription).unwrap();
    }
}
