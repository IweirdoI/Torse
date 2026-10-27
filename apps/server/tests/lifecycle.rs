//! Real files exercise initial publication, isolated recovery, and clean/failed shutdown.

mod support;

use std::fs::{self, OpenOptions};
use std::io::Write;

use support::*;
use torse_server::*;
use torse_types::*;

fn commit(server: &mut TestFileServer, sequence: u64, target: u64) {
    let binding = server.authority().unwrap().binding();
    assert!(matches!(
        server.submit(request(binding, sequence, target, true)),
        Admission::Queued { .. }
    ));
    assert!(matches!(
        server.drive_one(&mut NoReplicas).unwrap(),
        CommitStep::Durable(_)
    ));
}
fn add_tail(path: &std::path::Path) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"TOR")
        .unwrap();
}

#[test]
fn real_file_world_recovers_checkpoint_and_log_tail_then_continues_in_a_new_epoch() {
    let directory = Directory::new();
    let mut server = create_file(&directory, Control::new());
    let old_binding = server.authority().unwrap().binding();
    commit(&mut server, 1, 0);
    server.checkpoint().unwrap();
    commit(&mut server, 2, 1);
    drop(server);
    let control = Control::new();
    let mut server = open_file(&directory, control.clone()).unwrap();
    assert_eq!(&*control.trace.borrow(), &["restore", "replay", "finish"]);
    let authority = server.authority().unwrap();
    assert_eq!(authority.value, 2);
    assert_eq!(authority.cursor().seq, CommitSeq(2));
    assert_ne!(authority.binding().epoch, old_binding.epoch);
    assert_eq!(
        server.submit(request(old_binding, 3, 2, true)),
        Admission::Rejected(RejectCode::StaleSession)
    );
    commit(&mut server, 3, 2);
    let report = server.shutdown(Tick(3), &mut NoReplicas).unwrap();
    assert_eq!(report.through.seq, CommitSeq(3));
    let server = open_file(&directory, Control::new()).unwrap();
    assert_eq!(server.authority().unwrap().value, 3);
}

#[test]
fn compatibility_preflight_preserves_every_file_and_retains_the_directory_lease() {
    fn all_bytes(
        root: &std::path::Path,
    ) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut result = std::collections::BTreeMap::new();
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                result.extend(all_bytes(&entry.path()));
            } else {
                result.insert(entry.path(), fs::read(entry.path()).unwrap());
            }
        }
        result
    }
    let directory = Directory::new();
    drop(create_file(&directory, Control::new()));
    add_tail(&directory.0.join("checkpoints/manifest.log"));
    add_tail(&directory.0.join("journal.log"));
    let before = all_bytes(&directory.0);
    for incompatible in [
        Compatibility {
            authority_schema: 2,
            ..compatibility()
        },
        Compatibility {
            rules_digest: Digest([8; 32]),
            ..compatibility()
        },
        Compatibility {
            content_digest: Digest([9; 32]),
            ..compatibility()
        },
    ] {
        let lease = WorldLease::inspect(&directory.0, WORLD, 8192).unwrap();
        assert_eq!(
            TestFileServer::preflight_file(
                &lease,
                incompatible,
                Codec(Control::new()),
                file_config()
            )
            .err()
            .unwrap()
            .code,
            FaultCode::Incompatible
        );
        assert!(WorldDirectory::acquire(&directory.0, WORLD, 8192).is_err());
        drop(lease);
        assert_eq!(all_bytes(&directory.0), before);
    }
    let lease = WorldLease::inspect(&directory.0, WORLD, 8192).unwrap();
    let metadata = TestFileServer::preflight_file(
        &lease,
        compatibility(),
        Codec(Control::new()),
        file_config(),
    )
    .unwrap();
    assert_eq!(metadata.compatibility, compatibility());
    assert_eq!(metadata.cursor, CommitCursor::initial(WORLD));
    drop(lease);
    assert_eq!(all_bytes(&directory.0), before);
    let directory_guard = WorldLease::inspect(&directory.0, WORLD, 8192)
        .unwrap()
        .start()
        .unwrap();
    assert_eq!(directory_guard.epoch(), WorldEpoch(2));
    assert!(WorldLease::inspect(&directory.0, WORLD, 8192).is_err());
    drop(directory_guard);
}

