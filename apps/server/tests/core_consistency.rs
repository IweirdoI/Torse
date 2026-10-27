//! Durable execution of policy-filtered components and automatic block instances.
mod support;

use support::Directory;
use torse_content::{
    ComponentField, ComponentFieldType, ComponentSchema, PersistencePolicy, compile,
    m0_fixture_pack,
};
use torse_server::{bootstrap as base, execution::*, *};
use torse_sim::action::{ActionService, ActionTarget, ActorContext, register_action_module};
use torse_sim::durable::*;
use torse_sim::{OrderingKey, Phase, SystemSpec, Transaction, WorldAuthority, block_actions};
use torse_types::*;
use torse_world::*;

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).unwrap()
}
fn fields(value: u32) -> ComponentFields {
    [(key("test:value"), ComponentValue::U32(value))].into()
}
fn world() -> (WorldAuthority, ActionService) {
    world_config(7, false)
}
fn world_config(value: u32, persist: bool) -> (WorldAuthority, ActionService) {
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
        replication: ReplicationDeclaration::SERVER_PRIVATE,
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
    pack.blocks
        .iter_mut()
        .find(|b| b.value.id == key("base:glass"))
        .unwrap()
        .value
        .component_defaults = ["test:saved", "test:cache", "test:optional"]
        .map(key)
        .into();
    let content = compile(pack).unwrap();
    let (schemas, compatibility) = ComponentSchemas::assemble(
        &content,
        ComponentConfiguration {
            persistence: vec![PersistenceSelection {
                owner: key("test:module"),
                schema: key("test:optional"),
                persist,
            }],
            block_defaults: ["test:saved", "test:cache", "test:optional"]
                .map(|schema| BlockDefault {
                    owner: key("test:module"),
                    block: key("base:glass"),
                    schema: key(schema),
                    fields: fields(value),
                })
                .into(),
        },
        Compatibility {
            authority_schema: 1,
            rules_digest: content.authority_digest,
            content_digest: content.presentation_digest,
        },
    )
    .unwrap();
    let service = ActionService::bind(
        block_actions::descriptors(),
        block_actions::handlers(),
        &schemas,
    )
    .unwrap();
    let mut world = WorldAuthority::new(
        base::WORLD,
        BlockStateRegistry::build(&content).unwrap(),
        schemas.clone(),
        16,
        WorldBounds::new(0, 32).unwrap(),
        compatibility,
    )
    .unwrap();
    world.store_mut().request(SectionPos::ZERO).unwrap();
    world
        .store_mut()
        .install(SectionPos::ZERO, Section::empty(16).unwrap())
        .unwrap();
    world.store_mut().finish_load(SectionPos::ZERO).unwrap();
    let handle = world
        .entities_mut()
        .spawn(Position::new(8.5, 8.5, 8.5))
        .unwrap();
    for schema in ["test:saved", "test:cache", "test:optional"] {
        world
            .entities_mut()
            .components_mut(handle)
            .unwrap()
            .insert(&schemas, &key(schema), fields(7))
            .unwrap();
    }
    (world, service)
}

