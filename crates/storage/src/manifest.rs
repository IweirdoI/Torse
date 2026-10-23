//! Fixed-width checkpoint references, carried in the shared checksummed envelope.

use torse_codec::{CodecLimits, Decoder, Encoder, metadata, record};
use torse_types::{Binding, DurableSeq, Fault, FaultCode, OutputBudget, SnapshotMeta};

/// A durable reference to one immutable snapshot. Epochs are intentionally not persisted:
/// reopening a world creates a new live binding without changing its persisted identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointManifest {
    pub generation: u64,
    pub snapshot: SnapshotMeta,
    pub durable_through: DurableSeq,
    pub snapshot_bytes: u64,
    pub snapshot_crc: u32,
}

const PAYLOAD_BYTES: usize = 2 + 8 + metadata::SNAPSHOT_META_BYTES + 8 + 8 + 4;
pub(crate) const ENTRY_BYTES: usize = PAYLOAD_BYTES + record::OVERHEAD_BYTES;
const LIMITS: CodecLimits = CodecLimits {
    max_frame_bytes: PAYLOAD_BYTES as u32,
    max_field_bytes: PAYLOAD_BYTES as u32,
    max_depth: 1,
};

pub(crate) fn encode(value: CheckpointManifest) -> Result<Vec<u8>, Fault> {
    let mut payload = Vec::new();
    let mut out = Encoder::new(
        &mut payload,
        OutputBudget::new(1, PAYLOAD_BYTES as u64),
        LIMITS,
    );
    out.write_u16(1)?;
    out.write_u64(value.generation)?;
    metadata::write_snapshot_meta(&mut out, value.snapshot)?;
    out.write_u64(value.durable_through.0)?;
    out.write_u64(value.snapshot_bytes)?;
    out.write_u32(value.snapshot_crc)?;
    out.finish();
    wrap(record::RecordKind::Manifest, &payload, LIMITS)
}

pub(crate) fn wrap(
    kind: record::RecordKind,
    payload: &[u8],
    limits: CodecLimits,
) -> Result<Vec<u8>, Fault> {
    let mut bytes = Vec::new();
    let budget = OutputBudget::new(1, payload.len() as u64 + record::OVERHEAD_BYTES as u64);
    let mut out = Encoder::new(&mut bytes, budget, limits);
    record::write_record(&mut out, kind, payload)?;
    out.finish();
    Ok(bytes)
}

/// Validate the entire prefix before the caller is allowed to repair a torn final entry.
/// Complete corruption is fatal even when an earlier checkpoint would still be readable.
pub(crate) fn scan(
    bytes: &[u8],
    binding: Binding,
    max_snapshot_bytes: u64,
) -> Result<(Vec<CheckpointManifest>, usize), Fault> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let result = (|| {
            let record::RecordRead::Complete {
                kind,
                payload,
                encoded_bytes,
            } = record::read_record(&bytes[offset..], LIMITS)?
            else {
                return Ok(None);
            };
            if kind != record::RecordKind::Manifest || payload.len() != PAYLOAD_BYTES {
                return Err(corrupt("invalid manifest record kind or width"));
            }
            let mut input = Decoder::new(payload, OutputBudget::new(0, 0), LIMITS);
            if input.read_u16()? != 1 {
                return Err(Fault::new(
                    FaultCode::Incompatible,
                    "unsupported manifest version",
                ));
            }
            let value = CheckpointManifest {
                generation: input.read_u64()?,
                snapshot: metadata::read_snapshot_meta(&mut input)?,
                durable_through: DurableSeq(input.read_u64()?),
                snapshot_bytes: input.read_u64()?,
                snapshot_crc: input.read_u32()?,
            };
            validate_meta(value.snapshot, binding)?;
            if value.generation == 0
                || value.durable_through.0 < value.snapshot.cursor.seq.0
                || value.snapshot_bytes < record::OVERHEAD_BYTES as u64
            {
                return Err(corrupt(
                    "invalid manifest generation, durability, or length",
                ));
            }
            if value.snapshot_bytes > max_snapshot_bytes {
                return Err(Fault::resource_limit(
                    "manifest snapshot exceeds configured limit",
                ));
            }
            if let Some(previous) = entries.last() {
                let previous: &CheckpointManifest = previous;
                if value.generation <= previous.generation
                    || value.snapshot.cursor.seq < previous.snapshot.cursor.seq
                    || value.durable_through < previous.durable_through
                {
                    return Err(corrupt("manifest history regressed"));
                }
            } else if value.snapshot.cursor.seq.0 != 0 {
                return Err(corrupt("first checkpoint must describe position zero"));
            }
            Ok(Some((value, encoded_bytes)))
        })()
        .map_err(|fault: Fault| {
            Fault::new(
                fault.code,
                format!("manifest byte {offset}: {}", fault.context),
            )
        })?;
        let Some((value, length)) = result else { break };
        entries
            .try_reserve(1)
            .map_err(|_| Fault::resource_limit("manifest history allocation"))?;
        entries.push(value);
        offset += length;
    }
    Ok((entries, offset))
}

pub(crate) fn validate_meta(meta: SnapshotMeta, binding: Binding) -> Result<(), Fault> {
    if meta.cursor.world != binding.world {
        return Err(Fault::invalid_data(
            "checkpoint belongs to a different world",
        ));
    }
    if meta.compatibility != binding.compatibility {
        return Err(Fault::new(
            FaultCode::Incompatible,
            "checkpoint compatibility mismatch",
        ));
    }
    // Resume phases are authority-owned. The application, rather than this generic store,
    // verifies that capture occurred at a tick boundary and includes the complete world.
    Ok(())
}

pub(crate) fn corrupt(context: &str) -> Fault {
    Fault::new(FaultCode::CorruptData, context)
}
