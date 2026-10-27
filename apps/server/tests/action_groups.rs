//! Composed owner-authored actions through admission, frozen work and recovery.
mod support;
use support::Directory;
use torse_content::{SourceRef, Sourced, compile, m0_fixture_pack};
use torse_inventory::{Inventory, InventoryOperation, ItemCatalog, ItemStack, plan_inventory};
use torse_server::{bootstrap as base, execution::*, *};
use torse_sim::action::*;
use torse_sim::durable::*;
use torse_sim::{Phase, Transaction, TransactionGroup, WorldAuthority, block_actions};
use torse_types::*;
use torse_world::*;

fn key(value: &str) -> ContentKey {
    base::key(value)
}
const POS: BlockPos = BlockPos::new(8, 1, 1);

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Forged,
    Recursive,
    DuplicateOwner,
    Query,
    SmallBudget,
    SmallItems,
}

struct Compose(Mode);
impl ActionHandler for Compose {
    fn plan(
        &self,
        _: &ActionView<'_>,
        request: &ActionRequest,
    ) -> Result<ActionOutput, RejectCode> {
        if matches!(self.0, Mode::Forged) {
            let tx = Transaction::new(
                request.transaction,
                torse_inventory::owner(),
                request.ordering,
            );
            return Ok(ActionOutput::Atomic(Box::new(TransactionGroup::new(
                request.transaction,
                request.ordering,
                vec![tx],
            )?)));
        }
        let place = ActionInvocation {
            action: key("world:action/place"),
            api_version: 1,
            target: request.target,
            parameters: request.parameters.clone(),
        };
        let consume = ActionInvocation {
            action: key("inventory:action/consume"),
            api_version: 1,
            target: ActionTarget::Entity(EntityId(1)),
            parameters: ComponentFields::new(),
        };
        let invocations = match self.0 {
            Mode::Recursive => vec![ActionInvocation {
                action: request.action.clone(),
                api_version: 1,
                target: request.target,
                parameters: request.parameters.clone(),
            }],
            Mode::DuplicateOwner => vec![consume.clone(), consume],
            Mode::Query => vec![ActionInvocation {
                action: key("inventory:action/inspect"),
                ..consume
            }],
            _ => vec![consume, place],
        };
        Ok(ActionOutput::Invoke(invocations))
    }
}
struct Consume(ItemCatalog);
impl ActionHandler for Consume {
    fn plan(
        &self,
        view: &ActionView<'_>,
        request: &ActionRequest,
    ) -> Result<ActionOutput, RejectCode> {
        let ActionTarget::Entity(target) = request.target else {
            return Err(RejectCode::InvalidData);
        };
        let expected = [(
            target,
            view.component(target, &torse_inventory::schema())?.clone(),
        )]
        .into();
        let plan = plan_inventory(
            view,
            &self.0,
            &[InventoryOperation::Consume {
                target,
                slot: 0,
                stack: ItemStack {
                    item: key("base:stone"),
                    count: 1,
                },
            }],
            &expected,
            request.transaction,
            request.ordering,
        )?;
        Ok(ActionOutput::Mutation(Box::new(plan.into_parts().0)))
    }
}

