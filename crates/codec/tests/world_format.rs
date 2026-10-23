//! Byte-level checks of world format version 3 against an independent specification.
//!
//! The expected bytes below are assembled field by field from `WORLD.md`, without calling
//! the encoder. The encoder output, the stored fixture, and this specification must all
//! agree. Each spec line carries a label so corruption tests can patch one field at a time
//! and still know exactly which rule they exercise.
//!
//! Regenerate the fixtures after an intentional layout change with
//! `TORSE_BLESS_FIXTURES=1 cargo test -p torse-codec --test world_format`; a layout change
//! inside version 3 is a compatibility break and must also bump `WORLD_FORMAT_VERSION`.

mod world_support;

use std::fs;

use torse_codec::{CodecLimits, StorageCodec, WorldCodec};
use torse_types::{CommitSeq, EntityId, FaultCode, OutputBudget, WorldSnapshot};
use torse_world::{
    BlockStateId, ComponentEdit, ComponentValue, SnapshotCells, SnapshotComponent, SnapshotValue,
    WorldBounds, component_size,
};
use world_support::*;

// Spec builder ------------------------------------------------------------------------------

fn u8s(value: u8) -> Vec<u8> {
    vec![value]
}
fn u16s(value: u16) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}
fn u32s(value: u32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}
fn u64s(value: u64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}
fn i64s(value: i64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}
fn u128s(value: u128) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}
/// One-byte length followed by the `namespace:path` text.
fn keys(text: &str) -> Vec<u8> {
    [vec![text.len() as u8], text.as_bytes().to_vec()].concat()
}
fn position(x: f64, y: f64, z: f64) -> Vec<u8> {
    [x, y, z]
        .iter()
        .flat_map(|axis| axis.to_bits().to_le_bytes())
        .collect()
}
fn join(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// Ordered, labeled byte lines of one expected payload.
#[derive(Clone)]
struct Spec(Vec<(&'static str, Vec<u8>)>);

impl Spec {
    fn bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|(_, bytes)| bytes.clone()).collect()
    }

    fn index(&self, label: &str) -> usize {
        self.0
            .iter()
            .position(|(name, _)| *name == label)
            .unwrap_or_else(|| panic!("spec has no line {label}"))
    }

    fn replace(mut self, label: &str, bytes: Vec<u8>) -> Self {
        let index = self.index(label);
        self.0[index].1 = bytes;
        self
    }

    fn swap(mut self, left: &str, right: &str) -> Self {
        let (left, right) = (self.index(left), self.index(right));
        self.0.swap(left, right);
        self
    }

    fn push(mut self, label: &'static str, bytes: Vec<u8>) -> Self {
        let index = self
            .0
            .iter()
            .position(|(name, _)| *name == "allocator")
            .unwrap_or(self.0.len());
        self.0.insert(index, (label, bytes));
        self
    }

    /// Render as commented hex: one label line followed by its bytes.
    fn render(&self, title: &str) -> String {
        let mut text = format!("# {title}\n");
        for (label, bytes) in &self.0 {
            text.push_str(&format!("# {label}\n"));
            for byte in bytes {
                text.push_str(&format!("{byte:02x}"));
            }
            text.push('\n');
        }
        text
    }
}

/// Lines shared by both metadata layouts, with the B1 fixture values.
fn compatibility_lines() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("meta.schema", u32s(3)),
        ("meta.rules", vec![0x11; 32]),
        ("meta.content", vec![0x22; 32]),
        ("meta.resume", join(&[u64s(4), u16s(5), u64s(6)])),
    ]
}

