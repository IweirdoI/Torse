//! M0 experiment: batched calls into a Wasmtime sandbox.

use std::time::{Duration, Instant};

use wasmtime::{Engine, Instance, Module, Store};

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

#[derive(Clone, Copy, Debug)]
pub struct WasmProbeReport {
    pub calls: u32,
    pub iterations_per_call: u32,
    pub elapsed: Duration,
    pub checksum: u32,
}

pub fn run_wasm_probe(calls: u32, iterations_per_call: u32) -> Result<WasmProbeReport, String> {
    let engine = Engine::default();
    let module = Module::new(&engine, ADD_LOOP_WAT).map_err(|error| error.to_string())?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).map_err(|error| error.to_string())?;
    let add_loop = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "add_loop")
        .map_err(|error| error.to_string())?;

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
        let report = run_wasm_probe(4, 3).unwrap();
        assert_eq!(report.checksum, 90);
    }
}
