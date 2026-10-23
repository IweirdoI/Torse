//! Real world transactions use the durable host, not a parallel scenario coordinator.

mod support;

use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::rc::Rc;

use support::{Directory, WORLD};
use torse_codec::{CodecLimits, WorldCodec};
use torse_content::{compile, m0_fixture_pack};
use torse_inventory::{Inventory, ItemCatalog, ItemStack};
use torse_server::*;
use torse_sim::{OrderingKey, Phase, Transaction, WorldAuthority};
use torse_storage::*;
use torse_types::*;
use torse_world::*;

type Codec = WorldCodec<LocalWorldModel>;
type Files = FileServer<WorldHost, Codec>;

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).unwrap()
}

fn policy() -> FileServerConfig {
    let limits = CodecLimits {
        max_frame_bytes: 1_048_576,
        max_field_bytes: 65_536,
        max_depth: 12,
    };
    let budget = OutputBudget::new(8192, 1_048_576);
    FileServerConfig {
        server: ServerConfig {
            snapshot_budget: budget,
            max_invalidations: 0,
            shutdown_max_steps: 8,
            checkpoint_predecessors: 1,
        },
        journal: JournalConfig {
            max_pending_records: 2,
            max_pending_bytes: 2_097_152,
            codec_budget: budget,
            codec_limits: limits,
        },
        checkpoint: CheckpointConfig {
            codec_limits: limits,
            codec_budget: budget,
            max_manifest_bytes: 8192,
            max_snapshot_files: 4,
        },
    }
}

fn world() -> WorldAuthority {
    let mut pack = m0_fixture_pack();
    torse_inventory::register_content(&mut pack);
    let content = compile(pack).unwrap();
    let mut world = WorldAuthority::new(
        WORLD,
        BlockStateRegistry::build(&content).unwrap(),
        ComponentSchemas::build(&content),
        16,
        WorldBounds::new(0, 32).unwrap(),
        support::compatibility(),
    )
    .unwrap();
    for x in [-1, 0] {
        let pos = SectionPos::new(x, 0, 0);
        world.store_mut().request(pos).unwrap();
        world
            .store_mut()
            .install(pos, Section::empty(16).unwrap())
            .unwrap();
        world.store_mut().finish_load(pos).unwrap();
    }
    let schemas = world.schemas().clone();
    let catalog = ItemCatalog::new([(key("base:stone"), 64)]).unwrap();
    for index in 0..3 {
        let mut inventory = Inventory::new(1).unwrap();
        if index == 0 {
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
        }
        let handle = world
            .entities_mut()
            .spawn(Position::new(0.5 + index as f64, 2.0, 2.0))
            .unwrap();
        world
            .entities_mut()
            .components_mut(handle)
            .unwrap()
            .insert(&schemas, &torse_inventory::schema(), inventory.fields())
            .unwrap();
    }
    // A deleted identity remains consumed by every subsequent checkpoint and recovery.
    let deleted = world
        .entities_mut()
        .spawn(Position::new(4.0, 2.0, 2.0))
        .unwrap();
    world.entities_mut().despawn(deleted).unwrap();
    world
}

fn binding(directory: &WorldDirectory) -> Binding {
    Binding {
        world: WORLD,
        epoch: directory.epoch(),
        compatibility: support::compatibility(),
    }
}

fn create(directory: &Directory, config: FileServerConfig) -> Files {
    Files::create_file(
        directory.acquire(),
        support::compatibility(),
        Codec::new(config.journal.codec_limits).unwrap(),
        config,
        |binding| WorldHost::new(binding, world()),
    )
    .unwrap()
}

fn open(directory: &Directory) -> Result<Files, Fault> {
    let fresh = world();
    let config = policy();
    Files::open_file(
        directory.acquire(),
        support::compatibility(),
        Codec::new(config.journal.codec_limits).unwrap(),
        config,
        |binding| {
            Ok(WorldHostRecovery::new(
                binding,
                fresh.registry(),
                fresh.schemas(),
            ))
        },
    )
}

fn transaction(id: u128) -> Transaction {
    Transaction::new(
        TransactionId(id),
        torse_inventory::owner(),
        OrderingKey {
            phase: Phase::Interaction,
            rule_priority: 0,
            stable_task_key: id as u64,
            local_sequence: 0,
        },
    )
}

fn block(world: &WorldAuthority, id: u128, x: i64) -> Transaction {
    let mut tx = transaction(id);
    tx.read_block(BlockPos::new(x, 1, 1), BlockStateId::EMPTY)
        .write_block(
            BlockPos::new(x, 1, 1),
            world.registry().default_state(&key("base:stone")).unwrap(),
        );
    tx
}

