//! Declared systems bind, arbitrate, enforce output budgets and resume frozen work.
mod support;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use support::Directory;
use torse_content::{
    CompiledContent, ModuleDescriptor, ModuleSide, SourceRef, Sourced, SystemDescriptor, compile,
    m0_fixture_pack,
};
use torse_server::{bootstrap as base, execution::*, systems::*, *};
use torse_sim::action::ActionService;
use torse_sim::durable::{EventDraft, TimedEvent};
use torse_sim::{OrderingKey, Phase, SystemSpec, Transaction, WorldAuthority};
use torse_types::*;
use torse_world::*;

fn key(s: &str) -> ContentKey {
    ContentKey::parse(s).unwrap()
}
fn module(name: &str, dependencies: &[&str]) -> ModuleDescriptor {
    ModuleDescriptor {
        id: key(&format!("{name}:module")),
        module_version: 1,
        framework_api: 1,
        side: ModuleSide::Server,
        required_modules: dependencies.iter().map(|d| key(d)).collect(),
        optional_modules: vec![],
        provided_services: vec![],
        required_services: vec![],
        registered_schemas: vec![],
        registered_capabilities: vec![],
        registered_actions: vec![],
        registered_systems: vec![SystemDescriptor {
            id: key(&format!("{name}:system")),
            phase: 3,
            max_items: 4096,
            max_bytes: 262144,
        }],
        requested_permissions: vec![],
        configuration_schema: None,
        lifecycle_binding: None,
    }
}
fn content() -> CompiledContent {
    content_with_budget(4096)
}
fn content_with_budget(items: u32) -> CompiledContent {
    let mut pack = m0_fixture_pack();
    // The dependant sorts first by identity and requests higher callback priority.
    // The sealed module dependency must still make the provider win arbitration.
    for mut m in [module("z", &[]), module("a", &["z:module"])] {
        if m.id == key("a:module") {
            m.registered_systems[0].max_items = items;
        }
        pack.modules
            .push(Sourced::new(SourceRef::new("declared-systems", 1), m));
    }
    compile(pack).unwrap()
}
fn world(content: &CompiledContent) -> (WorldAuthority, ActionService) {
    let schemas = ComponentSchemas::build(content);
    let actions = ActionService::bind(vec![], vec![], &schemas).unwrap();
    let mut world = WorldAuthority::new(
        base::WORLD,
        BlockStateRegistry::build(content).unwrap(),
        schemas,
        16,
        WorldBounds::new(0, 32).unwrap(),
        Compatibility {
            authority_schema: 1,
            rules_digest: content.authority_digest,
            content_digest: content.authority_digest,
        },
    )
    .unwrap();
    world.store_mut().request(SectionPos::ZERO).unwrap();
    world
        .store_mut()
        .install(SectionPos::ZERO, Section::empty(16).unwrap())
        .unwrap();
    world.store_mut().finish_load(SectionPos::ZERO).unwrap();
    (world, actions)
}

