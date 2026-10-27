//! Event-local tickets, isolated page I/O, deterministic generation, and recovery.
mod support;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use support::Directory;
use torse_server::{bootstrap as base, execution::*, residency::*, section_pages::SectionPages, *};
use torse_sim::durable::*;
use torse_sim::{OrderingKey, Phase, SystemSpec, Transaction, WorldAuthority};
use torse_types::*;
use torse_world::*;

fn pages(dir: &Directory) -> SectionPages {
    let root = dir.0.join("sections");
    std::fs::create_dir_all(&root).unwrap();
    SectionPages::new(root, base::budget(), base::codec_limits(), 64).unwrap()
}
fn create(dir: &Directory, rules: Vec<Box<dyn DurableRule>>) -> base::Files {
    let (world, actions) = base::world().unwrap();
    base::Files::create_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| ExecutionHost::new(binding, world, actions, rules, base::execution_limits()),
    )
    .unwrap()
}
fn open(dir: &Directory, pages: SectionPages, rules: Vec<Box<dyn DurableRule>>) -> base::Files {
    let (world, actions) = base::world().unwrap();
    base::Files::open_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| {
            Ok(ExecutionRecovery::new(
                binding,
                world.registry(),
                world.schemas(),
                actions,
                rules,
                base::execution_limits(),
            )
            .with_section_pages(pages))
        },
    )
    .unwrap()
}
fn config() -> ResidencyConfig {
    ResidencyConfig {
        max_resident: 4,
        max_io_bytes: 32 * 1024 * 1024,
    }
}
fn settle(server: &mut base::Files) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match server.poll_residency(&mut NoReplicas).unwrap() {
            ResidencyProgress::Idle => return,
            ResidencyProgress::Pending | ResidencyProgress::Published(_) => {}
            other => panic!("unexpected residency result: {other:?}"),
        }
        assert!(Instant::now() < deadline, "residency did not settle");
        std::thread::sleep(Duration::from_millis(1));
    }
}

struct Rule {
    dependencies: Vec<EventDependency>,
    move_entity: bool,
    event_tick: u64,
}
impl DurableRule for Rule {
    fn spec(&self) -> SystemSpec {
        SystemSpec {
            id: base::key("test:event-residency"),
            phase: Phase::Simulation,
            rule_priority: 0,
            before: Vec::new(),
            after: Vec::new(),
        }
    }
    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        if tick != Tick(0) {
            return Ok(Vec::new());
        }
        let transaction = if self.move_entity {
            let handle = world.entities().handle_of(EntityId(1)).unwrap();
            let entity = world.entities().get(handle).unwrap();
            let ordering = OrderingKey {
                phase: Phase::Simulation,
                rule_priority: 0,
                stable_task_key: 1,
                local_sequence: 0,
            };
            let mut transaction =
                Transaction::new(TransactionId(1), base::key("test:move"), ordering);
            transaction.move_entity(
                handle,
                entity.id,
                entity.position,
                [16.0, 0.0, 0.0],
                [0.5; 3],
            );
            Some(
                torse_sim::TransactionGroup::new(TransactionId(1), ordering, vec![transaction])
                    .map_err(|_| Fault::invalid_data("test movement group"))?,
            )
        } else {
            None
        };
        Ok(vec![WorkPlan {
            transaction,
            schedule: vec![
                EventDraft {
                    tick: Tick(self.event_tick),
                    rule: self.spec().id,
                    payload: Default::default(),
                }
                .with_dependencies(self.dependencies.clone())?,
            ],
            cancel: Vec::new(),
        }])
    }
    fn event(&self, _: &WorldAuthority, _: Tick, _: &TimedEvent) -> Result<WorkPlan, Fault> {
        Ok(WorkPlan {
            transaction: None,
            schedule: Vec::new(),
            cancel: Vec::new(),
        })
    }
}
fn rule(dependencies: Vec<EventDependency>) -> Box<dyn DurableRule> {
    Box::new(Rule {
        dependencies,
        move_entity: false,
        event_tick: 10,
    })
}

