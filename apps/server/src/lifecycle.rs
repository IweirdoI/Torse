//! File lifecycle: inspect, restore in isolation, establish barriers, then open admission.

use std::fs::{self, OpenOptions};

use torse_codec::StorageCodec;
use torse_storage::{
    CheckpointConfig, Checkpoints, FileCheckpointBackend, FileCheckpoints, FileJournal,
    JournalConfig, scan_journal,
};
use torse_types::{Binding, CommitCursor, Compatibility, Fault, FaultCode};

use crate::directory::{io_fault, sync_directory};
use crate::{HostAuthority, RecoveryTarget, Server, ServerConfig, WorldDirectory};

pub type FileServer<A, C> = Server<
    A,
    FileJournal<<A as HostAuthority>::Model, C>,
    FileCheckpoints<<A as HostAuthority>::Model, C>,
>;

#[derive(Clone, Copy, Debug)]
pub struct FileServerConfig {
    pub server: ServerConfig,
    pub journal: JournalConfig,
    pub checkpoint: CheckpointConfig,
}

impl FileServerConfig {
    /// Check all policy before invoking domain construction or repairing any data tail.
    pub fn validate(self) -> Result<(), Fault> {
        self.server.validate()?;
        self.journal.validate()?;
        self.checkpoint.validate()?;
        if self.server.checkpoint_predecessors >= self.checkpoint.max_snapshot_files {
            return Err(Fault::invalid_data(
                "checkpoint capacity must fit retained predecessors and a new snapshot",
            ));
        }
        Ok(())
    }
}

impl<A: HostAuthority, C: StorageCodec<A::Model> + Clone> FileServer<A, C> {
    /// Create in a leased root. Existing data paths are rejected without overwriting them.
    /// Incomplete initialization is never silently treated as a new empty running world.
    pub fn create_file(
        directory: WorldDirectory,
        compatibility: Compatibility,
        codec: C,
        config: FileServerConfig,
        initialize: impl FnOnce(Binding) -> Result<A, Fault>,
    ) -> Result<Self, Fault> {
        config.validate()?;
        let binding = Binding {
            world: directory.world(),
            epoch: directory.epoch(),
            compatibility,
        };
        let authority = initialize(binding)?;
        let checkpoint_path = directory.root().join("checkpoints");
        let journal_path = directory.root().join("journal.log");
        if journal_path
            .try_exists()
            .map_err(|e| io_fault("inspect journal path", e))?
            || checkpoint_path
                .try_exists()
                .map_err(|e| io_fault("inspect checkpoint path", e))?
        {
            return Err(Fault::invalid_data("world data already exists"));
        }
        fs::create_dir(&checkpoint_path).map_err(|e| io_fault("create checkpoint directory", e))?;
        let checkpoints = Checkpoints::open(
            binding,
            codec.clone(),
            config.checkpoint,
            FileCheckpointBackend::open(checkpoint_path)?,
        )?;
        let journal = FileJournal::create(
            &journal_path,
            binding,
            CommitCursor::initial(binding.world),
            codec,
            config.journal,
        )?;
        // Position zero has no journal append to trigger a flush. Persist the empty file and
        // its parent namespace before publishing the complete initial checkpoint.
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(journal_path)
            .and_then(|file| file.sync_all())
            .map_err(|e| io_fault("sync initial journal", e))?;
        sync_directory(directory.root())?;
        Self::initialize(directory, authority, journal, checkpoints, config.server)
    }

    /// B2 currently retains the complete journal from position one. Validate that whole log,
    /// replay only records after the checkpoint, and require both C and the manifest's known
    /// durable position to exist. A removed whole-record suffix must not become silent loss.
    pub fn open_file<R>(
        directory: WorldDirectory,
        compatibility: Compatibility,
        codec: C,
        config: FileServerConfig,
        begin_restore: impl FnOnce(Binding) -> Result<R, Fault>,
    ) -> Result<Self, Fault>
    where
        R: RecoveryTarget<Model = A::Model, Authority = A>,
    {
        config.validate()?;
        let binding = Binding {
            world: directory.world(),
            epoch: directory.epoch(),
            compatibility,
        };
        let checkpoint_path = directory.root().join("checkpoints");
        let journal_path = directory.root().join("journal.log");
        let checkpoint_dir = fs::symlink_metadata(&checkpoint_path)
            .map_err(|e| io_fault("inspect recovery checkpoint directory", e))?;
        if !checkpoint_dir.file_type().is_dir() {
            return Err(Fault::invalid_data(
                "recovery checkpoint root must be a real directory",
            ));
        }
        for path in [&checkpoint_path.join("manifest.log"), &journal_path] {
            let meta =
                fs::symlink_metadata(path).map_err(|e| io_fault("inspect recovery file", e))?;
            if !meta.file_type().is_file() {
                return Err(Fault::invalid_data("recovery data must be regular files"));
            }
        }
        let mut checkpoints = Checkpoints::inspect(
            binding,
            codec.clone(),
            config.checkpoint,
            FileCheckpointBackend::open(checkpoint_path)?,
        )?;
        let manifest = checkpoints.current().ok_or_else(|| {
            Fault::new(
                FaultCode::CorruptData,
                "world has no complete initial checkpoint",
            )
        })?;
        let snapshot = checkpoints.load_current()?.expect("manifest was present");
        let baseline = snapshot.meta.cursor;
        let mut expected_resume = snapshot.meta.resume_at;
        let mut recovery = begin_restore(binding)?;
        recovery.restore_snapshot(snapshot)?;
        let mut reader = OpenOptions::new()
            .read(true)
            .open(&journal_path)
            .map_err(|e| io_fault("open recovery journal", e))?;
        let report = scan_journal::<A::Model, _, _, _>(
            &mut reader,
            binding,
            CommitCursor::initial(binding.world),
            &codec,
            config.journal.codec_limits,
            config.journal.codec_budget,
            |record| {
                if record.meta.cursor.seq > baseline.seq {
                    expected_resume = record.meta.resume_at;
                    recovery.replay(record)?;
                }
                Ok(())
            },
        )?;
        drop(reader);
        if report.through.seq < baseline.seq || report.through.seq.0 < manifest.durable_through.0 {
            return Err(Fault::new(
                FaultCode::SequenceGap,
                "journal is shorter than the checkpoint recovery expectation",
            ));
        }
        let authority = recovery.finish(report.through)?;
        // Validate the assembled adapter before repairing either storage tail. The public
        // server does not exist yet, so no transport can submit work or inspect this world.
        if authority.binding() != binding
            || authority.cursor() != report.through
            || authority.resume_point() != expected_resume
        {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "recovery produced the wrong authority binding, cursor, or resume point",
            ));
        }
        checkpoints.activate()?;
        let (journal, reopened) = FileJournal::open(
            journal_path,
            binding,
            CommitCursor::initial(binding.world),
            codec,
            config.journal,
        )?;
        if reopened != report {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "journal changed during isolated recovery",
            ));
        }
        Self::assemble(directory, authority, journal, checkpoints, config.server)
    }
}