#[derive(Clone, Copy)]
enum Mode {
    Write,
    Idle,
    TooMany,
    Large,
    Foreign,
    WrongPhase,
    Event,
}
struct Rule {
    name: &'static str,
    spec: SystemSpec,
    mode: Mode,
    calls: Arc<AtomicUsize>,
}
impl Rule {
    fn new(name: &'static str, mode: Mode, calls: Arc<AtomicUsize>) -> Self {
        Self {
            name,
            spec: SystemSpec {
                id: key(&format!("{name}:system")),
                phase: Phase::Simulation,
                rule_priority: if name == "a" { -100 } else { 100 },
                before: vec![],
                after: vec![],
            },
            mode,
            calls,
        }
    }
    fn write(&self, world: &WorldAuthority) -> WorkPlan {
        let mut tx = Transaction::new(
            TransactionId(if self.name == "a" { 1 } else { 2 }),
            key(&format!(
                "{}:module",
                if matches!(self.mode, Mode::Foreign) {
                    "other"
                } else {
                    self.name
                }
            )),
            OrderingKey {
                phase: if matches!(self.mode, Mode::WrongPhase) {
                    Phase::Input
                } else {
                    self.spec.phase
                },
                rule_priority: self.spec.rule_priority,
                stable_task_key: if self.name == "a" { 0 } else { 99 },
                local_sequence: 0,
            },
        );
        let pos = BlockPos::new(1, 1, 1);
        tx.read_block(pos, BlockStateId::EMPTY).write_block(
            pos,
            world
                .registry()
                .default_state(&key(if self.name == "a" {
                    "base:glass"
                } else {
                    "base:stone"
                }))
                .unwrap(),
        );
        WorkPlan::transaction(tx)
    }
    fn draft(&self) -> EventDraft {
        EventDraft {
            tick: Tick(1),
            rule: self.spec.id.clone(),
            payload: if matches!(self.mode, Mode::Large) {
                [(key("test:text"), ComponentValue::Text("x".repeat(1024)))].into()
            } else {
                Default::default()
            },
        }
    }
}
impl DurableRule for Rule {
    fn spec(&self) -> SystemSpec {
        self.spec.clone()
    }
    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if tick != Tick(0) {
            return Ok(vec![]);
        }
        Ok(match self.mode {
            Mode::Idle => vec![],
            Mode::TooMany => (0..3)
                .map(|_| WorkPlan {
                    transaction: None,
                    schedule: vec![],
                    cancel: vec![],
                })
                .collect(),
            Mode::Large | Mode::Event => vec![WorkPlan {
                transaction: None,
                schedule: vec![self.draft()],
                cancel: vec![],
            }],
            _ => vec![self.write(world)],
        })
    }
    fn event(&self, world: &WorldAuthority, _: Tick, _: &TimedEvent) -> Result<WorkPlan, Fault> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(self.mode, Mode::Large) {
            Ok(WorkPlan {
                transaction: None,
                schedule: vec![self.draft()],
                cancel: vec![],
            })
        } else {
            Ok(self.write(world))
        }
    }
    fn action_events(
        &self,
        _: &WorldAuthority,
        _: Tick,
        _: &Transaction,
    ) -> Result<Vec<EventDraft>, Fault> {
        Ok(vec![self.draft()])
    }
}
fn binding(name: &'static str, mode: Mode, calls: &Arc<AtomicUsize>) -> SystemBinding {
    SystemBinding {
        module: key(&format!("{name}:module")),
        rule: Box::new(Rule::new(name, mode, calls.clone())),
    }
}
fn bindings(calls: &Arc<AtomicUsize>) -> Vec<SystemBinding> {
    vec![
        binding("a", Mode::Write, calls),
        binding("z", Mode::Write, calls),
    ]
}

#[test]
fn dependency_order_is_frozen_into_transaction_arbitration_independent_of_binding_order() {
    let content = content();
    let (world, _) = world(&content);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut reference = vec![];
    for reverse in [false, true] {
        let mut provided = bindings(&calls);
        if reverse {
            provided.reverse();
        }
        let rules = bind_module_systems(&content, provided, base::execution_limits()).unwrap();
        let specs: Vec<_> = rules.iter().map(|r| r.spec()).collect();
        if reverse {
            assert_eq!(specs, reference)
        } else {
            reference = specs
        }
        let mut transactions: Vec<_> = rules
            .iter()
            .flat_map(|r| r.plan(&world, Tick(0)).unwrap())
            .map(|p| p.transaction.unwrap().members.remove(0))
            .collect();
        transactions = Transaction::order(transactions);
        assert_eq!(transactions[0].source, key("z:module"));
        assert_eq!(transactions[1].source, key("a:module"));
    }
}

