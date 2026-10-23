//! Storage codec for the authoritative world payloads registered by A5, A4, and A6.
//!
//! Each payload starts with a two-byte world format version, followed by the body defined in
//! `snapshot.rs` or `journal.rs`. The version describes only this domain layout. It evolves
//! independently of the outer record envelope and of `Compatibility::authority_schema`, which
//! the host derives from compiled content.
//!
//! Version 3 adds entity creation/removal records with allocator preconditions. Readers
//! also accept version 2 snapshots and change sets, which have no lifecycle suffix.
//! Version 1 is rejected because it lacks allocation history. This interim format has no
//! compatibility promise; the outer record envelope remains unchanged.
//! The complete byte layout is documented in `WORLD.md` next to this crate's manifest.

use std::marker::PhantomData;

use torse_types::{
    AuthorityData, CommitRecord, Fault, ModelCommit, ModelSnapshot, OutputBudget, WorldSnapshot,
};
use torse_world::{ChangeSet, MAX_VALUE_DEPTH, Snapshot};

use crate::journal::{read_changes, write_changes};
use crate::snapshot::{read_snapshot, write_snapshot};
use crate::value::invalid;
use crate::{CodecLimits, Decoder, EncodeResult, Encoder, StorageCodec};

/// Domain format version written at the start of every world payload.
pub const WORLD_FORMAT_VERSION: u16 = 3;

/// Smallest `CodecLimits::max_depth` that can carry every component the world accepts.
///
/// The world validates values nested up to `MAX_VALUE_DEPTH` levels below a field, which is
/// `MAX_VALUE_DEPTH + 1` open containers in the worst case. A lower configured depth would
/// make the codec refuse records the authority already accepted.
pub const MIN_WORLD_DEPTH: u16 = MAX_VALUE_DEPTH as u16 + 1;

/// Encodes `torse_world::Snapshot` and `torse_world::ChangeSet` for the journal and
/// checkpoint store.
///
/// The codec is generic over the authority model so that the server can define its model
/// with its own input and output types; the only requirement is that the model's snapshot
/// and change payloads are the world exchange types.
///
/// Every limit comes from local configuration. Field lengths are bounded by
/// `max_field_bytes`, nesting by `max_depth`, and owned allocations and logical entries by the
/// `OutputBudget` passed to each call. One item is charged for the record itself and one for
/// every element of every counted collection; dense cells and finite state values are scalar
/// arrays bounded by bytes instead.
pub struct WorldCodec<M> {
    limits: CodecLimits,
    model: PhantomData<fn() -> M>,
}

// Manual impls: a derive would require `M: Clone` and `M: Debug` even though no `M` is
// stored, which would force every authority model to implement traits it does not need.
impl<M> std::fmt::Debug for WorldCodec<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldCodec")
            .field("limits", &self.limits)
            .finish()
    }
}

impl<M> Clone for WorldCodec<M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for WorldCodec<M> {}

impl<M> WorldCodec<M> {
    /// Accept limits that can carry every value the world itself accepts.
    pub fn new(limits: CodecLimits) -> Result<Self, Fault> {
        if limits.max_depth < MIN_WORLD_DEPTH {
            return Err(Fault::invalid_data(format!(
                "world codec needs max_depth >= {MIN_WORLD_DEPTH}, got {}",
                limits.max_depth
            )));
        }
        Ok(Self {
            limits,
            model: PhantomData,
        })
    }

    pub fn limits(&self) -> CodecLimits {
        self.limits
    }
}

fn read_version(input: &mut Decoder<'_>) -> EncodeResult<u16> {
    let version = input.read_u16()?;
    if version != 2 && version != WORLD_FORMAT_VERSION {
        return Err(Fault::incompatible(format!(
            "unsupported world format version {version}"
        )));
    }
    Ok(version)
}

/// A decoded payload must account for every input byte; trailing data means the record was
/// written by a different layout, not that it has optional extensions.
fn finish_decode(input: &Decoder<'_>, what: &str) -> EncodeResult<()> {
    if !input.is_complete() {
        return Err(invalid(format!(
            "{} trailing bytes after the {what} payload",
            input.remaining()
        )));
    }
    Ok(())
}

impl<M> StorageCodec<M> for WorldCodec<M>
where
    M: AuthorityData<Snapshot = Snapshot, Changes = ChangeSet>,
{
    /// The wrapper metadata and the payload metadata describe the same position. They are
    /// written once, so a mismatch is rejected instead of choosing one of them silently.
    fn encode_snapshot(
        &self,
        value: &ModelSnapshot<M>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> EncodeResult<()> {
        if value.meta != value.state.meta {
            return Err(invalid("snapshot wrapper and payload metadata differ"));
        }
        // The encoder truncates `out` back to its original length unless `finish` runs.
        let mut encoder = Encoder::new(out, budget, self.limits);
        encoder.count_items(1)?;
        encoder.write_u16(WORLD_FORMAT_VERSION)?;
        write_snapshot(&mut encoder, &value.state)?;
        encoder.finish();
        Ok(())
    }

    fn decode_snapshot(
        &self,
        input: &[u8],
        budget: OutputBudget,
    ) -> EncodeResult<ModelSnapshot<M>> {
        let mut decoder = Decoder::new(input, budget, self.limits);
        decoder.charge_items(1)?;
        decoder.charge_bytes(size_of::<ModelSnapshot<M>>() as u64)?;
        read_version(&mut decoder)?;
        let state = read_snapshot(&mut decoder)?;
        finish_decode(&decoder, "snapshot")?;
        Ok(WorldSnapshot {
            meta: state.meta,
            state,
        })
    }

    fn encode_commit(
        &self,
        value: &ModelCommit<M>,
        out: &mut Vec<u8>,
        budget: OutputBudget,
    ) -> EncodeResult<()> {
        if value.meta != value.changes.meta {
            return Err(invalid("commit wrapper and payload metadata differ"));
        }
        let mut encoder = Encoder::new(out, budget, self.limits);
        encoder.count_items(1)?;
        encoder.write_u16(WORLD_FORMAT_VERSION)?;
        write_changes(&mut encoder, &value.changes)?;
        encoder.finish();
        Ok(())
    }

    fn decode_commit(&self, input: &[u8], budget: OutputBudget) -> EncodeResult<ModelCommit<M>> {
        let mut decoder = Decoder::new(input, budget, self.limits);
        decoder.charge_items(1)?;
        decoder.charge_bytes(size_of::<ModelCommit<M>>() as u64)?;
        let version = read_version(&mut decoder)?;
        let changes = read_changes(&mut decoder, version)?;
        finish_decode(&decoder, "commit")?;
        Ok(CommitRecord {
            meta: changes.meta,
            changes,
        })
    }
}
