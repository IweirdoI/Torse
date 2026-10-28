//! Real guest actions use the same native inventory and lifecycle preparation.
#![cfg(feature = "package")]

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use torse_mod_host::{
    authority::{ScenarioConfig, machine_scenario},
    component::{CallBudget, encode_component},
    durable_dispatch::{DurableGuest, InteractionRoute, SymbolTable},
    package, scenario,
};
use torse_sim::action::{ActionOrigin, ActionRequest, ActionTarget, ActorContext};
use torse_sim::{OrderingKey, Phase, Recovery, WorldAuthority, changes_from_prepared};
use torse_types::*;
use torse_world::{ComponentFields, ComponentValue as V};

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).unwrap()
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn component() -> &'static [u8] {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| {
        let status = std::process::Command::new("cargo")
            .args([
                "build",
                "--release",
                "--target",
                "wasm32-unknown-unknown",
                "-p",
                "torse-mod-machine",
            ])
            .current_dir(root())
            .status()
            .unwrap();
        assert!(status.success());
        encode_component(
            &std::fs::read(
                root().join("target/wasm32-unknown-unknown/release/torse_mod_machine.wasm"),
            )
            .unwrap(),
        )
        .unwrap()
    })
}
const PERMISSIONS: &[&str] = &[
    "inventory:permission/read",
    "inventory:permission/transfer",
    "world:permission/spawn",
    "world:permission/despawn",
    "machine:permission/interact",
];
fn actor(principal: &str, ids: &[EntityId]) -> ActorContext {
    let mut actor = ActorContext::new(key(principal)).section(SectionPos::ZERO);
    for permission in PERMISSIONS {
        actor = actor.permission(key(permission));
    }
    for id in ids {
        actor = actor.target(ActionTarget::Entity(*id));
    }
    actor
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
fn request(action: &str, target: EntityId, parameters: ComponentFields, id: u128) -> ActionRequest {
    ActionRequest {
        action: key(action),
        api_version: 1,
        origin: ActionOrigin::Network,
        target: ActionTarget::Entity(target),
        parameters,
        transaction: TransactionId(id),
        ordering: OrderingKey {
            phase: Phase::Interaction,
            rule_priority: 0,
            stable_task_key: 77,
            local_sequence: 0,
        },
    }
}

#[test]
fn scoped_guest_actions_preserve_native_atomicity_nested_values_and_replay() {
    let mut fixture = machine_scenario(&ScenarioConfig {
        package: "machine:module",
        schema: "machine:schema/state",
        stock_input: true,
        granted_permissions: PERMISSIONS,
    })
    .unwrap();
    let set = package::resolve(
        &[root().join("examples/mods/machine")],
        &scenario::engine_base(),
    )
    .unwrap();
    let symbols = Arc::new(SymbolTable::build(&set.content, &["base:stone".into()]));
    let mut guest = DurableGuest::new(component(), symbols, CallBudget::prototype()).unwrap();
    let ids = [fixture.machine, fixture.input, fixture.output];
    let mut route = InteractionRoute {
        action: key("machine:action/transfer"),
        api_version: 1,
        permission: key("machine:permission/interact"),
        grant: actor("machine:module", &ids),
        owned_schemas: vec![key("machine:schema/state")],
        catalog: fixture.catalog.clone(),
    };
    let caller = actor("user:alice", &ids);
    let world = fixture.host.authority_mut();
    let before = world.capture_snapshot(meta(world)).unwrap();
    let bad = request(
        "machine:action/transfer",
        ids[0],
        [(key("machine:field/count"), V::U32(2))].into(),
        1,
    );
    assert!(
        guest
            .plan_interaction(world, 10, &route, &caller, &bad)
            .is_err()
    );
    assert_eq!(world.capture_snapshot(meta(world)).unwrap(), before);
    let req = request(
        "machine:action/transfer",
        ids[0],
        [(key("machine:field/count"), V::U32(1))].into(),
        2,
    );
    assert!(
        guest
            .plan_interaction(world, 10, &route, &actor("user:limited", &ids[..1]), &req)
            .is_err()
    );
    assert!(
        guest
            .plan_interaction(
                world,
                10,
                &route,
                &ActorContext::new(key("user:none")),
                &req
            )
            .is_err()
    );
    let plan = guest
        .plan_interaction(world, 10, &route, &caller, &req)
        .unwrap();
    let native = torse_inventory::plan_transfers(
        &caller.view(world),
        &route.catalog,
        &[
            torse_inventory::Transfer {
                source: ids[1],
                source_slot: 0,
                destination: ids[2],
                destination_slot: 1,
                count: 1,
            },
            torse_inventory::Transfer {
                source: ids[2],
                source_slot: 1,
                destination: ids[2],
                destination_slot: 0,
                count: 1,
            },
        ],
        req.transaction,
        req.ordering,
    )
    .unwrap();
    let group = plan.transaction.unwrap();
    assert_eq!(group.members.len(), 1);
    assert_eq!(group.members[0], native);
    assert_eq!(
        world.capture_snapshot(meta(world)).unwrap(),
        before,
        "planning is read-only"
    );
    let prepared = world.prepare_group(&group).unwrap();
    let changes = changes_from_prepared(world.registry(), &prepared).unwrap();
    world.publish(prepared).unwrap();
    let mut recovery = Recovery::new(world.world(), world.registry(), world.schemas());
    recovery.restore_snapshot(&before).unwrap();
    recovery.replay(&changes).unwrap();
    let recovered = recovery.finish().unwrap();
    assert_eq!(
        recovered.capture_snapshot(meta(&recovered)).unwrap(),
        world.capture_snapshot(meta(world)).unwrap()
    );

    route.action = key("machine:action/set");
    let history = V::List(vec![V::Record(
        [(
            key("machine:field/note"),
            V::List(vec![V::Text("step".into()), V::U32(7)]),
        )]
        .into(),
    )]);
    let req = request(
        "machine:action/set",
        ids[0],
        [(key("machine:field/history"), history.clone())].into(),
        3,
    );
    let group = guest
        .plan_interaction(world, 11, &route, &caller, &req)
        .unwrap()
        .transaction
        .unwrap();
    let prepared = world.prepare_group(&group).unwrap();
    world.publish(prepared).unwrap();
    let handle = world.entities().handle_of(ids[0]).unwrap();
    assert_eq!(
        world
            .entities()
            .get(handle)
            .unwrap()
            .components
            .component(&key("machine:schema/state"))
            .unwrap()[&key("machine:field/history")],
        history
    );

    route.action = key("machine:action/spawn");
    let req = request("machine:action/spawn", ids[0], ComponentFields::new(), 4);
    let no_spawn = ActorContext::new(key("user:limited"))
        .permission(key("machine:permission/interact"))
        .target(ActionTarget::Entity(ids[0]))
        .section(SectionPos::ZERO);
    let snapshot = world.capture_snapshot(meta(world)).unwrap();
    assert!(
        guest
            .plan_interaction(world, 12, &route, &no_spawn, &req)
            .is_err()
    );
    assert_eq!(world.capture_snapshot(meta(world)).unwrap(), snapshot);
    let group = guest
        .plan_interaction(world, 12, &route, &caller, &req)
        .unwrap()
        .transaction
        .unwrap();
    let count = world.entities().iter().count();
    let prepared = world.prepare_group(&group).unwrap();
    world.publish(prepared).unwrap();
    assert_eq!(world.entities().iter().count(), count + 1);

    route.action = key("machine:action/despawn");
    let req = request("machine:action/despawn", ids[1], ComponentFields::new(), 5);
    assert!(
        guest
            .plan_interaction(world, 13, &route, &caller, &req)
            .is_err(),
        "foreign inventory cannot be despawned by the behavior owner"
    );
    let req = request("machine:action/despawn", ids[0], ComponentFields::new(), 6);
    let group = guest
        .plan_interaction(world, 13, &route, &caller, &req)
        .unwrap()
        .transaction
        .unwrap();
    let prepared = world.prepare_group(&group).unwrap();
    world.publish(prepared).unwrap();
    assert!(world.entities().handle_of(ids[0]).is_none());
}