fn mixed(world: &WorldAuthority, destination: EntityId) -> Transaction {
    let mut tx = block(world, 1, 2);
    let source = world.entities().handle_of(EntityId(1)).unwrap();
    let entity = world.entities().get(source).unwrap();
    tx.move_entity(
        source,
        entity.id,
        entity.position,
        [-1.0, 0.0, 0.0],
        [0.3, 0.9, 0.3],
    );
    let schema = torse_inventory::schema();
    let empty = Inventory::new(1).unwrap().fields();
    let held = entity.components.component(&schema).unwrap().clone();
    for (id, fields) in [(EntityId(1), empty), (destination, held)] {
        let handle = world.entities().handle_of(id).unwrap();
        tx.components.push(ComponentEdit {
            id,
            schema: schema.clone(),
            previous: Some(
                world
                    .entities()
                    .get(handle)
                    .unwrap()
                    .components
                    .component(&schema)
                    .unwrap()
                    .clone(),
            ),
            fields: Some(fields),
        });
    }
    tx
}

fn capture(world: &WorldAuthority) -> Snapshot {
    world
        .capture_snapshot(SnapshotMeta {
            cursor: CommitCursor {
                world: WORLD,
                seq: world.seq(),
            },
            compatibility: support::compatibility(),
            resume_at: world.coordinator().resume_point(),
        })
        .unwrap()
}

#[test]
fn mixed_commit_checkpoint_tail_recovery_and_shutdown_preserve_the_complete_world() {
    let directory = Directory::new();
    let mut server = create(&directory, policy());
    let old_epoch = server.binding().epoch;
    let world = server.authority().unwrap().world();
    let tx = mixed(world, EntityId(2));
    let competitor = mixed(world, EntityId(3));
    assert!(matches!(
        server.commit_transaction(&tx).unwrap(),
        LocalCommit::Durable(_)
    ));
    assert!(matches!(
        server.commit_transaction(&competitor).unwrap(),
        LocalCommit::Rejected(RejectCode::Conflict)
    ));
    server.checkpoint().unwrap();
    let tx = block(server.authority().unwrap().world(), 2, 3);
    let LocalCommit::Durable(result) = server.commit_transaction(&tx).unwrap() else {
        panic!("not durable")
    };
    assert_eq!(result.publication.at.seq, CommitSeq(2));
    assert_eq!(result.durability.through, DurableSeq(2));
    let expected = capture(server.authority().unwrap().world());
    drop(server);
    OpenOptions::new()
        .append(true)
        .open(directory.0.join("journal.log"))
        .unwrap()
        .write_all(b"TOR")
        .unwrap();
    let mut server = open(&directory).unwrap();
    assert_ne!(server.binding().epoch, old_epoch);
    assert_eq!(capture(server.authority().unwrap().world()), expected);
    let world = server.authority().unwrap().world();
    let moved = world.entities().handle_of(EntityId(1)).unwrap();
    assert_eq!(world.entities().get(moved).unwrap().position.x, -0.5);
    assert_eq!(world.entities().next_entity_id(), 5);
    let tx = block(world, 3, 4);
    assert!(matches!(
        server.commit_transaction(&tx).unwrap(),
        LocalCommit::Durable(_)
    ));
    assert_eq!(
        server
            .shutdown(Tick(0), &mut NoReplicas)
            .unwrap()
            .through
            .seq,
        CommitSeq(3)
    );
    assert_eq!(
        open(&directory).unwrap().authority().unwrap().world().seq(),
        CommitSeq(3)
    );
}

#[test]
fn oversized_journal_record_preserves_world_and_borrowed_plan_for_retry() {
    let directory = Directory::new();
    let mut config = policy();
    config.journal.codec_budget.max_bytes = 1;
    let mut server = create(&directory, config);
    let before = capture(server.authority().unwrap().world());
    let tx = mixed(server.authority().unwrap().world(), EntityId(2));
    for _ in 0..2 {
        assert!(matches!(
            server.commit_transaction(&tx).unwrap(),
            LocalCommit::Blocked(StageError::TooLarge)
        ));
        assert_eq!(server.state(), ServerState::Running);
        assert_eq!(capture(server.authority().unwrap().world()), before);
        assert_eq!(server.durability().through, DurableSeq(0));
    }
    drop(server);
    let mut server = open(&directory).unwrap();
    assert!(matches!(
        server.commit_transaction(&tx).unwrap(),
        LocalCommit::Durable(_)
    ));
}

