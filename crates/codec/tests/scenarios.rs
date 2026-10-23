//! Codec scenarios run through the shared headless runner.

mod support;

use support::{Metrics, Scenario};
use torse_codec::{CodecLimits, Decoder, Encoder, frame};
use torse_types::OutputBudget;

const LIMITS: CodecLimits = CodecLimits {
    max_frame_bytes: 1 << 20,
    max_field_bytes: 1 << 16,
    max_depth: 8,
};

struct FrameRoundtrip {
    name: &'static str,
    records: u32,
}

impl Scenario for FrameRoundtrip {
    fn name(&self) -> &str {
        self.name
    }

    fn run(&self, metrics: &mut Metrics) -> Result<(), String> {
        let mut buf = Vec::new();
        metrics.time("encode", || -> Result<(), String> {
            let budget = OutputBudget::new(self.records, 1 << 24);
            let mut encoder = Encoder::new(&mut buf, budget, LIMITS);
            for record in 0..self.records {
                encoder.count_items(1).map_err(|e| e.to_string())?;
                frame::write_frame(&mut encoder, &record.to_le_bytes())
                    .map_err(|e| e.to_string())?;
            }
            encoder.finish();
            Ok(())
        })?;
        metrics.count("records", u64::from(self.records));
        metrics.bytes("encoded", buf.len() as u64);

        metrics.time("decode", || -> Result<(), String> {
            let mut decoder = Decoder::new(&buf, OutputBudget::new(self.records, 0), LIMITS);
            for record in 0..self.records {
                let payload = frame::read_frame(&mut decoder).map_err(|e| e.to_string())?;
                if payload != record.to_le_bytes() {
                    return Err(format!("record {record} mismatch: {payload:?}"));
                }
            }
            if !decoder.is_complete() {
                return Err(format!("{} trailing bytes", decoder.remaining()));
            }
            Ok(())
        })
    }
}

struct BudgetRollback;

impl Scenario for BudgetRollback {
    fn name(&self) -> &str {
        "budget_rollback"
    }

    fn run(&self, metrics: &mut Metrics) -> Result<(), String> {
        let mut buf = vec![0xaa];
        let mut written = 0;
        {
            let mut encoder = Encoder::new(&mut buf, OutputBudget::new(1, 40), LIMITS);
            while encoder.write_u64(u64::MAX).is_ok() {
                written += 1;
            }
        }
        metrics.count("writes_before_limit", written);
        if written != 5 {
            return Err(format!("expected 5 writes within 40 bytes, got {written}"));
        }
        if buf != [0xaa] {
            return Err(format!("unfinished encoder left {} bytes", buf.len() - 1));
        }
        Ok(())
    }
}

#[test]
fn codec_scenarios() {
    support::run_all(&[
        &FrameRoundtrip {
            name: "frame_roundtrip_100",
            records: 100,
        },
        &FrameRoundtrip {
            name: "frame_roundtrip_10000",
            records: 10_000,
        },
        &BudgetRollback,
    ]);
}
