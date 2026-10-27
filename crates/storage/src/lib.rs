//! Bounded commit journals, checkpoint publication, and validated recovery.
//!
//! The application supplies a trusted checkpoint baseline and an exclusively owned journal.
//! This crate owns encoded data, never mutable authority objects. The application pauses at a
//! tick boundary and supplies a complete snapshot. World-directory orchestration is separate.

#![forbid(unsafe_code)]

mod backend;
mod checkpoint;
mod checkpoint_backend;
mod journal;
mod manifest;
mod recovery;
mod segments;

pub use backend::{FileBackend, JournalBackend, MemoryBackend};
pub use checkpoint::{
    CheckpointConfig, CheckpointRetention, CheckpointStore, Checkpoints, FileCheckpoints,
    MemoryCheckpoints,
};
pub use checkpoint_backend::{CheckpointBackend, FileCheckpointBackend, MemoryCheckpointBackend};
pub use journal::{CommitJournal, FileJournal, Journal, JournalConfig, MemoryJournal, Staged};
pub use manifest::CheckpointManifest;
pub use recovery::{RecoveryReport, scan_journal};
pub use segments::scan_file_journal;
