//! Directory leases and run identity allocation survive failed opens and process exits.

mod support;

use std::fs;

use support::*;
use torse_server::{WorldDirectory, WorldLease};
use torse_types::*;

#[test]
fn aliases_share_the_same_exclusive_lock_and_epochs_increase() {
    let directory = Directory::new();
    let lease = directory.acquire();
    assert_eq!(lease.epoch(), WorldEpoch(1));
    assert!(WorldDirectory::acquire(directory.0.join("."), WORLD, 8192).is_err());
    drop(lease);
    assert!(directory.0.join("world.lock").exists());
    let lease = directory.acquire();
    assert_eq!(lease.epoch(), WorldEpoch(2));
}

#[test]
fn different_world_or_full_ledger_preserves_all_existing_bytes() {
    let directory = Directory::new();
    let lease = WorldDirectory::acquire(&directory.0, WORLD, 48).unwrap();
    drop(lease);
    let path = directory.0.join("world.lock");
    let bytes = fs::read(&path).unwrap();
    assert!(WorldDirectory::acquire(&directory.0, WorldId(99), 8192).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        WorldDirectory::acquire(&directory.0, WORLD, 48)
            .err()
            .unwrap()
            .code,
        FaultCode::ResourceLimit
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(WorldDirectory::acquire(&directory.0, WORLD, 47).is_err());
}

#[test]
fn every_partial_unexposed_epoch_entry_can_be_repaired() {
    let directory = Directory::new();
    drop(directory.acquire());
    let path = directory.0.join("world.lock");
    let first = fs::read(&path).unwrap();
    let mut unexposed = first.clone();
    unexposed[24..40].copy_from_slice(&2u128.to_le_bytes());
    let crc = crc32c::crc32c(&unexposed[..40]);
    unexposed[40..44].copy_from_slice(&crc.to_le_bytes());
    for end in 0..48 {
        fs::write(&path, [&first[..], &unexposed[..end]].concat()).unwrap();
        let lease = directory.acquire();
        assert_eq!(lease.epoch(), WorldEpoch(2), "unexposed prefix {end}");
        drop(lease);
        assert_eq!(
            fs::read(&path).unwrap(),
            [&first[..], &unexposed[..]].concat()
        );
    }
}

#[test]
fn every_complete_epoch_record_bit_flip_is_fatal_and_not_repaired() {
    let directory = Directory::new();
    drop(directory.acquire());
    let path = directory.0.join("world.lock");
    let original = fs::read(&path).unwrap();
    for index in 0..original.len() {
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[index] ^= 1 << bit;
            fs::write(&path, &bytes).unwrap();
            assert!(WorldDirectory::acquire(&directory.0, WORLD, 8192).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
}

#[test]
fn checksum_valid_epoch_regression_is_fatal() {
    let directory = Directory::new();
    drop(directory.acquire());
    let path = directory.0.join("world.lock");
    let first = fs::read(&path).unwrap();
    fs::write(&path, [&first[..], &first[..]].concat()).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(WorldDirectory::acquire(&directory.0, WORLD, 8192).is_err());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn directory_lock_process_worker() {
    let Ok(root) = std::env::var("TORSE_DIRECTORY_TEST_ROOT") else {
        return;
    };
    match WorldDirectory::acquire(root, WORLD, 8192) {
        Ok(_lease) => std::process::exit(86),
        Err(error) if error.code == FaultCode::StorageUnavailable => std::process::exit(87),
        Err(_) => std::process::exit(88),
    }
}

fn child(directory: &Directory) -> i32 {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "directory_lock_process_worker"])
        .env("TORSE_DIRECTORY_TEST_ROOT", &directory.0)
        .output()
        .unwrap();
    output.status.code().unwrap()
}

#[test]
fn other_process_is_excluded_and_process_exit_releases_the_lock() {
    let directory = Directory::new();
    let lease = directory.acquire();
    assert_eq!(child(&directory), 87);
    drop(lease);
    assert_eq!(child(&directory), 86);
    let lease = directory.acquire();
    assert_eq!(lease.epoch(), WorldEpoch(3));
}

#[test]
fn inspection_is_exclusive_and_byte_preserving_until_explicit_start() {
    let directory = Directory::new();
    assert!(WorldLease::inspect(&directory.0, WORLD, 8192).is_err());
    assert!(fs::read_dir(&directory.0).unwrap().next().is_none());
    drop(directory.acquire());
    let path = directory.0.join("world.lock");
    let first = fs::read(&path).unwrap();
    let partial = [first.clone(), b"TORS".to_vec()].concat();
    fs::write(&path, &partial).unwrap();
    for _ in 0..2 {
        let lease = WorldLease::inspect(&directory.0, WORLD, 8192).unwrap();
        assert_eq!(lease.previous_epoch(), WorldEpoch(1));
        assert_eq!(child(&directory), 87);
        assert!(WorldLease::inspect(&directory.0, WORLD, 8192).is_err());
        drop(lease);
        assert_eq!(fs::read(&path).unwrap(), partial);
    }
    let lease = WorldLease::inspect(&directory.0, WORLD, 8192)
        .unwrap()
        .start()
        .unwrap();
    assert_eq!(lease.epoch(), WorldEpoch(2));
    assert_eq!(child(&directory), 87);
    drop(lease);
    assert_eq!(fs::read(&path).unwrap().len(), 96);
}

#[test]
fn full_ledger_can_be_inspected_but_cannot_start_without_capacity() {
    let directory = Directory::new();
    drop(WorldDirectory::acquire(&directory.0, WORLD, 48).unwrap());
    let path = directory.0.join("world.lock");
    let before = fs::read(&path).unwrap();
    let lease = WorldLease::inspect(&directory.0, WORLD, 48).unwrap();
    assert_eq!(lease.start().err().unwrap().code, FaultCode::ResourceLimit);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(WorldLease::inspect(&directory.0, WorldId(99), 8192).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}
