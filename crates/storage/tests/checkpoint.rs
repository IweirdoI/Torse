//! Publication failure boundaries and recovery selection use an intentionally opaque model.

mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use support::*;
use torse_codec::record;
use torse_storage::{
    CheckpointBackend, CheckpointConfig, CheckpointStore, Checkpoints, FileCheckpointBackend,
    FileCheckpoints, MemoryCheckpointBackend, MemoryCheckpoints,
};
use torse_types::*;

fn policy() -> CheckpointConfig {
    CheckpointConfig {
        codec_limits: LIMITS,
        codec_budget: OutputBudget::new(8, 2048),
        max_manifest_bytes: 4096,
        max_snapshot_files: 8,
    }
}

fn snapshot(sequence: u64) -> ModelSnapshot<TestModel> {
    WorldSnapshot {
        meta: SnapshotMeta {
            cursor: cursor(sequence),
            compatibility: binding().compatibility,
            resume_at: ResumePoint {
                tick: Tick(sequence),
                next_phase: 0,
                next_step: 0,
            },
        },
        state: vec![sequence as u8, 0, 255],
    }
}

fn durable(sequence: u64) -> Durability {
    Durability {
        binding: binding(),
        through: DurableSeq(sequence),
    }
}

fn memory() -> MemoryCheckpoints<TestModel, TestCodec> {
    Checkpoints::open(
        binding(),
        TestCodec::new(),
        policy(),
        MemoryCheckpointBackend::new(16384),
    )
    .unwrap()
}

/// The directory is unique, lives in the repository work area, and contains only files owned
/// by this test. Cleanup is non-recursive and never follows a computed directory tree.
struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.work/checkpoint-tests");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn snapshot(&self, generation: u64) -> PathBuf {
        self.0.join(format!("checkpoint-{generation:016x}.bin"))
    }
    fn open(&self) -> FileCheckpoints<TestModel, TestCodec> {
        Checkpoints::open(
            binding(),
            TestCodec::new(),
            policy(),
            FileCheckpointBackend::open(&self.0).unwrap(),
        )
        .unwrap()
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.0).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(&self.0).unwrap();
    }
}

#[test]
fn initial_checkpoint_and_later_generation_survive_reopen_with_a_new_epoch() {
    let directory = TestDirectory::new();
    let mut store = directory.open();
    assert!(store.load_current().unwrap().is_none());
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    store.publish_checkpoint(&snapshot(4), durable(6)).unwrap();
    assert!(directory.snapshot(1).exists());
    assert!(directory.snapshot(2).exists());
    drop(store);
    let mut new_binding = binding();
    new_binding.epoch = WorldEpoch(99);
    let mut reopened = Checkpoints::<TestModel, _, _>::open(
        new_binding,
        TestCodec::new(),
        policy(),
        FileCheckpointBackend::open(&directory.0).unwrap(),
    )
    .unwrap();
    assert_eq!(reopened.current().unwrap().durable_through, DurableSeq(6));
    let value = reopened.load_current().unwrap().unwrap();
    assert_eq!(value.meta, snapshot(4).meta);
    assert_eq!(value.state, snapshot(4).state);
    assert!(
        reopened
            .publish_checkpoint(&snapshot(5), durable(6))
            .is_err()
    );
    let new_durable = Durability {
        binding: new_binding,
        through: DurableSeq(6),
    };
    reopened
        .publish_checkpoint(&snapshot(5), new_durable)
        .unwrap();
}

