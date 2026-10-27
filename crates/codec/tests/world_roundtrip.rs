//! Round trips of payloads captured from a real world store and compiled registry.
//!
//! The format test proves the byte layout on a hand-built sample. This test proves that the
//! payloads A actually produces (capture order, state tables, dense sections at negative
//! coordinates, components) survive encode, decode, and restore without any difference.

mod world_support;

use torse_content::{compile, m0_fixture_pack};
use torse_types::{BlockPos, CommitCursor, CommitMeta, CommitSeq, SectionPos, SnapshotMeta};
use torse_world::{
    BlockEdit, BlockStateRegistry, ChangeSet, ComponentSchemas, EntityMove, EntityStore, Position,
    Section, SectionStorage, SectionStore, Snapshot, TerrainQuery, WorldBounds,
};
use world_support::*;

struct Fixture {
    registry: BlockStateRegistry,
    schemas: ComponentSchemas,
    store: SectionStore,
    entities: EntityStore,
}

fn load(store: &mut SectionStore, pos: SectionPos, section: Section) {
    store.request(pos).expect("request works");
    store.install(pos, section).expect("install works");
    store.finish_load(pos).expect("finish works");
}

/// Three sections cover the three cell layouts the capture can emit: an empty uniform
/// section, a uniform non-empty section, and a dense section with mixed states.
fn fixture() -> Fixture {
    let content = compile(m0_fixture_pack()).expect("fixture content compiles");
    let registry = BlockStateRegistry::build(&content).expect("registry builds");
    let schemas = ComponentSchemas::build(&content);
    let stone = registry.default_state(&key("base:stone")).expect("stone");
    let glass = registry
        .default_state(&key("base:glass_slab"))
        .expect("glass");
    let plant = registry
        .state_of(&key("base:plant"), &[2])
        .expect("plant growth 2");

    let bounds = WorldBounds::new(-16, 32).expect("bounds are valid");
    let mut store = SectionStore::new(16, bounds).expect("side is valid");
    load(
        &mut store,
        SectionPos::new(0, 1, 0),
        Section::empty(16).unwrap(),
    );
    let solid = Section::from_storage(16, SectionStorage::Uniform(stone)).unwrap();
    load(&mut store, SectionPos::new(-1, -1, 0), solid);
    load(
        &mut store,
        SectionPos::new(-1, 0, -1),
        Section::empty(16).unwrap(),
    );
    for (pos, state) in [
        (BlockPos::new(-1, 0, -1), stone),
        (BlockPos::new(-16, 15, -16), glass),
        (BlockPos::new(-2, 3, -5), plant),
    ] {
        store.edit(pos, state).expect("edit works");
    }

    let mut entities = EntityStore::new(16).expect("side is valid");
    let first = entities.spawn(Position::new(-0.5, 1.25, -3.0)).unwrap();
    entities.spawn(Position::new(4.0, 17.0, 0.125)).unwrap();
    entities
        .components_mut(first)
        .unwrap()
        .insert(
            &schemas,
            &key("base:schema/inventory_content"),
            Default::default(),
        )
        .expect("component is valid");
    Fixture {
        registry,
        schemas,
        store,
        entities,
    }
}

#[test]
fn deleted_identities_are_not_reissued_after_codec_and_world_restore() {
    for delete_all in [false, true] {
        let mut world = fixture();
        let handles: Vec<_> = world.entities.iter().map(|(handle, _)| handle).collect();
        for handle in handles.into_iter().filter(|h| delete_all || h.slot == 1) {
            world.entities.despawn(handle).unwrap();
        }
        let watermark = world.entities.next_entity_id();
        let snapshot =
            Snapshot::capture(meta(), &world.store, &world.entities, &world.registry).unwrap();
        let decoded = decode_snapshot(&encode_snapshot(&snapshot).unwrap()).unwrap();
        assert_eq!(decoded.next_entity_id, watermark);
        let mut restored = decoded.restore(&world.registry, &world.schemas).unwrap();
        let handle = restored
            .entities
            .spawn(Position::new(1.0, 1.0, 1.0))
            .unwrap();
        assert_eq!(restored.entities.get(handle).unwrap().id.0, watermark);
        assert_eq!(restored.entities.next_entity_id(), watermark + 1);
    }
}

