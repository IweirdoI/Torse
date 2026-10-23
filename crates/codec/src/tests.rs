//! Unit tests for budget tracking, encoding, decoding, and frames.

use super::*;
use torse_types::{FaultCode, OutputBudget};

const LIMITS: CodecLimits = CodecLimits {
    max_frame_bytes: 64,
    max_field_bytes: 16,
    max_depth: 2,
};

fn budget(max_items: u32, max_bytes: u64) -> OutputBudget {
    OutputBudget::new(max_items, max_bytes)
}

// ================================================================================================
// BudgetGuard
// ================================================================================================

#[test]
fn guard_starts_empty() {
    let guard = BudgetGuard::new(budget(10, 1000));
    assert_eq!(guard.used_items(), 0);
    assert_eq!(guard.used_bytes(), 0);
    assert_eq!(guard.remaining_items(), 10);
    assert_eq!(guard.remaining_bytes(), 1000);
}

#[test]
fn guard_charges_bytes_without_items() {
    let mut guard = BudgetGuard::new(budget(1, 1000));
    for _ in 0..10 {
        guard.charge_bytes(8).unwrap();
    }
    assert_eq!(guard.used_bytes(), 80);
    assert_eq!(guard.used_items(), 0);
}

#[test]
fn guard_accepts_bytes_up_to_limit() {
    let mut guard = BudgetGuard::new(budget(1, 100));
    guard.charge_bytes(100).unwrap();
    assert_eq!(guard.remaining_bytes(), 0);
    assert_eq!(
        guard.charge_bytes(1).unwrap_err().code,
        FaultCode::ResourceLimit
    );
}

#[test]
fn guard_rejects_byte_overflow() {
    let mut guard = BudgetGuard::new(budget(1, u64::MAX));
    guard.charge_bytes(u64::MAX).unwrap();
    assert!(guard.charge_bytes(1).is_err());
    assert_eq!(guard.used_bytes(), u64::MAX);
}

#[test]
fn guard_counts_items_separately() {
    let mut guard = BudgetGuard::new(budget(3, 1000));
    guard.charge_items(2).unwrap();
    guard.charge_items(1).unwrap();
    assert!(guard.charge_items(1).is_err());
    assert_eq!(guard.used_items(), 3);
    assert_eq!(guard.used_bytes(), 0);
}

// ================================================================================================
// Encoder
// ================================================================================================

#[test]
fn encoder_writes_little_endian() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 1000), LIMITS);
    encoder.write_u8(0x01).unwrap();
    encoder.write_u16(0x0302).unwrap();
    encoder.write_u32(0x07060504).unwrap();
    encoder.write_u64(0x0f0e0d0c0b0a0908).unwrap();
    assert_eq!(encoder.finish(), 15);
    assert_eq!(buf, (1..=15).collect::<Vec<u8>>());
}

#[test]
fn encoder_writes_u128() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 1000), LIMITS);
    encoder
        .write_u128(0x0123456789abcdef_fedcba9876543210)
        .unwrap();
    encoder.finish();
    assert_eq!(buf, 0x0123456789abcdef_fedcba9876543210u128.to_le_bytes());
}

#[test]
fn encoder_byte_budget_ignores_item_count() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 1000), LIMITS);
    for value in 0..10u64 {
        encoder.write_u64(value).unwrap();
    }
    assert_eq!(encoder.finish(), 80);
}

#[test]
fn encoder_rejects_write_over_budget() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 5), LIMITS);
    encoder.write_u32(0).unwrap();
    assert_eq!(
        encoder.write_u16(0).unwrap_err().code,
        FaultCode::ResourceLimit
    );
    assert_eq!(encoder.written(), 4);
}

#[test]
fn encoder_drop_without_finish_restores_buffer() {
    let mut buf = vec![1, 2, 3];
    {
        let mut encoder = Encoder::new(&mut buf, budget(1, 10), LIMITS);
        encoder.write_u32(0x12345678).unwrap();
        assert!(encoder.write_u64(0).is_err());
    }
    assert_eq!(buf, vec![1, 2, 3]);
}

#[test]
fn encoder_early_return_leaves_no_partial_record() {
    fn encode(out: &mut Vec<u8>) -> EncodeResult<usize> {
        let mut encoder = Encoder::new(out, budget(1, 6), LIMITS);
        encoder.write_u32(1)?;
        encoder.write_u32(2)?;
        Ok(encoder.finish())
    }

    let mut buf = vec![9];
    assert!(encode(&mut buf).is_err());
    assert_eq!(buf, vec![9]);
}

#[test]
fn encoder_finish_keeps_existing_prefix() {
    let mut buf = vec![9];
    let mut encoder = Encoder::new(&mut buf, budget(1, 10), LIMITS);
    encoder.write_u8(1).unwrap();
    assert_eq!(encoder.finish(), 1);
    assert_eq!(buf, vec![9, 1]);
}

#[test]
fn encoder_counts_items() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(2, 100), LIMITS);
    encoder.count_items(2).unwrap();
    assert!(encoder.count_items(1).is_err());
}

#[test]
fn encoder_rejects_field_over_limit() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 1000), LIMITS);
    let field = [0u8; 17];
    assert_eq!(
        encoder.write_field(&field).unwrap_err().code,
        FaultCode::ResourceLimit
    );
    assert_eq!(encoder.written(), 0);
}