#[test]
fn invalid_identity_durability_and_regression_never_publish() {
    let mut store = memory();
    assert!(store.publish_checkpoint(&snapshot(1), durable(1)).is_err());
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    let mut wrong_world = snapshot(1);
    wrong_world.meta.cursor.world = WorldId(99);
    let mut wrong_compat = snapshot(1);
    wrong_compat.meta.compatibility.authority_schema = 99;
    for value in [wrong_world, wrong_compat] {
        assert!(store.publish_checkpoint(&value, durable(1)).is_err());
    }
    let mut wrong_binding = durable(1);
    wrong_binding.binding.epoch = WorldEpoch(99);
    assert!(
        store
            .publish_checkpoint(&snapshot(1), wrong_binding)
            .is_err()
    );
    assert!(store.publish_checkpoint(&snapshot(2), durable(1)).is_err());
    store.publish_checkpoint(&snapshot(3), durable(4)).unwrap();
    assert!(store.publish_checkpoint(&snapshot(2), durable(4)).is_err());
    assert!(store.publish_checkpoint(&snapshot(4), durable(3)).is_err());
    assert_eq!(store.current().unwrap().generation, 2);
    assert!(!store.is_stopped());
}

#[test]
fn phase_and_step_are_preserved_for_the_authority_to_validate() {
    let mut value = snapshot(0);
    value.meta.resume_at.next_phase = 42;
    value.meta.resume_at.next_step = 77;
    let mut store = memory();
    store.publish_checkpoint(&value, durable(0)).unwrap();
    assert_eq!(store.load_current().unwrap().unwrap().meta, value.meta);
}

#[test]
fn pruning_uses_covered_positions_and_releases_snapshot_capacity() {
    let mut config = policy();
    config.max_snapshot_files = 2;
    let mut store = Checkpoints::<TestModel, _, _>::open(
        binding(),
        TestCodec::new(),
        config,
        MemoryCheckpointBackend::new(16384),
    )
    .unwrap();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    store.publish_checkpoint(&snapshot(4), durable(6)).unwrap();
    assert_eq!(
        store
            .publish_checkpoint(&snapshot(5), durable(6))
            .unwrap_err()
            .code,
        FaultCode::ResourceLimit
    );
    assert!(store.prune(0).is_err());
    let kept = store.prune(2).unwrap();
    assert_eq!(kept.log_floor, Some(cursor(0)));
    assert!(kept.removed_generations.is_empty());
    let pruned = store.prune(1).unwrap();
    assert_eq!(pruned.log_floor, Some(cursor(4)));
    assert_eq!(pruned.removed_generations, vec![1]);
    assert_eq!(store.prune(8).unwrap().log_floor, Some(cursor(4)));
    store.publish_checkpoint(&snapshot(5), durable(6)).unwrap();
    assert_eq!(store.prune(2).unwrap().log_floor, Some(cursor(4)));
}

