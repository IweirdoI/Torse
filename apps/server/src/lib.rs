//! Synchronous host coordination, independent of a concrete authority or transport.
//!
//! B4 exercises these adapters with a test model. The real world codec, authority adapter,
//! listeners, and permission-aware output queues are separate integration work.

#![forbid(unsafe_code)]

mod authority;
mod checkpoint;
mod coordinator;
mod directory;
mod lifecycle;

pub use authority::{HostAuthority, InvalidationSink, NoReplicas, PreparedChange, RecoveryTarget};
pub use checkpoint::HostCheckpointStore;
pub use coordinator::{
    CommitStep, DurablePublication, FlushMetrics, Server, ServerConfig, ServerState, ShutdownReport,
};
pub use directory::WorldDirectory;
pub use lifecycle::{FileServer, FileServerConfig};
