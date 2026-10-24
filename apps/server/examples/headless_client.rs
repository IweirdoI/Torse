//! Receive an authorized baseline from the standalone server, without a GPU.
use std::time::{Duration, Instant};
use torse_codec::wire;
use torse_net::{ReplicaMirror, TcpTransport, Transport, TransportLimits};
use torse_protocol::PROTOCOL_VERSION;
use torse_server::bootstrap as base;
use torse_server::network::{ClientMessage, ServerMessage};
use torse_types::Fault;

fn run() -> Result<(), Fault> {
    let address = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7777".into());
    let stream =
        std::net::TcpStream::connect(address).map_err(|e| Fault::invalid_data(e.to_string()))?;
    let limits = TransportLimits {
        frame: 8_388_608,
        queued_frames: 32,
        queued_bytes: 16_777_216,
        poll_bytes: 262_144,
    };
    let mut transport = TcpTransport::new(stream, limits)?;
    let (world, _) = base::world()?;
    for message in [
        ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            compatibility: world.coordinator().compatibility(),
        },
        ClientMessage::Subscribe {
            sections: base::scope().sections.into_iter().collect(),
        },
    ] {
        let mut bytes = Vec::new();
        wire::encode(&message, &mut bytes, base::budget(), base::codec_limits())?;
        transport.send(bytes)?;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut welcomed = None;
    while Instant::now() < deadline {
        transport.poll()?;
        while let Some(bytes) = transport.receive()? {
            let message: ServerMessage =
                wire::decode(&bytes, base::budget(), base::codec_limits())?;
            match message {
                ServerMessage::Welcome { binding, session } => {
                    println!("session={} epoch={}", session.0, binding.epoch.0);
                    welcomed = Some(binding);
                }
                ServerMessage::Seed(seed) => {
                    if welcomed != Some(seed.binding) {
                        return Err(Fault::invalid_data("baseline without matching welcome"));
                    }
                    let replica = ReplicaMirror::from_seed(seed)?;
                    println!(
                        "baseline={} sections={} entities={} component_sets={}",
                        replica.through.seq.0,
                        replica.state.sections.len(),
                        replica.state.entities.len(),
                        replica
                            .state
                            .entities
                            .iter()
                            .map(|e| e.components.len())
                            .sum::<usize>()
                    );
                    return Ok(());
                }
                ServerMessage::Rejected(reason) => {
                    return Err(Fault::invalid_data(format!("request rejected: {reason:?}")));
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err(Fault::invalid_data("baseline timeout"))
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("client failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
