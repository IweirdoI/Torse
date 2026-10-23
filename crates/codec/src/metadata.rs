//! Fixed-width authority metadata, encoded independently of Rust's memory layout.
//!
//! Every integer uses little-endian byte order. Digests are copied in their declared order,
//! without conversion to integers. These functions neither allocate nor charge logical items:
//! the enclosing snapshot or commit codec owns that accounting. Resume positions remain opaque
//! to I/O; their phase and step semantics belong to the authority's locked rule graph.
//! Failed reads may consume a prefix; callers discard the enclosing decode on any error.

use torse_types::{
    Binding, CommitCursor, CommitMeta, CommitSeq, Compatibility, Digest, ResumePoint, SnapshotMeta,
    Tick, WorldEpoch, WorldId,
};

use crate::{Decoder, EncodeResult, Encoder};

/// Encoded widths exclude any enclosing record header or domain payload.
pub const COMPATIBILITY_BYTES: usize = 68;
pub const BINDING_BYTES: usize = 100;
pub const RESUME_POINT_BYTES: usize = 18;
pub const SNAPSHOT_META_BYTES: usize = 110;
pub const COMMIT_META_BYTES: usize = 118;

/// Append the schema version, rules digest, then authority content digest.
pub fn write_compatibility(out: &mut Encoder<'_>, value: Compatibility) -> EncodeResult<()> {
    out.ensure(COMPATIBILITY_BYTES as u64)?;
    out.write_u32(value.authority_schema)?;
    out.write(&value.rules_digest.0)?;
    out.write(&value.content_digest.0)
}

pub fn read_compatibility(input: &mut Decoder<'_>) -> EncodeResult<Compatibility> {
    let authority_schema = input.read_u32()?;
    let mut rules_digest = [0; 32];
    let mut content_digest = [0; 32];
    rules_digest.copy_from_slice(input.read(32)?);
    content_digest.copy_from_slice(input.read(32)?);
    Ok(Compatibility {
        authority_schema,
        rules_digest: Digest(rules_digest),
        content_digest: Digest(content_digest),
    })
}

/// Bindings include an epoch for live instances. Persisted snapshot and commit metadata omit it.
pub fn write_binding(out: &mut Encoder<'_>, value: Binding) -> EncodeResult<()> {
    out.ensure(BINDING_BYTES as u64)?;
    out.write_u128(value.world.0)?;
    out.write_u128(value.epoch.0)?;
    write_compatibility(out, value.compatibility)
}

pub fn read_binding(input: &mut Decoder<'_>) -> EncodeResult<Binding> {
    Ok(Binding {
        world: WorldId(input.read_u128()?),
        epoch: WorldEpoch(input.read_u128()?),
        compatibility: read_compatibility(input)?,
    })
}

pub fn write_resume_point(out: &mut Encoder<'_>, value: ResumePoint) -> EncodeResult<()> {
    out.ensure(RESUME_POINT_BYTES as u64)?;
    out.write_u64(value.tick.0)?;
    out.write_u16(value.next_phase)?;
    out.write_u64(value.next_step)
}

pub fn read_resume_point(input: &mut Decoder<'_>) -> EncodeResult<ResumePoint> {
    Ok(ResumePoint {
        tick: Tick(input.read_u64()?),
        next_phase: input.read_u16()?,
        next_step: input.read_u64()?,
    })
}

fn write_cursor(out: &mut Encoder<'_>, value: CommitCursor) -> EncodeResult<()> {
    out.write_u128(value.world.0)?;
    out.write_u64(value.seq.0)
}

fn read_cursor(input: &mut Decoder<'_>) -> EncodeResult<CommitCursor> {
    Ok(CommitCursor {
        world: WorldId(input.read_u128()?),
        seq: CommitSeq(input.read_u64()?),
    })
}

/// A checkpoint may describe position zero, which is the complete initial baseline.
pub fn write_snapshot_meta(out: &mut Encoder<'_>, value: SnapshotMeta) -> EncodeResult<()> {
    out.ensure(SNAPSHOT_META_BYTES as u64)?;
    write_cursor(out, value.cursor)?;
    write_compatibility(out, value.compatibility)?;
    write_resume_point(out, value.resume_at)
}

pub fn read_snapshot_meta(input: &mut Decoder<'_>) -> EncodeResult<SnapshotMeta> {
    Ok(SnapshotMeta {
        cursor: read_cursor(input)?,
        compatibility: read_compatibility(input)?,
        resume_at: read_resume_point(input)?,
    })
}

/// Reject gaps, duplicate positions, and wraparound before appending any metadata.
pub fn write_commit_meta(out: &mut Encoder<'_>, value: CommitMeta) -> EncodeResult<()> {
    value.validate()?;
    out.ensure(COMMIT_META_BYTES as u64)?;
    write_cursor(out, value.cursor)?;
    out.write_u64(value.previous.0)?;
    write_compatibility(out, value.compatibility)?;
    write_resume_point(out, value.resume_at)
}

pub fn read_commit_meta(input: &mut Decoder<'_>) -> EncodeResult<CommitMeta> {
    let value = CommitMeta {
        cursor: read_cursor(input)?,
        previous: CommitSeq(input.read_u64()?),
        compatibility: read_compatibility(input)?,
        resume_at: read_resume_point(input)?,
    };
    value.validate()?;
    Ok(value)
}
