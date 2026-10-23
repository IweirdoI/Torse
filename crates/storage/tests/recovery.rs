//! Recovery validation is tested separately from journal acceptance and authority gameplay.

mod support;

use std::io::{self, Cursor, Read};

use support::*;
use torse_codec::record::{self, RecordKind};
use torse_storage::{FileJournal, RecoveryReport, scan_journal};
use torse_types::*;

fn scan(bytes: &[u8]) -> Result<(RecoveryReport, Vec<ModelCommit<TestModel>>), Fault> {
    let mut values = Vec::new();
    let report = scan_journal::<TestModel, _, _, _>(
        &mut Cursor::new(bytes),
        binding(),
        baseline(),
        &TestCodec::new(),
        LIMITS,
        config().codec_budget,
        |value| {
            values.push(value);
            Ok(())
        },
    )?;
    Ok((report, values))
}

#[test]
fn every_tail_truncation_returns_exactly_the_previous_complete_prefix() {
    let first = encoded(&commit(1));
    let second = encoded(&commit(2));
    for cut in 0..second.len() {
        let bytes = [&first[..], &second[..cut]].concat();
        let (report, values) = scan(&bytes).unwrap();
        assert_eq!(report.through, cursor(1));
        assert_eq!(report.valid_bytes, first.len() as u64);
        assert_eq!(report.records, 1);
        assert_eq!(report.incomplete_tail, cut != 0);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].changes, commit(1).changes);
    }
}

#[test]
fn complete_corruption_is_fatal_at_both_middle_and_final_record_positions() {
    let first = encoded(&commit(1));
    let second = encoded(&commit(2));
    for damage_first in [false, true] {
        for offset in 0..first.len() {
            let mut bytes = [&first[..], &second[..]].concat();
            let base = if damage_first { 0 } else { first.len() };
            bytes[base + offset] ^= 1;
            let error = scan(&bytes).expect_err("corruption must not be skipped");
            assert_eq!(error.code, FaultCode::CorruptData, "record byte {offset}");
            assert!(error.context.contains(&format!("offset {base}:")));
        }
    }
}

#[test]
fn gaps_duplicates_worlds_and_compatibility_are_validated_before_replay() {
    let mut wrong_world = commit(2);
    wrong_world.meta.cursor.world = WorldId(99);
    let mut incompatible = commit(2);
    incompatible.meta.compatibility.content_digest = Digest([4; 32]);
    for (value, expected) in [
        (commit(3), FaultCode::SequenceGap),
        (commit(1), FaultCode::SequenceGap),
        (wrong_world, FaultCode::InvalidData),
        (incompatible, FaultCode::Incompatible),
    ] {
        let bytes = [encoded(&commit(1)), encoded(&value)].concat();
        let mut replayed = 0;
        let result = scan_journal::<TestModel, _, _, _>(
            &mut Cursor::new(&bytes),
            binding(),
            baseline(),
            &TestCodec::new(),
            LIMITS,
            config().codec_budget,
            |_| {
                replayed += 1;
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().code, expected);
        assert_eq!(
            replayed, 1,
            "invalid records never reach the recovery builder"
        );
    }
}

#[test]
fn empty_logs_and_nonzero_checkpoint_baselines_have_explicit_positions() {
    let (report, values) = scan(b"").unwrap();
    assert_eq!(report.through, baseline());
    assert_eq!(report.valid_bytes, 0);
    assert!(!report.incomplete_tail);
    assert!(values.is_empty());
    for bytes in [Vec::new(), encoded(&commit(6))] {
        let report = scan_journal::<TestModel, _, _, _>(
            &mut Cursor::new(&bytes),
            binding(),
            cursor(5),
            &TestCodec::new(),
            LIMITS,
            config().codec_budget,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            report.through,
            if bytes.is_empty() {
                cursor(5)
            } else {
                cursor(6)
            }
        );
    }
    let error = scan_journal::<TestModel, _, _, _>(
        &mut Cursor::new([]),
        binding(),
        CommitCursor::initial(WorldId(99)),
        &TestCodec::new(),
        LIMITS,
        config().codec_budget,
        |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.code, FaultCode::InvalidData);
}

#[test]
fn each_record_has_an_allocation_budget_and_unknown_payload_bytes_are_preserved() {
    let mut value = commit(1);
    value.changes = (0..=255).collect();
    let bytes = encoded(&value);
    let (_, first) = scan(&bytes).unwrap();
    let (_, second) = scan(&bytes).unwrap();
    assert_eq!(first[0].changes, value.changes);
    assert_eq!(second[0].changes, value.changes);
    assert_eq!(encoded(&first[0]), bytes);
    for budget in [
        OutputBudget::new(0, 4096),
        OutputBudget::new(8, 0),
        OutputBudget::new(8, 200),
    ] {
        let result = scan_journal::<TestModel, _, _, _>(
            &mut Cursor::new(&bytes),
            binding(),
            baseline(),
            &TestCodec::new(),
            LIMITS,
            budget,
            |_| panic!("allocation budget failure must precede replay"),
        );
        assert_eq!(result.unwrap_err().code, FaultCode::ResourceLimit);
    }
}

#[test]
fn invalid_domain_payloads_and_wrong_record_kinds_are_rejected() {
    assert!(scan(&framed(b"", RecordKind::Commit)).is_err());
    assert_eq!(
        scan(&framed(b"", RecordKind::Snapshot)).err().unwrap().code,
        FaultCode::CorruptData
    );
    // The envelope checksum is valid, but the domain codec must reject trailing payload bytes.
    let valid = encoded(&commit(1));
    let mut payload = payload_of(&valid).to_vec();
    payload.push(0xaa);
    assert_eq!(
        scan(&framed(&payload, RecordKind::Commit))
            .err()
            .unwrap()
            .code,
        FaultCode::InvalidData
    );
}

fn payload_of(bytes: &[u8]) -> &[u8] {
    match record::read_record(bytes, LIMITS).unwrap() {
        record::RecordRead::Complete { payload, .. } => payload,
        record::RecordRead::Incomplete => panic!("fixture must be complete"),
    }
}

/// Exercises fragmented reads and Interrupted separately from physical EOF and device errors.
struct FragmentedReader {
    input: Cursor<Vec<u8>>,
    interrupt_once: bool,
    fail_at: Option<u64>,
}

impl Read for FragmentedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.interrupt_once {
            self.interrupt_once = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self
            .fail_at
            .is_some_and(|offset| self.input.position() >= offset)
        {
            return Err(io::Error::other("injected reader failure"));
        }
        let count = output.len().min(1);
        self.input.read(&mut output[..count])
    }
}