#[test]
fn legacy_preflight_and_rejected_recovery_do_not_create_a_namespace_lock() {
    let directory = Directory::new();
    drop(create_file(&directory, Control::new()));
    let lock = directory.0.join("checkpoints/manifest.lock");
    fs::remove_file(&lock).unwrap();
    let lease = WorldLease::inspect(&directory.0, WORLD, 8192).unwrap();
    TestFileServer::preflight_file(
        &lease,
        compatibility(),
        Codec(Control::new()),
        file_config(),
    )
    .unwrap();
    drop(lease);
    assert!(!lock.exists());
    let control = Control::new();
    control.failure.set(Failure::Restore);
    assert!(open_file(&directory, control).is_err());
    assert!(!lock.exists());
    drop(open_file(&directory, Control::new()).unwrap());
    assert!(lock.exists());
}

#[test]
fn semantic_recovery_failure_leaves_both_torn_tails_unchanged_and_releases_lock() {
    let directory = Directory::new();
    let mut server = create_file(&directory, Control::new());
    commit(&mut server, 1, 0);
    drop(server);
    let manifest = directory.0.join("checkpoints/manifest.log");
    let journal = directory.0.join("journal.log");
    add_tail(&manifest);
    add_tail(&journal);
    let before_manifest = fs::read(&manifest).unwrap();
    let before_journal = fs::read(&journal).unwrap();
    for failure in [
        Failure::Restore,
        Failure::Replay,
        Failure::Finish,
        Failure::WrongFinish,
    ] {
        let control = Control::new();
        control.failure.set(failure);
        assert!(open_file(&directory, control).is_err(), "{failure:?}");
        assert_eq!(fs::read(&manifest).unwrap(), before_manifest);
        assert_eq!(fs::read(&journal).unwrap(), before_journal);
    }
    let server = open_file(&directory, Control::new()).unwrap();
    assert_eq!(server.authority().unwrap().value, 1);
    assert_eq!(
        fs::metadata(manifest).unwrap().len(),
        before_manifest.len() as u64 - 3
    );
    assert_eq!(
        fs::metadata(journal).unwrap().len(),
        before_journal.len() as u64 - 3
    );
}

#[test]
fn missing_whole_record_suffix_is_detected_from_checkpoint_expectation() {
    let directory = Directory::new();
    let mut server = create_file(&directory, Control::new());
    commit(&mut server, 1, 0);
    server.checkpoint().unwrap();
    drop(server);
    let path = directory.0.join("journal.log");
    fs::write(&path, b"").unwrap();
    let error = open_file(&directory, Control::new()).err().unwrap();
    assert_eq!(error.code, FaultCode::SequenceGap);
    assert_eq!(fs::read(path).unwrap(), b"");
}

