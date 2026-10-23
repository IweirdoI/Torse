//! Host-local checkpoint maintenance without exposing mutable storage handles.

use torse_codec::StorageCodec;
use torse_storage::{CheckpointBackend, CheckpointRetention, CheckpointStore, Checkpoints};
use torse_types::{AuthorityData, Fault};

/// The host keeps at least the current generation before starting another publication.
/// Health is checked after failures because ResourceLimit can occur either before I/O or
/// after a partial write. Only a healthy store permits the host to retry a capacity error.
/// This adapter extends the application's needs without changing the shared storage trait.
pub trait HostCheckpointStore<M: AuthorityData>: CheckpointStore<M> {
    fn prune_checkpoints(&mut self, retain: usize) -> Result<CheckpointRetention, Fault>;
    fn is_stopped(&self) -> bool;
}

impl<M: AuthorityData, C: StorageCodec<M>, B: CheckpointBackend> HostCheckpointStore<M>
    for Checkpoints<M, C, B>
{
    fn prune_checkpoints(&mut self, retain: usize) -> Result<CheckpointRetention, Fault> {
        self.prune(retain)
    }

    fn is_stopped(&self) -> bool {
        Checkpoints::is_stopped(self)
    }
}
