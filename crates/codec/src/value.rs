//! Shared vocabulary of the world payload format.
//!
//! Snapshot and change-set bodies are built from the same small set of pieces: stable
//! content keys, bounded counts, entity identities, exact position bits, tagged component
//! values, and canonically ordered field maps. Keeping them in one module guarantees that
//! both record kinds encode the same logical value with identical bytes.
//!
//! Every reader follows the same order of checks:
//!
//! 1. Read the declared length.
//! 2. Prove that the remaining input can hold that many elements of the smallest legal width.
//! 3. Charge the item and byte budgets.
//! 4. Only then allocate.
//!
//! A hostile count therefore fails before any allocation proportional to it. Writers apply
//! the same canonical-order, range, and depth rules as readers, so a record this codec writes
//! is always a record it can read back under the same limits.

use std::collections::BTreeMap;

use torse_types::{ContentKey, EntityId, Fault};
use torse_world::{ComponentFields, ComponentValue, SnapshotValue};

use crate::{Decoder, EncodeResult, Encoder};

/// Content keys use a one-byte length prefix. `ContentKey::parse` caps a key at 193 bytes,
/// so the prefix always suffices. Raising the key limit above 255 requires a new format
/// version rather than a silent widening of this field.
const MAX_KEY_BYTES: usize = u8::MAX as usize;

// Value tags. The numbers are part of the persistent format; a retired tag is never reused.
const TAG_BOOL: u8 = 0;
const TAG_U32: u8 = 1;
const TAG_I64: u8 = 2;
const TAG_CONTENT: u8 = 3;
const TAG_ENTITY: u8 = 4;
const TAG_TEXT: u8 = 5;
const TAG_LIST: u8 = 6;
const TAG_RECORD: u8 = 7;

/// Smallest encoded value: a tag followed by a one-byte boolean.
const MIN_VALUE_BYTES: usize = 2;
/// Smallest keyed entry: the key length byte plus the smallest value. Real keys are never
/// empty, so this underestimates; a count check only needs a lower bound.
const MIN_ENTRY_BYTES: usize = 1 + MIN_VALUE_BYTES;

/// Width of one position: three IEEE-754 bit patterns.
pub(crate) const POSITION_BYTES: usize = 24;

pub(crate) fn invalid(context: impl Into<String>) -> Fault {
    Fault::invalid_data(context)
}

/// Reject an element that does not strictly follow its predecessor.
///
/// Strictness matters: an equal neighbor is a duplicate, and a later map insertion would
/// otherwise merge it silently. The comparison is the type's `Ord`, which is the order A
/// registered for every sorted collection.
pub(crate) fn ascending<T: Ord>(previous: Option<&T>, next: &T, what: &str) -> EncodeResult<()> {
    match previous {
        Some(previous) if previous >= next => Err(invalid(format!(
            "{what} entries are not in strictly ascending order"
        ))),
        _ => Ok(()),
    }
}

fn length(len: usize, what: &str) -> EncodeResult<u32> {
    u32::try_from(len)
        .map_err(|_| Fault::resource_limit(format!("{what} count {len} does not fit in u32")))
}

/// Write the length of a collection whose elements are logical items.
///
/// The whole collection is charged against the item budget before the first element is
/// written, matching [`read_items`] so encode and decode count items identically.
pub(crate) fn write_items(out: &mut Encoder<'_>, len: usize, what: &str) -> EncodeResult<()> {
    let count = length(len, what)?;
    out.count_items(count)?;
    out.write_u32(count)
}

/// Write the length of a raw scalar array, which is bounded by bytes rather than items.
pub(crate) fn write_len(out: &mut Encoder<'_>, len: usize, what: &str) -> EncodeResult<()> {
    let count = length(len, what)?;
    out.write_u32(count)
}

/// Read a declared count and prove the remaining input could hold it.
fn read_count(input: &mut Decoder<'_>, min_bytes: usize, what: &str) -> EncodeResult<usize> {
    let count = input.read_u32()?;
    let needed = u64::from(count) * min_bytes as u64;
    if needed > input.remaining() as u64 {
        return Err(invalid(format!(
            "{what} count {count} exceeds the remaining {} bytes",
            input.remaining()
        )));
    }
    Ok(count as usize)
}