#[test]
fn fragmented_reads_retry_interruptions_but_io_failure_is_never_treated_as_torn_eof() {
    for fail_at in [None, Some(0), Some(5), Some(30)] {
        let mut reader = FragmentedReader {
            input: Cursor::new(encoded(&commit(1))),
            interrupt_once: true,
            fail_at,
        };
        let result = scan_journal::<TestModel, _, _, _>(
            &mut reader,
            binding(),
            baseline(),
            &TestCodec::new(),
            LIMITS,
            config().codec_budget,
            |_| Ok(()),
        );
        if fail_at.is_none() {
            assert_eq!(result.unwrap().through, cursor(1));
        } else {
            assert_eq!(result.unwrap_err().code, FaultCode::StorageUnavailable);
        }
    }
}

#[test]
fn oversized_header_is_rejected_before_reading_or_allocating_its_payload() {
    let mut bytes = encoded(&commit(1));
    bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    // Damaged length bytes must fail header validation, even if they now claim an EOF tail.
    let mut input = Cursor::new(bytes);
    let error = scan_journal::<TestModel, _, _, _>(
        &mut input,
        binding(),
        baseline(),
        &TestCodec::new(),
        LIMITS,
        config().codec_budget,
        |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.code, FaultCode::CorruptData);
    assert_eq!(input.position(), record::HEADER_BYTES as u64);

    let mut small_limits = LIMITS;
    small_limits.max_frame_bytes = 1;
    let mut input = Cursor::new(encoded(&commit(1)));
    let error = scan_journal::<TestModel, _, _, _>(
        &mut input,
        binding(),
        baseline(),
        &TestCodec::new(),
        small_limits,
        config().codec_budget,
        |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.code, FaultCode::ResourceLimit);
    assert_eq!(input.position(), record::HEADER_BYTES as u64);
}

#[test]
fn callback_failure_ends_recovery_and_no_later_commit_is_delivered() {
    let bytes = [encoded(&commit(1)), encoded(&commit(2))].concat();
    let mut replayed = 0;
    let error = scan_journal::<TestModel, _, _, _>(
        &mut Cursor::new(&bytes),
        binding(),
        baseline(),
        &TestCodec::new(),
        LIMITS,
        config().codec_budget,
        |_| {
            replayed += 1;
            Err(Fault::new(
                FaultCode::InvalidState,
                "injected domain invariant failure",
            ))
        },
    )
    .unwrap_err();
    assert_eq!(error.code, FaultCode::InvalidState);
    assert_eq!(replayed, 1);
}

#[test]
fn inserted_or_removed_middle_bytes_and_trailing_garbage_are_never_skipped() {
    let first = encoded(&commit(1));
    let second = encoded(&commit(2));
    let mut removed = [&first[..], &second[..]].concat();
    removed.remove(record::HEADER_BYTES + 1);
    let mut inserted = [&first[..], &second[..]].concat();
    inserted.insert(record::HEADER_BYTES + 1, 0xaa);
    let garbage = [&first[..], b"BAD"].concat();
    for bytes in [removed, inserted, garbage] {
        assert_eq!(scan(&bytes).unwrap_err().code, FaultCode::CorruptData);
    }
}

#[test]
fn file_reopen_failure_does_not_truncate_a_gap_or_incompatible_or_over_budget_log() {
    type Disk = FileJournal<TestModel, TestCodec>;
    let path = TestFile::new();
    let bytes = [encoded(&commit(1)), encoded(&commit(3))].concat();
    std::fs::write(&path.0, &bytes).unwrap();
    assert!(Disk::open(&path.0, binding(), baseline(), TestCodec::new(), config()).is_err());
    assert_eq!(std::fs::read(&path.0).unwrap(), bytes);
    let bytes = encoded(&commit(1));
    std::fs::write(&path.0, &bytes).unwrap();
    let mut incompatible = binding();
    incompatible.compatibility.authority_schema += 1;
    assert!(
        Disk::open(
            &path.0,
            incompatible,
            baseline(),
            TestCodec::new(),
            config()
        )
        .is_err()
    );
    let mut too_small = config();
    too_small.codec_budget.max_bytes = 1;
    assert!(Disk::open(&path.0, binding(), baseline(), TestCodec::new(), too_small).is_err());
    assert_eq!(std::fs::read(&path.0).unwrap(), bytes);
}
