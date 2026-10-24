//! Durable Mod assembly acceptance: the machine survives restarts, settled
//! work never re-runs the guest, failures leave no partial changes, and one
//! package failing does not stop the others. Requires the `mods` feature and
//! builds the machine guest through cargo.

#![cfg(feature = "mods")]

mod support;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use support::Directory;
use torse_inventory::{Inventory, ItemCatalog, ItemStack};
use torse_server::execution::{ExecutionHost, ExecutionRecovery};
use torse_server::{FileServer, NoReplicas, bootstrap as base, mods};
use torse_sim::WorldAuthority;
use torse_types::EntityId;
use torse_world::ComponentValue;

fn machine_dir() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir.join("examples").join("mods").join("machine")
}

fn workspace_root() -> PathBuf {
    let mut dir = machine_dir();
    dir.pop();
    dir.pop();
    dir.pop();
    dir
}

fn catalog() -> ItemCatalog {
    ItemCatalog::new([(base::key("base:stone"), 64)]).expect("catalog")
}

/// A second package cloning the machine content under another namespace; its
/// guest binary resolves foreign schemas, so every plan settles as a
/// rejection while the primary machine keeps running.
fn clone_package(id: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("torse-mod-durability-{id}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("content")).expect("clone content dir");
    let manifest = format!(
        "id = \"{id}\"\nversion = \"0.1.0\"\napi = \"0.2\"\nside = \"server\"\npermissions = [\"inventory:permission/transfer\", \"inventory:permission/read\"]\n\n[server]\nentry = \"logic.wasm\"\n\n[build]\ncrate = \"torse-mod-machine\"\nworkspace = \"{}\"\n",
        workspace_root()
            .display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/")
    );
    std::fs::write(dir.join("mod.toml"), manifest).expect("clone manifest");
    let content = std::fs::read_to_string(machine_dir().join("content").join("machine.toml"))
        .expect("machine content");
    std::fs::write(
        dir.join("content").join("machine.toml"),
        content.replace("machine:", &format!("{id}:")),
    )
    .expect("clone content");
    dir
}

fn assemble(
    dirs: &[PathBuf],
    stock_input: bool,
    spin: &[String],
) -> (mods::ModWorld, Rc<RefCell<mods::ModStats>>) {
    let world = mods::mod_world(dirs, stock_input, spin).expect("mod world assembles");
    let stats = Rc::clone(&world.stats);
    (world, stats)
}

fn create(dir: &Directory, world: mods::ModWorld) -> FileServer<ExecutionHost, base::Codec> {
    let compatibility = world.world.coordinator().compatibility();
    let config = base::file_policy();
    FileServer::create_file(
        dir.acquire(),
        compatibility,
        base::Codec::new(base::codec_limits()).unwrap(),
        config,
        |binding| {
            ExecutionHost::new(
                binding,
                world.world,
                world.actions,
                world.rules,
                base::execution_limits(),
            )
        },
    )
    .unwrap()
}

fn reopen(
    dir: &Directory,
    dirs: &[PathBuf],
    stock: bool,
) -> (
    FileServer<ExecutionHost, base::Codec>,
    Rc<RefCell<mods::ModStats>>,
) {
    let (world, stats) = assemble(dirs, stock, &[]);
    let compatibility = world.world.coordinator().compatibility();
    let config = base::file_policy();
    let server = FileServer::open_file(
        dir.acquire(),
        compatibility,
        base::Codec::new(base::codec_limits()).unwrap(),
        config,
        |binding| {
            Ok(ExecutionRecovery::new(
                binding,
                world.world.registry(),
                world.world.schemas(),
                world.actions,
                world.rules,
                base::execution_limits(),
            ))
        },
    )
    .unwrap();
    (server, stats)
}

fn tick(server: &mut FileServer<ExecutionHost, base::Codec>) {
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
}

fn world_of(server: &FileServer<ExecutionHost, base::Codec>) -> &WorldAuthority {
    server.authority().unwrap().world()
}

fn progress(world: &WorldAuthority, entity: EntityId, schema: &str) -> u32 {
    let handle = world.entities().handle_of(entity).expect("entity");
    match world
        .entities()
        .get(handle)
        .expect("entity")
        .components
        .component(&base::key(schema))
        .expect("machine component")
        .get(&base::key(&format!(
            "{}:field/progress",
            schema.split(':').next().unwrap()
        ))) {
        Some(ComponentValue::U32(value)) => *value,
        other => panic!("unexpected progress: {other:?}"),
    }
}

fn slot0(world: &WorldAuthority, entity: EntityId) -> Option<ItemStack> {
    let handle = world.entities().handle_of(entity).expect("entity");
    let fields = world
        .entities()
        .get(handle)
        .expect("entity")
        .components
        .component(&torse_inventory::schema())
        .expect("inventory");
    let inventory = Inventory::from_fields(fields, &catalog()).expect("inventory decodes");
    inventory.slots()[0].clone()
}

#[test]
fn machine_survives_restart_and_keeps_counts_correct() {
    let dir = Directory::new();
    let (world, _stats) = assemble(&[machine_dir()], true, &[]);
    let machine = world.machines[0].1;
    let input = world.input;
    let output = world.output;
    let mut server = create(&dir, world);

    // Five ticks: arming, progress 1, 2, 3, then the ready settlement resets
    // to zero and schedules the transfer plus the next wakeup.
    for _ in 0..5 {
        tick(&mut server);
    }
    assert_eq!(
        progress(world_of(&server), machine, "machine:schema/state"),
        0
    );
    assert_eq!(
        slot0(world_of(&server), input),
        Some(ItemStack {
            item: base::key("base:stone"),
            count: 1,
        }),
        "transfer must not settle before its event tick"
    );
    assert_eq!(slot0(world_of(&server), output), None);

    // Mid-processing exit: drop the server with the transfer still queued.
    drop(server);
    let (mut server, _stats) = reopen(&dir, &[machine_dir()], true);
    tick(&mut server);
    assert_eq!(
        slot0(world_of(&server), input),
        None,
        "input emptied by resumed transfer"
    );
    assert_eq!(
        slot0(world_of(&server), output),
        Some(ItemStack {
            item: base::key("base:stone"),
            count: 1,
        }),
        "output received the resumed transfer"
    );
    assert_eq!(
        progress(world_of(&server), machine, "machine:schema/state"),
        1
    );

    // A full second cycle runs against an emptied input: the world holds one
    // stone, so the second transfer rejects and counts stay conserved.
    for _ in 0..4 {
        tick(&mut server);
    }
    assert_eq!(
        slot0(world_of(&server), output),
        Some(ItemStack {
            item: base::key("base:stone"),
            count: 1,
        }),
        "no duplication across restart and two cycles"
    );
    assert_eq!(slot0(world_of(&server), input), None);
    assert!(
        progress(world_of(&server), machine, "machine:schema/state") >= 1,
        "the machine keeps cycling after a rejected second transfer"
    );
}

#[test]
fn settled_tasks_do_not_rerun_the_guest_after_recovery() {
    let dir = Directory::new();
    let (world, stats) = assemble(&[machine_dir()], true, &[]);
    let mut server = create(&dir, world);
    for _ in 0..5 {
        tick(&mut server);
    }
    assert_eq!(
        stats.borrow().guest_dispatches.get("machine"),
        Some(&4),
        "four wakeups dispatched before the exit"
    );
    drop(server);

    // Reopen and settle the pending transfer plus the next wakeup. The
    // transfer is host-native, so only the one new wakeup calls the guest.
    let (mut server, stats) = reopen(&dir, &[machine_dir()], true);
    tick(&mut server);
    assert_eq!(
        stats.borrow().guest_dispatches.get("machine"),
        Some(&1),
        "recovery replays settled work without dispatching the guest"
    );
}

#[test]
fn transfer_rejection_leaves_no_partial_changes() {
    let dir = Directory::new();
    let (world, stats) = assemble(&[machine_dir()], false, &[]);
    let machine = world.machines[0].1;
    let input = world.input;
    let output = world.output;
    let mut server = create(&dir, world);

    for _ in 0..7 {
        tick(&mut server);
    }
    assert!(
        *stats
            .borrow()
            .transfer_rejections
            .get("machine")
            .unwrap_or(&0)
            >= 1,
        "empty input must reject the transfer plan"
    );
    assert_eq!(slot0(world_of(&server), input), None);
    assert_eq!(slot0(world_of(&server), output), None);
    assert!(
        progress(world_of(&server), machine, "machine:schema/state") >= 1,
        "the machine keeps cycling after a rejected transfer"
    );
}

#[test]
fn trapped_guest_parks_bounded_without_side_effects() {
    let dir = Directory::new();
    let (world, stats) = assemble(&[machine_dir()], true, &[String::from("machine")]);
    let machine = world.machines[0].1;
    let input = world.input;
    let output = world.output;
    let mut server = create(&dir, world);

    for _ in 0..64 {
        tick(&mut server);
    }
    let stats = stats.borrow();
    assert_eq!(stats.traps.get("machine"), Some(&8), "bounded attempts");
    assert_eq!(stats.parks.get("machine"), Some(&1), "parked after budget");
    assert_eq!(stats.guest_dispatches.get("machine"), Some(&8));
    drop(stats);
    assert_eq!(
        progress(world_of(&server), machine, "machine:schema/state"),
        0
    );
    assert_eq!(
        slot0(world_of(&server), input),
        Some(ItemStack {
            item: base::key("base:stone"),
            count: 1,
        }),
        "a trapped guest never moves items"
    );
    assert_eq!(slot0(world_of(&server), output), None);
    assert!(
        world_of(&server).seq().0 > 0,
        "the server keeps committing barrier records while the package is parked"
    );
}

#[test]
fn failing_package_does_not_stop_the_others() {
    let dir = Directory::new();
    let clone = clone_package("machine2");
    let dirs = vec![machine_dir(), clone.clone()];
    let (world, stats) = assemble(&dirs, true, &[String::from("machine2")]);
    let primary = world.machines[0].1;
    let input = world.input;
    let output = world.output;
    let mut server = create(&dir, world);

    for _ in 0..7 {
        tick(&mut server);
    }
    let stats = stats.borrow();
    assert!(
        *stats.traps.get("machine2").unwrap_or(&0) >= 1,
        "the spinning clone traps without disturbing the phase"
    );
    drop(stats);

    // The primary machine completes its cycle with correct counts.
    assert_eq!(slot0(world_of(&server), input), None);
    assert_eq!(
        slot0(world_of(&server), output),
        Some(ItemStack {
            item: base::key("base:stone"),
            count: 1,
        })
    );
    assert!(
        progress(world_of(&server), primary, "machine:schema/state") >= 1,
        "the primary machine keeps cycling beside a trapped package"
    );
    let _ = std::fs::remove_dir_all(clone);
}

#[test]
fn task_budget_shortfall_commits_nothing_and_recovers() {
    let dir = Directory::new();
    let (world, _stats) = assemble(&[machine_dir()], true, &[]);
    let machine = world.machines[0].1;
    let compatibility = world.world.coordinator().compatibility();
    let config = base::file_policy();
    let mut limits = base::execution_limits();
    limits.phase_tasks = 1;
    let mut server = FileServer::create_file(
        dir.acquire(),
        compatibility,
        base::Codec::new(base::codec_limits()).unwrap(),
        config,
        |binding| ExecutionHost::new(binding, world.world, world.actions, world.rules, limits),
    )
    .unwrap();

    // Tick 5 settles the ready step and schedules two events for tick 6;
    // collecting both exceeds the phase budget and faults before anything is
    // committed for that tick.
    for _ in 0..5 {
        tick(&mut server);
    }
    assert!(
        server.drive_tick(&mut NoReplicas, 1024).is_err(),
        "budget shortfall must fault the tick"
    );
    drop(server);

    // Entity identities are deterministic: input 1, output 2, machine 3.
    let (mut server, _stats) = reopen(&dir, &[machine_dir()], true);
    let (input, output) = (EntityId(1), EntityId(2));
    assert_eq!(
        progress(world_of(&server), machine, "machine:schema/state"),
        0,
        "no partial progress survived the shortfall"
    );
    assert_eq!(
        slot0(world_of(&server), input),
        Some(ItemStack {
            item: base::key("base:stone"),
            count: 1,
        }),
        "the stone was not moved or duplicated"
    );
    assert_eq!(slot0(world_of(&server), output), None);

    for _ in 0..2 {
        tick(&mut server);
    }
    assert_eq!(
        slot0(world_of(&server), output),
        Some(ItemStack {
            item: base::key("base:stone"),
            count: 1,
        }),
        "recovered execution completes the cycle"
    );
}