struct Update;
impl DurableRule for Update {
    fn spec(&self) -> SystemSpec {
        SystemSpec {
            id: key("test:update"),
            phase: Phase::Simulation,
            rule_priority: 0,
            before: vec![],
            after: vec![],
        }
    }
    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        if tick != Tick(0) {
            return Ok(vec![]);
        }
        let entity = world
            .entities()
            .get(world.entities().handle_of(EntityId(1)).unwrap())
            .unwrap();
        let mut tx = Transaction::new(
            TransactionId(100),
            key("test:module"),
            OrderingKey {
                phase: Phase::Simulation,
                rule_priority: 0,
                stable_task_key: 1,
                local_sequence: 0,
            },
        );
        for schema in ["test:saved", "test:cache", "test:optional"] {
            tx.components.push(ComponentEdit {
                id: entity.id,
                schema: key(schema),
                previous: entity.components.component(&key(schema)).cloned(),
                fields: Some(fields(8)),
            });
        }
        // One task has only runtime state. Its durable world projection is empty
        // while its committed outcome and sequence must still be replayable.
        let mut runtime_only = tx.clone();
        runtime_only.id = TransactionId(101);
        runtime_only.ordering.stable_task_key = 2;
        runtime_only
            .components
            .retain(|e| e.schema != key("test:saved"));
        tx.components.retain(|e| e.schema == key("test:saved"));
        Ok(vec![
            WorkPlan::transaction(tx),
            WorkPlan::transaction(runtime_only),
        ])
    }
}
fn rules() -> Vec<Box<dyn DurableRule>> {
    vec![Box::new(Update)]
}
fn create(directory: WorldDirectory) -> base::Files {
    create_with(directory, rules())
}
fn create_with(directory: WorldDirectory, registered: Vec<Box<dyn DurableRule>>) -> base::Files {
    let (world, service) = world();
    base::Files::create_file(
        directory,
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| {
            ExecutionHost::new(
                binding,
                world,
                service,
                registered,
                base::execution_limits(),
            )
        },
    )
    .unwrap()
}
fn reopen(dir: &Directory) -> base::Files {
    reopen_with(dir, rules())
}
fn reopen_with(dir: &Directory, registered: Vec<Box<dyn DurableRule>>) -> base::Files {
    let (world, service) = world();
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
                service,
                registered,
                base::execution_limits(),
            ))
        },
    )
    .unwrap()
}
fn pos() -> BlockPos {
    BlockPos::new(3, 3, 3)
}
fn submit(server: &mut base::Files, place: bool, seq: u64) -> RequestId {
    let viewer = Viewer {
        session: SessionId(1),
        actor: ActorContextId(1),
    };
    server
        .register_session(
            viewer,
            ActorContext::new(key("test:editor"))
                .permission(key("world:permission/edit"))
                .section(SectionPos::ZERO),
            torse_sim::replica::ReplicaScope {
                sections: [SectionPos::ZERO].into(),
            },
        )
        .unwrap();
    let id = RequestId {
        world: base::WORLD,
        epoch: server.binding().epoch,
        session: SessionId(1),
        seq: RequestSeq(seq),
    };
    let call = Call {
        action: key(if place {
            "world:action/place"
        } else {
            "world:action/break"
        }),
        version: 1,
        target: ActionTarget::Block(pos()),
        parameters: if place {
            [(
                key("world:field/block"),
                ComponentValue::Content(key("base:glass")),
            )]
            .into()
        } else {
            ComponentFields::new()
        },
    };
    assert!(matches!(
        server.submit(WorldRequest {
            id,
            actor: ActorContextId(1),
            input: call
        }),
        Admission::Queued { .. }
    ));
    id
}
fn assert_restored(server: &base::Files, instance: bool) {
    let world = server.authority().unwrap().world();
    let ordinary = world
        .entities()
        .get(world.entities().handle_of(EntityId(1)).unwrap())
        .unwrap();
    assert_eq!(
        ordinary.components.component(&key("test:saved")),
        Some(&fields(8))
    );
    assert!(ordinary.components.component(&key("test:cache")).is_none());
    assert!(
        ordinary
            .components
            .component(&key("test:optional"))
            .is_none()
    );
    let attached = world.entities().block_instance_at(pos()).unwrap();
    assert_eq!(attached.is_some(), instance);
    if let Some((_, attached)) = attached {
        assert_eq!(attached.id, EntityId(2));
        assert_eq!(
            attached.components.component(&key("test:saved")),
            Some(&fields(7))
        );
        assert!(attached.components.component(&key("test:cache")).is_none());
        assert!(
            attached
                .components
                .component(&key("test:optional"))
                .is_none()
        );
    }
    assert_eq!(world.entities().next_entity_id(), 3);
}

#[test]
fn execution_journal_checkpoint_and_tail_use_the_same_persistent_projection() {
    let dir = Directory::new();
    let mut server = create(dir.acquire());
    let id = submit(&mut server, true, 1);
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    assert_eq!(
        server.receipt(id).unwrap().unwrap().reply.status,
        ReplyStatus::Committed
    );
    let live = server.authority().unwrap().world();
    assert_eq!(
        live.entities()
            .block_instance_at(pos())
            .unwrap()
            .unwrap()
            .1
            .components
            .component(&key("test:cache")),
        Some(&fields(7))
    );
    drop(server);
    let mut server = reopen(&dir);
    assert_restored(&server, true);
    server.checkpoint().unwrap();
    let id = submit(&mut server, false, 1);
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    assert_eq!(
        server.receipt(id).unwrap().unwrap().reply.status,
        ReplyStatus::Committed
    );
    drop(server);
    let server = reopen(&dir);
    assert_restored(&server, false);
}

#[test]
fn changed_startup_defaults_or_save_policy_reject_reopen_without_rewriting_data() {
    fn bytes(dir: &Directory) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut result = std::collections::BTreeMap::new();
        let journal = dir.0.join("journal.log");
        result.insert(journal.clone(), std::fs::read(journal).unwrap());
        for file in std::fs::read_dir(dir.0.join("checkpoints")).unwrap() {
            let path = file.unwrap().path();
            result.insert(path.clone(), std::fs::read(path).unwrap());
        }
        result
    }
    let dir = Directory::new();
    let mut server = create(dir.acquire());
    let id = submit(&mut server, true, 1);
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    assert_eq!(
        server.receipt(id).unwrap().unwrap().reply.status,
        ReplyStatus::Committed
    );
    drop(server);
    let before = bytes(&dir);
    for (value, persist) in [(8, false), (7, true)] {
        let (world, service) = world_config(value, persist);
        let result = base::Files::open_file(
            dir.acquire(),
            world.coordinator().compatibility(),
            base::Codec::new(base::codec_limits()).unwrap(),
            base::file_policy(),
            |binding| {
                Ok(ExecutionRecovery::new(
                    binding,
                    world.registry(),
                    world.schemas(),
                    service,
                    rules(),
                    base::execution_limits(),
                ))
            },
        );
        assert!(result.is_err());
        assert_eq!(bytes(&dir), before);
    }
    let mut server = reopen(&dir);
    assert_restored(&server, true);
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    drop(server);
    assert_restored(&reopen(&dir), true);
}