/// Read the length of an item collection and charge every item before allocation.
pub(crate) fn read_items(
    input: &mut Decoder<'_>,
    min_bytes: usize,
    what: &str,
) -> EncodeResult<usize> {
    let count = read_count(input, min_bytes, what)?;
    // `read_count` returned a value that came from a u32, so the cast is lossless.
    input.charge_items(count as u32)?;
    Ok(count)
}

/// Read the length of a raw scalar array of `width`-byte elements.
pub(crate) fn read_len(input: &mut Decoder<'_>, width: usize, what: &str) -> EncodeResult<usize> {
    read_count(input, width, what)
}

/// Charge the owned storage of a vector of `len` elements of type `T`.
///
/// Allocator overhead and container headers are not counted; the budget bounds payload
/// storage, as documented in the storage crate.
pub(crate) fn charge_vec<T>(input: &mut Decoder<'_>, len: usize) -> EncodeResult<()> {
    let bytes = (len as u64)
        .checked_mul(size_of::<T>() as u64)
        .ok_or_else(|| Fault::resource_limit("decoded allocation size overflows u64"))?;
    input.charge_bytes(bytes)
}

/// Write `namespace:path` with a one-byte length prefix, without building a temporary string.
pub(crate) fn write_key(out: &mut Encoder<'_>, key: &ContentKey) -> EncodeResult<()> {
    let len = key.namespace().len() + 1 + key.path().len();
    if len > MAX_KEY_BYTES {
        return Err(Fault::resource_limit(format!(
            "content key length {len} exceeds {MAX_KEY_BYTES}"
        )));
    }
    out.ensure(1 + len as u64)?;
    out.write_u8(len as u8)?;
    out.write(key.namespace().as_bytes())?;
    out.write(b":")?;
    out.write(key.path().as_bytes())
}

/// Read a key and validate it with the same parser every other boundary uses.
///
/// `ContentKey::parse` rejects non-canonical spellings instead of normalizing them, so a
/// decoded key always re-encodes to the bytes it was read from.
pub(crate) fn read_key(input: &mut Decoder<'_>) -> EncodeResult<ContentKey> {
    let len = usize::from(input.read_u8()?);
    let bytes = input.read(len)?;
    // The key owns its namespace and path; the encoded length bounds both strings.
    input.charge_bytes(len as u64)?;
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("content key is not UTF-8"))?;
    ContentKey::parse(text).map_err(|error| invalid(format!("content key {text:?}: {error}")))
}

/// Booleans are exactly 0 or 1 so that every value has a single encoding.
pub(crate) fn write_bool(out: &mut Encoder<'_>, value: bool) -> EncodeResult<()> {
    out.write_u8(u8::from(value))
}

pub(crate) fn read_bool(input: &mut Decoder<'_>, what: &str) -> EncodeResult<bool> {
    match input.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(invalid(format!("{what} flag {other} is neither 0 nor 1"))),
    }
}

/// Persistent identities start at 1; zero is the reserved invalid sentinel.
pub(crate) fn write_entity(out: &mut Encoder<'_>, id: EntityId) -> EncodeResult<()> {
    id.validate().map_err(|error| invalid(error.to_string()))?;
    out.write_u128(id.0)
}

pub(crate) fn read_entity(input: &mut Decoder<'_>) -> EncodeResult<EntityId> {
    let id = EntityId(input.read_u128()?);
    id.validate().map_err(|error| invalid(error.to_string()))?;
    Ok(id)
}

/// Positions are stored as raw bit patterns. The codec does not judge finiteness or range;
/// the authority validates positions when it restores or replays them.
pub(crate) fn write_position(out: &mut Encoder<'_>, bits: [u64; 3]) -> EncodeResult<()> {
    out.ensure(POSITION_BYTES as u64)?;
    for axis in bits {
        out.write_u64(axis)?;
    }
    Ok(())
}

pub(crate) fn read_position(input: &mut Decoder<'_>) -> EncodeResult<[u64; 3]> {
    Ok([input.read_u64()?, input.read_u64()?, input.read_u64()?])
}

/// UTF-8 text with a u32 length prefix bounded by the configured field limit.
fn read_text(input: &mut Decoder<'_>) -> EncodeResult<String> {
    let bytes = input.read_field()?;
    input.charge_bytes(bytes.len() as u64)?;
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("text value is not UTF-8"))?;
    Ok(text.to_owned())
}

