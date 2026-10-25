//! Wire one optional recorder through the durable serial execution path.

use torse_codec::StorageCodec;
use torse_diagnostics::SerialProfiler;
use torse_storage::{Journal, JournalBackend};

use crate::execution::{ExecutionHost, ExecutionModel};
use crate::{HostCheckpointStore, Server};

impl<C, B, S> Server<ExecutionHost, Journal<ExecutionModel, C, B>, S>
where
    C: StorageCodec<ExecutionModel>,
    B: JournalBackend,
    S: HostCheckpointStore<ExecutionModel>,
{
    /// Attach between drive calls. Initialization, recovery, and subscription
    /// seeds are excluded; clearing the recorder also excludes warm-up work.
    pub fn set_serial_profiler(&mut self, profiler: SerialProfiler) {
        self.authority.set_serial_profiler(profiler.clone());
        self.journal.set_serial_profiler(profiler.clone());
        self.profiler = profiler;
    }
}
