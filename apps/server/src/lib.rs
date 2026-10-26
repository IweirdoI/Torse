//! Synchronous host coordination and durable local world transactions.

#![forbid(unsafe_code)]

pub mod archive;
mod authority;
pub mod bootstrap;
mod checkpoint;
mod coordinator;
mod directory;
pub mod execution;
mod lifecycle;
#[cfg(feature = "mods")]
pub mod mods;
pub mod network;
mod profiling;
pub mod systems;
pub use execution::PlanningConfig;
mod world;

pub use authority::{HostAuthority, InvalidationSink, NoReplicas, PreparedChange, RecoveryTarget};
pub use checkpoint::HostCheckpointStore;
pub use coordinator::{
    CommitBatch, CommitStep, DurablePublication, FlushMetrics, Server, ServerConfig, ServerState,
    ShutdownReport,
};
pub use directory::WorldDirectory;
pub use lifecycle::{FileServer, FileServerConfig};
pub use world::{LocalCommit, LocalWorldModel, WorldHost, WorldHostRecovery, WorldPrepared};