#[test]
fn future_events_pin_only_declared_sections_and_replay_uses_the_same_rule() {
    let dir = Directory::new();
    let pages = pages(&dir);
    let dependencies = vec![EventDependency::Section(SectionPos::ZERO)];
    let mut server = create(&dir, vec![rule(dependencies.clone())]);
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    let events = server.authority().unwrap().runtime().events.clone();
    assert!(
        server
            .unload_section(&pages, SectionPos::ZERO, &mut NoReplicas)
            .is_err()
    );
    assert!(matches!(
        server
            .unload_section(&pages, SectionPos::new(1, 0, 0), &mut NoReplicas)
            .unwrap(),
        CommitStep::Durable(_)
    ));
    drop(server);
    let mut server = open(&dir, pages.clone(), vec![rule(dependencies)]);
    assert_eq!(server.authority().unwrap().runtime().events, events);
    server.checkpoint().unwrap();
    drop(server);
    let server = open(
        &dir,
        pages,
        vec![rule(vec![EventDependency::Section(SectionPos::ZERO)])],
    );
    assert!(
        server
            .authority()
            .unwrap()
            .section_is_event_pinned(SectionPos::ZERO)
            .unwrap()
    );
}

#[test]
fn entity_tickets_follow_committed_movement_and_cross_section_scopes_hold_both_sides() {
    let dir = Directory::new();
    let pages = pages(&dir);
    let mut server = create(
        &dir,
        vec![Box::new(Rule {
            dependencies: vec![EventDependency::Entity(EntityId(1))],
            move_entity: true,
            event_tick: 10,
        })],
    );
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    assert!(
        !server
            .authority()
            .unwrap()
            .section_is_event_pinned(SectionPos::ZERO)
            .unwrap()
    );
    assert!(
        server
            .authority()
            .unwrap()
            .section_is_event_pinned(SectionPos::new(1, 0, 0))
            .unwrap()
    );
    assert!(matches!(
        server
            .unload_section(&pages, SectionPos::ZERO, &mut NoReplicas)
            .unwrap(),
        CommitStep::Durable(_)
    ));
    assert!(
        server
            .unload_section(&pages, SectionPos::new(1, 0, 0), &mut NoReplicas)
            .is_err()
    );
    drop(server);
    let dir = Directory::new();
    let pages = SectionPages::new(
        {
            let p = dir.0.join("sections");
            std::fs::create_dir(&p).unwrap();
            p
        },
        base::budget(),
        base::codec_limits(),
        64,
    )
    .unwrap();
    let mut server = create(
        &dir,
        vec![rule(vec![
            EventDependency::Section(SectionPos::ZERO),
            EventDependency::Section(SectionPos::new(1, 0, 0)),
        ])],
    );
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    for pos in [SectionPos::ZERO, SectionPos::new(1, 0, 0)] {
        assert!(server.unload_section(&pages, pos, &mut NoReplicas).is_err());
    }
}