fn snapshot_spec() -> Spec {
    let dense: Vec<Vec<u8>> = [0u16, 1, 3, 0, 0, 0, 0, 1].map(u16s).to_vec();
    let mut lines = vec![
        ("version", u16s(3)),
        ("meta.cursor", join(&[u128s(1), u64s(2)])),
    ];
    lines.extend(compatibility_lines());
    lines.extend([
        ("extent", join(&[u32s(2), i64s(-2), i64s(2)])),
        ("states", u32s(2)),
        ("state.1", join(&[u16s(1), keys("base:stone"), u32s(0)])),
        (
            "state.3",
            join(&[u16s(3), keys("base:plant"), u32s(1), u16s(2)]),
        ),
        ("sections", u32s(2)),
        (
            "section.west",
            join(&[i64s(-1), i64s(0), i64s(0), u8s(0), u16s(1)]),
        ),
        (
            "section.origin",
            join(&[i64s(0), i64s(0), i64s(0), u8s(1), dense.concat()]),
        ),
        ("entities", u32s(1)),
        ("entity.id", u128s(5)),
        ("entity.position", position(0.5, -1.0, 2.25)),
        ("components", u32s(1)),
        ("component.tags", join(&[keys("base:schema/tags"), u32s(7)])),
        ("field.a", join(&[keys("base:field/a"), u8s(0), u8s(1)])),
        ("field.b", join(&[keys("base:field/b"), u8s(1), u32s(7)])),
        ("field.c", join(&[keys("base:field/c"), u8s(2), i64s(-3)])),
        (
            "field.d",
            join(&[keys("base:field/d"), u8s(3), keys("base:stone")]),
        ),
        ("field.e", join(&[keys("base:field/e"), u8s(4), u128s(9)])),
        (
            "field.f",
            join(&[keys("base:field/f"), u8s(5), u32s(2), b"hi".to_vec()]),
        ),
        ("field.g", join(&[keys("base:field/g"), u8s(6), u32s(2)])),
        ("field.g.0", join(&[u8s(1), u32s(1)])),
        ("field.g.1", join(&[u8s(7), u32s(1)])),
        ("field.h", join(&[keys("base:field/h"), u8s(0), u8s(0)])),
        ("allocator", u128s(8)),
    ]);
    Spec(lines)
}

fn changes_spec() -> Spec {
    let mut lines = vec![
        ("version", u16s(3)),
        ("meta.cursor", join(&[u128s(1), u64s(2)])),
        ("meta.previous", u64s(1)),
    ];
    lines.extend(compatibility_lines());
    lines.extend([
        ("edits", u32s(2)),
        (
            "edit.plant",
            join(&[
                i64s(1),
                i64s(-1),
                i64s(2),
                u8s(1),
                keys("base:plant"),
                u32s(1),
                u16s(3),
            ]),
        ),
        ("edit.empty", join(&[i64s(-4), i64s(0), i64s(0), u8s(0)])),
        ("moves", u32s(1)),
        ("move.id", u128s(5)),
        ("move.previous", position(0.5, -1.0, 2.25)),
        ("move.position", position(1.5, -1.0, 2.25)),
        ("component_edits", u32s(3)),
        (
            "replace.target",
            join(&[u128s(5), keys("base:schema/tags")]),
        ),
        (
            "replace.previous",
            join(&[u8s(1), u32s(1), keys("base:field/b"), u8s(1), u32s(7)]),
        ),
        (
            "replace.fields",
            join(&[u8s(1), u32s(1), keys("base:field/b"), u8s(1), u32s(8)]),
        ),
        ("create.target", join(&[u128s(9), keys("base:schema/tags")])),
        ("create.previous", u8s(0)),
        ("create.fields", join(&[u8s(1), u32s(0)])),
        (
            "remove.target",
            join(&[u128s(5), keys("base:schema/other")]),
        ),
        (
            "remove.previous",
            join(&[
                u8s(1),
                u32s(1),
                keys("base:field/f"),
                u8s(5),
                u32s(2),
                b"ok".to_vec(),
            ]),
        ),
        ("remove.fields", u8s(0)),
        ("lifecycle", u8s(0)),
    ]);
    Spec(lines)
}

#[test]
fn invalid_allocator_watermarks_are_rejected_before_restore() {
    for watermark in [0, 1, 5] {
        assert_fault(
            decode_snapshot(
                &snapshot_spec()
                    .replace("allocator", u128s(watermark))
                    .bytes(),
            ),
            "allocator watermark",
        );
    }
}

/// Logical items in each sample, counted by hand from the rules in `WORLD.md`.
const SNAPSHOT_ITEMS: u32 = 1 + 2 + 2 + 1 + 1 + 7 + 2 + 1;
const CHANGES_ITEMS: u32 = 1 + 2 + 1 + 3 + 1 + 1 + 1;

// Fixture files -----------------------------------------------------------------------------

