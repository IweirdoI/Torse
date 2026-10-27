//! Segment boundaries, retention roots, interrupted cleanup, and read-only inspection.
mod support;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use support::*;
use torse_storage::{CommitJournal, FileJournal, scan_file_journal};
use torse_types::*;

type Disk = FileJournal<TestModel, TestCodec>;
struct Files(TestFile, PathBuf);
impl Files {
    fn new() -> Self {
        let file = TestFile::new();
        let root = PathBuf::from(format!("{}.segments", file.0.display()));
        Self(file, root)
    }
    fn segments(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = fs::read_dir(&self.1)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|s| s == "seg"))
            .collect();
        paths.sort();
        paths
    }
    fn create(&self) -> Disk {
        Disk::create(&self.0.0, binding(), baseline(), TestCodec::new(), policy()).unwrap()
    }
    fn open(&self, checkpoint: u64) -> Result<(Disk, torse_storage::RecoveryReport), Fault> {
        Disk::open_from_checkpoint(
            &self.0.0,
            binding(),
            baseline(),
            cursor(checkpoint),
            TestCodec::new(),
            policy(),
        )
    }
    fn scan(
        &self,
        checkpoint: u64,
        seen: &mut Vec<u64>,
    ) -> Result<torse_storage::RecoveryReport, Fault> {
        scan_file_journal(
            &self.0.0,
            binding(),
            baseline(),
            cursor(checkpoint),
            &TestCodec::new(),
            policy(),
            |r| {
                seen.push(r.meta.cursor.seq.0);
                Ok(())
            },
        )
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        if self.1.exists() {
            for entry in fs::read_dir(&self.1).unwrap() {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
            fs::remove_dir(&self.1).unwrap();
        }
    }
}
fn policy() -> torse_storage::JournalConfig {
    let mut config = config();
    config.max_segment_bytes = 320;
    config.max_segment_files = 8;
    config
}
fn append(journal: &mut Disk, sequence: u64) {
    let token = journal.try_stage(&commit(sequence)).unwrap();
    journal.accept(token).unwrap();
    journal.flush_through(cursor(sequence)).unwrap();
}

#[test]
fn legacy_migration_preserves_overlap_and_recovers_after_partial_cleanup() {
    let files = Files::new();
    let mut journal = files.create();
    append(&mut journal, 1);
    append(&mut journal, 2);
    journal.reclaim(cursor(1)).unwrap();
    for sequence in 3..=8 {
        append(&mut journal, sequence);
    }
    let old_files: Vec<_> = files
        .segments()
        .iter()
        .map(|p| (p.clone(), fs::read(p).unwrap()))
        .collect();
    journal.reclaim(cursor(5)).unwrap();
    assert!(files.segments().len() < old_files.len());
    drop(journal);
    assert_eq!(fs::read(&files.0.0).unwrap(), b"TORSEJS1");
    // Reintroduce files as if the root survived before obsolete-file deletion.
    for (path, bytes) in old_files {
        if !path.exists() {
            fs::write(path, bytes).unwrap();
        }
    }
    fs::write(files.1.join("head.next"), b"torn future root").unwrap();
    let mut seen = Vec::new();
    assert_eq!(files.scan(5, &mut seen).unwrap().through, cursor(8));
    assert_eq!(seen, vec![6, 7, 8]);
    assert!(files.open(4).is_err());
    let (mut journal, _) = files.open(5).unwrap();
    journal.reclaim(cursor(5)).unwrap();
    append(&mut journal, 9);
    drop(journal);
    assert_eq!(files.open(5).unwrap().1.through, cursor(9));
}

#[test]
fn repeated_reclamation_bounds_disk_use_and_keeps_the_stable_lock() {
    let files = Files::new();
    let mut journal = files.create();
    journal.reclaim(cursor(0)).unwrap();
    for sequence in 1..=256 {
        append(&mut journal, sequence);
        if sequence % 4 == 0 {
            journal.reclaim(cursor(sequence - 2)).unwrap();
            assert!(files.segments().len() <= 2);
            assert!(files.open(sequence).is_err());
        }
    }
    journal.reclaim(cursor(256)).unwrap();
    assert!(files.segments().is_empty());
    drop(journal);
    let (mut journal, report) = files.open(256).unwrap();
    assert_eq!(report.through, cursor(256));
    append(&mut journal, 257);
    drop(journal);
    assert_eq!(files.open(256).unwrap().1.through, cursor(257));
}

#[test]
fn only_final_incomplete_segments_are_repairable_and_inspection_never_writes() {
    let files = Files::new();
    let mut journal = files.create();
    journal.reclaim(cursor(0)).unwrap();
    for sequence in 1..=5 {
        append(&mut journal, sequence);
    }
    drop(journal);
    let last = files.segments().pop().unwrap();
    let original = fs::read(&last).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&last)
        .unwrap()
        .write_all(b"TOR")
        .unwrap();
    let mut seen = Vec::new();
    assert!(files.scan(0, &mut seen).unwrap().incomplete_tail);
    assert_eq!(
        fs::metadata(&last).unwrap().len(),
        original.len() as u64 + 3
    );
    let (journal, report) = files.open(0).unwrap();
    assert!(report.incomplete_tail);
    drop(journal);
    assert_eq!(fs::read(&last).unwrap(), original);
    let first = files.segments()[0].clone();
    fs::write(&first, b"TOR").unwrap();
    assert!(files.open(0).is_err());
    assert_eq!(fs::read(first).unwrap(), b"TOR");
}

#[test]
fn missing_segments_root_corruption_and_complete_frame_damage_never_fall_back() {
    for scenario in 0..4 {
        let files = Files::new();
        let mut journal = files.create();
        journal.reclaim(cursor(0)).unwrap();
        for sequence in 1..=5 {
            append(&mut journal, sequence);
        }
        drop(journal);
        match scenario {
            0 => fs::remove_file(&files.segments()[0]).unwrap(),
            1 => {
                let path = files.segments()[0].clone();
                let mut bytes = fs::read(&path).unwrap();
                bytes[30] ^= 1;
                fs::write(path, bytes).unwrap();
            }
            2 => fs::write(files.1.join("head"), b"invalid").unwrap(),
            _ => fs::remove_file(files.1.join("head")).unwrap(),
        }
        assert!(files.open(0).is_err());
    }
}

#[test]
fn invalid_floors_and_outstanding_reservations_do_not_start_reclamation() {
    let files = Files::new();
    let mut journal = files.create();
    let token = journal.try_stage(&commit(1)).unwrap();
    assert!(journal.reclaim(cursor(0)).is_err());
    drop(token);
    assert!(journal.reclaim(cursor(1)).is_err());
    assert!(!files.1.exists());
    assert!(!journal.is_stopped());
    journal.reclaim(cursor(0)).unwrap();
}
