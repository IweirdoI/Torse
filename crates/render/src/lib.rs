//! Visual extraction, meshing, materials, and GPU device probes.

#![forbid(unsafe_code)]

#[cfg(feature = "gpu-probe")]
mod gpu_probe;

#[cfg(feature = "gpu-probe")]
pub use gpu_probe::{GpuProbeReport, run_gpu_probe};
