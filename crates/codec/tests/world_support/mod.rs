//! Shared model, limits, and sample payloads for the world codec tests.
//!
//! The sample payloads are deliberately tiny but touch every tag of the format: both cell
//! layouts, a finite state value, every component value kind, nested containers, and all
//! three shapes of component edit (replace, create, remove).

#![allow(dead_code)]

use torse_codec::{CodecLimits, MIN_WORLD_DEPTH, StorageCodec, WorldCodec};
use torse_types::{
    AuthorityData, BlockPos, CommitCursor, CommitMeta, CommitRecord, CommitSeq, Compatibility,
    ContentKey, Digest, EntityId, Fault, OutputBudget, ResumePoint, SectionPos, SnapshotMeta, Tick,
    WorldId, WorldSnapshot,
};
use torse_world::{
    BlockStateId, CellContent, ChangeEdit, ChangeSet, ComponentEdit, ComponentValue, EntityMove,
    Snapshot, SnapshotCells, SnapshotComponent, SnapshotEntity, SnapshotSection, SnapshotValue,
    StateEntry, WorldBounds,
};

/// Test-only authority model. Only the snapshot and change types matter to the codec.
pub struct Model;

impl AuthorityData for Model {
    type Snapshot = Snapshot;
    type Changes = ChangeSet;
    type Input = ();
    type Output = ();
    type ReplicaSnapshot = ();
    type ReplicaChanges = ();
}

pub const LIMITS: CodecLimits = CodecLimits {
    max_frame_bytes: 1 << 20,
    max_field_bytes: 1 << 16,
    max_depth: MIN_WORLD_DEPTH,
};

/// Generous enough for every sample; budget tests pass their own values.
pub const BUDGET: OutputBudget = OutputBudget {
    max_items: 1 << 16,
    max_bytes: 1 << 24,
};

pub fn codec() -> WorldCodec<Model> {
    WorldCodec::new(LIMITS).expect("test limits carry world values")
}

pub fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("test key is valid")
}

/// Same values as the B1 metadata fixtures, so payload prefixes can be compared with them.
pub fn compatibility() -> Compatibility {
    Compatibility {
        authority_schema: 3,
        rules_digest: Digest([0x11; 32]),
        content_digest: Digest([0x22; 32]),
    }
}

pub fn resume() -> ResumePoint {
    ResumePoint {
        tick: Tick(4),
        next_phase: 5,
        next_step: 6,
    }
}

pub fn cursor() -> CommitCursor {
    CommitCursor {
        world: WorldId(1),
        seq: CommitSeq(2),
    }
}

pub fn snapshot_meta() -> SnapshotMeta {
    SnapshotMeta {
        cursor: cursor(),
        compatibility: compatibility(),
        resume_at: resume(),
    }
}

pub fn commit_meta() -> CommitMeta {
    CommitMeta {
        cursor: cursor(),
        previous: CommitSeq(1),
        compatibility: compatibility(),
        resume_at: resume(),
    }
}

pub fn bits(x: f64, y: f64, z: f64) -> [u64; 3] {
    [x.to_bits(), y.to_bits(), z.to_bits()]
}

/// Side 2, two sections (uniform and dense), one entity with every value kind.
pub fn sample_snapshot() -> Snapshot {
    let dense = [0, 1, 3, 0, 0, 0, 0, 1].map(BlockStateId).to_vec();
    Snapshot {
        meta: snapshot_meta(),
        side: 2,
        bounds: WorldBounds::new(-2, 2).expect("bounds are valid"),
        states: vec![
            StateEntry {
                id: BlockStateId(1),
                block: key("base:stone"),
                values: Vec::new(),
            },
            StateEntry {
                id: BlockStateId(3),
                block: key("base:plant"),
                values: vec![2],
            },
        ],
        sections: vec![
            SnapshotSection {
                pos: SectionPos::new(-1, 0, 0),
                cells: SnapshotCells::Uniform(BlockStateId(1)),
            },
            SnapshotSection {
                pos: SectionPos::ZERO,
                cells: SnapshotCells::Dense(dense),
            },
        ],
        entities: vec![SnapshotEntity {
            id: EntityId(5),
            position_bits: bits(0.5, -1.0, 2.25),
            components: vec![SnapshotComponent {
                schema: key("base:schema/tags"),
                fields: vec![
                    (key("base:field/a"), SnapshotValue::Bool(true)),
                    (key("base:field/b"), SnapshotValue::U32(7)),
                    (key("base:field/c"), SnapshotValue::I64(-3)),
                    (
                        key("base:field/d"),
                        SnapshotValue::Content(key("base:stone")),
                    ),
                    (key("base:field/e"), SnapshotValue::Entity(EntityId(9))),
                    (key("base:field/f"), SnapshotValue::Text("hi".to_owned())),
                    (
                        key("base:field/g"),
                        SnapshotValue::List(vec![
                            SnapshotValue::U32(1),
                            SnapshotValue::Record(vec![(
                                key("base:field/h"),
                                SnapshotValue::Bool(false),
                            )]),
                        ]),
                    ),
                ],
            }],
        }],
    }
}

