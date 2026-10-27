//! Cached collision work cannot alter serial journal, projection or recovery.
#[allow(dead_code)]
#[path = "../examples/serial_baseline/workload.rs"]
mod serial;
mod support;
#[allow(dead_code)]
#[path = "../examples/movement_baseline/workload.rs"]
mod workload;
use support::Directory;
use torse_codec::wire;
use torse_server::{bootstrap as base, execution::*, *};
use torse_sim::{
    Phase,
    action::{ActionTarget, ActorContext},
};
use torse_types::*;
use workload::Motion;

fn load() -> Motion {
    Motion {
        tasks: 8,
        half: [4.0; 3],
        contended: false,
        mixed: false,
        deny_plan: false,
    }
}
fn create(dir: &Directory, load: Motion) -> base::Files {
    let world = load.world();
    let (_, actions) = base::world().unwrap();
    let mut policy = base::file_policy();
    // Deliberately smaller than the requested serial publication batch.
    policy.journal.max_pending_records = 2;
    base::Files::create_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        policy,
        |binding| {
            ExecutionHost::new(
                binding,
                world,
                actions,
                load.rules(),
                base::execution_limits(),
            )
        },
    )
    .unwrap()
}
fn reopen(dir: &Directory, load: Motion) -> base::Files {
    let world = load.world();
    let (_, actions) = base::world().unwrap();
    base::Files::open_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| {
            Ok(ExecutionRecovery::new(
                binding,
                world.registry(),
                world.schemas(),
                actions,
                load.rules(),
                base::execution_limits(),
            ))
        },
    )
    .unwrap()
}
fn config(workers: usize) -> PrevalidationConfig {
    PrevalidationConfig {
        workers,
        max_in_flight: 4,
        ..Default::default()
    }
}

#[test]
fn shared_prevalidation_reservations_release_after_the_committed_window() {
    let dir = Directory::new();
    let pool = MemoryPool::new(MemoryBudget::new(256 * 1024 * 1024));
    let mut server = create(&dir, load());
    server
        .attach_memory(pool.clone(), 48 * 1024 * 1024)
        .unwrap();
    server.set_prevalidation(Some(config(4))).unwrap();
    server.drive_tick(&mut NoReplicas, 1024).unwrap();
    assert!(server.authority().unwrap().prevalidation_stats().prepared > 0);
    assert!(server.authority().unwrap().prevalidation_stats().hits > 0);
    let retained = pool.usage().current[MemoryClass::Executor.index()];
    assert!(pool.usage().peak[MemoryClass::Executor.index()] > retained);
    drop(server);
    assert_eq!(pool.usage().total, 0);
}

#[test]
fn serial_one_and_four_workers_preserve_exact_records_and_authorized_projection_with_busy_retries()
{
    for (contended, mixed) in [(false, false), (true, false), (false, true)] {
        let load = Motion {
            contended,
            mixed,
            ..load()
        };
        let mut reference = None;
        for workers in [0, 1, 4] {
            let dir = Directory::new();
            let mut server = create(&dir, load);
            server.set_commit_batch_limit(16).unwrap();
            if workers != 0 {
                server.set_prevalidation(Some(config(workers))).unwrap();
            }
            let actor = server.authority().unwrap().world().entities().iter().fold(
                ActorContext::new(base::key("movement:viewer")),
                |a, (_, e)| a.target(ActionTarget::Entity(e.id)),
            );
            server
                .register_session(
                    Viewer {
                        session: SessionId(1),
                        actor: ActorContextId(1),
                    },
                    actor,
                    base::scope(),
                )
                .unwrap();
            let subscription = server
                .open_replica(SessionId(1), base::scope(), base::budget())
                .unwrap()
                .subscription;
            server.drive_tick(&mut NoReplicas, 128).unwrap();
            let mut replica = vec![];
            wire::encode(
                &server
                    .take_replica(subscription, base::budget())
                    .unwrap()
                    .into_iter()
                    .map(|b| b.changes)
                    .collect::<Vec<_>>(),
                &mut replica,
                base::budget(),
                base::codec_limits(),
            )
            .unwrap();
            let stats = server.authority().unwrap().prevalidation_stats();
            if workers != 0 && !contended {
                assert!(stats.hits >= if mixed { 6 } else { 8 });
                assert!(stats.peak_jobs <= 4);
                assert!(stats.peak_reserved_bytes <= 4 * config(workers).max_job_bytes);
                assert!(stats.peak_cached_bytes <= stats.peak_reserved_bytes);
            }
            if contended && workers != 0 {
                assert!(stats.prepared < load.tasks as u64);
                assert!(stats.skipped > 0);
            }
            let state = serial::state_bytes(server.authority().unwrap()).unwrap();
            drop(server);
            let result = (
                state,
                std::fs::read(dir.0.join("journal.log")).unwrap(),
                replica,
            );
            if let Some(expected) = &reference {
                assert_eq!(&result, expected);
            } else {
                reference = Some(result);
            }
        }
    }
}

#[test]
fn partial_frozen_phase_discards_cache_and_recomputes_remaining_work_without_callbacks() {
    for completed in [0, 1, 3, 7] {
        let dir = Directory::new();
        let load = load();
        let mut server = create(&dir, load);
        server.set_prevalidation(Some(config(4))).unwrap();
        while server.authority().unwrap().runtime().work.is_none()
            || usize::from(server.authority().unwrap().resume_point().next_phase)
                != Phase::Simulation.index()
        {
            server.drive_one(&mut NoReplicas).unwrap();
        }
        for _ in 0..completed {
            server.drive_one(&mut NoReplicas).unwrap();
        }
        drop(server);
        let mut recovered = reopen(
            &dir,
            Motion {
                deny_plan: true,
                ..load
            },
        );
        assert_eq!(
            recovered
                .authority()
                .unwrap()
                .prevalidation_stats()
                .prepared,
            0
        );
        recovered.set_prevalidation(Some(config(4))).unwrap();
        recovered.drive_tick(&mut NoReplicas, 128).unwrap();
        let state = serial::state_bytes(recovered.authority().unwrap()).unwrap();
        let reference_dir = Directory::new();
        let mut reference = create(&reference_dir, load);
        reference.drive_tick(&mut NoReplicas, 128).unwrap();
        assert_eq!(
            state,
            serial::state_bytes(reference.authority().unwrap()).unwrap()
        );
    }
}

#[test]
fn small_budget_and_small_colliders_fall_back_and_invalid_configuration_is_atomic() {
    for (half, bytes) in [([4.0; 3], 1), ([0.3, 0.9, 0.3], 8 * 1024 * 1024)] {
        let load = Motion { half, ..load() };
        let dir = Directory::new();
        let mut server = create(&dir, load);
        server
            .set_prevalidation(Some(PrevalidationConfig {
                max_job_bytes: bytes,
                ..config(4)
            }))
            .unwrap();
        assert!(
            server
                .set_prevalidation(Some(PrevalidationConfig {
                    workers: 0,
                    ..config(4)
                }))
                .is_err()
        );
        server.drive_tick(&mut NoReplicas, 128).unwrap();
        let host = server.authority().unwrap();
        assert_eq!(host.prevalidation_stats().prepared, 0);
        assert!(host.prevalidation_stats().skipped > 0);
        let state = serial::state_bytes(host).unwrap();
        let other = Directory::new();
        let mut reference = create(&other, load);
        reference.drive_tick(&mut NoReplicas, 128).unwrap();
        assert_eq!(
            state,
            serial::state_bytes(reference.authority().unwrap()).unwrap()
        );
    }
}