#[test]
fn domain_recovery_failure_keeps_journal_and_manifest_tails_unchanged() {
    let directory = Directory::new();
    let mut server = create(&directory, policy());
    let tx = block(server.authority().unwrap().world(), 1, 2);
    server.commit_transaction(&tx).unwrap();
    drop(server);
    let paths = [
        directory.0.join("journal.log"),
        directory.0.join("checkpoints/manifest.log"),
    ];
    for path in &paths {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"TOR")
            .unwrap();
    }
    let before = paths.each_ref().map(|path| fs::read(path).unwrap());
    // Empty locked content cannot resolve the stone in the otherwise valid journal tail.
    let content = compile(Default::default()).unwrap();
    let registry = BlockStateRegistry::build(&content).unwrap();
    let fresh = world();
    let schemas = fresh.schemas();
    let config = policy();
    let result = Files::open_file(
        directory.acquire(),
        support::compatibility(),
        Codec::new(config.journal.codec_limits).unwrap(),
        config,
        |binding| Ok(WorldHostRecovery::new(binding, &registry, schemas)),
    );
    assert!(result.is_err());
    assert_eq!(paths.each_ref().map(|path| fs::read(path).unwrap()), before);
    assert_eq!(
        open(&directory).unwrap().authority().unwrap().world().seq(),
        CommitSeq(1)
    );
}

struct BarrierFailure(Rc<Cell<bool>>);

