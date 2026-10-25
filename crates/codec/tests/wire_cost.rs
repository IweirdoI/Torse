//! Wire frame size and decoded allocation budgets are independent limits.
use torse_codec::{CodecLimits, wire};
use torse_types::OutputBudget;

#[test]
fn cost_measurement_uses_frame_limit_only_for_encoded_bytes() {
    let value = vec![vec![7u64]; 8];
    let budget = OutputBudget::new(100, 4096);
    let mut limits = CodecLimits {
        max_frame_bytes: 4096,
        max_field_bytes: 1024,
        max_depth: 12,
    };
    let mut bytes = vec![];
    wire::encode(&value, &mut bytes, budget, limits).unwrap();
    limits.max_frame_bytes = bytes.len() as u32;
    assert_eq!(
        wire::decode::<Vec<Vec<u64>>>(&bytes, budget, limits).unwrap(),
        value
    );
    let cost = wire::measure_inner(&value, budget, limits).unwrap();
    assert!(cost.owned_bytes > u64::from(limits.max_frame_bytes));
    assert_eq!(cost.encoded_bytes + 2, bytes.len() as u64);
    assert!(
        wire::measure_inner(&value, OutputBudget::new(100, cost.owned_bytes - 1), limits).is_err()
    );
    limits.max_frame_bytes = cost.encoded_bytes as u32 - 1;
    assert!(wire::measure_inner(&value, budget, limits).is_err());
}
