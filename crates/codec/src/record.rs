//! Versioned storage envelopes with independently checked headers and payloads.
//!
//! Version 1 is `[magic:8, version:u16, kind:u16, payload_len:u32, header_crc:u32,
//! payload:N, payload_crc:u32, end_magic:4]`, with little-endian integers. The header CRC
//! covers its first 16 bytes; the payload CRC covers exactly the payload. Protecting the
//! length separately prevents an accidental length change from masquerading as a torn tail.
//! A complete record with a bad checksum is always corruption, including the final record.

use torse_types::{Fault, FaultCode};

use crate::{CodecLimits, EncodeResult, Encoder};

pub const MAGIC: [u8; 8] = *b"TORSEFRM";
pub const VERSION: u16 = 1;
pub const HEADER_BYTES: usize = 20;
pub const TRAILER_BYTES: usize = 8;
pub const OVERHEAD_BYTES: usize = HEADER_BYTES + TRAILER_BYTES;
const END_MAGIC: [u8; 4] = *b"TEND";

/// Envelope kinds evolve separately from the authority payload schema version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum RecordKind {
    Commit = 1,
    Snapshot = 2,
}

/// A validated header permits bounded allocation before reading the payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordHeader {
    pub kind: RecordKind,
    pub payload_bytes: u32,
}

impl RecordHeader {
    pub fn encoded_bytes(self) -> EncodeResult<usize> {
        usize::try_from(u64::from(self.payload_bytes) + OVERHEAD_BYTES as u64)
            .map_err(|_| Fault::resource_limit("record length is not addressable"))
    }
}

/// Incomplete input is reported separately from complete but invalid bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordRead<'a> {
    Incomplete,
    Complete {
        kind: RecordKind,
        payload: &'a [u8],
        encoded_bytes: usize,
    },
}

/// Validate a header prefix without allocation. `None` means more header bytes are needed.
///
/// Even partial headers must match every available magic byte and any complete version/kind
/// field. Only a full, checksum-valid header is allowed to declare a payload allocation.
pub fn read_header(input: &[u8], limits: CodecLimits) -> EncodeResult<Option<RecordHeader>> {
    let magic_bytes = input.len().min(MAGIC.len());
    if input[..magic_bytes] != MAGIC[..magic_bytes] {
        return Err(corrupt("record magic mismatch"));
    }
    if input.len() >= HEADER_BYTES {
        let expected = u32::from_le_bytes(input[16..20].try_into().expect("fixed header range"));
        if crc32c::crc32c(&input[..16]) != expected {
            return Err(corrupt("record header checksum mismatch"));
        }
    }
    if input.len() >= 10 {
        let version = u16::from_le_bytes([input[8], input[9]]);
        if version != VERSION {
            return Err(Fault::new(
                FaultCode::Incompatible,
                "unsupported record envelope version",
            ));
        }
    }
    let kind = if input.len() >= 12 {
        match u16::from_le_bytes([input[10], input[11]]) {
            1 => RecordKind::Commit,
            2 => RecordKind::Snapshot,
            _ => {
                return Err(Fault::new(
                    FaultCode::Incompatible,
                    "unsupported record kind",
                ));
            }
        }
    } else {
        return Ok(None);
    };
    if input.len() < HEADER_BYTES {
        return Ok(None);
    }
    let payload_bytes = u32::from_le_bytes(input[12..16].try_into().expect("fixed header range"));
    if payload_bytes > limits.max_frame_bytes {
        return Err(Fault::resource_limit(
            "record payload exceeds configured frame limit",
        ));
    }
    let header = RecordHeader {
        kind,
        payload_bytes,
    };
    header.encoded_bytes()?;
    Ok(Some(header))
}

/// Append one complete envelope, checking all sizes and budget before changing the buffer.
pub fn write_record(out: &mut Encoder<'_>, kind: RecordKind, payload: &[u8]) -> EncodeResult<()> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| *length <= out.limits().max_frame_bytes)
        .ok_or_else(|| Fault::resource_limit("record payload exceeds configured frame limit"))?;
    let header = RecordHeader {
        kind,
        payload_bytes: length,
    };
    out.ensure(header.encoded_bytes()? as u64)?;
    out.count_items(1)?;
    let mut bytes = [0; HEADER_BYTES];
    bytes[..8].copy_from_slice(&MAGIC);
    bytes[8..10].copy_from_slice(&VERSION.to_le_bytes());
    bytes[10..12].copy_from_slice(&(kind as u16).to_le_bytes());
    bytes[12..16].copy_from_slice(&length.to_le_bytes());
    let checksum = crc32c::crc32c(&bytes[..16]);
    bytes[16..20].copy_from_slice(&checksum.to_le_bytes());
    out.write(&bytes)?;
    out.write(payload)?;
    out.write_u32(crc32c::crc32c(payload))?;
    out.write(&END_MAGIC)
}

/// Borrow the first complete envelope. Any following bytes belong to the next record.
pub fn read_record(input: &[u8], limits: CodecLimits) -> EncodeResult<RecordRead<'_>> {
    let Some(header) = read_header(input, limits)? else {
        return Ok(RecordRead::Incomplete);
    };
    let encoded_bytes = header.encoded_bytes()?;
    if input.len() < encoded_bytes {
        return Ok(RecordRead::Incomplete);
    }
    let payload_end = encoded_bytes - TRAILER_BYTES;
    let payload = &input[HEADER_BYTES..payload_end];
    let checksum = u32::from_le_bytes(
        input[payload_end..payload_end + 4]
            .try_into()
            .expect("validated record range"),
    );
    if crc32c::crc32c(payload) != checksum {
        return Err(corrupt("record payload checksum mismatch"));
    }
    if input[payload_end + 4..encoded_bytes] != END_MAGIC {
        return Err(corrupt("record end marker mismatch"));
    }
    Ok(RecordRead::Complete {
        kind: header.kind,
        payload,
        encoded_bytes,
    })
}

fn corrupt(context: &str) -> Fault {
    Fault::new(FaultCode::CorruptData, context)
}