/// Fail before writing a container that the reader would refuse to enter.
///
/// `depth` counts enclosing containers. The decoder enters one level per list or record and
/// fails once `max_depth` levels are open, so the writer applies the identical bound.
fn check_depth(out: &Encoder<'_>, depth: u16) -> EncodeResult<()> {
    let max = out.limits().max_depth;
    if depth >= max {
        return Err(Fault::resource_limit(format!(
            "nested value depth exceeds limit {max}"
        )));
    }
    Ok(())
}

// Snapshot values ---------------------------------------------------------------------------
//
// `SnapshotValue` and `ComponentValue` share one wire layout. They are encoded by separate
// functions only because their record containers differ: snapshots keep a sorted vector,
// change sets keep a `BTreeMap`. The tags, widths, and ordering rules are identical.

pub(crate) fn write_snapshot_value(
    out: &mut Encoder<'_>,
    value: &SnapshotValue,
    depth: u16,
) -> EncodeResult<()> {
    match value {
        SnapshotValue::Bool(flag) => {
            out.write_u8(TAG_BOOL)?;
            write_bool(out, *flag)
        }
        SnapshotValue::U32(number) => {
            out.write_u8(TAG_U32)?;
            out.write_u32(*number)
        }
        SnapshotValue::I64(number) => {
            out.write_u8(TAG_I64)?;
            out.write_i64(*number)
        }
        SnapshotValue::Content(key) => {
            out.write_u8(TAG_CONTENT)?;
            write_key(out, key)
        }
        SnapshotValue::Entity(id) => {
            out.write_u8(TAG_ENTITY)?;
            write_entity(out, *id)
        }
        SnapshotValue::Text(text) => {
            out.write_u8(TAG_TEXT)?;
            out.write_field(text.as_bytes())
        }
        SnapshotValue::List(items) => {
            check_depth(out, depth)?;
            out.write_u8(TAG_LIST)?;
            write_items(out, items.len(), "list element")?;
            for item in items {
                write_snapshot_value(out, item, depth + 1)?;
            }
            Ok(())
        }
        SnapshotValue::Record(entries) => {
            check_depth(out, depth)?;
            out.write_u8(TAG_RECORD)?;
            write_snapshot_entries(out, entries, depth + 1)
        }
    }
}

/// Write component fields or record entries; keys must be strictly ascending.
pub(crate) fn write_snapshot_entries(
    out: &mut Encoder<'_>,
    entries: &[(ContentKey, SnapshotValue)],
    depth: u16,
) -> EncodeResult<()> {
    write_items(out, entries.len(), "field")?;
    let mut previous = None;
    for (key, value) in entries {
        ascending(previous, key, "field")?;
        previous = Some(key);
        write_key(out, key)?;
        write_snapshot_value(out, value, depth)?;
    }
    Ok(())
}

pub(crate) fn read_snapshot_value(input: &mut Decoder<'_>) -> EncodeResult<SnapshotValue> {
    Ok(match input.read_u8()? {
        TAG_BOOL => SnapshotValue::Bool(read_bool(input, "boolean value")?),
        TAG_U32 => SnapshotValue::U32(input.read_u32()?),
        TAG_I64 => SnapshotValue::I64(input.read_i64()?),
        TAG_CONTENT => SnapshotValue::Content(read_key(input)?),
        TAG_ENTITY => SnapshotValue::Entity(read_entity(input)?),
        TAG_TEXT => SnapshotValue::Text(read_text(input)?),
        TAG_LIST => {
            input.enter()?;
            let count = read_items(input, MIN_VALUE_BYTES, "list element")?;
            charge_vec::<SnapshotValue>(input, count)?;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(read_snapshot_value(input)?);
            }
            input.leave();
            SnapshotValue::List(items)
        }
        TAG_RECORD => {
            input.enter()?;
            let entries = read_snapshot_entries(input)?;
            input.leave();
            SnapshotValue::Record(entries)
        }
        tag => return Err(invalid(format!("unknown value tag {tag}"))),
    })
}

