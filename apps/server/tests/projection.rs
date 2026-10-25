//! Projection acceleration must preserve encoded streams and durable execution.
mod support;
#[allow(dead_code)]
#[path = "../examples/serial_baseline/workload.rs"]
mod workload;
use support::Directory;
use torse_codec::wire::{self, Wire};
use torse_server::{bootstrap as base, execution::*, *};
use torse_sim::action::{ActionTarget, ActorContext};
use torse_types::*;
use workload::Workload;

fn encoded<T: Wire>(value: &T) -> Vec<u8> {
    let mut bytes = vec![];
    wire::encode(value, &mut bytes, base::budget(), base::codec_limits()).unwrap();
    bytes
}
fn create(dir: &Directory, load: Workload) -> base::Files {
    let (world, actions) = base::world().unwrap();
    let mut policy = base::file_policy();
    policy.journal.max_pending_records = 4;
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
fn reopen(dir: &Directory, load: Workload) -> base::Files {
    let (world, actions) = base::world().unwrap();
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
fn register(server: &mut base::Files, id: u128) -> torse_sim::replica::WorldReplicaSeed {
    server
        .register_session(
            Viewer {
                session: SessionId(id),
                actor: ActorContextId(id),
            },
            ActorContext::new(base::key("projection:viewer"))
                .target(ActionTarget::Entity(EntityId(1))),
            base::scope(),
        )
        .unwrap();
    server
        .open_replica(SessionId(id), base::scope(), base::budget())
        .unwrap()
}

#[test]
fn full_and_incremental_preserve_encoded_streams_journal_bytes_and_recovered_seeds() {
    let mut reference = None;
    let load = Workload {
        tasks: 8,
        contended: true,
    };
    for optimized in [false, true] {
        for batch in [1, 16] {
            let dir = Directory::new();
            let mut server = create(&dir, load);
            server.set_projection_optimization(optimized).unwrap();
            server.set_commit_batch_limit(batch).unwrap();
            let seed = register(&mut server, 1);
            let mut stream = encoded(&seed);
            for _ in 0..3 {
                server.drive_tick(&mut NoReplicas, 64).unwrap();
                stream.extend(encoded(
                    &server
                        .take_replica(seed.subscription, base::budget())
                        .unwrap(),
                ));
            }
            let final_seed = register(&mut server, 2);
            let expected = workload::state_bytes(server.authority().unwrap()).unwrap();
            drop(server);
            let result = (
                expected.clone(),
                stream,
                std::fs::read(dir.0.join("journal.log")).unwrap(),
            );
            if let Some(previous) = &reference {
                assert_eq!(&result, previous);
            } else {
                reference = Some(result);
            }
            let mut recovered = reopen(&dir, load);
            recovered.set_projection_optimization(optimized).unwrap();
            assert_eq!(
                expected,
                workload::state_bytes(recovered.authority().unwrap()).unwrap()
            );
            assert_eq!(final_seed.state, register(&mut recovered, 1).state);
            recovered.drive_tick(&mut NoReplicas, 64).unwrap();
            load.verify(recovered.authority().unwrap().world(), 4);
        }
    }
}