fn fixture_path(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Compare the stored fixture text with the rendered spec, writing it first when blessing.
fn check_fixture(name: &str, spec: &Spec, title: &str) {
    let path = fixture_path(name);
    let rendered = spec.render(title);
    if std::env::var_os("TORSE_BLESS_FIXTURES").is_some() {
        fs::write(&path, &rendered).expect("write fixture");
    }
    let stored = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
        .replace("\r\n", "\n");
    assert_eq!(stored, rendered, "{name} differs from the specification");
}

/// Hex digits with `#` comment lines; used to compare payload prefixes with B1 fixtures.
fn hex_fixture(name: &str) -> Vec<u8> {
    let text = fs::read_to_string(fixture_path(name)).expect("read fixture");
    let digits: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .flat_map(str::chars)
        .filter(|value| !value.is_whitespace())
        .collect();
    digits
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn assert_code(result: Result<impl std::fmt::Debug, torse_types::Fault>, code: FaultCode) {
    match result {
        Ok(value) => panic!("expected {code:?}, decoded {value:?}"),
        Err(error) => assert_eq!(error.code, code, "unexpected fault: {error}"),
    }
}

/// Like [`assert_code`], and also pin the reason. A patched field that shifted the layout
/// would usually still fail as invalid data, but for an unrelated reason; the message check
/// keeps each case testing the rule it names.
fn assert_fault(result: Result<impl std::fmt::Debug, torse_types::Fault>, reason: &str) {
    match result {
        Ok(value) => panic!("expected a fault containing {reason:?}, decoded {value:?}"),
        Err(error) => {
            assert_eq!(
                error.code,
                FaultCode::InvalidData,
                "unexpected fault: {error}"
            );
            assert!(
                error.context.contains(reason),
                "fault {error} does not mention {reason:?}"
            );
        }
    }
}

// Layout ------------------------------------------------------------------------------------

#[test]
fn snapshot_matches_specification_and_fixture() {
    let spec = snapshot_spec();
    check_fixture(
        "world_snapshot.hex",
        &spec,
        "world snapshot, format version 3",
    );
    let expected = spec.bytes();
    assert_eq!(encode_snapshot(&sample_snapshot()).unwrap(), expected);
    assert_eq!(decode_snapshot(&expected).unwrap(), sample_snapshot());
    assert_eq!(hex_fixture("world_snapshot.hex"), expected);
    // The metadata block is the unchanged B1 SnapshotMeta layout.
    assert_eq!(expected[2..112], hex_fixture("snapshot.hex"));
}

#[test]
fn changes_match_specification_and_fixture() {
    let spec = changes_spec();
    check_fixture(
        "world_changes.hex",
        &spec,
        "world change set, format version 3",
    );
    let expected = spec.bytes();
    assert_eq!(encode_changes(&sample_changes()).unwrap(), expected);
    assert_eq!(decode_changes(&expected).unwrap(), sample_changes());
    assert_eq!(hex_fixture("world_changes.hex"), expected);
    // The metadata block is the unchanged B1 CommitMeta layout.
    assert_eq!(expected[2..120], hex_fixture("commit.hex"));
}

#[test]
fn reencoding_decoded_bytes_is_identity() {
    let snapshot = snapshot_spec().bytes();
    let changes = changes_spec().bytes();
    assert_eq!(
        encode_snapshot(&decode_snapshot(&snapshot).unwrap()).unwrap(),
        snapshot
    );
    assert_eq!(
        encode_changes(&decode_changes(&changes).unwrap()).unwrap(),
        changes
    );
}

// Envelope-independent framing --------------------------------------------------------------

#[test]
fn every_truncation_fails_to_decode() {
    let snapshot = snapshot_spec().bytes();
    for length in 0..snapshot.len() {
        assert!(
            decode_snapshot(&snapshot[..length]).is_err(),
            "snapshot prefix {length}"
        );
    }
    let changes = changes_spec().bytes();
    for length in 0..changes.len() {
        assert!(
            decode_changes(&changes[..length]).is_err(),
            "changes prefix {length}"
        );
    }
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut snapshot = snapshot_spec().bytes();
    snapshot.push(0);
    assert_code(decode_snapshot(&snapshot), FaultCode::InvalidData);
    let mut changes = changes_spec().bytes();
    changes.push(0);
    assert_code(decode_changes(&changes), FaultCode::InvalidData);
}

#[test]
fn unknown_format_versions_are_incompatible() {
    for version in [0, 1, u16::MAX] {
        let snapshot = snapshot_spec().replace("version", u16s(version)).bytes();
        assert_code(decode_snapshot(&snapshot), FaultCode::Incompatible);
        let changes = changes_spec().replace("version", u16s(version)).bytes();
        assert_code(decode_changes(&changes), FaultCode::Incompatible);
    }
}

// Budgets -----------------------------------------------------------------------------------

#[test]
fn byte_budget_below_the_encoded_length_fails_without_output() {
    let snapshot = encode_snapshot(&sample_snapshot()).unwrap();
    for length in 0..snapshot.len() as u64 {
        let budget = OutputBudget::new(BUDGET.max_items, length);
        assert_code(
            encode_snapshot_with(&sample_snapshot(), budget),
            FaultCode::ResourceLimit,
        );
    }
    let exact = OutputBudget::new(BUDGET.max_items, snapshot.len() as u64);
    assert_eq!(
        encode_snapshot_with(&sample_snapshot(), exact).unwrap(),
        snapshot
    );

    let changes = encode_changes(&sample_changes()).unwrap();
    for length in 0..changes.len() as u64 {
        let budget = OutputBudget::new(BUDGET.max_items, length);
        assert_code(
            encode_changes_with(&sample_changes(), budget),
            FaultCode::ResourceLimit,
        );
    }
}

#[test]
fn item_budget_counts_every_collection_element() {
    let snapshot = snapshot_spec().bytes();
    let exact = OutputBudget::new(SNAPSHOT_ITEMS, BUDGET.max_bytes);
    let short = OutputBudget::new(SNAPSHOT_ITEMS - 1, BUDGET.max_bytes);
    assert!(encode_snapshot_with(&sample_snapshot(), exact).is_ok());
    assert_code(
        encode_snapshot_with(&sample_snapshot(), short),
        FaultCode::ResourceLimit,
    );
    assert!(decode_snapshot_with(&snapshot, exact).is_ok());
    assert_code(
        decode_snapshot_with(&snapshot, short),
        FaultCode::ResourceLimit,
    );

    let changes = changes_spec().bytes();
    let exact = OutputBudget::new(CHANGES_ITEMS, BUDGET.max_bytes);
    let short = OutputBudget::new(CHANGES_ITEMS - 1, BUDGET.max_bytes);
    assert!(encode_changes_with(&sample_changes(), exact).is_ok());
    assert_code(
        encode_changes_with(&sample_changes(), short),
        FaultCode::ResourceLimit,
    );
    assert!(decode_changes_with(&changes, exact).is_ok());
    assert_code(
        decode_changes_with(&changes, short),
        FaultCode::ResourceLimit,
    );
}

#[test]
fn decoding_charges_owned_allocations() {
    let snapshot = snapshot_spec().bytes();
    let small = OutputBudget::new(BUDGET.max_items, 64);
    assert_code(
        decode_snapshot_with(&snapshot, small),
        FaultCode::ResourceLimit,
    );
    let changes = changes_spec().bytes();
    assert_code(
        decode_changes_with(&changes, small),
        FaultCode::ResourceLimit,
    );
}

#[test]
fn hostile_counts_fail_before_allocation() {
    // Each count is checked against the remaining input before any budget charge or
    // allocation, so even the generous test budget never sees a four-billion-entry request.
    let max = u32::MAX;
    let snapshots = [
        ("state", snapshot_spec().replace("states", u32s(max))),
        (
            "state value",
            snapshot_spec().replace("state.3", join(&[u16s(3), keys("base:plant"), u32s(max)])),
        ),
        ("section", snapshot_spec().replace("sections", u32s(max))),
        ("entity", snapshot_spec().replace("entities", u32s(max))),
        (
            "component",
            snapshot_spec().replace("components", u32s(max)),
        ),
        (
            "field",
            snapshot_spec().replace(
                "component.tags",
                join(&[keys("base:schema/tags"), u32s(max)]),
            ),
        ),
        (
            "list element",
            snapshot_spec().replace("field.g", join(&[keys("base:field/g"), u8s(6), u32s(max)])),
        ),
        (
            "field",
            snapshot_spec().replace("field.g.1", join(&[u8s(7), u32s(max)])),
        ),
    ];
    for (what, spec) in snapshots {
        assert_fault(
            decode_snapshot(&spec.bytes()),
            &format!("{what} count {max} exceeds"),
        );
    }
    let changes = [
        ("block edit", changes_spec().replace("edits", u32s(max))),
        ("entity move", changes_spec().replace("moves", u32s(max))),
        (
            "component edit",
            changes_spec().replace("component_edits", u32s(max)),
        ),
        (
            "field",
            changes_spec().replace("create.fields", join(&[u8s(1), u32s(max)])),
        ),
    ];
    for (what, spec) in changes {
        assert_fault(
            decode_changes(&spec.bytes()),
            &format!("{what} count {max} exceeds"),
        );
    }
}

// Canonical order ---------------------------------------------------------------------------

#[test]
fn reordered_or_duplicated_snapshot_entries_are_rejected() {
    let cases = [
        ("state", snapshot_spec().swap("state.1", "state.3")),
        (
            "section",
            snapshot_spec().swap("section.west", "section.origin"),
        ),
        ("field", snapshot_spec().swap("field.a", "field.b")),
        (
            "field",
            snapshot_spec().replace("field.b", join(&[keys("base:field/a"), u8s(1), u32s(7)])),
        ),
        // A record entry follows the same rule as a top-level field.
        (
            "field",
            snapshot_spec()
                .replace("field.g.1", join(&[u8s(7), u32s(2)]))
                .push(
                    "field.h.again",
                    join(&[keys("base:field/h"), u8s(0), u8s(0)]),
                ),
        ),
    ];
    for (what, spec) in cases {
        let reason = format!("{what} entries are not in strictly ascending order");
        assert_fault(decode_snapshot(&spec.bytes()), &reason);
    }
}

#[test]
fn entity_identities_must_ascend() {
    let with_second = |id: u128| {
        snapshot_spec()
            .replace("entities", u32s(2))
            .push("entity2.id", u128s(id))
            .push("entity2.position", position(0.0, 0.0, 0.0))
            .push("entity2.components", u32s(0))
    };
    // Control: the same insertion with a larger identity is a valid snapshot.
    assert_eq!(
        decode_snapshot(&with_second(6).bytes())
            .unwrap()
            .entities
            .len(),
        2
    );
    let reason = "entity entries are not in strictly ascending order";
    assert_fault(decode_snapshot(&with_second(5).bytes()), reason);
    assert_fault(decode_snapshot(&with_second(4).bytes()), reason);
}

#[test]
fn repeated_moves_and_component_edits_are_rejected() {
    // A minimal record with two moves, so only the move identities differ between cases.
    let duplicate_move = Spec(vec![
        ("version", u16s(2)),
        ("meta", changes_spec().bytes()[2..120].to_vec()),
        ("edits", u32s(0)),
        ("moves", u32s(2)),
        (
            "move.a",
            join(&[u128s(5), position(0.0, 0.0, 0.0), position(1.0, 0.0, 0.0)]),
        ),
        (
            "move.b",
            join(&[u128s(5), position(1.0, 0.0, 0.0), position(2.0, 0.0, 0.0)]),
        ),
        ("component_edits", u32s(0)),
    ]);
    assert_fault(
        decode_changes(&duplicate_move.bytes()),
        "entity 5 moves more than once",
    );
    let distinct = duplicate_move.clone().replace(
        "move.b",
        join(&[u128s(6), position(1.0, 0.0, 0.0), position(2.0, 0.0, 0.0)]),
    );
    assert_eq!(
        decode_changes(&distinct.bytes())
            .unwrap()
            .entity_moves
            .len(),
        2
    );

    let duplicate_edit =
        changes_spec().replace("create.target", join(&[u128s(5), keys("base:schema/tags")]));
    assert_fault(
        decode_changes(&duplicate_edit.bytes()),
        "edits component base:schema/tags more than once",
    );
}

// Malformed scalars -------------------------------------------------------------------------

#[test]
fn malformed_snapshot_scalars_are_rejected() {
    let cases = [
        (
            "field.a",
            join(&[keys("base:field/a"), u8s(0), u8s(2)]),
            "flag 2 is neither 0 nor 1",
        ),
        (
            "field.f",
            join(&[keys("base:field/f"), u8s(5), u32s(2), vec![0xff, 0xfe]]),
            "text value is not UTF-8",
        ),
        (
            "field.e",
            join(&[keys("base:field/e"), u8s(4), u128s(0)]),
            "entity ids start at 1",
        ),
        (
            "field.a",
            join(&[keys("base:field/a"), u8s(8), u8s(1)]),
            "unknown value tag 8",
        ),
        (
            "field.a",
            join(&[keys("base:FIELD/a"), u8s(0), u8s(1)]),
            "content key \"base:FIELD/a\"",
        ),
        (
            "field.a",
            join(&[vec![12], vec![0xff; 12], u8s(0), u8s(1)]),
            "content key is not UTF-8",
        ),
        (
            "section.west",
            join(&[i64s(-1), i64s(0), i64s(0), u8s(2), u16s(1)]),
            "unknown section cell tag 2",
        ),
        (
            "state.1",
            join(&[u16s(0), keys("base:stone"), u32s(0)]),
            "lists the empty sentinel",
        ),
        ("entity.id", u128s(0), "entity ids start at 1"),
        (
            "extent",
            join(&[u32s(0), i64s(-2), i64s(2)]),
            "section side 0",
        ),
        (
            "extent",
            join(&[u32s(65), i64s(-2), i64s(2)]),
            "section side 65",
        ),
        ("extent", join(&[u32s(2), i64s(2), i64s(2)]), "are empty"),
    ];
    for (label, bytes, reason) in cases {
        let spec = snapshot_spec().replace(label, bytes);
        assert_fault(decode_snapshot(&spec.bytes()), reason);
    }
}

#[test]
fn malformed_change_scalars_are_rejected() {
    let cases = [
        (
            "edit.empty",
            join(&[i64s(-4), i64s(0), i64s(0), u8s(2)]),
            "unknown cell content tag 2",
        ),
        (
            "create.previous",
            u8s(2),
            "unknown component presence tag 2",
        ),
        ("move.id", u128s(0), "entity ids start at 1"),
        (
            "replace.target",
            join(&[u128s(0), keys("base:schema/tags")]),
            "entity ids start at 1",
        ),
        // A gap between previous and cursor fails commit metadata validation.
        ("meta.previous", u64s(0), "non-consecutive commit"),
    ];
    for (label, bytes, reason) in cases {
        let spec = changes_spec().replace(label, bytes);
        assert_fault(decode_changes(&spec.bytes()), reason);
    }
}

// Writer-side rules -------------------------------------------------------------------------

/// Apply one defect to the sample snapshot and expect the encoder to name it.
fn reject_snapshot(reason: &str, defect: impl FnOnce(&mut torse_world::Snapshot)) {
    let mut value = sample_snapshot();
    defect(&mut value);
    // `encode_snapshot` also asserts that the failed call left the output untouched.
    assert_fault(encode_snapshot(&value), reason);
}

fn reject_changes(reason: &str, defect: impl FnOnce(&mut torse_world::ChangeSet)) {
    let mut value = sample_changes();
    defect(&mut value);
    assert_fault(encode_changes(&value), reason);
}

#[test]
fn encoder_rejects_payloads_the_decoder_would_refuse() {
    let unordered = |what: &str| format!("{what} entries are not in strictly ascending order");
    reject_snapshot(&unordered("state"), |value| value.states.swap(0, 1));
    reject_snapshot(&unordered("section"), |value| value.sections.swap(0, 1));
    reject_snapshot(&unordered("field"), |value| {
        value.entities[0].components[0].fields.swap(0, 1);
    });
    reject_snapshot(&unordered("field"), |value| {
        let duplicate = value.entities[0].components[0].fields[0].clone();
        value.entities[0].components[0].fields.insert(1, duplicate);
    });
    reject_snapshot(&unordered("component"), |value| {
        let component = SnapshotComponent {
            schema: key("base:schema/zz"),
            fields: Vec::new(),
        };
        value.entities[0].components.insert(0, component);
    });
    reject_snapshot("holds 7 cells, expected 8", |value| {
        value.sections[1].cells = SnapshotCells::Dense(vec![BlockStateId(1); 7]);
    });
    reject_snapshot("section side 0", |value| value.side = 0);
    reject_snapshot("snapshot bounds are empty", |value| {
        value.bounds = WorldBounds { min_y: 2, max_y: 2 };
    });
    reject_snapshot("lists the empty sentinel", |value| {
        value.states[0].id = BlockStateId::EMPTY;
    });
    reject_snapshot("entity ids start at 1", |value| {
        value.entities[0].id = EntityId(0)
    });
    reject_snapshot("entity ids start at 1", |value| {
        value.entities[0].components[0].fields[4].1 = SnapshotValue::Entity(EntityId(0));
    });

    reject_changes("non-consecutive commit", |value| {
        value.meta.previous = CommitSeq(0)
    });
    reject_changes("entity 5 moves more than once", |value| {
        value.entity_moves.push(value.entity_moves[0]);
    });
    reject_changes("edits component base:schema/tags more than once", |value| {
        let repeat = ComponentEdit {
            fields: None,
            ..value.component_edits[0].clone()
        };
        value.component_edits.push(repeat);
    });
}

#[test]
fn wrapper_metadata_must_match_the_payload() {
    let codec = codec();
    let state = sample_snapshot();
    let mut meta = state.meta;
    meta.cursor.seq = CommitSeq(3);
    let mut out = vec![0xaa];
    let result = codec.encode_snapshot(&WorldSnapshot { meta, state }, &mut out, BUDGET);
    assert_code(result, FaultCode::InvalidData);
    assert_eq!(out, [0xaa]);

    let mut record = wrap_changes(&sample_changes());
    record.meta.resume_at.next_step = 7;
    assert_code(
        codec.encode_commit(&record, &mut out, BUDGET),
        FaultCode::InvalidData,
    );
    assert_eq!(out, [0xaa]);
}

#[test]
fn text_longer_than_the_field_limit_is_a_resource_limit() {
    let mut value = sample_snapshot();
    let long = "x".repeat(LIMITS.max_field_bytes as usize + 1);
    value.entities[0].components[0].fields[5].1 = SnapshotValue::Text(long);
    assert_code(encode_snapshot(&value), FaultCode::ResourceLimit);
}

// Nesting -----------------------------------------------------------------------------------

/// `levels` nested lists around an empty innermost list.
fn nested(levels: usize) -> ComponentValue {
    (1..levels).fold(ComponentValue::List(Vec::new()), |inner, _| {
        ComponentValue::List(vec![inner])
    })
}

fn nested_changes(levels: usize) -> torse_world::ChangeSet {
    let mut value = sample_changes();
    value.component_edits[1].fields = Some([(key("base:field/n"), nested(levels))].into());
    value
}

#[test]
fn minimum_depth_matches_what_the_world_accepts() {
    let depth = torse_codec::MIN_WORLD_DEPTH as usize;
    let deepest: torse_world::ComponentFields = [(key("base:field/n"), nested(depth))].into();
    let deeper: torse_world::ComponentFields = [(key("base:field/n"), nested(depth + 1))].into();
    // The world accepts exactly `MIN_WORLD_DEPTH` nested containers and no more.
    assert!(component_size(&deepest).is_ok());
    assert!(component_size(&deeper).is_err());

    let bytes = encode_changes(&nested_changes(depth)).unwrap();
    assert_eq!(decode_changes(&bytes).unwrap(), nested_changes(depth));
    assert_code(
        encode_changes(&nested_changes(depth + 1)),
        FaultCode::ResourceLimit,
    );

    // Bytes written under a deeper limit are still refused by the configured reader.
    let wide = WorldCodec::<Model>::new(CodecLimits {
        max_depth: LIMITS.max_depth + 1,
        ..LIMITS
    })
    .unwrap();
    let mut bytes = Vec::new();
    wide.encode_commit(
        &wrap_changes(&nested_changes(depth + 1)),
        &mut bytes,
        BUDGET,
    )
    .unwrap();
    assert_code(decode_changes(&bytes), FaultCode::InvalidData);
}

#[test]
fn shallow_limits_are_refused_at_construction() {
    let limits = CodecLimits {
        max_depth: torse_codec::MIN_WORLD_DEPTH - 1,
        ..LIMITS
    };
    assert_code(
        WorldCodec::<Model>::new(limits).map(|_| ()),
        FaultCode::InvalidData,
    );
}