#[test]
fn automatic_load_unload_budget_backpressure_and_page_io_retry_preserve_authority() {
    let dir = Directory::new();
    let pages = pages(&dir);
    let mut server = create(&dir, Vec::new());
    let position = SectionPos::new(1, 0, 0);
    server
        .enable_residency(
            ResidencyConfig {
                max_io_bytes: 1,
                ..config()
            },
            pages.clone(),
            None,
            &[SectionPos::ZERO],
        )
        .unwrap();
    let before = server.authority().unwrap().cursor();
    assert!(matches!(
        server.poll_residency(&mut NoReplicas).unwrap(),
        ResidencyProgress::BudgetBlocked
    ));
    assert_eq!(server.authority().unwrap().cursor(), before);
    server
        .enable_residency(config(), pages.clone(), None, &[SectionPos::ZERO])
        .unwrap();
    settle(&mut server);
    let digest = server.authority().unwrap().parked_sections()[&position].digest;
    let saved = std::fs::read(pages.path(digest)).unwrap();
    std::fs::write(pages.path(digest), b"broken").unwrap();
    server
        .set_residency_interest(&[SectionPos::ZERO, position])
        .unwrap();
    let before = server.authority().unwrap().cursor();
    assert!(matches!(
        server.poll_residency(&mut NoReplicas).unwrap(),
        ResidencyProgress::Pending
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if server.poll_residency(&mut NoReplicas).is_err() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(server.authority().unwrap().cursor(), before);
    std::fs::write(pages.path(digest), saved).unwrap();
    settle(&mut server);
    assert!(
        server
            .authority()
            .unwrap()
            .world()
            .store()
            .section(position)
            .is_some()
    );
    server.checkpoint().unwrap();
    drop(server);
    assert!(
        open(&dir, pages, Vec::new())
            .authority()
            .unwrap()
            .parked_sections()
            .is_empty()
    );
}

struct EmptyGenerator {
    identity: GenerationIdentity,
    gate: Option<Arc<(Mutex<bool>, Condvar)>>,
}
struct GateRelease(Arc<(Mutex<bool>, Condvar)>);
impl Drop for GateRelease {
    fn drop(&mut self) {
        let (lock, condition) = &*self.0;
        *lock.lock().unwrap() = true;
        condition.notify_all();
    }
}
impl GenerationSource for EmptyGenerator {
    fn identity(&self) -> GenerationIdentity {
        self.identity.clone()
    }
    fn generate(&self, context: GenerationContext, pos: SectionPos) -> Result<Snapshot, Fault> {
        if let Some(gate) = &self.gate {
            let (lock, condition) = &**gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = condition.wait(open).unwrap();
            }
        }
        Ok(Snapshot {
            meta: context.meta,
            side: context.side,
            bounds: context.bounds,
            states: Vec::new(),
            sections: vec![SnapshotSection {
                pos,
                cells: SnapshotCells::Uniform(BlockStateId::EMPTY),
            }],
            entities: Vec::new(),
            next_entity_id: context.next_entity_id,
        })
    }
}
fn identity() -> GenerationIdentity {
    GenerationIdentity {
        id: base::key("test:empty-generator"),
        version: 1,
        configuration: Digest([7; 32]),
    }
}

#[test]
fn due_dependencies_pause_before_the_tick_and_automatic_io_resumes_with_queued_input() {
    use torse_sim::action::{ActionTarget, ActorContext};
    let dir = Directory::new();
    let pages = pages(&dir);
    let target = SectionPos::new(2, 0, 0);
    let mut server = create(
        &dir,
        vec![Box::new(Rule {
            dependencies: vec![EventDependency::Section(target)],
            move_entity: false,
            event_tick: 1,
        })],
    );
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    let before = server.authority().unwrap().cursor();
    assert!(
        server
            .authority()
            .unwrap()
            .waiting_for_event_sections()
            .unwrap()
    );
    assert!(matches!(
        server.drive_one(&mut NoReplicas).unwrap(),
        CommitStep::Idle
    ));
    assert_eq!(server.authority().unwrap().cursor(), before);
    server
        .register_session(
            Viewer {
                session: SessionId(1),
                actor: ActorContextId(1),
            },
            ActorContext::new(base::key("test:actor")),
            base::scope(),
        )
        .unwrap();
    assert!(matches!(
        server.submit(WorldRequest {
            id: RequestId {
                world: server.binding().world,
                epoch: server.binding().epoch,
                session: SessionId(1),
                seq: RequestSeq(1)
            },
            actor: ActorContextId(1),
            input: Call {
                action: base::key("test:unknown"),
                version: 1,
                target: ActionTarget::Block(BlockPos::new(0, 0, 0)),
                parameters: Default::default()
            },
        }),
        Admission::Queued { .. }
    ));
    let source = Arc::new(EmptyGenerator {
        identity: identity(),
        gate: None,
    });
    server
        .enable_residency(
            config(),
            pages,
            Some(source),
            &[SectionPos::ZERO, SectionPos::new(1, 0, 0)],
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.authority().unwrap().resume_point().tick == Tick(1) {
        server.drive_tick(&mut NoReplicas, 1024).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(server.authority().unwrap().runtime().events.is_empty());
    assert!(
        server
            .authority()
            .unwrap()
            .world()
            .store()
            .section(target)
            .is_some()
    );
}

#[test]
fn generator_descriptor_damage_and_overfull_interests_fail_without_rewriting_data() {
    let mut encoded = Vec::new();
    assert!(
        torse_codec::wire::encode(
            &GenerationIdentity {
                version: 0,
                ..identity()
            },
            &mut encoded,
            base::budget(),
            base::codec_limits()
        )
        .is_err()
    );
    assert!(encoded.is_empty());
    let dir = Directory::new();
    let pages = pages(&dir);
    let mut server = create(&dir, Vec::new());
    let before = server.authority().unwrap().cursor();
    assert!(
        server
            .enable_residency(config(), pages.clone(), None, &[SectionPos::ZERO; 5])
            .is_err()
    );
    assert_eq!(server.authority().unwrap().cursor(), before);
    let source = Arc::new(EmptyGenerator {
        identity: identity(),
        gate: None,
    });
    server
        .enable_residency(
            config(),
            pages,
            Some(source),
            &[SectionPos::ZERO, SectionPos::new(1, 0, 0)],
        )
        .unwrap();
    drop(server);
    let path = dir.0.join("generation.bin");
    let bytes = std::fs::read(&path).unwrap();
    for bad in [bytes[..bytes.len() - 1].to_vec(), {
        let mut damaged = bytes.clone();
        damaged[15] ^= 1;
        damaged
    }] {
        std::fs::write(&path, &bad).unwrap();
        let lease = WorldLease::inspect(&dir.0, base::WORLD, 8192).unwrap();
        assert!(GenerationIdentity::inspect(lease.root()).is_err());
        let (world, _) = base::world().unwrap();
        assert!(
            base::Files::preflight_file(
                &lease,
                world.coordinator().compatibility(),
                base::Codec::new(base::codec_limits()).unwrap(),
                base::file_policy()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bad);
    }
}

#[test]
fn blocked_generation_does_not_block_poll_or_allow_page_collection_and_stale_results_are_discarded()
{
    let dir = Directory::new();
    let pages = pages(&dir);
    let mut server = create(&dir, Vec::new());
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let source = Arc::new(EmptyGenerator {
        identity: identity(),
        gate: Some(gate.clone()),
    });
    let fresh = SectionPos::new(2, 0, 0);
    server
        .enable_residency(
            config(),
            pages.clone(),
            Some(source),
            &[SectionPos::ZERO, SectionPos::new(1, 0, 0), fresh],
        )
        .unwrap();
    let _release_on_failure = GateRelease(gate.clone());
    let before = server.authority().unwrap().cursor();
    assert!(matches!(
        server.poll_residency(&mut NoReplicas).unwrap(),
        ResidencyProgress::Pending
    ));
    for _ in 0..16 {
        assert!(matches!(
            server.poll_residency(&mut NoReplicas).unwrap(),
            ResidencyProgress::Pending
        ));
    }
    assert!(server.collect_section_pages(&pages, 1).is_err());
    server
        .set_residency_interest(&[SectionPos::ZERO, SectionPos::new(1, 0, 0)])
        .unwrap();
    {
        let (lock, condition) = &*gate;
        *lock.lock().unwrap() = true;
        condition.notify_all();
    }
    settle(&mut server);
    assert_eq!(server.authority().unwrap().cursor(), before);
    assert!(
        server
            .authority()
            .unwrap()
            .world()
            .store()
            .section(fresh)
            .is_none()
    );
    server
        .set_residency_interest(&[SectionPos::ZERO, SectionPos::new(1, 0, 0), fresh])
        .unwrap();
    settle(&mut server);
    assert!(
        server
            .authority()
            .unwrap()
            .world()
            .store()
            .section(fresh)
            .is_some()
    );
    drop(server);
    let lease = WorldLease::inspect(&dir.0, base::WORLD, 8192).unwrap();
    assert!(identity().verify(lease.root()).unwrap());
    let original = std::fs::read(dir.0.join("generation.bin")).unwrap();
    for changed in [
        GenerationIdentity {
            version: 2,
            ..identity()
        },
        GenerationIdentity {
            configuration: Digest([8; 32]),
            ..identity()
        },
        GenerationIdentity {
            id: base::key("test:other"),
            ..identity()
        },
    ] {
        assert!(changed.verify(lease.root()).is_err());
        assert_eq!(
            std::fs::read(dir.0.join("generation.bin")).unwrap(),
            original
        );
    }
    drop(lease);
    let mut server = open(&dir, pages.clone(), Vec::new());
    let source = Arc::new(EmptyGenerator {
        identity: identity(),
        gate: None,
    });
    server
        .enable_residency(config(), pages, Some(source), &[fresh])
        .unwrap();
    settle(&mut server);
    assert!(
        server
            .authority()
            .unwrap()
            .world()
            .store()
            .section(fresh)
            .is_some()
    );
}
