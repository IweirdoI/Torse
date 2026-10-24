//! Nonblocking standalone finite-world server, using the same durable engine as tests.
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use torse_net::{TcpTransport, TransportLimits};
use torse_server::{
    HostAuthority, WorldDirectory, bootstrap as base,
    execution::{ExecutionHost, ExecutionRecovery},
    network::{NetworkHub, PeerPolicy},
};
use torse_sim::action::ActorContext;
use torse_types::{Fault, Tick};
fn run() -> Result<(), Fault> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        println!(
            "usage: torse-server <world-directory> [--listen 127.0.0.1:7777] [--ticks N; 0=continuous] [--fill] [--editors]"
        );
        return Ok(());
    };
    if path == "--help" {
        println!("torse-server <world-directory> [--listen ADDRESS] [--ticks N]");
        return Ok(());
    }
    let path = PathBuf::from(path);
    let mut address = "127.0.0.1:7777".to_owned();
    let mut ticks = 0u64;
    let mut fill = false;
    let mut editors = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
            _ => return Err(Fault::invalid_data("unknown server argument")),
        }
    }
    std::fs::create_dir_all(&path).map_err(|e| Fault::invalid_data(e.to_string()))?;
    let (mut world, actions) = base::world()?;
    if fill && path.join("checkpoints/manifest.log").exists() {
        return Err(Fault::invalid_data("--fill only applies to a fresh world"));
    }
    if fill {
        base::fill_floor(&mut world)?;
    }
    let compatibility = world.coordinator().compatibility();
    let limits = base::execution_limits();
    let policy = base::file_policy();
    let directory = WorldDirectory::acquire(&path, base::WORLD, 65_536)?;
    let codec = base::Codec::new(base::codec_limits())?;
    let mut server = if path.join("checkpoints/manifest.log").exists() {
        base::Files::open_file(directory, compatibility, codec, policy, |binding| {
            Ok(ExecutionRecovery::new(
                binding,
                world.registry(),
                world.schemas(),
                actions,
                Vec::new(),
                limits,
            ))
        })?
    } else {
        base::Files::create_file(directory, compatibility, codec, policy, |binding| {
            ExecutionHost::new(binding, world, actions, Vec::new(), limits)
        })?
    };
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
    let mut hub = NetworkHub::new(server.binding(), base::network_limits())?;
    let transport = TransportLimits {
        frame: 8_388_608,
        queued_frames: 32,
        queued_bytes: 16_777_216,
        poll_bytes: 262_144,
    };
    let interval = Duration::from_millis(50);
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
                    let policy = PeerPolicy { actor, scope };
                    let _ = hub.add(stream, policy, server.authority()?.resume_point().tick);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(Fault::invalid_data(e.to_string())),
            }
        }
        hub.poll(&mut server)?;
        if Instant::now() >= next {
            let started = Instant::now();
            server.drive_tick(&mut hub, 4096)?;
            done += 1;
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