#[test]
fn instance_crash_child() {
    let Ok(path) = std::env::var("TORSE_INSTANCE_CRASH_PATH") else {
        return;
    };
    let mode = std::env::var("TORSE_INSTANCE_CRASH_MODE").unwrap();
    let registered: Vec<Box<dyn DurableRule>> = if mode == "arrival-frozen" {
        vec![Box::new(Arrive)]
    } else {
        rules()
    };
    let mut server = create_with(
        WorldDirectory::acquire(path, base::WORLD, 8192).unwrap(),
        registered,
    );
    let id = submit(&mut server, true, 1);
    for _ in 0..64 {
        server.drive_one(&mut NoReplicas).unwrap();
        let host = server.authority().unwrap();
        let frozen = host.resume_point().next_phase == Phase::Interaction.index_u16()
            && host.runtime().work.as_ref().is_some_and(|w| w.next == 0);
        if mode == "arrival-frozen" && frozen {
            assert_eq!(host.runtime().work.as_ref().unwrap().tasks.len(), 2);
            std::process::exit(76)
        }
        if mode == "frozen" && frozen {
            std::process::exit(74)
        }
        if mode == "durable" && server.receipt(id).unwrap().is_some() {
            std::process::exit(75)
        }
    }
    panic!("instance crash point not reached");
}

struct Arrive;
impl DurableRule for Arrive {
    fn spec(&self) -> SystemSpec {
        SystemSpec {
            id: key("test:arrival"),
            phase: Phase::Interaction,
            rule_priority: -1,
            before: vec![],
            after: vec![],
        }
    }
    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        if tick != Tick(0) {
            return Ok(vec![]);
        }
        let handle = world.entities().handle_of(EntityId(1)).unwrap();
        let entity = world.entities().get(handle).unwrap();
        let mut tx = Transaction::new(
            TransactionId(200),
            key("test:module"),
            OrderingKey {
                phase: Phase::Interaction,
                rule_priority: -1,
                stable_task_key: 0,
                local_sequence: 0,
            },
        );
        tx.move_entity(
            handle,
            entity.id,
            entity.position,
            [-5.0; 3],
            [0.3, 0.9, 0.3],
        );
        Ok(vec![WorkPlan::transaction(tx)])
    }
}

#[test]
fn frozen_placement_rechecks_an_earlier_arrival_after_process_exit() {
    let dir = Directory::new();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "instance_crash_child", "--nocapture"])
        .env("TORSE_INSTANCE_CRASH_PATH", &dir.0)
        .env("TORSE_INSTANCE_CRASH_MODE", "arrival-frozen")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(76));
    let mut server = reopen_with(&dir, vec![Box::new(Arrive)]);
    let tasks = &server
        .authority()
        .unwrap()
        .runtime()
        .work
        .as_ref()
        .unwrap()
        .tasks;
    assert!(tasks.iter().any(|t| {
        t.transaction
            .as_ref()
            .is_some_and(|g| !g.members[0].clearance.is_empty())
    }));
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    let id = RequestId {
        world: base::WORLD,
        epoch: WorldEpoch(1),
        session: SessionId(1),
        seq: RequestSeq(1),
    };
    for _ in 0..2 {
        assert_eq!(
            server.receipt(id).unwrap().unwrap().reply.status,
            ReplyStatus::Rejected(RejectCode::Conflict)
        );
        let world = server.authority().unwrap().world();
        assert_eq!(
            world.store().query(pos()),
            TerrainQuery::Ready(BlockStateId::EMPTY)
        );
        assert_eq!(world.entities().len(), 1);
        assert_eq!(world.entities().next_entity_id(), 2);
        let entity = world
            .entities()
            .get(world.entities().handle_of(EntityId(1)).unwrap())
            .unwrap();
        assert_eq!(entity.position, Position::new(3.5, 3.5, 3.5));
        drop(server);
        server = reopen_with(&dir, vec![Box::new(Arrive)]);
    }
}

#[test]
fn process_exit_before_instance_execution_and_after_durability_never_reuses_identity() {
    for mode in ["frozen", "durable"] {
        let dir = Directory::new();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "instance_crash_child", "--nocapture"])
            .env("TORSE_INSTANCE_CRASH_PATH", &dir.0)
            .env("TORSE_INSTANCE_CRASH_MODE", mode)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(if mode == "frozen" { 74 } else { 75 }));
        let mut server = reopen(&dir);
        server.drive_tick(&mut NoReplicas, 1024).unwrap();
        let id = RequestId {
            world: base::WORLD,
            epoch: WorldEpoch(1),
            session: SessionId(1),
            seq: RequestSeq(1),
        };
        assert_eq!(
            server.receipt(id).unwrap().unwrap().reply.status,
            ReplyStatus::Committed
        );
        drop(server);
        let server = reopen(&dir);
        assert_restored(&server, true);
    }
}