#[test]
fn allocator_rewind_and_exhaustion_are_preserved_at_recovery_boundaries() {
    let world = fixture();
    let snapshot =
        Snapshot::capture(meta(), &world.store, &world.entities, &world.registry).unwrap();
    for invalid in [0, 1, 2] {
        let mut bad = snapshot.clone();
        bad.next_entity_id = invalid;
        assert!(bad.restore(&world.registry, &world.schemas).is_err());
        let codec = codec();
        let mut out = Vec::new();
        use torse_codec::StorageCodec;
        assert!(
            codec
                .encode_snapshot(
                    &torse_types::WorldSnapshot {
                        meta: bad.meta,
                        state: bad
                    },
                    &mut out,
                    BUDGET
                )
                .is_err()
        );
        assert!(out.is_empty());
    }
    let mut exhausted = snapshot;
    exhausted.next_entity_id = u128::MAX;
    let decoded = decode_snapshot(&encode_snapshot(&exhausted).unwrap()).unwrap();
    let mut restored = decoded.restore(&world.registry, &world.schemas).unwrap();
    assert!(
        restored
            .entities
            .spawn(Position::new(0.0, 0.0, 0.0))
            .is_err()
    );
    assert_eq!(restored.entities.next_entity_id(), u128::MAX);
    assert!(restored.entities.restore_next_entity_id(3).is_err());
}

fn meta() -> SnapshotMeta {
    SnapshotMeta {
        cursor: CommitCursor {
            world: cursor().world,
            seq: CommitSeq(4),
        },
        compatibility: compatibility(),
        resume_at: resume(),
    }
}

#[test]
fn captured_snapshot_survives_encode_decode_and_restore() {
    let world = fixture();
    let captured =
        Snapshot::capture(meta(), &world.store, &world.entities, &world.registry).unwrap();
    assert_eq!(captured.sections.len(), 3);
    assert_eq!(
        captured.states.len(),
        3,
        "stone, glass, and one plant state"
    );

    let bytes = encode_snapshot(&captured).unwrap();
    let decoded = decode_snapshot(&bytes).unwrap();
    assert_eq!(decoded, captured);
    // Determinism: a decoded snapshot re-encodes to exactly the stored bytes.
    assert_eq!(encode_snapshot(&decoded).unwrap(), bytes);

    let restored = decoded.restore(&world.registry, &world.schemas).unwrap();
    for pos in world.store.positions() {
        assert_eq!(
            restored
                .store
                .section(pos)
                .map(|s| s.values().collect::<Vec<_>>()),
            world
                .store
                .section(pos)
                .map(|s| s.values().collect::<Vec<_>>()),
            "section {pos:?}"
        );
    }
    let positions: Vec<_> = restored
        .entities
        .iter()
        .map(|(_, e)| (e.id, e.position))
        .collect();
    let expected: Vec<_> = world
        .entities
        .iter()
        .map(|(_, e)| (e.id, e.position))
        .collect();
    assert_eq!(positions, expected);
    assert_eq!(restored.resume, meta().resume_at);
}

#[test]
fn captured_changes_replay_after_decoding() {
    let world = fixture();
    let snapshot =
        Snapshot::capture(meta(), &world.store, &world.entities, &world.registry).unwrap();
    let mut restored = snapshot.restore(&world.registry, &world.schemas).unwrap();

    let stone = world.registry.default_state(&key("base:stone")).unwrap();
    let plant = world.registry.state_of(&key("base:plant"), &[3]).unwrap();
    let edits = [
        BlockEdit {
            pos: BlockPos::new(-3, 2, -2),
            state: plant,
        },
        BlockEdit {
            pos: BlockPos::new(-1, 0, -1),
            state: torse_world::BlockStateId::EMPTY,
        },
        BlockEdit {
            pos: BlockPos::new(5, 20, 5),
            state: stone,
        },
    ];
    let commit = CommitMeta {
        cursor: CommitCursor {
            world: cursor().world,
            seq: CommitSeq(5),
        },
        previous: CommitSeq(4),
        compatibility: compatibility(),
        resume_at: resume(),
    };
    let mut changes = ChangeSet::from_edits(commit, &world.registry, &edits).unwrap();
    let (_, mover) = world.entities.iter().next().unwrap();
    changes.entity_moves.push(EntityMove {
        id: mover.id,
        previous_bits: mover.position.to_bits(),
        position_bits: Position::new(-0.5, 1.25, -2.0).to_bits(),
    });

    let bytes = encode_changes(&changes).unwrap();
    let decoded = decode_changes(&bytes).unwrap();
    assert_eq!(decoded, changes);
    assert_eq!(encode_changes(&decoded).unwrap(), bytes);

    decoded
        .apply_with_components(
            &mut restored.store,
            &mut restored.entities,
            &world.registry,
            &world.schemas,
        )
        .expect("decoded changes replay");
    assert_eq!(
        restored.store.query(BlockPos::new(-3, 2, -2)),
        TerrainQuery::Ready(plant)
    );
    assert_eq!(
        restored.store.query(BlockPos::new(-1, 0, -1)),
        TerrainQuery::Ready(torse_world::BlockStateId::EMPTY)
    );
    assert_eq!(
        restored.store.query(BlockPos::new(5, 20, 5)),
        TerrainQuery::Ready(stone)
    );
    let handle = restored.entities.handle_of(mover.id).unwrap();
    assert_eq!(
        restored.entities.get(handle).unwrap().position,
        Position::new(-0.5, 1.25, -2.0)
    );
}
