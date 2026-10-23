//! Unit tests for codec encoding and decoding.

use super::*;
use torse_types::OutputBudget;

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================================
    // BudgetGuard Tests
    // ============================================================================================

    #[test]
    fn budget_guard_creation() {
        let budget = OutputBudget::new(10, 1000);
        let guard = BudgetGuard::new(budget);
        assert_eq!(guard.used_items(), 0);
        assert_eq!(guard.used_bytes(), 0);
        assert_eq!(guard.remaining_items(), 10);
        assert_eq!(guard.remaining_bytes(), 1000);
    }

    #[test]
    fn budget_guard_can_add_within_limits() {
        let budget = OutputBudget::new(10, 1000);
        let guard = BudgetGuard::new(budget);
        assert!(guard.can_add(100));
    }

    #[test]
    fn budget_guard_rejects_bytes_exceeded() {
        let budget = OutputBudget::new(10, 1000);
        let guard = BudgetGuard::new(budget);
        assert!(!guard.can_add(1001));
    }

    #[test]
    fn budget_guard_reserve_succeeds() {
        let budget = OutputBudget::new(10, 1000);
        let mut guard = BudgetGuard::new(budget);
        assert!(guard.reserve(100).is_ok());
        assert_eq!(guard.used_items(), 1);
        assert_eq!(guard.used_bytes(), 100);
    }

    #[test]
    fn budget_guard_reserve_tracks_multiple() {
        let budget = OutputBudget::new(10, 1000);
        let mut guard = BudgetGuard::new(budget);
        assert!(guard.reserve(100).is_ok());
        assert!(guard.reserve(200).is_ok());
        assert_eq!(guard.used_items(), 2);
        assert_eq!(guard.used_bytes(), 300);
    }

    #[test]
    fn budget_guard_reserve_fails_on_item_limit() {
        let budget = OutputBudget::new(2, 1000);
        let mut guard = BudgetGuard::new(budget);
        assert!(guard.reserve(100).is_ok());
        assert!(guard.reserve(100).is_ok());
        assert!(guard.reserve(100).is_err());
    }

    #[test]
    fn budget_guard_reserve_fails_on_byte_limit() {
        let budget = OutputBudget::new(10, 1000);
        let mut guard = BudgetGuard::new(budget);
        assert!(guard.reserve(500).is_ok());
        assert!(guard.reserve(501).is_err());
    }

    // ============================================================================================
    // Encoder Tests
    // ============================================================================================

    #[test]
    fn encoder_write_u8() {
        let mut buf = Vec::new();
        let budget = OutputBudget::new(10, 1000);
        {
            let mut encoder = Encoder::new(&mut buf, budget);
            assert!(encoder.write_u8(42).is_ok());
        }
        assert_eq!(buf, vec![42]);
    }

    #[test]
    fn encoder_write_u16() {
        let mut buf = Vec::new();
        let budget = OutputBudget::new(10, 1000);
        {
            let mut encoder = Encoder::new(&mut buf, budget);
            assert!(encoder.write_u16(0x1234).is_ok());
        }
        assert_eq!(buf, vec![0x34, 0x12]);
    }

    #[test]
    fn encoder_write_u32() {
        let mut buf = Vec::new();
        let budget = OutputBudget::new(10, 1000);
        {
            let mut encoder = Encoder::new(&mut buf, budget);
            assert!(encoder.write_u32(0x12345678).is_ok());
        }
        assert_eq!(buf, vec![0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn encoder_write_u64() {
        let mut buf = Vec::new();
        let budget = OutputBudget::new(10, 1000);
        {
            let mut encoder = Encoder::new(&mut buf, budget);
            assert!(encoder.write_u64(0x123456789ABCDEF0).is_ok());
        }
        assert_eq!(buf, vec![0xF0, 0xDE, 0xBC, 0x9A, 0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn encoder_write_u128() {
        let mut buf = Vec::new();
        let budget = OutputBudget::new(10, 1000);
        {
            let mut encoder = Encoder::new(&mut buf, budget);
            assert!(
                encoder
                    .write_u128(0x123456789ABCDEF0FEDCBA9876543210)
                    .is_ok()
            );
        }
        assert_eq!(buf.len(), 16);
    }

    #[test]
    fn encoder_respects_budget() {
        let mut buf = Vec::new();
        let budget = OutputBudget::new(10, 5);
        let mut encoder = Encoder::new(&mut buf, budget);
        assert!(encoder.write_u32(0x12345678).is_ok());
        assert!(encoder.write_u16(0x1234).is_err());
    }

    #[test]
    fn encoder_restore_on_error() {
        let mut buf = vec![1, 2, 3];
        let budget = OutputBudget::new(10, 10);
        {
            let mut encoder = Encoder::new(&mut buf, budget);
            let _ = encoder.write_u32(0x12345678);
            let result = encoder.write_u64(0x1234567890ABCDEF);
            assert!(result.is_err());
            encoder.restore_on_error();
        }
        // After restore, buffer should be back to initial state
        assert_eq!(buf, vec![1, 2, 3]);
    }

    // ============================================================================================
    // Decoder Tests
    // ============================================================================================

    #[test]
    fn decoder_read_u8() {
        let input = vec![42];
        let budget = OutputBudget::new(10, 1000);
        let mut decoder = Decoder::new(&input, budget);
        assert_eq!(decoder.read_u8().unwrap(), 42);
        assert!(decoder.is_complete());
    }

    #[test]
    fn decoder_read_u16() {
        let input = vec![0x34, 0x12];
        let budget = OutputBudget::new(10, 1000);
        let mut decoder = Decoder::new(&input, budget);
        assert_eq!(decoder.read_u16().unwrap(), 0x1234);
        assert!(decoder.is_complete());
    }

    #[test]
    fn decoder_read_u32() {
        let input = vec![0x78, 0x56, 0x34, 0x12];
        let budget = OutputBudget::new(10, 1000);
        let mut decoder = Decoder::new(&input, budget);
        assert_eq!(decoder.read_u32().unwrap(), 0x12345678);
        assert!(decoder.is_complete());
    }

    #[test]
    fn decoder_read_u64() {
        let input = vec![0xF0, 0xDE, 0xBC, 0x9A, 0x78, 0x56, 0x34, 0x12];
        let budget = OutputBudget::new(10, 1000);
        let mut decoder = Decoder::new(&input, budget);
        assert_eq!(decoder.read_u64().unwrap(), 0x123456789ABCDEF0);
        assert!(decoder.is_complete());
    }

    #[test]
    fn decoder_detects_incomplete_input() {
        let input = vec![0x78, 0x56];
        let budget = OutputBudget::new(10, 1000);
        let mut decoder = Decoder::new(&input, budget);
        assert!(decoder.read_u32().is_err());
    }

    #[test]
    fn decoder_tracks_remaining() {
        let input = vec![1, 2, 3, 4, 5];
        let budget = OutputBudget::new(10, 1000);
        let mut decoder = Decoder::new(&input, budget);
        assert_eq!(decoder.remaining(), 5);
        let _ = decoder.read_u8();
        assert_eq!(decoder.remaining(), 4);
    }

    // ============================================================================================
    // Frame Tests
    // ============================================================================================

    #[test]
    fn frame_write_and_read() {
        let data = b"test data";
        let mut buf = Vec::new();
        let budget = OutputBudget::new(10, 1000);
        {
            let mut encoder = Encoder::new(&mut buf, budget);
            assert!(frame::write_frame(&mut encoder, data).is_ok());
        }

        let budget = OutputBudget::new(10, 1000);
        let mut decoder = Decoder::new(&buf, budget);
        let result = frame::read_frame(&mut decoder).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn frame_rejects_oversized() {
        let mut buf = Vec::new();
        let budget = OutputBudget::new(10, 100_000_000);
        let mut encoder = Encoder::new(&mut buf, budget);
        let large_data = vec![0u8; (frame::MAX_FRAME_SIZE + 1) as usize];
        assert!(frame::write_frame(&mut encoder, &large_data).is_err());
    }
}
