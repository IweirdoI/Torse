//! Change-set body: the validated final effects of one committed transaction.
//!
//! A journal record replays exactly what the authority published. It carries stable content
//! for every written cell, collision-resolved entity positions, and component replacements
//! with their observed previous values. Replay never reruns collision, recipes, or Mod code,
//! so nothing here depends on rule implementations.
//!
//! Lists keep commit order because that order is semantic. Two uniqueness rules that replay
//! enforces are also enforced here, on both sides: an entity moves at most once per record,
//! and an (entity, schema) pair is edited at most once. Checking them before the record is
//! staged keeps the journal free of records that recovery would later refuse; a failure
//! here happens before publication, so the transaction is simply not committed.
//!
//! As with snapshots, content keys are not resolved and positions are not range checked.
//! `ChangeSet::apply_with_components` validates the whole group before applying any of it.

use std::collections::BTreeSet;

use torse_types::{BlockPos, ContentKey, EntityId};
use torse_world::{CellContent, ChangeEdit, ChangeSet, ComponentEdit, ComponentFields, EntityMove};

use crate::metadata::{COMMIT_META_BYTES, read_commit_meta, write_commit_meta};
use crate::value::{
    POSITION_BYTES, charge_vec, invalid, read_component_fields, read_entity, read_items, read_key,
    read_len, read_position, write_component_fields, write_entity, write_items, write_key,
    write_len, write_position,
};
use crate::{Decoder, EncodeResult, Encoder};

// Cell content tags.
const CELL_EMPTY: u8 = 0;
const CELL_BLOCK: u8 = 1;

// Presence tags for optional component fields.
const ABSENT: u8 = 0;
const PRESENT: u8 = 1;

/// Minimum widths used to reject impossible counts before allocation.
const MIN_EDIT_BYTES: usize = 24 + 1; // position, empty tag
const MIN_MOVE_BYTES: usize = 16 + 2 * POSITION_BYTES; // id, previous, position
const MIN_COMPONENT_EDIT_BYTES: usize = 16 + 1 + 1 + 1; // id, key length, two absent tags

/// Append one change-set body: metadata, then edits, moves, and component edits.
///
/// Uniqueness is checked before any byte is written, so a rejected record never leaves a
/// partial prefix even inside a caller-owned encoder.
pub(crate) fn write_changes(out: &mut Encoder<'_>, value: &ChangeSet) -> EncodeResult<()> {
    unique_moves(&value.entity_moves)?;
    unique_component_edits(&value.component_edits)?;
    validate_lifecycle_overlap(value)?;
    out.ensure(COMMIT_META_BYTES as u64)?;
    // Rejects gaps, duplicates, and sequence overflow before anything else is written.
    write_commit_meta(out, value.meta)?;
    write_edits(out, &value.edits)?;
    write_moves(out, &value.entity_moves)?;
    write_component_edits(out, &value.component_edits)?;
    crate::lifecycle::write_lifecycle(out, value.entity_lifecycle.as_ref())
}

pub(crate) fn read_changes(input: &mut Decoder<'_>, version: u16) -> EncodeResult<ChangeSet> {
    let meta = read_commit_meta(input)?;
    let edits = read_edits(input)?;
    let entity_moves = read_moves(input)?;
    let component_edits = read_component_edits(input)?;
    let entity_lifecycle = if version >= 3 {
        crate::lifecycle::read_lifecycle(input)?
    } else {
        None
    };
    unique_moves(&entity_moves)?;
    unique_component_edits(&component_edits)?;
    let changes = ChangeSet {
        meta,
        edits,
        entity_moves,
        component_edits,
        entity_lifecycle,
    };
    validate_lifecycle_overlap(&changes)?;
    Ok(changes)
}

fn validate_lifecycle_overlap(changes: &ChangeSet) -> EncodeResult<()> {
    if let Some(change) = &changes.entity_lifecycle {
        change
            .validate_structure()
            .map_err(|error| invalid(error.to_string()))?;
        if changes
            .entity_moves
            .iter()
            .any(|movement| change.conflicts_with(movement.id))
            || changes
                .component_edits
                .iter()
                .any(|edit| change.conflicts_with(edit.id))
        {
            return Err(invalid(
                "entity lifecycle overlaps a movement or component edit",
            ));
        }
    }
    Ok(())
}

fn unique_moves(moves: &[EntityMove]) -> EncodeResult<()> {
    let mut seen = BTreeSet::new();
    for movement in moves {
        if !seen.insert(movement.id) {
            return Err(invalid(format!(
                "entity {} moves more than once in one record",
                movement.id.0
            )));
        }
    }
    Ok(())
}

fn unique_component_edits(edits: &[ComponentEdit]) -> EncodeResult<()> {
    let mut seen: BTreeSet<(EntityId, &ContentKey)> = BTreeSet::new();
    for edit in edits {
        if !seen.insert((edit.id, &edit.schema)) {
            return Err(invalid(format!(
                "entity {} edits component {} more than once in one record",
                edit.id.0, edit.schema
            )));
        }
    }
    Ok(())
}

