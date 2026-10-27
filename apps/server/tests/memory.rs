//! Shared admission remains retryable across world, replica and file lifetimes.
mod support;
use support::Directory;
use torse_net::{MemoryTransport, Transport};
use torse_server::{bootstrap as base, execution::*, network::*, section_pages::SectionPages, *};
use torse_sim::action::ActorContext;
use torse_types::*;

fn create(dir: &Directory, pool: &MemoryPool) -> base::Files {
    let (world, actions) = base::world().unwrap();
    let compatibility = world.coordinator().compatibility();
    let mut server = base::Files::create_file(
        dir.acquire(),
        compatibility,
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| {
            ExecutionHost::new(
                binding,
                world,
                actions,
                Vec::new(),
                base::execution_limits(),
            )
        },
    )
    .unwrap();
    server
        .attach_memory(pool.clone(), 48 * 1024 * 1024)
        .unwrap();
    server
}

#[test]
fn preparation_pressure_does_not_advance_durability_and_retries_after_release() {
    let dir = Directory::new();
    let pool = MemoryPool::new(MemoryBudget::new(128 * 1024 * 1024));
    let mut server = create(&dir, &pool);
    let before = server.durability();
    let resident = pool.usage().total;
    let temporary = pool
        .reserve(
            MemoryClass::Mesh,
            128 * 1024 * 1024 - resident,
            EvictionRank::Pinned,
        )
        .unwrap();
    for _ in 0..3 {
        assert!(matches!(
            server.drive_one(&mut NoReplicas).unwrap(),
            CommitStep::Idle
        ));
        assert_eq!(server.durability(), before);
    }
    drop(temporary);
    assert!(matches!(
        server.drive_one(&mut NoReplicas).unwrap(),
        CommitStep::Durable(_)
    ));
    assert!(server.durability().through.0 > before.through.0);
    assert_eq!(
        pool.usage().current[MemoryClass::Codec.index()],
        48 * 1024 * 1024
    );
    drop(server);
    assert_eq!(pool.usage().total, 0);
}

#[test]
fn section_and_replica_lifetimes_release_their_own_reservations() {
    let dir = Directory::new();
    let pool = MemoryPool::new(MemoryBudget::new(256 * 1024 * 1024));
    let mut server = create(&dir, &pool);
    let viewer = Viewer {
        session: SessionId(1),
        actor: ActorContextId(1),
    };
    server
        .register_session(
            viewer,
            ActorContext::new(base::key("test:reader")),
            base::scope(),
        )
        .unwrap();
    let seed = server
        .open_replica(viewer.session, base::scope(), base::budget())
        .unwrap();
    assert!(pool.usage().current[MemoryClass::Replica.index()] > 0);
    server.close_replica(seed.subscription).unwrap();
    assert_eq!(pool.usage().current[MemoryClass::Replica.index()], 0);
    let root = dir.0.join("sections");
    std::fs::create_dir_all(&root).unwrap();
    let pages = SectionPages::new(root, base::budget(), base::codec_limits(), 64).unwrap();
    let before = pool.usage().current[MemoryClass::World.index()];
    server
        .unload_section(&pages, SectionPos::ZERO, &mut NoReplicas)
        .unwrap();
    assert!(pool.usage().current[MemoryClass::World.index()] < before);
    server
        .reload_section(&pages, SectionPos::ZERO, &mut NoReplicas)
        .unwrap();
    assert_eq!(
        pool.usage().current[MemoryClass::World.index()],
        server.authority().unwrap().world().accounted_bytes()
    );
    let mut hub =
        NetworkHub::with_memory(server.binding(), base::network_limits(), pool.clone()).unwrap();
    let (peer, mut client) = MemoryTransport::pair(torse_net::TransportLimits {
        frame: 1024,
        queued_bytes: 4096,
        queued_frames: 4,
        poll_bytes: 4096,
    })
    .unwrap();
    let id = hub
        .add(
            peer,
            PeerPolicy {
                actor: ActorContext::new(base::key("test:peer")),
                scope: base::scope(),
            },
            Tick(0),
        )
        .unwrap();
    assert!(pool.usage().current[MemoryClass::Replica.index()] > 0);
    let temporary = pool
        .reserve(
            MemoryClass::Resource,
            256 * 1024 * 1024 - pool.usage().total,
            EvictionRank::Pinned,
        )
        .unwrap();
    let mut close = Vec::new();
    torse_codec::wire::encode(
        &torse_protocol::ClientMessage::<torse_sim::durable::Call>::Close,
        &mut close,
        base::budget(),
        base::codec_limits(),
    )
    .unwrap();
    client.send(close).unwrap();
    hub.poll(&mut server).unwrap();
    assert!(!hub.contains(id));
    drop(temporary);
    assert_eq!(pool.usage().current[MemoryClass::Replica.index()], 0);
    assert!(client.receive().is_err());
    drop(server);
    drop(hub);
    assert_eq!(pool.usage().total, 0);
}
