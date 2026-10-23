//! Example codec scenarios demonstrating the test framework.

#[path = "test_framework/lib.rs"]
mod test_framework;

use test_framework::{MetricsCollector, Scenario, ScenarioRunner};

// External dependencies need explicit declaration in integration tests
extern crate torse_codec;
extern crate torse_types;

use torse_codec::{Decoder, Encoder};
use torse_types::OutputBudget;

struct CodecRoundtripScenario {
    item_count: u32,
}

impl Scenario for CodecRoundtripScenario {
    fn name(&self) -> &str {
        "codec_roundtrip"
    }

    fn description(&self) -> &str {
        "Test basic encoder/decoder roundtrip with budget tracking"
    }

    fn run(&self, metrics: &mut MetricsCollector) -> Result<(), String> {
        let mut buffer = Vec::new();
        let budget = OutputBudget::new(self.item_count, 10000);

        metrics.start_timer("encode");
        {
            let mut encoder = Encoder::new(&mut buffer, budget);
            for i in 0..self.item_count {
                encoder
                    .write_u32(i)
                    .map_err(|e| format!("encode failed: {}", e.context))?;
            }
            metrics.count("encoded_items", self.item_count as u64);
            metrics.bytes("encoded_bytes", buffer.len() as u64);
        }
        metrics.stop_timer("encode");

        metrics.start_timer("decode");
        let budget = OutputBudget::new(self.item_count, 10000);
        let mut decoder = Decoder::new(&buffer, budget);
        for i in 0..self.item_count {
            let value = decoder
                .read_u32()
                .map_err(|e| format!("decode failed: {}", e.context))?;
            if value != i {
                return Err(format!("mismatch at {}: got {}, expected {}", i, value, i));
            }
        }
        metrics.count("decoded_items", self.item_count as u64);
        metrics.stop_timer("decode");

        if !decoder.is_complete() {
            return Err(format!("decoder has {} remaining bytes", decoder.remaining()));
        }

        Ok(())
    }
}

struct BudgetEnforcementScenario;

impl Scenario for BudgetEnforcementScenario {
    fn name(&self) -> &str {
        "budget_enforcement"
    }

    fn description(&self) -> &str {
        "Verify budget limits are enforced during encoding"
    }

    fn run(&self, metrics: &mut MetricsCollector) -> Result<(), String> {
        let mut buffer = Vec::new();
        let budget = OutputBudget::new(5, 1000);

        metrics.start_timer("encode_until_limit");
        let mut encoder = Encoder::new(&mut buffer, budget);
        let mut count = 0;

        loop {
            match encoder.write_u64(0x1234567890ABCDEF) {
                Ok(_) => count += 1,
                Err(_) => break,
            }
        }
        metrics.stop_timer("encode_until_limit");

        metrics.count("items_before_limit", count);

        if count != 5 {
            return Err(format!("expected 5 items, got {}", count));
        }

        Ok(())
    }
}

#[test]
fn run_codec_scenarios() {
    let scenarios: Vec<Box<dyn Scenario>> = vec![
        Box::new(CodecRoundtripScenario { item_count: 100 }),
        Box::new(CodecRoundtripScenario { item_count: 1000 }),
        Box::new(BudgetEnforcementScenario),
    ];

    let runner = ScenarioRunner::new().with_verbose(true);

    let results: Vec<_> = scenarios.into_iter().map(|s| runner.run(s)).collect();

    runner.print_summary(&results);

    let failed_count = results.iter().filter(|r| !r.success).count();
    assert_eq!(failed_count, 0, "some scenarios failed");
}
