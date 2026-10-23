//! M0 experiment: batched calls into a Wasmtime sandbox.
//!
//! The probe compiles a small module containing real arithmetic and a loop, then
//! performs repeated typed calls through the host boundary. It measures call and
//! execution overhead without claiming that a no-op function represents gameplay
//! logic. Permissions, fuel/epoch quotas, memory limits, and trap recovery are
//! intentionally tested by later Mod-host milestones.

use std::time::{Duration, Instant};

use wasmtime::{Engine, Instance, Module, Store};

/// WebAssembly text module used by the probe.
///
/// `add_loop(iterations, seed)` adds seven `iterations` times and returns the
/// resulting signed 32-bit value. The loop makes each call do measurable work.
const ADD_LOOP_WAT: &str = r#"
(module
  (func (export "add_loop")
    (param $iterations i32) (param $seed i32) (result i32)
    (local $index i32)
    (local $value i32)
    local.get $seed
    local.set $value
    (block $done
      (loop $next
        local.get $index
        local.get $iterations
        i32.ge_u
        br_if $done
        local.get $value
        i32.const 7
        i32.add
        local.set $value
        local.get $index
        i32.const 1
        i32.add
        local.set $index
        br $next
      )
    )
    local.get $value
  )
)
"#;

/// Observable result of one Wasmtime probe.
#[derive(Clone, Copy, Debug)]
pub struct WasmProbeReport {
    /// Number of host-to-Wasm calls in the timed section.
    pub calls: u32,
    /// Arithmetic loop iterations performed per call.
    pub iterations_per_call: u32,
    /// Wall-clock time for calls only; compilation and instantiation are excluded.
    pub elapsed: Duration,
    /// Wrapping sum of returned values, used as a correctness signal.
    pub checksum: u32,
}

/// Compile the fixture module and execute a deterministic batch of typed calls.
///
/// Errors are converted to owned strings because this probe is called by the
/// tool application and does not define the production host error model.
pub fn run_wasm_probe(calls: u32, iterations_per_call: u32) -> Result<WasmProbeReport, String> {
    let engine = Engine::default();
    let module = Module::new(&engine, ADD_LOOP_WAT).map_err(|error| error.to_string())?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).map_err(|error| error.to_string())?;
    let add_loop = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "add_loop")
        .map_err(|error| error.to_string())?;

    // Start timing after compilation/instantiation so the sample reflects repeated
    // call overhead and guest work rather than one-time setup cost.
    let start = Instant::now();
    let mut checksum = 0_u32;
    for call in 0..calls {
        let value = add_loop
            .call(&mut store, (iterations_per_call as i32, call as i32))
            .map_err(|error| error.to_string())?;
        checksum = checksum.wrapping_add(value as u32);
    }
    let elapsed = start.elapsed();

    Ok(WasmProbeReport {
        calls,
        iterations_per_call,
        elapsed,
        checksum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_loop_returns_expected_value() {
        // Seeds 0..3 with three iterations each produce 21, 22, 23, and 24.
        let report = run_wasm_probe(4, 3).unwrap();
        assert_eq!(report.checksum, 90);
    }
}
