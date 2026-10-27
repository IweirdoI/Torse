//! Nonblocking standalone finite-world server, using the same durable engine as tests.
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use torse_net::{TcpTransport, TransportLimits};
use torse_server::{
    HostAuthority, PlanningConfig, WorldDirectory, bootstrap as base,
    execution::{ExecutionHost, ExecutionRecovery, PrevalidationConfig},
    network::{NetworkHub, PeerPolicy},
};
use torse_sim::action::ActorContext;
use torse_types::{Fault, Tick};
fn run() -> Result<(), Fault> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        println!(
            "usage: torse-server <world-directory> [--listen ADDRESS] [--ticks N; 0=continuous] [--fill] [--editors] [--mod DIRECTORY] [--machine-operators] [--planning-workers N] [--prevalidate-workers N] [--prevalidate-components; 64MiB/job] [--commit-batch N; default 16]"
        );
        return Ok(());
    };
    if path == "--help" {
        println!(
            "torse-server <world-directory> [--listen ADDRESS] [--ticks N] [--players N; 0..16] [--mod DIRECTORY] [--machine-operators] [--planning-workers N] [--prevalidate-workers N] [--prevalidate-components; 64MiB/job] [--commit-batch N; default 16]"
        );
        return Ok(());
    }
    let path = PathBuf::from(path);
    let mut address = "127.0.0.1:7777".to_owned();
    let mut ticks = 0u64;
    let mut fill = false;
    let mut players = 0usize;
    let mut editors = false;
    let mut revoke_after = 0u64;
    let mut mod_dirs = Vec::new();
    let mut machine_operators = false;
    let mut planning = None;
    let mut prevalidation = None;
    let mut component_prevalidation = false;
    let mut commit_batch = 16;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--players" => {
                players = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|n| *n <= 16)
                    .ok_or_else(|| Fault::invalid_data("players must be 0..16"))?;
            }
            "--prevalidate-components" => component_prevalidation = true,
            "--prevalidate-workers" => {
                let workers = args
                    .next()
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|n| (1..=64).contains(n))
                    .ok_or_else(|| Fault::invalid_data("prevalidation workers must be 1..64"))?;
                prevalidation = Some(PrevalidationConfig {
                    workers,
                    ..Default::default()
                });
            }
            "--commit-batch" => {
                commit_batch = args
                    .next()
                    .and_then(|v| v.parse::<u32>().ok())
                    .filter(|n| (1..=1024).contains(n))
                    .ok_or_else(|| Fault::invalid_data("commit batch must be 1..1024"))?;
            }
            "--planning-workers" => {
                let workers = args
                    .next()
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|n| (1..=64).contains(n))
                    .ok_or_else(|| Fault::invalid_data("planning workers must be 1..64"))?;
                planning = Some(PlanningConfig {
                    workers,
                    max_in_flight: workers,
                });
            }
            "--mod" => mod_dirs
                .push(PathBuf::from(args.next().ok_or_else(|| {
                    Fault::invalid_data("missing mod directory")
                })?)),
            "--machine-operators" => machine_operators = true,
            "--listen" => {
                address = args
                    .next()
                    .ok_or_else(|| Fault::invalid_data("missing listen address"))?
            }
            "--ticks" => {
                ticks = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| Fault::invalid_data("invalid tick count"))?
            }
            "--fill" => fill = true,
            "--editors" => editors = true,
            "--revoke-after" => {
                revoke_after = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| Fault::invalid_data("invalid revoke tick count"))?
            }
            _ => return Err(Fault::invalid_data("unknown server argument")),
        }
    }
    if component_prevalidation {
        let config = prevalidation.as_mut().ok_or_else(|| {
            Fault::invalid_data("component prevalidation requires --prevalidate-workers")
        })?;
        config.components = true;
        config.max_job_bytes = 64 * 1024 * 1024;
    }
    std::fs::create_dir_all(&path).map_err(|e| Fault::invalid_data(e.to_string()))?;
    if machine_operators && mod_dirs.is_empty() {
        return Err(Fault::invalid_data("--machine-operators requires --mod"));
    }
    let (mut world, actions, rules, machine_targets) = if mod_dirs.is_empty() {
        let (world, actions) = base::world()?;
        (
            world,
            actions,
            Vec::new(),
            Vec::<torse_types::EntityId>::new(),
        )
    } else {
        #[cfg(feature = "mods")]
        {
            let game = torse_server::mods::mod_world(&mod_dirs, true, &[])?;
            let targets = game
                .machines
                .iter()
                .map(|(_, id)| *id)
                .chain([game.input, game.output])
                .collect();
            (game.world, game.actions, game.rules, targets)
        }
        #[cfg(not(feature = "mods"))]
        return Err(Fault::invalid_data("--mod requires the mods feature"));
    };
    if fill && path.join("checkpoints/manifest.log").exists() {
        return Err(Fault::invalid_data("--fill only applies to a fresh world"));
    }
    if fill {
        base::fill_floor(&mut world)?;
    }
    if !path.join("checkpoints/manifest.log").exists() {
        base::spawn_players(&mut world, players)?;
    }
    let compatibility = world.coordinator().compatibility();
    let limits = base::execution_limits();
    let mut policy = base::file_policy();
    policy.journal.max_pending_records = commit_batch;
    let directory = WorldDirectory::acquire(&path, base::WORLD, 65_536)?;
    let codec = base::Codec::new(base::codec_limits())?;
    let mut server = if path.join("checkpoints/manifest.log").exists() {
        base::Files::open_file(directory, compatibility, codec, policy, |binding| {
            Ok(ExecutionRecovery::new(
                binding,
                world.registry(),
                world.schemas(),
                actions,
                rules,
                limits,
            ))
        })?
    } else {
        base::Files::create_file(directory, compatibility, codec, policy, |binding| {
            ExecutionHost::new(binding, world, actions, rules, limits)
        })?
    };
    server.set_parallel_planning(planning)?;
    server.set_prevalidation(prevalidation)?;
    server.set_commit_batch_limit(commit_batch)?;
    let player_ids: Vec<_> = server
        .authority()?
        .world()
        .entities()
        .iter()
        .filter(|(_, entity)| {
            torse_world::PlayerMotion::of(entity)
                .ok()
                .flatten()
                .is_some()
        })
        .map(|(_, entity)| entity.id)
        .take(players)
        .collect();
    if player_ids.len() != players {
        return Err(Fault::invalid_data(
            "requested players must exist in the saved world",
        ));
    }
    let mut player_peers = std::collections::BTreeMap::new();
    let listener = TcpListener::bind(&address).map_err(|e| Fault::invalid_data(e.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| Fault::invalid_data(e.to_string()))?;
    println!(
        "listening={} epoch={} tick={}",
        listener
            .local_addr()
            .map_err(|e| Fault::invalid_data(e.to_string()))?,
        server.binding().epoch.0,
        server.authority()?.resume_point().tick.0
    );
    let limits = base::network_limits();
    limits.validate_timing(base::TICK_PERIOD)?;
    let mut hub = NetworkHub::new(server.binding(), limits)?;
    let mut peers: Vec<u128> = Vec::new();
    // Anchor the demo revocation to the first connection, not process start.
    let mut revoke_at: Option<u64> = None;
    let transport = TransportLimits {
        frame: 8_388_608,
        queued_frames: 32,
        queued_bytes: 16_777_216,
        poll_bytes: 262_144,
    };
    let interval = base::TICK_PERIOD;
    let mut next = Instant::now();
    let mut done = 0u64;
    loop {
        for _ in 0..16 {
            match listener.accept() {
                Ok((stream, _)) => {
                    let stream = TcpTransport::new(stream, transport)?;
                    let scope = base::scope();
                    let actor = if editors {
                        // Editors may edit inside the approved scope; the
                        // grant is host-issued and follows the subscription.
                        let base_actor = ActorContext::new(base::key("base:player"))
                            .permission(base::key("world:permission/edit"));
                        scope
                            .sections
                            .iter()
                            .fold(base_actor, |actor, section| actor.section(*section))
                    } else {
                        ActorContext::new(base::key("base:guest"))
                    };
                    let actor = if machine_operators {
                        // Explicit local demo policy; production hosts must
                        // map authenticated identities to their own grants.
                        let actor = actor
                            .permission(base::key("inventory:permission/read"))
                            .permission(base::key("inventory:permission/transfer"));
                        machine_targets.iter().fold(actor, |actor, id| {
                            actor.target(torse_sim::action::ActionTarget::Entity(*id))
                        })
                    } else {
                        actor
                    };
                    let policy = PeerPolicy { actor, scope };
                    if let Ok(id) = hub.add(stream, policy, server.authority()?.resume_point().tick)
                    {
                        player_peers.retain(|peer, _| hub.contains(*peer));
                        if let Some(player) = player_ids
                            .iter()
                            .find(|id| !player_peers.values().any(|used| used == *id))
                        {
                            hub.assign_player(id, *player)?;
                            player_peers.insert(id, *player);
                        }
                        peers.push(id);
                        if revoke_at.is_none() && revoke_after != 0 {
                            revoke_at = Some(done + revoke_after);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(Fault::invalid_data(e.to_string())),
            }
        }
        hub.poll(&mut server)?;
        if let Some(at) = revoke_at
            && done >= at
        {
            revoke_at = None;
            let count = peers.len();
            for id in std::mem::take(&mut peers) {
                hub.revoke(&mut server, id)?;
            }
            println!("revoked_sessions={count}");
        }
        if Instant::now() >= next {
            let started = Instant::now();
            let before_tick = server.authority()?.resume_point().tick.0;
            server.drive_tick(&mut hub, 4096)?;
            done += server.authority()?.resume_point().tick.0 - before_tick;
            next += interval;
            if started.elapsed() > interval {
                eprintln!("tick_over_budget_ms={}", started.elapsed().as_millis());
            }
            hub.poll(&mut server)?;
            if ticks != 0 && done >= ticks {
                break;
            }
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let tick = server.authority()?.resume_point().tick;
    let report = server.shutdown(Tick(tick.0), &mut hub)?;
    println!("closed durable_seq={} ticks={done}", report.through.seq.0);
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("server failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