#[test]
fn manifest_and_codec_budgets_fail_before_creating_a_new_snapshot() {
    let mut store = memory();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    let entry_length = store.into_backend().read_manifest(4096).unwrap().len();
    let mut config = policy();
    config.max_manifest_bytes = entry_length as u64;
    let mut store = Checkpoints::<TestModel, _, _>::open(
        binding(),
        TestCodec::new(),
        config,
        MemoryCheckpointBackend::new(16384),
    )
    .unwrap();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    assert_eq!(
        store
            .publish_checkpoint(&snapshot(1), durable(1))
            .unwrap_err()
            .code,
        FaultCode::ResourceLimit
    );
    let mut backend = store.into_backend();
    assert_eq!(backend.snapshot_generations(8).unwrap(), vec![1]);
    config = policy();
    config.codec_budget = OutputBudget::new(0, 2048);
    let mut store = Checkpoints::<TestModel, _, _>::open(
        binding(),
        TestCodec::new(),
        config,
        MemoryCheckpointBackend::new(16384),
    )
    .unwrap();
    assert!(store.publish_checkpoint(&snapshot(0), durable(0)).is_err());
    assert!(
        store
            .into_backend()
            .snapshot_generations(8)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn file_manifest_is_exclusively_locked() {
    let directory = TestDirectory::new();
    let store = directory.open();
    assert!(FileCheckpointBackend::open(&directory.0).is_err());
    drop(store);
    assert!(FileCheckpointBackend::open(&directory.0).is_ok());
}

#[test]
fn every_torn_manifest_tail_selects_old_checkpoint_then_skips_orphan_generation() {
    let directory = TestDirectory::new();
    let mut store = directory.open();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    store.publish_checkpoint(&snapshot(3), durable(3)).unwrap();
    drop(store);
    let path = directory.0.join("manifest.log");
    let complete = fs::read(&path).unwrap();
    let width = complete.len() / 2;
    for end in width..complete.len() {
        fs::write(&path, &complete[..end]).unwrap();
        let mut store = directory.open();
        assert_eq!(store.current().unwrap().snapshot.cursor, cursor(0));
        assert_eq!(fs::metadata(&path).unwrap().len(), width as u64);
        assert_eq!(store.prune(1).unwrap().removed_generations, vec![2]);
        store.publish_checkpoint(&snapshot(5), durable(5)).unwrap();
        assert_eq!(store.current().unwrap().generation, 3);
        drop(store);
        fs::remove_file(directory.snapshot(3)).unwrap();
        // Recreate the same abandoned generation for the next cut position.
        fs::copy(directory.snapshot(1), directory.snapshot(2)).unwrap();
    }
}

#[test]
fn complete_manifest_corruption_never_repairs_or_falls_back() {
    let directory = TestDirectory::new();
    let mut store = directory.open();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    store.publish_checkpoint(&snapshot(2), durable(2)).unwrap();
    drop(store);
    let path = directory.0.join("manifest.log");
    let original = fs::read(&path).unwrap();
    for index in 0..original.len() {
        let mut bytes = original.clone();
        bytes[index] ^= 1;
        fs::write(&path, &bytes).unwrap();
        let result = Checkpoints::<TestModel, _, _>::open(
            binding(),
            TestCodec::new(),
            policy(),
            FileCheckpointBackend::open(&directory.0).unwrap(),
        );
        assert!(result.is_err(), "manifest byte {index}");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(directory.snapshot(1).exists());
    }
}

#[test]
fn latest_snapshot_corruption_prevents_manifest_tail_repair_and_cleanup() {
    let directory = TestDirectory::new();
    let mut store = directory.open();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    store.publish_checkpoint(&snapshot(2), durable(2)).unwrap();
    let bytes = fs::read(directory.snapshot(2)).unwrap();
    for end in [0, bytes.len() - 1, bytes.len()] {
        let mut damaged = bytes[..end].to_vec();
        if end == bytes.len() {
            damaged[record::HEADER_BYTES] ^= 1;
        }
        fs::write(directory.snapshot(2), damaged).unwrap();
        assert!(store.prune(1).is_err());
        assert!(directory.snapshot(1).exists());
        drop(store);
        let path = directory.0.join("manifest.log");
        let mut manifest = fs::read(&path).unwrap();
        manifest.extend_from_slice(b"TOR");
        fs::write(&path, &manifest).unwrap();
        let result = Checkpoints::<TestModel, _, _>::open(
            binding(),
            TestCodec::new(),
            policy(),
            FileCheckpointBackend::open(&directory.0).unwrap(),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), manifest);
        fs::write(directory.snapshot(2), &bytes).unwrap();
        store = directory.open();
    }
}

#[test]
fn missing_latest_snapshot_is_fatal_even_when_old_checkpoint_exists() {
    let directory = TestDirectory::new();
    let mut store = directory.open();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    store.publish_checkpoint(&snapshot(2), durable(2)).unwrap();
    drop(store);
    fs::remove_file(directory.snapshot(2)).unwrap();
    assert!(
        Checkpoints::<TestModel, _, _>::open(
            binding(),
            TestCodec::new(),
            policy(),
            FileCheckpointBackend::open(&directory.0).unwrap()
        )
        .is_err()
    );
    assert!(directory.snapshot(1).exists());
}

#[test]
fn unexpected_files_and_over_limit_lengths_are_rejected_without_deleting_data() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("unrelated.txt"), b"keep").unwrap();
    assert!(
        Checkpoints::<TestModel, _, _>::open(
            binding(),
            TestCodec::new(),
            policy(),
            FileCheckpointBackend::open(&directory.0).unwrap()
        )
        .is_err()
    );
    assert_eq!(
        fs::read(directory.0.join("unrelated.txt")).unwrap(),
        b"keep"
    );
    fs::remove_file(directory.0.join("unrelated.txt")).unwrap();
    let mut store = directory.open();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    drop(store);
    fs::write(directory.snapshot(1), vec![0; 4096]).unwrap();
    let error = Checkpoints::<TestModel, _, _>::open(
        binding(),
        TestCodec::new(),
        policy(),
        FileCheckpointBackend::open(&directory.0).unwrap(),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, FaultCode::ResourceLimit);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    None,
    Create,
    SnapshotSync,
    ReadBack,
    Append(usize),
    ManifestSync,
    Remove,
}

