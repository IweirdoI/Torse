//! Mod host scaffolding and bounded runtime probes.

#![forbid(unsafe_code)]

#[cfg(feature = "component")]
mod budget;

#[cfg(feature = "authority")]
pub mod authority;

#[cfg(feature = "package")]
pub mod package;

#[cfg(feature = "package")]
pub mod durable_dispatch;

#[cfg(feature = "package")]
pub mod scenario;

#[cfg(feature = "component")]
pub mod component;

#[cfg(feature = "wasm-probe")]
mod wasm_probe;

#[cfg(feature = "wasm-probe")]
pub use wasm_probe::{WasmProbeReport, run_wasm_probe};