impl JournalBackend for BarrierFailure {
    fn append(&mut self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        if self.0.get() {
            Err(io::Error::other("injected synchronization failure"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn failed_real_world_barrier_stops_reads_and_further_commits() {
    let directory = Directory::new();
    let lease = directory.acquire();
    let binding = binding(&lease);
    let config = policy();
    let codec = Codec::new(config.journal.codec_limits).unwrap();
    let fail = Rc::new(Cell::new(false));
    let journal = Journal::with_backend(
        binding,
        CommitCursor::initial(WORLD),
        codec,
        config.journal,
        BarrierFailure(fail.clone()),
    )
    .unwrap();
    let checkpoints = Checkpoints::open(
        binding,
        codec,
        config.checkpoint,
        MemoryCheckpointBackend::new(4_194_304),
    )
    .unwrap();
    let mut server = Server::initialize(
        lease,
        WorldHost::new(binding, world()).unwrap(),
        journal,
        checkpoints,
        config.server,
    )
    .unwrap();
    let tx = mixed(server.authority().unwrap().world(), EntityId(2));
    fail.set(true);
    assert!(server.commit_transaction(&tx).is_err());
    assert_eq!(server.state(), ServerState::Faulted);
    assert!(server.authority().is_err());
    assert!(server.commit_transaction(&tx).is_err());
    assert_eq!(server.durability().through, DurableSeq(0));
}

#[test]
fn bounded_snapshot_rejects_dense_cells_and_nested_inventory_before_capture() {
    let mut world = world();
    let tx = block(&world, 1, 2);
    world.commit(&tx).unwrap();
    let before = capture(&world);
    for budget in [
        OutputBudget::new(1, u64::MAX),
        OutputBudget::new(17, u64::MAX),
        OutputBudget::new(u32::MAX, 8192),
    ] {
        let error = world
            .capture_snapshot_with_budget(before.meta, budget)
            .unwrap_err();
        assert_eq!(error.code, FaultCode::ResourceLimit);
        assert_eq!(capture(&world), before);
    }
    assert_eq!(
        world
            .capture_snapshot_with_budget(before.meta, OutputBudget::new(18, 1_048_576))
            .unwrap(),
        before
    );
}

struct ExitBeforeAccept(FileJournal<LocalWorldModel, Codec>);

impl CommitJournal<LocalWorldModel> for ExitBeforeAccept {
    type Staged = Staged;

    fn try_stage(&mut self, record: &ModelCommit<LocalWorldModel>) -> Result<Staged, StageError> {
        self.0.try_stage(record)
    }

    fn accept(&mut self, _: Staged) -> Result<(), Fault> {
        std::process::exit(73)
    }

    fn flush_through(&mut self, target: CommitCursor) -> Result<Durability, Fault> {
        self.0.flush_through(target)
    }

    fn durability(&self) -> Durability {
        self.0.durability()
    }
}

#[test]
fn crash_child() {
    let Some(root) = std::env::var_os("TORSE_WORLD_CRASH_ROOT") else {
        return;
    };
    let lease = WorldDirectory::acquire(&root, WORLD, 8192).unwrap();
    let config = policy();
    let codec = Codec::new(config.journal.codec_limits).unwrap();
    if std::env::var("TORSE_WORLD_CRASH_MODE").unwrap() == "durable" {
        let mut server =
            Files::create_file(lease, support::compatibility(), codec, config, |binding| {
                WorldHost::new(binding, world())
            })
            .unwrap();
        let tx = mixed(server.authority().unwrap().world(), EntityId(2));
        assert!(matches!(
            server.commit_transaction(&tx).unwrap(),
            LocalCommit::Durable(_)
        ));
        std::process::exit(74);
    }
    let binding = binding(&lease);
    let path = lease.root().join("checkpoints");
    fs::create_dir(&path).unwrap();
    let checkpoints = Checkpoints::open(
        binding,
        codec,
        config.checkpoint,
        FileCheckpointBackend::open(path).unwrap(),
    )
    .unwrap();
    let journal = FileJournal::create(
        lease.root().join("journal.log"),
        binding,
        CommitCursor::initial(WORLD),
        codec,
        config.journal,
    )
    .unwrap();
    let mut server = Server::initialize(
        lease,
        WorldHost::new(binding, world()).unwrap(),
        ExitBeforeAccept(journal),
        checkpoints,
        config.server,
    )
    .unwrap();
    let tx = mixed(server.authority().unwrap().world(), EntityId(2));
    let _ = server.commit_transaction(&tx);
    panic!("expected exit between publication and journal acceptance");
}

#[test]
fn process_exit_recovers_only_the_durable_world_prefix() {
    for (mode, exit, sequence) in [("before_accept", 73, 0), ("durable", 74, 1)] {
        let directory = Directory::new();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_child", "--nocapture"])
            .env("TORSE_WORLD_CRASH_ROOT", &directory.0)
            .env("TORSE_WORLD_CRASH_MODE", mode)
            .output()
            .unwrap();
        assert_eq!(
            child.status.code(),
            Some(exit),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        let server = open(&directory).unwrap();
        let mut expected = world();
        if sequence == 1 {
            let tx = mixed(&expected, EntityId(2));
            expected.commit(&tx).unwrap();
        }
        assert_eq!(
            capture(server.authority().unwrap().world()),
            capture(&expected)
        );
        assert_eq!(server.durability().through, DurableSeq(sequence));
    }
}

#[test]
fn local_recovery_rejects_metadata_progress_and_compatibility_mismatches() {
    let world = world();
    let snapshot = capture(&world);
    let binding = Binding {
        world: WORLD,
        epoch: WorldEpoch(2),
        compatibility: support::compatibility(),
    };
    for variant in 0..4 {
        let mut state = snapshot.clone();
        match variant {
            0 => state.meta.resume_at.next_phase = 3,
            1 => state.meta.resume_at.next_step = 1,
            2 => state.meta.compatibility.authority_schema += 1,
            _ => state.meta.cursor.world = WorldId(99),
        }
        let mut recovery = WorldHostRecovery::new(binding, world.registry(), world.schemas());
        assert!(
            recovery
                .restore_snapshot(WorldSnapshot {
                    meta: state.meta,
                    state
                })
                .is_err()
        );
    }
    let mut recovery = WorldHostRecovery::new(binding, world.registry(), world.schemas());
    recovery
        .restore_snapshot(WorldSnapshot {
            meta: snapshot.meta,
            state: snapshot,
        })
        .unwrap();
    assert!(
        recovery
            .finish(CommitCursor {
                world: WORLD,
                seq: CommitSeq(1)
            })
            .is_err()
    );

    let snapshot = capture(&world);
    let mut recovery = WorldHostRecovery::new(binding, world.registry(), world.schemas());
    recovery
        .restore_snapshot(WorldSnapshot {
            meta: snapshot.meta,
            state: snapshot,
        })
        .unwrap();
    let prepared = world.prepare(&block(&world, 1, 2)).unwrap();
    let original = torse_sim::changes_from_prepared(world.registry(), &prepared).unwrap();
    for variant in 0..4 {
        let mut changes = original.clone();
        match variant {
            0 => changes.meta.resume_at.tick = Tick(1),
            1 => changes.meta.compatibility.authority_schema += 1,
            2 => changes.meta.resume_at.next_phase = 1,
            _ => changes.meta.previous = CommitSeq(1),
        }
        assert!(
            recovery
                .replay(CommitRecord {
                    meta: changes.meta,
                    changes
                })
                .is_err()
        );
    }
    assert_eq!(
        recovery
            .finish(CommitCursor::initial(WORLD))
            .unwrap()
            .world()
            .seq(),
        CommitSeq(0)
    );
}