fn world(mode: Mode) -> (WorldAuthority, ActionService) {
    let mut pack = m0_fixture_pack();
    block_actions::register_content(&mut pack);
    torse_inventory::register_content(&mut pack);
    let mut consume = torse_inventory::descriptors()[1].clone();
    consume.id = key("inventory:action/consume");
    consume.handler.handler = key("inventory:handler/consume");
    consume.parameter_schema = key("world:schema/result");
    consume.permission.permission = Some(key("inventory:permission/consume"));
    consume.commands.clear();
    let mut compose = block_actions::descriptors()[0].clone();
    compose.id = key("craft:action/place");
    compose.handler.handler = key("craft:handler/place");
    compose.handler.owner_module = key("craft:module");
    compose.commands.clear();
    compose.max_items = if matches!(mode, Mode::SmallItems) {
        1
    } else {
        64
    };
    compose.max_bytes = if matches!(mode, Mode::SmallBudget) {
        1
    } else {
        65536
    };
    register_action_module(
        &mut pack,
        key("craft:module"),
        key("craft:state"),
        Vec::new(),
        vec![compose.clone()],
        Vec::new(),
    );
    pack.modules
        .iter_mut()
        .find(|m| m.value.id == torse_inventory::owner())
        .unwrap()
        .value
        .registered_actions
        .push(consume.id.clone());
    pack.actions.push(Sourced::new(
        SourceRef::new("native/test", 0),
        consume.clone(),
    ));
    let content = compile(pack).unwrap();
    let (schemas, compatibility) = ComponentSchemas::assemble(
        &content,
        ComponentConfiguration::default(),
        Compatibility {
            authority_schema: 4,
            rules_digest: content.authority_digest,
            content_digest: content.presentation_digest,
        },
    )
    .unwrap();
    let catalog = ItemCatalog::new([(key("base:stone"), 64)]).unwrap();
    let mut descriptors = block_actions::descriptors();
    descriptors.extend(torse_inventory::descriptors());
    descriptors.extend([consume, compose]);
    let mut handlers = block_actions::handlers();
    handlers.extend(torse_inventory::handlers(catalog.clone()));
    handlers.extend([
        NativeHandler {
            handler: key("inventory:handler/consume"),
            owner: torse_inventory::owner(),
            implementation: Box::new(Consume(catalog.clone())),
        },
        NativeHandler {
            handler: key("craft:handler/place"),
            owner: key("craft:module"),
            implementation: Box::new(Compose(mode)),
        },
    ]);
    let service = ActionService::bind(descriptors, handlers, &schemas).unwrap();
    let mut world = WorldAuthority::new(
        base::WORLD,
        BlockStateRegistry::build(&content).unwrap(),
        schemas.clone(),
        16,
        WorldBounds::new(0, 32).unwrap(),
        compatibility,
    )
    .unwrap();
    for section in base::scope().sections {
        world.store_mut().request(section).unwrap();
        world
            .store_mut()
            .install(section, Section::empty(16).unwrap())
            .unwrap();
        world.store_mut().finish_load(section).unwrap();
    }
    let handle = world
        .entities_mut()
        .spawn(Position::new(2.0, 2.0, 2.0))
        .unwrap();
    let mut inventory = Inventory::new(1).unwrap();
    inventory
        .set(
            0,
            Some(ItemStack {
                item: key("base:stone"),
                count: 1,
            }),
            &catalog,
        )
        .unwrap();
    world
        .entities_mut()
        .components_mut(handle)
        .unwrap()
        .insert(&schemas, &torse_inventory::schema(), inventory.fields())
        .unwrap();
    (world, service)
}
fn create(dir: &Directory, mode: Mode) -> base::Files {
    let (world, actions) = world(mode);
    base::Files::create_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| {
            ExecutionHost::new(
                binding,
                world,
                actions,
                Vec::new(),
                base::execution_limits(),
            )
        },
    )
    .unwrap()
}
fn reopen(dir: &Directory) -> base::Files {
    let (world, actions) = world(Mode::Normal);
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
                Vec::new(),
                base::execution_limits(),
            ))
        },
    )
    .unwrap()
}
fn actor(consume: bool, entity: bool) -> ActorContext {
    let actor = ActorContext::new(key("test:actor"))
        .permission(key("world:permission/edit"))
        .target(ActionTarget::Block(POS));
    let actor = if consume {
        actor.permission(key("inventory:permission/consume"))
    } else {
        actor
    };
    if entity {
        actor.target(ActionTarget::Entity(EntityId(1)))
    } else {
        actor
    }
}
fn session(server: &mut base::Files, actor: ActorContext) {
    server
        .register_session(
            Viewer {
                session: SessionId(1),
                actor: ActorContextId(1),
            },
            actor,
            base::scope(),
        )
        .unwrap();
}
fn call() -> Call {
    Call {
        action: key("craft:action/place"),
        version: 1,
        target: ActionTarget::Block(POS),
        parameters: [(
            key("world:field/block"),
            ComponentValue::Content(key("base:stone")),
        )]
        .into(),
    }
}
fn request(server: &base::Files, seq: u64) -> WorldRequest<Call> {
    WorldRequest {
        id: RequestId {
            world: base::WORLD,
            epoch: server.binding().epoch,
            session: SessionId(1),
            seq: RequestSeq(seq),
        },
        actor: ActorContextId(1),
        input: call(),
    }
}
fn stock(server: &base::Files) -> ComponentFields {
    let world = server.authority().unwrap().world();
    world
        .entities()
        .get(world.entities().handle_of(EntityId(1)).unwrap())
        .unwrap()
        .components
        .component(&torse_inventory::schema())
        .unwrap()
        .clone()
}
fn placed(server: &base::Files) -> bool {
    !server
        .authority()
        .unwrap()
        .world()
        .store()
        .query(POS)
        .ready()
        .unwrap()
        .is_empty()
}
fn freeze(server: &mut base::Files) {
    loop {
        let host = server.authority().unwrap();
        if host.resume_point().next_phase == Phase::Interaction.index() as u16
            && host.runtime().work.is_some()
        {
            break;
        }
        server.drive_one(&mut NoReplicas).unwrap();
    }
}