// ================================================================================================
// Decoder
// ================================================================================================

#[test]
fn decoder_reads_little_endian() {
    let input: Vec<u8> = (1..=15).collect();
    let mut decoder = Decoder::new(&input, budget(1, 0), LIMITS);
    assert_eq!(decoder.read_u8().unwrap(), 0x01);
    assert_eq!(decoder.read_u16().unwrap(), 0x0302);
    assert_eq!(decoder.read_u32().unwrap(), 0x07060504);
    assert_eq!(decoder.read_u64().unwrap(), 0x0f0e0d0c0b0a0908);
    assert!(decoder.is_complete());
}

#[test]
fn decoder_reads_u128() {
    let value = 0x0123456789abcdef_fedcba9876543210u128;
    let input = value.to_le_bytes();
    let mut decoder = Decoder::new(&input, budget(1, 0), LIMITS);
    assert_eq!(decoder.read_u128().unwrap(), value);
    assert!(decoder.is_complete());
}

#[test]
fn decoder_reads_do_not_charge_budget() {
    let input = [0u8; 64];
    let mut decoder = Decoder::new(&input, budget(1, 0), LIMITS);
    for _ in 0..8 {
        decoder.read_u64().unwrap();
    }
    assert_eq!(decoder.guard().used_bytes(), 0);
    assert_eq!(decoder.guard().used_items(), 0);
}

#[test]
fn decoder_rejects_truncated_input() {
    let input = [0x78, 0x56];
    let mut decoder = Decoder::new(&input, budget(1, 0), LIMITS);
    assert_eq!(decoder.read_u32().unwrap_err().code, FaultCode::InvalidData);
    assert_eq!(decoder.remaining(), 2);
}

#[test]
fn decoder_rejects_huge_length_without_overflow() {
    let input = [1, 2, 3];
    let mut decoder = Decoder::new(&input, budget(1, 0), LIMITS);
    decoder.read(1).unwrap();
    assert!(decoder.read(usize::MAX).is_err());
    assert_eq!(decoder.remaining(), 2);
}

#[test]
fn decoder_charges_allocations() {
    let mut decoder = Decoder::new(&[], budget(2, 100), LIMITS);
    decoder.charge_items(2).unwrap();
    decoder.charge_bytes(100).unwrap();
    assert!(decoder.charge_items(1).is_err());
    assert!(decoder.charge_bytes(1).is_err());
}

#[test]
fn decoder_rejects_field_over_limit_before_reading() {
    let mut input = 17u32.to_le_bytes().to_vec();
    input.extend_from_slice(&[0; 17]);
    let mut decoder = Decoder::new(&input, budget(1, 0), LIMITS);
    assert_eq!(
        decoder.read_field().unwrap_err().code,
        FaultCode::InvalidData
    );
}

#[test]
fn decoder_limits_nesting_depth() {
    let mut decoder = Decoder::new(&[], budget(1, 0), LIMITS);
    decoder.enter().unwrap();
    decoder.enter().unwrap();
    assert!(decoder.enter().is_err());
    decoder.leave();
    decoder.enter().unwrap();
}

// ================================================================================================
// Frames and Fields
// ================================================================================================

#[test]
fn field_roundtrip() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 1000), LIMITS);
    encoder.write_field(b"abc").unwrap();
    encoder.write_field(b"").unwrap();
    encoder.finish();

    let mut decoder = Decoder::new(&buf, budget(1, 0), LIMITS);
    assert_eq!(decoder.read_field().unwrap(), b"abc");
    assert_eq!(decoder.read_field().unwrap(), b"");
    assert!(decoder.is_complete());
}

#[test]
fn frame_roundtrip() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 1000), LIMITS);
    frame::write_frame(&mut encoder, b"test data").unwrap();
    encoder.finish();

    let mut decoder = Decoder::new(&buf, budget(1, 0), LIMITS);
    assert_eq!(frame::read_frame(&mut decoder).unwrap(), b"test data");
    assert!(decoder.is_complete());
}

#[test]
fn frame_write_rejects_payload_over_limit() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 1000), LIMITS);
    let payload = [0u8; 65];
    assert!(frame::write_frame(&mut encoder, &payload).is_err());
    assert_eq!(encoder.written(), 0);
}

#[test]
fn frame_write_appends_nothing_when_budget_short() {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf, budget(1, 10), LIMITS);
    assert!(frame::write_frame(&mut encoder, &[0u8; 8]).is_err());
    assert_eq!(encoder.written(), 0);
    encoder.write_u32(7).unwrap();
    assert_eq!(encoder.finish(), 4);
    assert_eq!(buf, 7u32.to_le_bytes());
}

#[test]
fn frame_read_rejects_declared_length_over_limit() {
    let input = u32::MAX.to_le_bytes();
    let mut decoder = Decoder::new(&input, budget(1, 0), LIMITS);
    assert_eq!(
        frame::read_frame(&mut decoder).unwrap_err().code,
        FaultCode::InvalidData
    );
}

#[test]
fn frame_read_rejects_truncated_payload() {
    let mut input = 8u32.to_le_bytes().to_vec();
    input.extend_from_slice(&[0; 3]);
    let mut decoder = Decoder::new(&input, budget(1, 0), LIMITS);
    assert!(frame::read_frame(&mut decoder).is_err());
}
