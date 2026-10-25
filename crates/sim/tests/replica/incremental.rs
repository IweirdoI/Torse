use super::*;

#[test]
fn unavailable_foreign_preparation_preserves_the_original_publication_fault() {
    let foreign = world();
    let mut initial = world();
    initial
        .store_mut()
        .unload(SectionPos::new(1, 0, 0))
        .unwrap();
    let binding = Binding {
        world: initial.world(),
        epoch: WorldEpoch(1),
        compatibility: initial.coordinator().compatibility(),
    };
    let mut source = ReplicaSource::new(initial, binding, limits()).unwrap();
    let seed = open(&mut source, 1, false);
    let mut change = tx(600);
    change.write_block(BlockPos::new(20, 1, 1), BlockStateId(1));
    let prepared = foreign.prepare(&change).unwrap();
    let seq = source.world().seq();
    assert!(source.publish(prepared).is_err());
    assert_eq!(seq, source.world().seq());
    assert!(
        take(&mut source, binding, seed.subscription, budget())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn repeated_cells_in_an_unchecked_group_fall_back_to_final_state_projection() {
    let (mut a, mut b, binding) = pair(limits(), false);
    let seed = open(&mut a, 1, false);
    assert_eq!(seed_view(&seed), seed_view(&open(&mut b, 1, false)));
    let mut first = tx(601);
    first.write_block(BlockPos::new(1, 1, 1), BlockStateId(1));
    let mut second = tx(602);
    second.source = key("test:other");
    second.write_block(BlockPos::new(1, 1, 1), BlockStateId(2));
    let group = torse_sim::TransactionGroup {
        id: first.id,
        ordering: first.ordering,
        members: vec![first, second],
    };
    let left = a.world().prepare_group(&group).unwrap();
    let right = b.world().prepare_group(&group).unwrap();
    assert_eq!(a.publish(left).unwrap(), b.publish(right).unwrap());
    let left = take(&mut a, binding, seed.subscription, budget()).unwrap();
    let right = take(&mut b, binding, seed.subscription, budget()).unwrap();
    assert_eq!(left, right);
    assert_eq!(
        left[0].4.states.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![BlockStateId(2)]
    );
}

fn seed_view(seed: &WorldReplicaSeed) -> (Binding, SubscriptionId, CommitCursor, &ReplicaSnapshot) {
    (seed.binding, seed.subscription, seed.baseline, &seed.state)
}
type BatchView = (
    Binding,
    SubscriptionId,
    ReplicaSeq,
    CommitCursor,
    ReplicaChanges,
);
fn take(
    source: &mut ReplicaSource,
    binding: Binding,
    id: SubscriptionId,
    budget: OutputBudget,
) -> Result<Vec<BatchView>, FaultCode> {
    source
        .take_replica_batches(binding, id, budget)
        .map(|batches| {
            batches
                .into_iter()
                .map(|b| (b.binding, b.subscription, b.seq, b.through, b.changes))
                .collect()
        })
        .map_err(|e| e.code)
}

fn pair(limits: ReplicaLimits, uniform: bool) -> (ReplicaSource, ReplicaSource, Binding) {
    let make = || {
        let mut world = world_with_payload(true);
        if uniform {
            let stone = world.registry().default_state(&key("base:stone")).unwrap();
            world.store_mut().unload(SectionPos::ZERO).unwrap();
            world.store_mut().request(SectionPos::ZERO).unwrap();
            world
                .store_mut()
                .install(SectionPos::ZERO, Section::new(16, stone).unwrap())
                .unwrap();
            world.store_mut().finish_load(SectionPos::ZERO).unwrap();
        }
        let binding = Binding {
            world: world.world(),
            epoch: WorldEpoch(1),
            compatibility: world.coordinator().compatibility(),
        };
        (ReplicaSource::new(world, binding, limits).unwrap(), binding)
    };
    let (a, binding) = make();
    let (mut b, _) = make();
    b.set_projection_optimization(false);
    (a, b, binding)
}
fn nested(value: usize) -> ComponentFields {
    [(
        key("test:value"),
        ComponentValue::List(vec![ComponentValue::Record(
            [
                (
                    key("test:content"),
                    ComponentValue::Content(key("test:entry")),
                ),
                (key("test:text"), ComponentValue::Text("x".repeat(value))),
                (
                    key("test:list"),
                    ComponentValue::List(vec![ComponentValue::I64(-3), ComponentValue::Bool(true)]),
                ),
            ]
            .into(),
        )]),
    )]
    .into()
}
fn mutation(source: &ReplicaSource, step: u32) -> Transaction {
    let mut change = tx(u128::from(step) + 1000);
    let world = source.world();
    let handle = world.entities().handle_of(EntityId(1)).unwrap();
    let entity = world.entities().get(handle).unwrap();
    let component = |schema: &str, fields| ComponentEdit {
        id: entity.id,
        schema: key(schema),
        previous: entity.components.component(&key(schema)).cloned(),
        fields,
    };
    let pos = BlockPos::new(i64::from(step % 32), 1, 1);
    match step % 10 {
        0 | 1 => {
            let states = [
                BlockStateId::EMPTY,
                BlockStateId(1),
                BlockStateId((world.registry().state_count() - 1) as u16),
            ];
            change.write_block(pos, states[(step / 10 % 3) as usize]);
        }
        2 => {
            change
                .components
                .push(component("test:public", Some(fields(step))));
        }
        3 => {
            change.components.push(component(
                "test:authorized",
                if step % 20 == 3 {
                    None
                } else {
                    Some(fields(step))
                },
            ));
        }
        4 => {
            change
                .components
                .push(component("test:private", Some(fields(step))));
        }
        5 => {
            change.move_entity(
                handle,
                entity.id,
                entity.position,
                [
                    if entity.position.x < 16.0 {
                        13.0
                    } else {
                        -13.0
                    },
                    0.0,
                    0.0,
                ],
                [0.25; 3],
            );
        }
        6 => {
            change.spawn_entity(
                Position::new(4.0, 4.0, 4.0),
                [
                    (key("test:public"), fields(step)),
                    (key("test:authorized"), fields(step)),
                ]
                .into(),
            );
        }
        7 => {
            let (h, e) = world.entities().iter().max_by_key(|(_, e)| e.id).unwrap();
            assert_ne!(e.id, EntityId(1));
            change.despawn_entity(h, e);
        }
        8 => {
            change.components.push(component(
                "test:payload",
                Some(nested((step as usize / 10 % 4) * 700)),
            ));
        }
        _ => {
            change.components.push(component("test:payload", None));
            change.write_block(pos, BlockStateId::EMPTY);
            change.write_block(BlockPos::new(31, 2, 1), BlockStateId(1));
        }
    }
    change
}

#[test]
fn full_and_incremental_match_mixed_commits_nested_fields_scope_lifecycle_and_mode_switches() {
    for (uniform, switches) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut limits = limits();
        limits.max_subscriptions = 8;
        let (mut incremental, mut reference, binding) = pair(limits, uniform);
        let mut subscriptions = vec![];
        for i in 1..=4 {
            let scope = ReplicaScope {
                sections: match i {
                    1 => [SectionPos::ZERO].into(),
                    2 => [SectionPos::new(1, 0, 0)].into(),
                    _ => [SectionPos::ZERO, SectionPos::new(1, 0, 0)].into(),
                },
            };
            let actor = ActorContext::new(key("test:viewer"));
            let actor = if i == 3 {
                actor.target(ActionTarget::Entity(EntityId(1)))
            } else {
                actor
            };
            let a = incremental
                .open_replica(viewer(i), actor.clone(), scope.clone(), budget())
                .unwrap();
            let b = reference
                .open_replica(viewer(i), actor, scope, budget())
                .unwrap();
            assert_eq!(seed_view(&a), seed_view(&b));
            subscriptions.push(a.subscription);
        }
        for step in 0..100 {
            // A live diagnostic switch must rebuild derived bookkeeping exactly once.
            incremental.set_projection_optimization(!switches || (step / 7) % 2 == 0);
            let change = mutation(&incremental, step);
            match (incremental.prepare(&change), reference.prepare(&change)) {
                (Ok(a), Ok(b)) => assert_eq!(
                    incremental.publish(a).unwrap(),
                    reference.publish(b).unwrap()
                ),
                (Err(a), Err(b)) => assert_eq!(a, b, "step={step}, uniform={uniform}"),
                _ => panic!("preparation diverged at step={step}, uniform={uniform}"),
            }
            if step == 55 {
                assert_eq!(
                    incremental.invalidate_viewer(viewer(3)),
                    reference.invalidate_viewer(viewer(3))
                );
            }
            if step % 3 == 0 {
                for &id in &subscriptions[..3] {
                    let a = take(&mut incremental, binding, id, budget());
                    let b = take(&mut reference, binding, id, budget());
                    assert_eq!(a, b, "step={step}, uniform={uniform}");
                }
            }
            if step % 10 == 0 {
                let actor = ActorContext::new(key("test:viewer"));
                let a = incremental
                    .open_replica(viewer(9), actor.clone(), scope(), budget())
                    .unwrap();
                let b = reference
                    .open_replica(viewer(9), actor, scope(), budget())
                    .unwrap();
                assert_eq!(seed_view(&a), seed_view(&b));
                incremental.close_replica(binding, a.subscription).unwrap();
                reference.close_replica(binding, b.subscription).unwrap();
            }
        }
        // The fourth subscriber was never drained and must have reset atomically.
        for id in subscriptions {
            assert_eq!(
                take(&mut incremental, binding, id, budget()),
                take(&mut reference, binding, id, budget())
            );
        }
    }
}

#[test]
fn full_and_incremental_enforce_identical_snapshot_queue_and_take_budget_boundaries() {
    let mut accepted = 0;
    let mut reset = 0;
    for items in [8, 16, 32, 64, 256] {
        for bytes in [512, 1024, 4096, 9000, 18000, 40000] {
            for queue in [
                OutputBudget::new(16, 9000),
                OutputBudget::new(128, 18000),
                budget(),
            ] {
                let mut limits = limits();
                limits.snapshot = OutputBudget::new(items, bytes);
                limits.queue = queue;
                let (mut a, mut b, binding) = pair(limits, false);
                let actor =
                    ActorContext::new(key("test:viewer")).target(ActionTarget::Entity(EntityId(1)));
                let left = a.open_replica(viewer(1), actor.clone(), scope(), budget());
                let right = b.open_replica(viewer(1), actor, scope(), budget());
                assert_eq!(
                    left.as_ref().map(seed_view).map_err(|e| e.code),
                    right.as_ref().map(seed_view).map_err(|e| e.code)
                );
                let Ok(seed) = left else {
                    continue;
                };
                accepted += 1;
                for step in [0, 8, 18, 28, 38, 9, 5, 15] {
                    let change = mutation(&a, step);
                    let left = publish(&mut a, &change);
                    let right = publish(&mut b, &change);
                    assert_eq!(left, right, "items={items} bytes={bytes}");
                    reset += left.invalidated_replicas.len();
                    for output in [OutputBudget::new(1, 1), OutputBudget::new(8, 512), budget()] {
                        assert_eq!(
                            take(&mut a, binding, seed.subscription, output),
                            take(&mut b, binding, seed.subscription, output)
                        );
                    }
                }
            }
        }
    }
    assert!(accepted > 0 && reset > 0);
}

#[test]
fn symbol_counts_keep_shared_symbols_until_last_cell_and_support_swaps_and_noops() {
    let (mut a, mut b, binding) = pair(limits(), false);
    let actor = ActorContext::new(key("test:viewer"));
    let interest = ReplicaScope {
        sections: [SectionPos::ZERO, SectionPos::new(1, 0, 0)].into(),
    };
    let seed = a
        .open_replica(viewer(1), actor.clone(), interest.clone(), budget())
        .unwrap();
    assert_eq!(
        seed_view(&seed),
        seed_view(
            &b.open_replica(viewer(1), actor, interest, budget())
                .unwrap()
        )
    );
    for (step, writes) in [
        vec![(1, 1), (2, 2), (17, 1)],
        vec![(1, 2), (2, 1)],
        vec![(1, 2)],
        vec![(2, 0)],
        vec![(17, 0)],
        vec![(1, 0)],
    ]
    .into_iter()
    .enumerate()
    {
        let mut change = tx(step as u128 + 500);
        for (x, state) in writes {
            change.write_block(BlockPos::new(x, 1, 1), BlockStateId(state));
        }
        assert_eq!(publish(&mut a, &change), publish(&mut b, &change));
        assert_eq!(
            take(&mut a, binding, seed.subscription, budget()),
            take(&mut b, binding, seed.subscription, budget())
        );
    }
    let fresh = open(&mut a, 2, false);
    assert!(fresh.state.states.is_empty());
}