pub(crate) fn read_snapshot_entries(
    input: &mut Decoder<'_>,
) -> EncodeResult<Vec<(ContentKey, SnapshotValue)>> {
    let count = read_items(input, MIN_ENTRY_BYTES, "field")?;
    charge_vec::<(ContentKey, SnapshotValue)>(input, count)?;
    let mut entries: Vec<(ContentKey, SnapshotValue)> = Vec::with_capacity(count);
    for _ in 0..count {
        let key = read_key(input)?;
        ascending(entries.last().map(|(key, _)| key), &key, "field")?;
        let value = read_snapshot_value(input)?;
        entries.push((key, value));
    }
    Ok(entries)
}

// Component values --------------------------------------------------------------------------

pub(crate) fn write_component_value(
    out: &mut Encoder<'_>,
    value: &ComponentValue,
    depth: u16,
) -> EncodeResult<()> {
    match value {
        ComponentValue::Bool(flag) => {
            out.write_u8(TAG_BOOL)?;
            write_bool(out, *flag)
        }
        ComponentValue::U32(number) => {
            out.write_u8(TAG_U32)?;
            out.write_u32(*number)
        }
        ComponentValue::I64(number) => {
            out.write_u8(TAG_I64)?;
            out.write_i64(*number)
        }
        ComponentValue::Content(key) => {
            out.write_u8(TAG_CONTENT)?;
            write_key(out, key)
        }
        ComponentValue::Entity(id) => {
            out.write_u8(TAG_ENTITY)?;
            write_entity(out, *id)
        }
        ComponentValue::Text(text) => {
            out.write_u8(TAG_TEXT)?;
            out.write_field(text.as_bytes())
        }
        ComponentValue::List(items) => {
            check_depth(out, depth)?;
            out.write_u8(TAG_LIST)?;
            write_items(out, items.len(), "list element")?;
            for item in items {
                write_component_value(out, item, depth + 1)?;
            }
            Ok(())
        }
        ComponentValue::Record(entries) => {
            check_depth(out, depth)?;
            out.write_u8(TAG_RECORD)?;
            write_component_fields(out, entries, depth + 1)
        }
    }
}

/// Write a field map. `BTreeMap` iteration is already strictly ascending and duplicate-free,
/// so no order check is needed on this side; the reader still checks.
pub(crate) fn write_component_fields(
    out: &mut Encoder<'_>,
    fields: &ComponentFields,
    depth: u16,
) -> EncodeResult<()> {
    write_items(out, fields.len(), "field")?;
    for (key, value) in fields {
        write_key(out, key)?;
        write_component_value(out, value, depth)?;
    }
    Ok(())
}

pub(crate) fn read_component_value(input: &mut Decoder<'_>) -> EncodeResult<ComponentValue> {
    Ok(match input.read_u8()? {
        TAG_BOOL => ComponentValue::Bool(read_bool(input, "boolean value")?),
        TAG_U32 => ComponentValue::U32(input.read_u32()?),
        TAG_I64 => ComponentValue::I64(input.read_i64()?),
        TAG_CONTENT => ComponentValue::Content(read_key(input)?),
        TAG_ENTITY => ComponentValue::Entity(read_entity(input)?),
        TAG_TEXT => ComponentValue::Text(read_text(input)?),
        TAG_LIST => {
            input.enter()?;
            let count = read_items(input, MIN_VALUE_BYTES, "list element")?;
            charge_vec::<ComponentValue>(input, count)?;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(read_component_value(input)?);
            }
            input.leave();
            ComponentValue::List(items)
        }
        TAG_RECORD => {
            input.enter()?;
            let entries = read_component_fields(input)?;
            input.leave();
            ComponentValue::Record(entries)
        }
        tag => return Err(invalid(format!("unknown value tag {tag}"))),
    })
}

/// Read a field map, rejecting any key that does not strictly follow the previous one.
///
/// Without this check a duplicated key would be collapsed by `BTreeMap::insert`, and the
/// decoded value would differ from the stored bytes without any error.
pub(crate) fn read_component_fields(input: &mut Decoder<'_>) -> EncodeResult<ComponentFields> {
    let count = read_items(input, MIN_ENTRY_BYTES, "field")?;
    // Map nodes are charged by their payload size, like vector elements.
    charge_vec::<(ContentKey, ComponentValue)>(input, count)?;
    let mut fields = BTreeMap::new();
    for _ in 0..count {
        let key = read_key(input)?;
        ascending(fields.last_key_value().map(|(key, _)| key), &key, "field")?;
        let value = read_component_value(input)?;
        fields.insert(key, value);
    }
    Ok(fields)
}