#[test]
fn complete_journal_corruption_stops_startup_without_repairing_manifest_tail() {
    let directory = Directory::new();
    let mut server = create_file(&directory, Control::new());
    commit(&mut server, 1, 0);
    drop(server);
    let path = directory.0.join("journal.log");
    let mut bytes = fs::read(&path).unwrap();
    bytes[25] ^= 1;
    fs::write(&path, &bytes).unwrap();
    let manifest = directory.0.join("checkpoints/manifest.log");
    add_tail(&manifest);
    let manifest_before = fs::read(&manifest).unwrap();
    assert!(open_file(&directory, Control::new()).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(fs::read(manifest).unwrap(), manifest_before);
}

#[test]
fn missing_initial_checkpoint_does_not_reinitialize_a_world() {
    let directory = Directory::new();
    let server = create_file(&directory, Control::new());
    drop(server);
    let manifest = directory.0.join("checkpoints/manifest.log");
    fs::write(&manifest, b"").unwrap();
    assert!(open_file(&directory, Control::new()).is_err());
    assert_eq!(fs::read(manifest).unwrap(), b"");
}

#[test]
fn missing_manifest_or_journal_is_never_created_by_recovery() {
    for remove_manifest in [false, true] {
        let directory = Directory::new();
        let server = create_file(&directory, Control::new());
        drop(server);
        let path = directory.0.join(if remove_manifest {
            "checkpoints/manifest.log"
        } else {
            "journal.log"
        });
        fs::remove_file(&path).unwrap();
        assert!(open_file(&directory, Control::new()).is_err());
        assert!(!path.exists());
    }
}

#[test]
fn create_refuses_existing_data_without_overwrite() {
    let directory = Directory::new();
    let server = create_file(&directory, Control::new());
    drop(server);
    let before = fs::read(directory.0.join("checkpoints/manifest.log")).unwrap();
    let result = TestFileServer::create_file(
        directory.acquire(),
        compatibility(),
        Codec(Control::new()),
        file_config(),
        |binding| Ok(Authority::new(binding, Control::new())),
    );
    assert!(result.is_err());
    assert_eq!(
        fs::read(directory.0.join("checkpoints/manifest.log")).unwrap(),
        before
    );
}

#[test]
fn real_shutdown_settles_cutoff_and_releases_all_file_handles_before_unlocking() {
    let directory = Directory::new();
    let control = Control::new();
    let mut server = create_file(&directory, control.clone());
    let binding = server.authority().unwrap().binding();
    assert!(matches!(
        server.submit(request(binding, 1, 0, false)),
        Admission::Queued { .. }
    ));
    assert!(matches!(
        server.submit(request(binding, 2, 999, true)),
        Admission::Queued { .. }
    ));
    let report = server.shutdown(Tick(0), &mut NoReplicas).unwrap();
    assert_eq!(report.committed_during_shutdown, 2);
    assert_eq!(control.canceled.get(), 1);
    let server = open_file(&directory, Control::new()).unwrap();
    assert_eq!(server.authority().unwrap().value, 1);
    assert_eq!(server.authority().unwrap().resume_point(), resume(1, 0));
    assert_eq!(server.durability().through, DurableSeq(2));
}

#[test]
fn incompatible_content_refuses_startup_before_authority_construction() {
    let directory = Directory::new();
    let server = create_file(&directory, Control::new());
    drop(server);
    let mut incompatible = compatibility();
    incompatible.rules_digest = Digest([99; 32]);
    let control = Control::new();
    let result = TestFileServer::open_file(
        directory.acquire(),
        incompatible,
        Codec(control.clone()),
        file_config(),
        |binding| Ok(Recovery::new(binding, control.clone())),
    );
    assert!(result.is_err());
    assert!(control.trace.borrow().is_empty());
}

#[test]
fn invalid_policy_is_rejected_before_restoring_or_repairing_data() {
    let directory = Directory::new();
    let server = create_file(&directory, Control::new());
    drop(server);
    let manifest = directory.0.join("checkpoints/manifest.log");
    add_tail(&manifest);
    let original = fs::read(&manifest).unwrap();
    let mut config = file_config();
    config.journal.max_pending_records = 0;
    let control = Control::new();
    assert!(
        TestFileServer::open_file(
            directory.acquire(),
            compatibility(),
            Codec(control.clone()),
            config,
            |binding| Ok(Recovery::new(binding, control.clone()))
        )
        .is_err()
    );
    assert!(control.trace.borrow().is_empty());
    assert_eq!(fs::read(manifest).unwrap(), original);
}

#[test]
fn publication_crash_worker() {
    let Ok(root) = std::env::var("TORSE_PUBLICATION_TEST_ROOT") else {
        return;
    };
    let control = Control::new();
    let directory = WorldDirectory::acquire(root, WORLD, 8192).unwrap();
    let mut server = TestFileServer::open_file(
        directory,
        compatibility(),
        Codec(control.clone()),
        file_config(),
        |binding| Ok(Recovery::new(binding, control.clone())),
    )
    .unwrap();
    control.failure.set(Failure::ExitAfterPublish);
    commit(&mut server, 1, 0);
    panic!("publication crash hook was not reached");
}

#[test]
fn process_exit_after_publication_before_acceptance_recovers_only_the_durable_prefix() {
    let directory = Directory::new();
    let server = create_file(&directory, Control::new());
    drop(server);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "publication_crash_worker"])
        .env("TORSE_PUBLICATION_TEST_ROOT", &directory.0)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(86), "{output:?}");
    let server = open_file(&directory, Control::new()).unwrap();
    assert_eq!(server.authority().unwrap().value, 0);
    assert_eq!(server.authority().unwrap().cursor().seq, CommitSeq(0));
    assert_eq!(server.binding().epoch, WorldEpoch(3));
}

