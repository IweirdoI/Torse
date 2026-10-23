//! Mod host scaffolding and bounded runtime probes.

#![forbid(unsafe_code)]

#[cfg(feature = "wasm-probe")]
mod wasm_probe;

#[cfg(feature = "wasm-probe")]
pub use wasm_probe::{WasmProbeReport, run_wasm_probe};