/// Write failures may leave any prefix. Sync failure deliberately preserves all written
/// bytes, exercising the valid-new-generation outcome after an ambiguous publication.
struct Failing {
    inner: MemoryCheckpointBackend,
    failure: Failure,
}
fn injected() -> Fault {
    Fault::new(FaultCode::StorageUnavailable, "injected checkpoint failure")
}
impl CheckpointBackend for Failing {
    fn read_manifest(&mut self, limit: u64) -> Result<Vec<u8>, Fault> {
        self.inner.read_manifest(limit)
    }
    fn truncate_manifest(&mut self, length: u64) -> Result<(), Fault> {
        self.inner.truncate_manifest(length)
    }
    fn append_manifest(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        if let Failure::Append(cut) = self.failure {
            self.inner.append_manifest(&bytes[..cut.min(bytes.len())])?;
            return Err(injected());
        }
        self.inner.append_manifest(bytes)
    }
    fn sync_manifest(&mut self) -> Result<(), Fault> {
        if self.failure == Failure::ManifestSync && self.inner.read_manifest(4096)?.len() >= 504 {
            return Err(injected());
        }
        self.inner.sync_manifest()
    }
    fn create_snapshot(&mut self, generation: u64, bytes: &[u8]) -> Result<(), Fault> {
        if self.failure == Failure::Create {
            self.inner
                .create_snapshot(generation, &bytes[..bytes.len() / 2])?;
            return Err(injected());
        }
        self.inner.create_snapshot(generation, bytes)
    }
    fn sync_snapshot(&mut self, generation: u64) -> Result<(), Fault> {
        if self.failure == Failure::SnapshotSync {
            return Err(injected());
        }
        self.inner.sync_snapshot(generation)
    }
    fn read_snapshot(&mut self, generation: u64, limit: u64) -> Result<Vec<u8>, Fault> {
        if self.failure == Failure::ReadBack && generation > 2 {
            return Err(injected());
        }
        self.inner.read_snapshot(generation, limit)
    }
    fn snapshot_generations(&mut self, limit: u32) -> Result<Vec<u64>, Fault> {
        self.inner.snapshot_generations(limit)
    }
    fn remove_snapshot(&mut self, generation: u64) -> Result<(), Fault> {
        if self.failure == Failure::Remove {
            return Err(injected());
        }
        self.inner.remove_snapshot(generation)
    }
}

fn seeded(failure: Failure) -> Checkpoints<TestModel, TestCodec, Failing> {
    let mut initial = memory();
    initial
        .publish_checkpoint(&snapshot(0), durable(0))
        .unwrap();
    let mut store = Checkpoints::open(
        binding(),
        TestCodec::new(),
        policy(),
        Failing {
            inner: initial.into_backend(),
            failure: Failure::None,
        },
    )
    .unwrap();
    // The backend is owned by the store. Failure conditions apply to generation three,
    // allowing the reopening barrier to validate the already-published generation two.
    store.publish_checkpoint(&snapshot(1), durable(1)).unwrap();
    let mut backend = store.into_backend();
    backend.failure = failure;
    Checkpoints::open(binding(), TestCodec::new(), policy(), backend).unwrap()
}