#[test]
fn repeated_checkpoints_reclaim_capacity_and_remain_recoverable_across_shutdown() {
    for predecessors in [1, 2] {
        let directory = Directory::new();
        let control = Control::new();
        let mut config = file_config();
        config.server.checkpoint_predecessors = predecessors;
        config.checkpoint.max_snapshot_files = predecessors + 1;
        let mut server = TestFileServer::create_file(
            directory.acquire(),
            compatibility(),
            Codec(control.clone()),
            config,
            |binding| Ok(Authority::new(binding, control)),
        )
        .unwrap();
        for sequence in 1..=6 {
            commit(&mut server, sequence, sequence - 1);
            server.checkpoint().unwrap();
            let files = fs::read_dir(directory.0.join("checkpoints"))
                .unwrap()
                .map(|entry| entry.unwrap())
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("checkpoint-")
                })
                .count();
            assert!(files <= (predecessors + 1) as usize);
        }
        assert!(
            !directory
                .0
                .join("checkpoints/checkpoint-0000000000000001.bin")
                .exists()
        );
        drop(server);
        let control = Control::new();
        let server = TestFileServer::open_file(
            directory.acquire(),
            compatibility(),
            Codec(control.clone()),
            config,
            |binding| Ok(Recovery::new(binding, control)),
        )
        .unwrap();
        assert_eq!(server.authority().unwrap().value, 6);
        server.shutdown(Tick(6), &mut NoReplicas).unwrap();
        let server = open_file(&directory, Control::new()).unwrap();
        assert_eq!(server.authority().unwrap().value, 6);
    }
}

#[test]
fn invalid_retention_policy_is_rejected_before_world_initialization() {
    for predecessors in [0, 2] {
        let directory = Directory::new();
        let control = Control::new();
        let mut config = file_config();
        config.server.checkpoint_predecessors = predecessors;
        config.checkpoint.max_snapshot_files = 2;
        let result = TestFileServer::create_file(
            directory.acquire(),
            compatibility(),
            Codec(control.clone()),
            config,
            |binding| {
                control.event("initialize");
                Ok(Authority::new(binding, control.clone()))
            },
        );
        assert!(result.is_err());
        assert!(control.trace.borrow().is_empty());
        assert!(!directory.0.join("checkpoints").exists());
        assert!(!directory.0.join("journal.log").exists());
    }
}

#[test]
fn manifest_capacity_failure_does_not_poison_a_healthy_running_server() {
    let directory = Directory::new();
    let control = Control::new();
    let mut config = file_config();
    config.checkpoint.max_manifest_bytes = 168;
    let mut server = TestFileServer::create_file(
        directory.acquire(),
        compatibility(),
        Codec(control.clone()),
        config,
        |binding| Ok(Authority::new(binding, control)),
    )
    .unwrap();
    assert_eq!(
        server.checkpoint().unwrap_err().code,
        FaultCode::ResourceLimit
    );
    assert_eq!(server.state(), ServerState::Running);
    assert!(server.authority().is_ok());
    commit(&mut server, 1, 0);
    drop(server);
    let server = open_file(&directory, Control::new()).unwrap();
    assert_eq!(server.authority().unwrap().value, 1);
}

#[test]
fn wrong_recovered_tick_phase_or_step_is_rejected_before_tail_repair() {
    for snapshot_only in [false, true] {
        let directory = Directory::new();
        let mut server = create_file(&directory, Control::new());
        commit(&mut server, 1, 8);
        if snapshot_only {
            server.checkpoint().unwrap();
        }
        drop(server);
        let manifest = directory.0.join("checkpoints/manifest.log");
        let journal = directory.0.join("journal.log");
        add_tail(&manifest);
        add_tail(&journal);
        let manifest_before = fs::read(&manifest).unwrap();
        let journal_before = fs::read(&journal).unwrap();
        for failure in [
            Failure::WrongResumeTick,
            Failure::WrongResumePhase,
            Failure::WrongResumeStep,
        ] {
            let control = Control::new();
            control.failure.set(failure);
            let error = open_file(&directory, control.clone())
                .err()
                .expect("invalid resume point must fail");
            assert_eq!(error.code, FaultCode::InvalidState);
            assert!(!control.trace.borrow().contains(&"submit"));
            assert_eq!(fs::read(&manifest).unwrap(), manifest_before);
            assert_eq!(fs::read(&journal).unwrap(), journal_before);
        }
        let server = open_file(&directory, Control::new()).unwrap();
        assert_eq!(server.authority().unwrap().resume_point(), resume(9, 0));
        assert_eq!(server.authority().unwrap().cursor().seq, CommitSeq(1));
    }
}
