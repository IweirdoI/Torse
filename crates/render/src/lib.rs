//! Visual extraction, meshing, materials, mesh-cache accounting, and GPU device probes.
//!
//! The mesher is dependency-free CPU code so it can run on a server, in tests,
//! and on a client that has no GPU device. Device selection and the GPU probe
//! stay behind the `gpu` and `gpu-probe` features.

#![forbid(unsafe_code)]

mod frame_budget;
mod mesh;
pub use frame_budget::{FrameBudget, FrameLimits, SectionUploadQueue};

#[cfg(feature = "gpu")]
mod gpu;

#[cfg(feature = "gpu-probe")]
mod gpu_probe;

pub use mesh::{
    CachedMesh, Face, InsertOutcome, MeshBuildError, MeshCache, MeshData, MeshJob, MeshSample,
    ModelMeshError, ModelQuad, RenderBlock, RenderBox, append_model, build_mesh,
    build_mesh_bounded, build_mesh_optimized, build_mesh_optimized_bounded,
};

#[cfg(feature = "gpu")]
pub use gpu::{
    GpuSession, apply_present_mode, backend_order, open_for_window, open_headless, startup_line,
};

#[cfg(feature = "gpu-probe")]
pub use gpu_probe::{GpuProbeReport, run_gpu_probe};