#[test]
fn partial_manifest_appends_stop_instance_and_recover_only_complete_references() {
    let width = 168;
    for cut in 0..=width {
        let mut store = seeded(Failure::Append(cut));
        assert!(store.publish_checkpoint(&snapshot(3), durable(3)).is_err());
        assert!(store.is_stopped());
        assert_eq!(store.current().unwrap().snapshot.cursor, cursor(1));
        assert!(store.publish_checkpoint(&snapshot(3), durable(3)).is_err());
        assert!(store.prune(1).is_err());
        let mut backend = store.into_backend();
        backend.failure = Failure::None;
        assert_eq!(backend.snapshot_generations(8).unwrap(), vec![1, 2, 3]);
        let mut reopened =
            Checkpoints::<TestModel, _, _>::open(binding(), TestCodec::new(), policy(), backend)
                .unwrap();
        let expected = if cut == width { 3 } else { 1 };
        assert_eq!(
            reopened.load_current().unwrap().unwrap().meta.cursor,
            cursor(expected)
        );
    }
}

#[test]
fn snapshot_failures_keep_previous_checkpoint_and_orphans_for_explicit_cleanup() {
    for failure in [Failure::Create, Failure::SnapshotSync, Failure::ReadBack] {
        let mut store = seeded(failure);
        assert!(
            store.publish_checkpoint(&snapshot(3), durable(3)).is_err(),
            "{failure:?}"
        );
        assert!(store.is_stopped());
        let mut backend = store.into_backend();
        backend.failure = Failure::None;
        let mut reopened =
            Checkpoints::<TestModel, _, _>::open(binding(), TestCodec::new(), policy(), backend)
                .unwrap();
        assert_eq!(reopened.current().unwrap().snapshot.cursor, cursor(1));
        assert_eq!(reopened.prune(1).unwrap().removed_generations, vec![1, 3]);
        reopened
            .publish_checkpoint(&snapshot(4), durable(4))
            .unwrap();
        assert_eq!(reopened.current().unwrap().generation, 4);
    }
}

#[test]
fn cleanup_failure_stops_instance_without_losing_current_checkpoint() {
    let mut store = seeded(Failure::Remove);
    assert!(store.prune(1).is_err());
    assert!(store.is_stopped());
    let mut backend = store.into_backend();
    backend.failure = Failure::None;
    let mut reopened =
        Checkpoints::<TestModel, _, _>::open(binding(), TestCodec::new(), policy(), backend)
            .unwrap();
    assert_eq!(
        reopened.load_current().unwrap().unwrap().meta.cursor,
        cursor(1)
    );
}

#[test]
fn manifest_sync_failure_never_reports_success_but_can_recover_new_generation() {
    let mut store = seeded(Failure::ManifestSync);
    assert!(store.publish_checkpoint(&snapshot(3), durable(3)).is_err());
    assert!(store.is_stopped());
    assert_eq!(store.current().unwrap().snapshot.cursor, cursor(1));
    let mut backend = store.into_backend();
    backend.failure = Failure::None;
    assert_eq!(backend.snapshot_generations(8).unwrap(), vec![1, 2, 3]);
    let mut reopened =
        Checkpoints::<TestModel, _, _>::open(binding(), TestCodec::new(), policy(), backend)
            .unwrap();
    assert_eq!(
        reopened.load_current().unwrap().unwrap().meta.cursor,
        cursor(3)
    );
}

