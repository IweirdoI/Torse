//! Independent version-3 lifecycle bytes, corruption, budgets, and version-2 input.

mod world_support;

use torse_codec::StorageCodec;
use torse_types::{EntityId, OutputBudget};
use torse_world::{ComponentValue, EntityLifecycle, EntityState, Position};
use world_support::*;

fn hex_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    let hex = text
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<String>();
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn payload() -> torse_world::ChangeSet {
    let mut changes = sample_changes();
    changes.entity_lifecycle = Some(EntityLifecycle {
        previous_next_id: 10,
        next_id: 11,
        spawned: vec![EntityState {
            id: EntityId(10),
            position_bits: Position::new(-0.0, 2.0, -0.5).to_bits(),
            components: [(
                key("base:schema/tags"),
                [
                    (key("base:field/name"), ComponentValue::Text("new".into())),
                    (
                        key("base:field/slots"),
                        ComponentValue::List(vec![ComponentValue::Record(
                            [(key("base:field/count"), ComponentValue::U32(2))]
                                .into_iter()
                                .collect(),
                        )]),
                    ),
                ]
                .into_iter()
                .collect(),
            )]
            .into_iter()
            .collect(),
        }],
        despawned: vec![EntityState {
            id: EntityId(4),
            position_bits: Position::new(1.0, 2.0, 3.0).to_bits(),
            components: Default::default(),
        }],
    });
    changes
}

fn key_bytes(key: &str) -> Vec<u8> {
    [vec![key.len() as u8], key.as_bytes().to_vec()].concat()
}

fn position(values: [f64; 3]) -> Vec<u8> {
    values
        .into_iter()
        .flat_map(|value| value.to_bits().to_le_bytes())
        .collect()
}

fn specification() -> Vec<u8> {
    let mut prefix = hex_fixture("world_changes_v02.hex");
    prefix[..2].copy_from_slice(&3u16.to_le_bytes());
    let suffix = [
        vec![1], // present
        10u128.to_le_bytes().to_vec(),
        11u128.to_le_bytes().to_vec(),
        1u32.to_le_bytes().to_vec(), // spawn count
        10u128.to_le_bytes().to_vec(),
        position([-0.0, 2.0, -0.5]),
        1u32.to_le_bytes().to_vec(),
        key_bytes("base:schema/tags"),
        2u32.to_le_bytes().to_vec(), // two fields in key order
        key_bytes("base:field/name"),
        vec![5],
        3u32.to_le_bytes().to_vec(),
        b"new".to_vec(),
        key_bytes("base:field/slots"),
        vec![6],
        1u32.to_le_bytes().to_vec(),
        vec![7],
        1u32.to_le_bytes().to_vec(),
        key_bytes("base:field/count"),
        vec![1],
        2u32.to_le_bytes().to_vec(),
        1u32.to_le_bytes().to_vec(), // removal count
        4u128.to_le_bytes().to_vec(),
        position([1.0, 2.0, 3.0]),
        0u32.to_le_bytes().to_vec(),
    ]
    .concat();
    prefix.extend(suffix);
    prefix
}

#[test]
fn lifecycle_bytes_match_independent_fields_and_every_truncation_is_rejected() {
    let changes = payload();
    let expected = specification();
    assert_eq!(encode_changes(&changes).unwrap(), expected);
    assert_eq!(decode_changes(&expected).unwrap(), changes);
    for length in 0..expected.len() {
        assert!(
            decode_changes(&expected[..length]).is_err(),
            "length {length}"
        );
    }
    let path = format!(
        "{}/tests/fixtures/world_lifecycle.hex",
        env!("CARGO_MANIFEST_DIR")
    );
    let rendered = format!(
        "# World lifecycle, format version 3\n{}\n",
        expected
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    if std::env::var_os("TORSE_BLESS_FIXTURES").is_some() {
        std::fs::write(&path, &rendered).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(path).unwrap().replace("\r\n", "\n"),
        rendered
    );
}

#[test]
fn lifecycle_budget_failure_rolls_back_and_counts_nested_creation_fields() {
    let changes = payload();
    let expected = specification();
    for bytes in 0..expected.len() as u64 {
        let mut out = vec![9, 8];
        assert!(
            codec()
                .encode_commit(
                    &wrap_changes(&changes),
                    &mut out,
                    OutputBudget::new(BUDGET.max_items, bytes)
                )
                .is_err()
        );
        assert_eq!(out, [9, 8]);
    }
    // Ten existing items, plus a spawn, schema, two fields, list item, record field, removal.
    for items in 0..17 {
        let mut out = vec![9, 8];
        assert!(
            codec()
                .encode_commit(
                    &wrap_changes(&changes),
                    &mut out,
                    OutputBudget::new(items, BUDGET.max_bytes)
                )
                .is_err()
        );
        assert_eq!(out, [9, 8]);
        assert!(
            codec()
                .decode_commit(&expected, OutputBudget::new(items, BUDGET.max_bytes))
                .is_err()
        );
    }
    let mut out = Vec::new();
    codec()
        .encode_commit(
            &wrap_changes(&changes),
            &mut out,
            OutputBudget::new(17, expected.len() as u64),
        )
        .unwrap();
    assert_eq!(out, expected);
    assert_eq!(
        codec()
            .decode_commit(&expected, OutputBudget::new(17, BUDGET.max_bytes))
            .unwrap()
            .changes,
        changes
    );
}

#[test]
fn invalid_lifecycle_arithmetic_duplicates_overlap_and_tags_are_rejected() {
    let original = payload();
    for variant in 0..5 {
        let mut changes = original.clone();
        let change = changes.entity_lifecycle.as_mut().unwrap();
        match variant {
            0 => change.previous_next_id = 0,
            1 => change.next_id += 1,
            2 => change.spawned[0].id = EntityId(11),
            3 => change.despawned[0].id = EntityId(10),
            _ => change.despawned[0].id = EntityId(5), // overlaps an existing move
        }
        let mut out = vec![1, 2];
        assert!(
            codec()
                .encode_commit(&wrap_changes(&changes), &mut out, BUDGET)
                .is_err()
        );
        assert_eq!(out, [1, 2]);
    }
    let offset = hex_fixture("world_changes_v02.hex").len();
    for (position, replacement) in [
        (offset, vec![9]),
        (offset + 1, 0u128.to_le_bytes().to_vec()),
        (offset + 17, 12u128.to_le_bytes().to_vec()),
        (offset + 33, u32::MAX.to_le_bytes().to_vec()),
        (offset + 37, 11u128.to_le_bytes().to_vec()),
    ] {
        let mut bytes = specification();
        bytes[position..position + replacement.len()].copy_from_slice(&replacement);
        assert!(decode_changes(&bytes).is_err());
    }
}

#[test]
fn version_two_fixtures_remain_readable_and_reencode_as_version_three() {
    let snapshot = decode_snapshot(&hex_fixture("world_snapshot_v02.hex")).unwrap();
    assert_eq!(snapshot, sample_snapshot());
    assert_eq!(&encode_snapshot(&snapshot).unwrap()[..2], &[3, 0]);
    let changes = decode_changes(&hex_fixture("world_changes_v02.hex")).unwrap();
    assert_eq!(changes, sample_changes());
    assert!(changes.entity_lifecycle.is_none());
    assert_eq!(&encode_changes(&changes).unwrap()[..2], &[3, 0]);
}