fn fields(entries: &[(&str, ComponentValue)]) -> torse_world::ComponentFields {
    entries
        .iter()
        .map(|(name, value)| (key(name), value.clone()))
        .collect()
}

/// Two block edits, one move, and a replace, a create, and a remove of components.
pub fn sample_changes() -> ChangeSet {
    ChangeSet {
        meta: commit_meta(),
        edits: vec![
            ChangeEdit {
                pos: BlockPos::new(1, -1, 2),
                content: CellContent::Block {
                    block: key("base:plant"),
                    values: vec![3],
                },
            },
            ChangeEdit {
                pos: BlockPos::new(-4, 0, 0),
                content: CellContent::Empty,
            },
        ],
        entity_moves: vec![EntityMove {
            id: EntityId(5),
            previous_bits: bits(0.5, -1.0, 2.25),
            position_bits: bits(1.5, -1.0, 2.25),
        }],
        component_edits: vec![
            ComponentEdit {
                id: EntityId(5),
                schema: key("base:schema/tags"),
                previous: Some(fields(&[("base:field/b", ComponentValue::U32(7))])),
                fields: Some(fields(&[("base:field/b", ComponentValue::U32(8))])),
            },
            ComponentEdit {
                id: EntityId(9),
                schema: key("base:schema/tags"),
                previous: None,
                fields: Some(fields(&[])),
            },
            ComponentEdit {
                id: EntityId(5),
                schema: key("base:schema/other"),
                previous: Some(fields(&[(
                    "base:field/f",
                    ComponentValue::Text("ok".to_owned()),
                )])),
                fields: None,
            },
        ],
    }
}

pub fn wrap_snapshot(value: &Snapshot) -> WorldSnapshot<Snapshot> {
    WorldSnapshot {
        meta: value.meta,
        state: value.clone(),
    }
}

pub fn wrap_changes(value: &ChangeSet) -> CommitRecord<ChangeSet> {
    CommitRecord {
        meta: value.meta,
        changes: value.clone(),
    }
}

/// Encode after a sentinel byte and prove that failure leaves the sentinel alone.
pub fn encode_snapshot_with(value: &Snapshot, budget: OutputBudget) -> Result<Vec<u8>, Fault> {
    let mut out = vec![0xaa];
    let result = codec().encode_snapshot(&wrap_snapshot(value), &mut out, budget);
    match result {
        Ok(()) => Ok(out.split_off(1)),
        Err(error) => {
            assert_eq!(out, [0xaa], "failed snapshot encode left bytes behind");
            Err(error)
        }
    }
}

pub fn encode_changes_with(value: &ChangeSet, budget: OutputBudget) -> Result<Vec<u8>, Fault> {
    let mut out = vec![0xaa];
    let result = codec().encode_commit(&wrap_changes(value), &mut out, budget);
    match result {
        Ok(()) => Ok(out.split_off(1)),
        Err(error) => {
            assert_eq!(out, [0xaa], "failed commit encode left bytes behind");
            Err(error)
        }
    }
}

pub fn encode_snapshot(value: &Snapshot) -> Result<Vec<u8>, Fault> {
    encode_snapshot_with(value, BUDGET)
}

pub fn encode_changes(value: &ChangeSet) -> Result<Vec<u8>, Fault> {
    encode_changes_with(value, BUDGET)
}

pub fn decode_snapshot(bytes: &[u8]) -> Result<Snapshot, Fault> {
    decode_snapshot_with(bytes, BUDGET)
}

pub fn decode_changes(bytes: &[u8]) -> Result<ChangeSet, Fault> {
    decode_changes_with(bytes, BUDGET)
}

/// Decoding also checks that the wrapper metadata was rebuilt from the payload.
pub fn decode_snapshot_with(bytes: &[u8], budget: OutputBudget) -> Result<Snapshot, Fault> {
    let value = codec().decode_snapshot(bytes, budget)?;
    assert_eq!(value.meta, value.state.meta);
    Ok(value.state)
}

pub fn decode_changes_with(bytes: &[u8], budget: OutputBudget) -> Result<ChangeSet, Fault> {
    let value = codec().decode_commit(bytes, budget)?;
    assert_eq!(value.meta, value.changes.meta);
    Ok(value.changes)
}