#[test]
fn missing_duplicate_foreign_and_phase_mismatched_bindings_fail_before_callbacks() {
    let content = content();
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = vec![binding("a", Mode::Idle, &calls)];
    assert!(bind_module_systems(&content, bad, base::execution_limits()).is_err());
    let mut bad = bindings(&calls);
    bad.push(binding("a", Mode::Idle, &calls));
    assert!(bind_module_systems(&content, bad, base::execution_limits()).is_err());
    let mut bad = bindings(&calls);
    bad[0].module = key("z:module");
    assert!(bind_module_systems(&content, bad, base::execution_limits()).is_err());
    let mut bad = bindings(&calls);
    let mut rule = Rule::new("a", Mode::Idle, calls.clone());
    rule.spec.phase = Phase::Input;
    bad[0].rule = Box::new(rule);
    assert!(bind_module_systems(&content, bad, base::execution_limits()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_dependencies_duplicate_systems_invalid_budgets_and_client_dependencies_reject() {
    let calls = Arc::new(AtomicUsize::new(0));
    for mode in 0..5 {
        let mut content = content();
        match mode {
            0 => {
                content
                    .modules
                    .get_mut(&key("a:module"))
                    .unwrap()
                    .required_modules = vec![key("missing:module")]
            }
            1 => {
                content
                    .modules
                    .get_mut(&key("z:module"))
                    .unwrap()
                    .registered_systems[0]
                    .id = key("a:system")
            }
            2 => {
                content
                    .modules
                    .get_mut(&key("a:module"))
                    .unwrap()
                    .registered_systems[0]
                    .max_items = 0
            }
            3 => {
                content
                    .modules
                    .get_mut(&key("a:module"))
                    .unwrap()
                    .registered_systems[0]
                    .phase = 6
            }
            _ => content.modules.get_mut(&key("z:module")).unwrap().side = ModuleSide::Client,
        }
        assert!(bind_module_systems(&content, bindings(&calls), base::execution_limits()).is_err());
    }
    let mut content = content();
    content.modules.get_mut(&key("a:module")).unwrap().side = ModuleSide::Client;
    // Unused client declarations must not force server startup to resolve
    // client-only dependencies, handlers or budgets.
    let client = content.modules.get_mut(&key("a:module")).unwrap();
    client.required_modules = vec![key("client:unavailable")];
    client.registered_systems[0].phase = 255;
    client.registered_systems[0].max_items = 0;
    let rules = bind_module_systems(
        &content,
        vec![binding("z", Mode::Idle, &calls)],
        base::execution_limits(),
    )
    .unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn system_order_cycles_and_cross_phase_edges_reject_during_assembly() {
    let content = content();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut provided = bindings(&calls);
    let mut contrary = Rule::new("z", Mode::Idle, calls.clone());
    contrary.spec.after.push(key("a:system"));
    provided[1].rule = Box::new(contrary);
    assert!(bind_module_systems(&content, provided, base::execution_limits()).is_err());
    let mut provided = bindings(&calls);
    let mut unknown = Rule::new("z", Mode::Idle, calls.clone());
    unknown.spec.after.push(key("missing:system"));
    provided[1].rule = Box::new(unknown);
    assert!(bind_module_systems(&content, provided, base::execution_limits()).is_err());
    let mut content = content.clone();
    content
        .modules
        .get_mut(&key("z:module"))
        .unwrap()
        .registered_systems[0]
        .phase = 0;
    let mut provided = bindings(&calls);
    let mut provider = Rule::new("z", Mode::Idle, calls.clone());
    provider.spec.phase = Phase::Input;
    provided[1].rule = Box::new(provider);
    let mut consumer = Rule::new("a", Mode::Idle, calls.clone());
    consumer.spec.after.push(key("z:system"));
    provided[0].rule = Box::new(consumer);
    assert!(bind_module_systems(&content, provided, base::execution_limits()).is_err());
}

#[test]
fn module_order_includes_transitive_dependencies_without_systems() {
    let mut content = content();
    let mut bridge = module("bridge", &["z:module"]);
    bridge.registered_systems.clear();
    content.modules.insert(bridge.id.clone(), bridge);
    content
        .modules
        .get_mut(&key("a:module"))
        .unwrap()
        .required_modules = vec![key("bridge:module")];
    let calls = Arc::new(AtomicUsize::new(0));
    let rules = bind_module_systems(&content, bindings(&calls), base::execution_limits()).unwrap();
    let consumer = rules
        .iter()
        .find(|r| r.spec().id == key("a:system"))
        .unwrap()
        .spec();
    assert!(consumer.after.contains(&key("z:system")));
}

#[test]
fn declared_output_limits_apply_to_periodic_event_and_action_callbacks() {
    let calls = Arc::new(AtomicUsize::new(0));
    for mode in [Mode::TooMany, Mode::Large, Mode::Foreign, Mode::WrongPhase] {
        let mut content = content();
        if matches!(mode, Mode::TooMany) {
            content
                .modules
                .get_mut(&key("a:module"))
                .unwrap()
                .registered_systems[0]
                .max_items = 2;
        }
        if matches!(mode, Mode::Large) {
            content
                .modules
                .get_mut(&key("a:module"))
                .unwrap()
                .registered_systems[0]
                .max_bytes = 128;
        }
        let (world, _) = world(&content);
        let rules = bind_module_systems(
            &content,
            vec![binding("a", mode, &calls), binding("z", Mode::Idle, &calls)],
            base::execution_limits(),
        )
        .unwrap();
        let rule = &rules[0];
        assert!(rule.plan(&world, Tick(0)).is_err());
        if matches!(mode, Mode::Large) {
            let event = TimedEvent {
                id: EventId(1),
                event: EventDraft {
                    tick: Tick(1),
                    rule: key("a:system"),
                    payload: Default::default(),
                },
            };
            assert!(rule.event(&world, Tick(1), &event).is_err());
            let tx = Transaction::new(
                TransactionId(1),
                key("a:module"),
                OrderingKey {
                    phase: Phase::Simulation,
                    rule_priority: 0,
                    stable_task_key: 0,
                    local_sequence: 0,
                },
            );
            assert!(rule.action_events(&world, Tick(0), &tx).is_err());
        }
        assert_eq!(world.seq(), CommitSeq(0));
        assert_eq!(
            world.store().query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId::EMPTY)
        );
    }
}

fn create(
    dir: &Directory,
    content: &CompiledContent,
    calls: &Arc<AtomicUsize>,
    events: bool,
) -> base::Files {
    let (world, actions) = world(content);
    let provided = if events {
        vec![
            binding("a", Mode::Event, calls),
            binding("z", Mode::Idle, calls),
        ]
    } else {
        bindings(calls)
    };
    base::Files::create_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |identity| {
            ExecutionHost::with_modules(
                identity,
                world,
                actions,
                content,
                provided,
                base::execution_limits(),
            )
        },
    )
    .unwrap()
}
fn reopen(
    dir: &Directory,
    content: &CompiledContent,
    calls: &Arc<AtomicUsize>,
    events: bool,
) -> Result<base::Files, Fault> {
    let (world, actions) = world(content);
    let provided = if events {
        vec![
            binding("a", Mode::Event, calls),
            binding("z", Mode::Idle, calls),
        ]
    } else {
        bindings(calls)
    };
    base::Files::open_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits())?,
        base::file_policy(),
        |identity| {
            ExecutionRecovery::with_modules(
                identity,
                world.registry(),
                world.schemas(),
                actions,
                content,
                provided,
                base::execution_limits(),
            )
        },
    )
}