/// Recompute both checksums so semantic failures cannot be attributed to a bad envelope.
fn rewrite_payload(bytes: &mut [u8], field: std::ops::Range<usize>, value: &[u8]) {
    bytes[record::HEADER_BYTES + field.start..record::HEADER_BYTES + field.end]
        .copy_from_slice(value);
    let end = bytes.len() - record::TRAILER_BYTES;
    let crc = crc32c::crc32c(&bytes[record::HEADER_BYTES..end]);
    bytes[end..end + 4].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn checksum_valid_manifest_semantic_errors_are_fatal() {
    let directory = TestDirectory::new();
    let mut store = directory.open();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    store.publish_checkpoint(&snapshot(3), durable(3)).unwrap();
    drop(store);
    let path = directory.0.join("manifest.log");
    let original = fs::read(&path).unwrap();
    let width = original.len() / 2;
    // Layout: version[0..2], generation[2..10], SnapshotMeta[10..120],
    // durable[120..128], snapshot bytes[128..136], snapshot CRC[136..140].
    let cases: Vec<(std::ops::Range<usize>, Vec<u8>)> = vec![
        (0..2, 2u16.to_le_bytes().to_vec()),
        (2..10, 1u64.to_le_bytes().to_vec()),
        (10..26, 99u128.to_le_bytes().to_vec()),
        (34..38, 99u32.to_le_bytes().to_vec()),
        (120..128, 2u64.to_le_bytes().to_vec()),
        (128..136, 0u64.to_le_bytes().to_vec()),
        (136..140, 0u32.to_le_bytes().to_vec()),
    ];
    for (field, value) in cases {
        let mut bytes = original.clone();
        rewrite_payload(&mut bytes[width..], field, &value);
        fs::write(&path, &bytes).unwrap();
        assert!(
            Checkpoints::<TestModel, _, _>::open(
                binding(),
                TestCodec::new(),
                policy(),
                FileCheckpointBackend::open(&directory.0).unwrap()
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn manifest_has_fixed_little_endian_layout_and_binds_exact_snapshot_bytes() {
    let mut store = memory();
    store.publish_checkpoint(&snapshot(0), durable(7)).unwrap();
    let mut backend = store.into_backend();
    let bytes = backend.read_manifest(4096).unwrap();
    let snapshot_bytes = backend.read_snapshot(1, 4096).unwrap();
    assert_eq!(bytes.len(), 168);
    let record::RecordRead::Complete {
        kind,
        payload,
        encoded_bytes,
    } = record::read_record(&bytes, LIMITS).unwrap()
    else {
        panic!("complete manifest expected")
    };
    assert_eq!(kind, record::RecordKind::Manifest);
    assert_eq!(encoded_bytes, bytes.len());
    assert_eq!(&payload[..10], &[1, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&payload[10..26], &7u128.to_le_bytes());
    assert_eq!(&payload[26..34], &0u64.to_le_bytes());
    assert_eq!(&payload[34..38], &1u32.to_le_bytes());
    assert_eq!(&payload[38..70], &[2; 32]);
    assert_eq!(&payload[70..102], &[3; 32]);
    assert_eq!(&payload[102..120], &[0; 18]);
    assert_eq!(&payload[120..128], &7u64.to_le_bytes());
    assert_eq!(
        &payload[128..136],
        &(snapshot_bytes.len() as u64).to_le_bytes()
    );
    assert_eq!(
        &payload[136..140],
        &crc32c::crc32c(&snapshot_bytes).to_le_bytes()
    );
}

#[test]
fn valid_checksums_do_not_bypass_snapshot_kind_metadata_or_decode_validation() {
    let directory = TestDirectory::new();
    let mut store = directory.open();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    drop(store);
    let path = directory.0.join("manifest.log");
    let manifest = fs::read(&path).unwrap();
    let original = fs::read(directory.snapshot(1)).unwrap();
    for case in 0..4 {
        let mut bytes = original.clone();
        match case {
            0 => {
                bytes[10..12].copy_from_slice(&(record::RecordKind::Commit as u16).to_le_bytes());
                let crc = crc32c::crc32c(&bytes[..16]);
                bytes[16..20].copy_from_slice(&crc.to_le_bytes());
            }
            1 => bytes.push(0),
            2 => rewrite_payload(&mut bytes, 16..24, &1u64.to_le_bytes()),
            3 => rewrite_payload(&mut bytes, 110..114, &1024u32.to_le_bytes()),
            _ => unreachable!(),
        }
        let mut reference = manifest.clone();
        rewrite_payload(
            &mut reference,
            128..136,
            &(bytes.len() as u64).to_le_bytes(),
        );
        rewrite_payload(
            &mut reference,
            136..140,
            &crc32c::crc32c(&bytes).to_le_bytes(),
        );
        fs::write(directory.snapshot(1), bytes).unwrap();
        fs::write(&path, &reference).unwrap();
        assert!(
            Checkpoints::<TestModel, _, _>::open(
                binding(),
                TestCodec::new(),
                policy(),
                FileCheckpointBackend::open(&directory.0).unwrap()
            )
            .is_err(),
            "case {case}"
        );
        assert_eq!(fs::read(&path).unwrap(), reference);
    }
}

#[test]
fn incompatible_reopen_and_decode_budget_failure_preserve_files() {
    let directory = TestDirectory::new();
    let mut store = directory.open();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    drop(store);
    let path = directory.0.join("manifest.log");
    let original = fs::read(&path).unwrap();
    for case in 0..3 {
        let mut changed_binding = binding();
        let mut config = policy();
        match case {
            0 => changed_binding.world = WorldId(999),
            1 => changed_binding.compatibility.authority_schema += 1,
            2 => config.codec_budget.max_items = 0,
            _ => unreachable!(),
        }
        assert!(
            Checkpoints::<TestModel, _, _>::open(
                changed_binding,
                TestCodec::new(),
                config,
                FileCheckpointBackend::open(&directory.0).unwrap()
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(directory.snapshot(1).exists());
    }
}

#[test]
fn invalid_policy_and_memory_capacity_are_explicit_failures() {
    for config in [
        CheckpointConfig {
            max_manifest_bytes: 167,
            ..policy()
        },
        CheckpointConfig {
            max_snapshot_files: 1,
            ..policy()
        },
    ] {
        assert!(
            Checkpoints::<TestModel, _, _>::open(
                binding(),
                TestCodec::new(),
                config,
                MemoryCheckpointBackend::new(16384)
            )
            .is_err()
        );
    }
    let mut store = Checkpoints::<TestModel, _, _>::open(
        binding(),
        TestCodec::new(),
        policy(),
        MemoryCheckpointBackend::new(1),
    )
    .unwrap();
    assert!(store.publish_checkpoint(&snapshot(0), durable(0)).is_err());
    assert!(store.is_stopped());
    assert!(store.current().is_none());
}

#[test]
fn checkpoint_and_contiguous_journal_suffix_restore_the_expected_position() {
    use std::io::Cursor;
    use torse_storage::{CommitJournal, MemoryBackend, MemoryJournal, scan_journal};
    let mut store = memory();
    store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
    let mut journal = MemoryJournal::with_backend(
        binding(),
        cursor(0),
        TestCodec::new(),
        config(),
        MemoryBackend::new(16384),
    )
    .unwrap();
    for sequence in 1..=5 {
        let staged = journal.try_stage(&commit(sequence)).unwrap();
        journal.accept(staged).unwrap();
        journal.flush_through(cursor(sequence)).unwrap();
    }
    let barrier = journal.flush_through(cursor(5)).unwrap();
    store.publish_checkpoint(&snapshot(3), barrier).unwrap();
    let expected = store.current().unwrap().durable_through;
    let snapshot = store.load_current().unwrap().unwrap();
    // B2 retains the whole log. The application selects a suffix starting immediately
    // after the checkpoint; a segmented journal can later make this selection cheaper.
    let prefix_bytes: usize = (1..=3)
        .map(|sequence| encoded(&commit(sequence)).len())
        .sum();
    let bytes = &journal.backend().bytes()[prefix_bytes..];
    let mut isolated_state = snapshot.state;
    let report = scan_journal::<TestModel, _, _, _>(
        &mut Cursor::new(bytes),
        binding(),
        snapshot.meta.cursor,
        &TestCodec::new(),
        LIMITS,
        config().codec_budget,
        |value| {
            isolated_state = value.changes;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(report.through.seq.0, expected.0);
    assert_eq!(isolated_state, commit(5).changes);
    let missing = scan_journal::<TestModel, _, _, _>(
        &mut Cursor::new(encoded(&commit(4))),
        binding(),
        snapshot.meta.cursor,
        &TestCodec::new(),
        LIMITS,
        config().codec_budget,
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        missing.through.seq.0 < expected.0,
        "the host must reject a missing acknowledged suffix"
    );
}

/// A real process exit skips Rust drops and leaves the manifest lock to the operating system.
/// Each hook exits only after the corresponding file operation has completed successfully.
struct CrashBackend {
    inner: FileCheckpointBackend,
    phase: String,
    publishing: bool,
}
impl CrashBackend {
    fn exit_at(&self, phase: &str) {
        if self.publishing && self.phase == phase {
            std::process::exit(86);
        }
    }
}
impl CheckpointBackend for CrashBackend {
    fn read_manifest(&mut self, limit: u64) -> Result<Vec<u8>, Fault> {
        self.inner.read_manifest(limit)
    }
    fn truncate_manifest(&mut self, length: u64) -> Result<(), Fault> {
        self.inner.truncate_manifest(length)
    }
    fn append_manifest(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        self.inner.append_manifest(bytes)?;
        self.exit_at("append");
        Ok(())
    }
    fn sync_manifest(&mut self) -> Result<(), Fault> {
        self.inner.sync_manifest()?;
        self.exit_at("manifest-sync");
        Ok(())
    }
    fn create_snapshot(&mut self, generation: u64, bytes: &[u8]) -> Result<(), Fault> {
        self.publishing = true;
        self.inner.create_snapshot(generation, bytes)?;
        self.exit_at("create");
        Ok(())
    }
    fn sync_snapshot(&mut self, generation: u64) -> Result<(), Fault> {
        self.inner.sync_snapshot(generation)?;
        self.exit_at("snapshot-sync");
        Ok(())
    }
    fn read_snapshot(&mut self, generation: u64, limit: u64) -> Result<Vec<u8>, Fault> {
        let result = self.inner.read_snapshot(generation, limit)?;
        self.exit_at("read-back");
        Ok(result)
    }
    fn snapshot_generations(&mut self, limit: u32) -> Result<Vec<u64>, Fault> {
        self.inner.snapshot_generations(limit)
    }
    fn remove_snapshot(&mut self, generation: u64) -> Result<(), Fault> {
        self.inner.remove_snapshot(generation)
    }
}

#[test]
fn checkpoint_crash_worker() {
    let Ok(root) = std::env::var("TORSE_CHECKPOINT_CRASH_ROOT") else {
        return;
    };
    let backend = CrashBackend {
        inner: FileCheckpointBackend::open(root).unwrap(),
        phase: std::env::var("TORSE_CHECKPOINT_CRASH_PHASE").unwrap(),
        publishing: false,
    };
    let mut store =
        Checkpoints::<TestModel, _, _>::open(binding(), TestCodec::new(), policy(), backend)
            .unwrap();
    store.publish_checkpoint(&snapshot(3), durable(3)).unwrap();
    panic!("crash hook was not reached");
}

#[test]
fn abrupt_process_exits_recover_at_each_publication_boundary() {
    for phase in [
        "create",
        "snapshot-sync",
        "read-back",
        "append",
        "manifest-sync",
    ] {
        let directory = TestDirectory::new();
        let mut store = directory.open();
        store.publish_checkpoint(&snapshot(0), durable(0)).unwrap();
        drop(store);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "checkpoint_crash_worker", "--nocapture"])
            .env("TORSE_CHECKPOINT_CRASH_ROOT", &directory.0)
            .env("TORSE_CHECKPOINT_CRASH_PHASE", phase)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(86), "{phase}: {output:?}");
        assert!(directory.snapshot(1).exists());
        let mut reopened = directory.open();
        let expected = if ["append", "manifest-sync"].contains(&phase) {
            3
        } else {
            0
        };
        assert_eq!(
            reopened.load_current().unwrap().unwrap().meta.cursor,
            cursor(expected),
            "{phase}"
        );
    }
}