#[test]
fn composed_action_recovers_frozen_members_and_deduplicates_one_receipt() {
    let dir = Directory::new();
    let mut server = create(&dir, Mode::Normal);
    session(&mut server, actor(true, true));
    let before = stock(&server);
    let id = request(&server, 1).id;
    assert!(matches!(
        server.submit(request(&server, 1)),
        Admission::Queued { .. }
    ));
    assert_eq!(server.submit(request(&server, 1)), Admission::Duplicate);
    freeze(&mut server);
    assert!(!placed(&server));
    assert_eq!(stock(&server), before);
    let work = server.authority().unwrap().runtime().work.as_ref().unwrap();
    assert_eq!(work.tasks[0].transaction.as_ref().unwrap().members.len(), 2);
    drop(server);
    let mut recovered = reopen(&dir);
    recovered.drive_tick(&mut NoReplicas, 1024).unwrap();
    assert!(placed(&recovered));
    assert_ne!(stock(&recovered), before);
    assert_eq!(
        recovered.receipt(id).unwrap().unwrap().reply.status,
        ReplyStatus::Committed
    );
    let cursor = recovered.receipt(id).unwrap().unwrap().reply.at;
    drop(recovered);
    let recovered = reopen(&dir);
    assert!(placed(&recovered));
    assert_eq!(recovered.receipt(id).unwrap().unwrap().reply.at, cursor);
}

#[test]
fn permissions_targets_and_composition_bounds_reject_without_partial_effects() {
    for (mode, actor, expected) in [
        (
            Mode::Normal,
            actor(false, true),
            RejectCode::PermissionDenied,
        ),
        (
            Mode::Normal,
            actor(true, false),
            RejectCode::PermissionDenied,
        ),
        (
            Mode::Forged,
            actor(true, true),
            RejectCode::PermissionDenied,
        ),
        (
            Mode::Recursive,
            actor(true, true),
            RejectCode::QuotaExceeded,
        ),
        (
            Mode::DuplicateOwner,
            actor(true, true),
            RejectCode::InvalidData,
        ),
        (Mode::Query, actor(true, true), RejectCode::PermissionDenied),
        (
            Mode::SmallBudget,
            actor(true, true),
            RejectCode::QuotaExceeded,
        ),
        (
            Mode::SmallItems,
            actor(true, true),
            RejectCode::QuotaExceeded,
        ),
    ] {
        let dir = Directory::new();
        let mut server = create(&dir, mode);
        session(&mut server, actor);
        let before = stock(&server);
        let queued = request(&server, 1);
        let id = queued.id;
        server.submit(queued);
        server.drive_tick(&mut NoReplicas, 1024).unwrap();
        assert_eq!(
            server.receipt(id).unwrap().unwrap().reply.status,
            ReplyStatus::Rejected(expected)
        );
        assert!(!placed(&server));
        assert_eq!(stock(&server), before);
    }
}

#[test]
fn contending_actions_spend_one_item_and_commit_one_placement() {
    let dir = Directory::new();
    let mut server = create(&dir, Mode::Normal);
    session(&mut server, actor(true, true));
    let first = request(&server, 1);
    let second = request(&server, 2);
    let ids = [first.id, second.id];
    server.submit(first);
    server.submit(second);
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    assert_eq!(
        server.receipt(ids[0]).unwrap().unwrap().reply.status,
        ReplyStatus::Committed
    );
    assert!(matches!(
        server.receipt(ids[1]).unwrap().unwrap().reply.status,
        ReplyStatus::Rejected(_)
    ));
    assert_eq!(server.submit(request(&server, 1)), Admission::Duplicate);
    assert!(placed(&server));
}

#[test]
fn either_owner_failure_leaves_both_resources_unchanged() {
    for blocked in [false, true] {
        let (mut world, service) = world(Mode::Normal);
        let request = ActionRequest {
            action: call().action,
            api_version: 1,
            origin: ActionOrigin::Network,
            target: ActionTarget::Block(POS),
            parameters: call().parameters,
            transaction: TransactionId(10),
            ordering: torse_sim::OrderingKey {
                phase: Phase::Interaction,
                rule_priority: 0,
                stable_task_key: 0,
                local_sequence: 0,
            },
        };
        if blocked {
            let mut tx =
                Transaction::new(TransactionId(9), block_actions::owner(), request.ordering);
            tx.write_block(
                POS,
                world.registry().default_state(&key("base:stone")).unwrap(),
            );
            world.commit(&tx).unwrap();
        } else {
            let handle = world.entities().handle_of(EntityId(1)).unwrap();
            let schemas = world.schemas().clone();
            world
                .entities_mut()
                .components_mut(handle)
                .unwrap()
                .insert(
                    &schemas,
                    &torse_inventory::schema(),
                    Inventory::new(1).unwrap().fields(),
                )
                .unwrap();
        }
        let seq = world.seq();
        let fields = actor(true, true)
            .view(&world)
            .component(EntityId(1), &torse_inventory::schema())
            .unwrap()
            .clone();
        assert!(
            service
                .plan(&world, &actor(true, true), Phase::Interaction, &request)
                .is_err()
        );
        assert_eq!(world.seq(), seq);
        assert_eq!(
            world.store().query(POS).ready().unwrap().is_empty(),
            !blocked
        );
        assert_eq!(
            actor(true, true)
                .view(&world)
                .component(EntityId(1), &torse_inventory::schema())
                .unwrap(),
            &fields
        );
    }
}