#[test]
fn declared_periodic_and_timed_work_commit_and_reopen_without_replaying_callbacks() {
    let content = content();
    let dir = Directory::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = create(&dir, &content, &calls, true);
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    let count = calls.load(Ordering::SeqCst);
    assert!(count >= 5);
    let seq = server.authority().unwrap().world().seq();
    drop(server);
    let server = reopen(&dir, &content, &calls, true).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), count);
    let world = server.authority().unwrap().world();
    assert_eq!(world.seq(), seq);
    assert_eq!(
        world
            .registry()
            .definition_key(world.store().query(BlockPos::new(1, 1, 1)).ready().unwrap()),
        Some(&key("base:glass"))
    );
}

#[test]
fn changed_system_budget_changes_compatibility_and_reopen_preserves_the_original_world() {
    let content = content();
    let dir = Directory::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = create(&dir, &content, &calls, false);
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    let count = calls.load(Ordering::SeqCst);
    drop(server);
    let changed = content_with_budget(2048);
    assert_ne!(changed.authority_digest, content.authority_digest);
    assert!(reopen(&dir, &changed, &calls, false).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), count);
    let server = reopen(&dir, &content, &calls, false).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), count);
    assert_eq!(server.authority().unwrap().resume_point().tick, Tick(1));
}

#[test]
fn declared_system_crash_child() {
    let Ok(path) = std::env::var("TORSE_DECLARED_SYSTEM_CRASH_PATH") else {
        return;
    };
    let dir = Directory(std::path::PathBuf::from(path));
    let content = content();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = create(&dir, &content, &calls, false);
    for _ in 0..64 {
        server.drive_one(&mut NoReplicas).unwrap();
        let host = server.authority().unwrap();
        if host.resume_point().next_phase == Phase::Simulation.index_u16()
            && host.runtime().work.as_ref().is_some_and(|w| w.next == 0)
        {
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            std::process::exit(77);
        }
    }
    panic!("declared system crash point not reached");
}

#[test]
fn frozen_declared_systems_resume_after_process_exit_in_dependency_order() {
    let dir = Directory::new();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "declared_system_crash_child", "--nocapture"])
        .env("TORSE_DECLARED_SYSTEM_CRASH_PATH", &dir.0)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    let content = content();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = reopen(&dir, &content, &calls, false).unwrap();
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(server);
    let server = reopen(&dir, &content, &calls, false).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let world = server.authority().unwrap().world();
    assert_eq!(
        world
            .registry()
            .definition_key(world.store().query(BlockPos::new(1, 1, 1)).ready().unwrap()),
        Some(&key("base:stone"))
    );
}
