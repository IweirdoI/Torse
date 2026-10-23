//! Bounded commit journals and validated, streaming recovery.
//!
//! The application supplies a trusted checkpoint baseline and an exclusively owned journal.
//! This crate owns encoded data, never mutable authority objects. Checkpoint publication and
//! the world-directory lifecycle are separate integration steps.

#![forbid(unsafe_code)]

mod backend;
mod checkpoint;
mod journal;
mod recovery;

pub use backend::{FileBackend, JournalBackend, MemoryBackend};
pub use journal::{CommitJournal, FileJournal, Journal, JournalConfig, MemoryJournal, Staged};
pub use recovery::{RecoveryReport, scan_journal};
