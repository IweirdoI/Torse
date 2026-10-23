//! Compatibility fixtures and corruption tests for the storage envelope and metadata.

use torse_codec::metadata::*;
use torse_codec::record::{self, RecordKind, RecordRead};
use torse_codec::{CodecLimits, Decoder, EncodeResult, Encoder};
use torse_types::*;

const LIMITS: CodecLimits = CodecLimits {
    max_frame_bytes: 1024,
    max_field_bytes: 512,
    max_depth: 4,
};

fn hex(value: &str) -> Vec<u8> {
    let value: String = value
        .chars()
        .filter(|value| !value.is_whitespace())
        .collect();
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn compatibility() -> Compatibility {
    Compatibility {
        authority_schema: 3,
        rules_digest: Digest([0x11; 32]),
        content_digest: Digest([0x22; 32]),
    }
}

fn resume() -> ResumePoint {
    ResumePoint {
        tick: Tick(4),
        next_phase: 5,
        next_step: 6,
    }
}

fn check_metadata<T: Copy + Eq + std::fmt::Debug>(
    value: T,
    fixture: &str,
    write: fn(&mut Encoder<'_>, T) -> EncodeResult<()>,
    read: fn(&mut Decoder<'_>) -> EncodeResult<T>,
) {
    let expected = hex(fixture);
    let mut bytes = Vec::new();
    let mut out = Encoder::new(&mut bytes, OutputBudget::new(0, 1024), LIMITS);
    write(&mut out, value).unwrap();
    out.finish();
    assert_eq!(bytes, expected);
    let mut input = Decoder::new(&expected, OutputBudget::new(0, 0), LIMITS);
    assert_eq!(read(&mut input).unwrap(), value);
    assert!(input.is_complete());
    for length in 0..expected.len() {
        let mut input = Decoder::new(&expected[..length], OutputBudget::new(0, 0), LIMITS);
        assert!(read(&mut input).is_err(), "truncation at {length}");
        let mut output = vec![0xaa];
        let mut out = Encoder::new(&mut output, OutputBudget::new(0, length as u64), LIMITS);
        assert!(write(&mut out, value).is_err());
        assert_eq!(
            out.written(),
            0,
            "metadata must check its entire fixed width first"
        );
        out.finish();
        assert_eq!(output, [0xaa]);
    }
}

#[test]
fn all_metadata_matches_fixed_little_endian_fixtures() {
    let cursor = CommitCursor {
        world: WorldId(1),
        seq: CommitSeq(2),
    };
    check_metadata(
        compatibility(),
        include_str!("fixtures/compatibility.hex"),
        write_compatibility,
        read_compatibility,
    );
    check_metadata(
        Binding {
            world: WorldId(1),
            epoch: WorldEpoch(7),
            compatibility: compatibility(),
        },
        include_str!("fixtures/binding.hex"),
        write_binding,
        read_binding,
    );
    check_metadata(
        resume(),
        include_str!("fixtures/resume.hex"),
        write_resume_point,
        read_resume_point,
    );
    check_metadata(
        SnapshotMeta {
            cursor,
            compatibility: compatibility(),
            resume_at: resume(),
        },
        include_str!("fixtures/snapshot.hex"),
        write_snapshot_meta,
        read_snapshot_meta,
    );
    check_metadata(
        CommitMeta {
            cursor,
            previous: CommitSeq(1),
            compatibility: compatibility(),
            resume_at: resume(),
        },
        include_str!("fixtures/commit.hex"),
        write_commit_meta,
        read_commit_meta,
    );
    assert_eq!(
        hex(include_str!("fixtures/compatibility.hex")).len(),
        COMPATIBILITY_BYTES
    );
    assert_eq!(
        hex(include_str!("fixtures/binding.hex")).len(),
        BINDING_BYTES
    );
    assert_eq!(
        hex(include_str!("fixtures/resume.hex")).len(),
        RESUME_POINT_BYTES
    );
    assert_eq!(
        hex(include_str!("fixtures/snapshot.hex")).len(),
        SNAPSHOT_META_BYTES
    );
    assert_eq!(
        hex(include_str!("fixtures/commit.hex")).len(),
        COMMIT_META_BYTES
    );
}

#[test]
fn commit_metadata_rejects_gaps_duplicates_and_wraparound() {
    for (previous, sequence) in [(1, 1), (1, 3), (u64::MAX, 0)] {
        let value = CommitMeta {
            cursor: CommitCursor {
                world: WorldId(1),
                seq: CommitSeq(sequence),
            },
            previous: CommitSeq(previous),
            compatibility: compatibility(),
            resume_at: resume(),
        };
        let mut bytes = vec![0xaa];
        let mut out = Encoder::new(&mut bytes, OutputBudget::new(1, 1024), LIMITS);
        assert!(write_commit_meta(&mut out, value).is_err());
        assert_eq!(out.written(), 0);
        drop(out);
        assert_eq!(bytes, [0xaa]);
        let mut encoded = hex(include_str!("fixtures/commit.hex"));
        encoded[16..24].copy_from_slice(&sequence.to_le_bytes());
        encoded[24..32].copy_from_slice(&previous.to_le_bytes());
        let mut input = Decoder::new(&encoded, OutputBudget::new(1, 0), LIMITS);
        assert!(read_commit_meta(&mut input).is_err());
    }
}

#[test]
fn resume_positions_are_opaque_and_snapshot_zero_is_valid() {
    let value = ResumePoint {
        tick: Tick(u64::MAX),
        next_phase: u16::MAX,
        next_step: u64::MAX,
    };
    let mut bytes = Vec::new();
    let mut out = Encoder::new(&mut bytes, OutputBudget::new(0, 1024), LIMITS);
    write_resume_point(&mut out, value).unwrap();
    write_snapshot_meta(
        &mut out,
        SnapshotMeta {
            cursor: CommitCursor::initial(WorldId(1)),
            compatibility: compatibility(),
            resume_at: value,
        },
    )
    .unwrap();
    out.finish();
    let mut input = Decoder::new(&bytes, OutputBudget::new(0, 0), LIMITS);
    assert_eq!(read_resume_point(&mut input).unwrap(), value);
    assert_eq!(
        read_snapshot_meta(&mut input).unwrap().cursor.seq,
        CommitSeq::INITIAL
    );
}

#[test]
fn crc32c_matches_independent_reference_vectors() {
    assert_eq!(crc32c::crc32c(b""), 0);
    assert_eq!(crc32c::crc32c(b"123456789"), 0xe306_9283);
    assert_eq!(crc32c::crc32c(b"Hello world!"), 0x7b98_e751);
}

fn envelope(kind: RecordKind, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut out = Encoder::new(&mut bytes, OutputBudget::new(1, 2048), LIMITS);
    record::write_record(&mut out, kind, payload).unwrap();
    out.finish();
    bytes
}

#[test]
fn envelope_matches_fixed_bytes_and_can_read_concatenated_records() {
    let mut bytes = envelope(RecordKind::Commit, b"abc");
    let golden = hex(include_str!("fixtures/record.hex"));
    assert_eq!(bytes, golden);
    let length = bytes.len();
    bytes.extend(envelope(RecordKind::Snapshot, b""));
    assert_eq!(
        record::read_record(&bytes, LIMITS).unwrap(),
        RecordRead::Complete {
            kind: RecordKind::Commit,
            payload: b"abc",
            encoded_bytes: length,
        }
    );
    assert_eq!(
        record::read_record(&bytes[length..], LIMITS).unwrap(),
        RecordRead::Complete {
            kind: RecordKind::Snapshot,
            payload: b"",
            encoded_bytes: record::OVERHEAD_BYTES,
        }
    );
}

#[test]
fn every_valid_truncation_is_incomplete_and_every_single_bit_flip_is_corruption() {
    let bytes = envelope(RecordKind::Commit, b"abc");
    for end in 0..bytes.len() {
        assert_eq!(
            record::read_record(&bytes[..end], LIMITS).unwrap(),
            RecordRead::Incomplete
        );
    }
    for index in 0..bytes.len() {
        for bit in 0..8 {
            let mut damaged = bytes.clone();
            damaged[index] ^= 1 << bit;
            assert_eq!(
                record::read_record(&damaged, LIMITS).unwrap_err().code,
                FaultCode::CorruptData,
                "byte {index}, bit {bit}"
            );
        }
    }
}

#[test]
fn header_checks_limits_and_compatibility_before_payload_allocation() {
    let mut bytes = envelope(RecordKind::Commit, b"abc");
    bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    let checksum = crc32c::crc32c(&bytes[..16]);
    bytes[16..20].copy_from_slice(&checksum.to_le_bytes());
    assert_eq!(
        record::read_header(&bytes[..20], LIMITS).unwrap_err().code,
        FaultCode::ResourceLimit
    );
    for range in [8..10, 10..12] {
        let mut bytes = envelope(RecordKind::Commit, b"abc");
        bytes[range].copy_from_slice(&99u16.to_le_bytes());
        let checksum = crc32c::crc32c(&bytes[..16]);
        bytes[16..20].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            record::read_record(&bytes, LIMITS).unwrap_err().code,
            FaultCode::Incompatible
        );
    }
    assert_eq!(
        record::read_header(b"TORX", LIMITS).unwrap_err().code,
        FaultCode::CorruptData
    );
}

#[test]
fn envelope_failure_preserves_output_even_if_caller_finishes_encoder() {
    for budget in [OutputBudget::new(0, 100), OutputBudget::new(1, 30)] {
        let mut bytes = vec![0xaa];
        let mut out = Encoder::new(&mut bytes, budget, LIMITS);
        assert!(record::write_record(&mut out, RecordKind::Commit, b"abc").is_err());
        assert_eq!(out.written(), 0);
        out.finish();
        assert_eq!(bytes, [0xaa]);
    }
    let mut bytes = Vec::new();
    let mut out = Encoder::new(&mut bytes, OutputBudget::new(1, 4096), LIMITS);
    assert!(record::write_record(&mut out, RecordKind::Commit, &[0; 1025]).is_err());
    assert_eq!(out.written(), 0);
}
