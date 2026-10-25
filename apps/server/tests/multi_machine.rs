//! Real Guest workloads preserve durable bytes with optional CPU paths and Busy.
#![cfg(feature = "mods")]
mod support;
#[allow(dead_code)]
#[path = "../examples/multi_machine/workload.rs"]
mod workload;
use support::Directory;
use torse_server::{bootstrap as base, execution::*, *};
use torse_sim::action::{ActionTarget, ActorContext};
use torse_types::*;
use workload::Load;

fn create(dir: &Directory, load: Load, slots: u32) -> base::Files {
    let game = load.assemble().unwrap();
    let mut policy = base::file_policy();
    policy.journal.max_pending_records = slots;
    base::Files::create_file(
        dir.acquire(),
        game.world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        policy,
        |binding| {
            ExecutionHost::new(
                binding,
                game.world,
                game.actions,
                game.rules,
                base::execution_limits(),
            )
        },
    )
    .unwrap()
}
fn configure(server: &mut base::Files, workers: usize) {
    server.set_commit_batch_limit(16).unwrap();
    if workers != 0 {
        server
            .set_parallel_planning(Some(PlanningConfig {
                workers,
                max_in_flight: workers,
            }))
            .unwrap();
        server
            .set_prevalidation(Some(PrevalidationConfig {
                workers,
                components: true,
                max_job_bytes: 64 * 1024 * 1024,
                ..Default::default()
            }))
            .unwrap();
    }
}
fn register(server: &mut base::Files) -> (SubscriptionId, Vec<u8>) {
    let actor = server.authority().unwrap().world().entities().iter().fold(
        ActorContext::new(base::key("test:operator")),
        |actor, (_, e)| actor.target(ActionTarget::Entity(e.id)),
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
    let seed = server
        .open_replica(SessionId(1), base::scope(), base::budget())
        .unwrap();
    (seed.subscription, workload::encoded(&seed).unwrap())
}
fn run_ticks(
    server: &mut base::Files,
    target: u64,
    subscription: SubscriptionId,
    stream: &mut Vec<u8>,
) {
    for _ in 0..1024 {
        if server.authority().unwrap().resume_point().tick.0 == target {
            return;
        }
        let result = server.drive_batch(&mut NoReplicas, 16).unwrap();
        assert!(result.committed > 0);
        for batch in server.take_replica(subscription, base::budget()).unwrap() {
            stream.extend(workload::encoded(&batch).unwrap());
        }
    }
    panic!("multi-machine tick did not finish");
}

#[test]
fn real_guests_preserve_logs_replicas_and_state_across_optional_workers_and_busy() {
    for shared in [false, true] {
        let load = Load {
            machines: 8,
            shared,
        };
        let mut reference = None;
        for (workers, slots) in [(0, 16), (1, 2), (4, 2)] {
            let dir = Directory::new();
            let mut server = create(&dir, load, slots);
            configure(&mut server, workers);
            let (subscription, mut stream) = register(&mut server);
            run_ticks(&mut server, 10, subscription, &mut stream);
            load.verify(server.authority().unwrap(), 10);
            assert_eq!(
                server.authority().unwrap().prevalidation_stats().prepared,
                0,
                "real small machine components must skip expensive prevalidation"
            );
            let state = workload::state(server.authority().unwrap()).unwrap();
            drop(server);
            let actual = (
                state,
                stream,
                std::fs::read(dir.0.join("journal.log")).unwrap(),
            );
            if let Some(expected) = &reference {
                assert_eq!(&actual, expected);
            } else {
                reference = Some(actual);
            }
        }
    }
}

#[test]
fn sixty_four_real_machines_fit_default_limits_and_resume_frozen_work_without_guests() {
    let load = Load {
        machines: 64,
        shared: false,
    };
    for completed in [0, 3] {
        let dir = Directory::new();
        let mut server = create(&dir, load, 2);
        configure(&mut server, 4);
        server.drive_tick(&mut NoReplicas, 256).unwrap();
        loop {
            let host = server.authority().unwrap();
            if host.resume_point().tick == Tick(1)
                && host
                    .runtime()
                    .work
                    .as_ref()
                    .is_some_and(|w| w.tasks.len() == 64 && w.next == completed)
            {
                break;
            }
            server.drive_one(&mut NoReplicas).unwrap();
        }
        let expected = workload::state(server.authority().unwrap()).unwrap();
        drop(server);
        let game = load.assemble().unwrap();
        let stats = game.stats.clone();
        let mut server = base::Files::open_file(
            dir.acquire(),
            game.world.coordinator().compatibility(),
            base::Codec::new(base::codec_limits()).unwrap(),
            base::file_policy(),
            |binding| {
                Ok(ExecutionRecovery::new(
                    binding,
                    game.world.registry(),
                    game.world.schemas(),
                    game.actions,
                    game.rules,
                    base::execution_limits(),
                ))
            },
        )
        .unwrap();
        assert_eq!(
            expected,
            workload::state(server.authority().unwrap()).unwrap()
        );
        configure(&mut server, 4);
        server.drive_tick(&mut NoReplicas, 256).unwrap();
        assert_eq!(stats.borrow().guest_dispatches.values().sum::<u64>(), 0);
        load.verify(server.authority().unwrap(), 2);
        server.drive_tick(&mut NoReplicas, 256).unwrap();
        assert_eq!(stats.borrow().guest_dispatches.values().sum::<u64>(), 64);
        load.verify(server.authority().unwrap(), 3);
    }
}
