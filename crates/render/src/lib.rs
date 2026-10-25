//! Visual extraction, meshing, materials, and GPU device probes.
//!
//! The mesher is dependency-free CPU code so it can run on a server, in tests,
//! and on a client that has no GPU device. The GPU probe stays behind the
//! `gpu-probe` feature.

#![forbid(unsafe_code)]

mod mesh;

#[cfg(feature = "gpu-probe")]
mod gpu_probe;

pub use mesh::{
    CachedMesh, Face, InsertOutcome, MeshBuildError, MeshCache, MeshData, MeshJob, MeshSample,
    RenderBlock, RenderBox, build_mesh, build_mesh_bounded,
};

#[cfg(feature = "gpu-probe")]
pub use gpu_probe::{GpuProbeReport, run_gpu_probe};