// Block edits -------------------------------------------------------------------------------

fn write_edits(out: &mut Encoder<'_>, edits: &[ChangeEdit]) -> EncodeResult<()> {
    write_items(out, edits.len(), "block edit")?;
    for edit in edits {
        out.write_i64(edit.pos.x)?;
        out.write_i64(edit.pos.y)?;
        out.write_i64(edit.pos.z)?;
        match &edit.content {
            CellContent::Empty => out.write_u8(CELL_EMPTY)?,
            CellContent::Block { block, values } => {
                out.write_u8(CELL_BLOCK)?;
                write_key(out, block)?;
                write_len(out, values.len(), "state value")?;
                for value in values {
                    out.write_u16(*value)?;
                }
            }
        }
    }
    Ok(())
}

fn read_edits(input: &mut Decoder<'_>) -> EncodeResult<Vec<ChangeEdit>> {
    let count = read_items(input, MIN_EDIT_BYTES, "block edit")?;
    charge_vec::<ChangeEdit>(input, count)?;
    let mut edits = Vec::with_capacity(count);
    for _ in 0..count {
        let pos = BlockPos::new(input.read_i64()?, input.read_i64()?, input.read_i64()?);
        let content = match input.read_u8()? {
            CELL_EMPTY => CellContent::Empty,
            CELL_BLOCK => {
                let block = read_key(input)?;
                let len = read_len(input, 2, "state value")?;
                charge_vec::<u16>(input, len)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(input.read_u16()?);
                }
                CellContent::Block { block, values }
            }
            tag => return Err(invalid(format!("unknown cell content tag {tag}"))),
        };
        edits.push(ChangeEdit { pos, content });
    }
    Ok(edits)
}

// Entity moves ------------------------------------------------------------------------------
//
// `previous_bits` is the position observed during preparation. Replay compares it with the
// restored entity before applying `position_bits`, which detects a journal that does not
// continue the state it is replayed onto.

fn write_moves(out: &mut Encoder<'_>, moves: &[EntityMove]) -> EncodeResult<()> {
    write_items(out, moves.len(), "entity move")?;
    for movement in moves {
        write_entity(out, movement.id)?;
        write_position(out, movement.previous_bits)?;
        write_position(out, movement.position_bits)?;
    }
    Ok(())
}

fn read_moves(input: &mut Decoder<'_>) -> EncodeResult<Vec<EntityMove>> {
    let count = read_items(input, MIN_MOVE_BYTES, "entity move")?;
    charge_vec::<EntityMove>(input, count)?;
    let mut moves = Vec::with_capacity(count);
    for _ in 0..count {
        moves.push(EntityMove {
            id: read_entity(input)?,
            previous_bits: read_position(input)?,
            position_bits: read_position(input)?,
        });
    }
    Ok(moves)
}

// Component edits ---------------------------------------------------------------------------
//
// `previous` and `fields` are each optional. Absent `previous` means the component must not
// exist yet; absent `fields` removes it. Both absent is structurally valid here and left to
// the world's component batch validation, which owns the meaning of an edit.

fn write_component_edits(out: &mut Encoder<'_>, edits: &[ComponentEdit]) -> EncodeResult<()> {
    write_items(out, edits.len(), "component edit")?;
    for edit in edits {
        write_entity(out, edit.id)?;
        write_key(out, &edit.schema)?;
        write_optional_fields(out, edit.previous.as_ref())?;
        write_optional_fields(out, edit.fields.as_ref())?;
    }
    Ok(())
}

fn write_optional_fields(
    out: &mut Encoder<'_>,
    fields: Option<&ComponentFields>,
) -> EncodeResult<()> {
    match fields {
        None => out.write_u8(ABSENT),
        Some(fields) => {
            out.write_u8(PRESENT)?;
            // Top-level fields are not nested; depth counts containers inside a value.
            write_component_fields(out, fields, 0)
        }
    }
}

fn read_component_edits(input: &mut Decoder<'_>) -> EncodeResult<Vec<ComponentEdit>> {
    let count = read_items(input, MIN_COMPONENT_EDIT_BYTES, "component edit")?;
    charge_vec::<ComponentEdit>(input, count)?;
    let mut edits = Vec::with_capacity(count);
    for _ in 0..count {
        edits.push(ComponentEdit {
            id: read_entity(input)?,
            schema: read_key(input)?,
            previous: read_optional_fields(input)?,
            fields: read_optional_fields(input)?,
        });
    }
    Ok(edits)
}

fn read_optional_fields(input: &mut Decoder<'_>) -> EncodeResult<Option<ComponentFields>> {
    match input.read_u8()? {
        ABSENT => Ok(None),
        PRESENT => Ok(Some(read_component_fields(input)?)),
        tag => Err(invalid(format!("unknown component presence tag {tag}"))),
    }
}
