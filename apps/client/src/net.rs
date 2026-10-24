//! Client side of the version 1 protocol: connect, mirror, and request.
//!
//! The connection owns one transport and one [`ReplicaMirror`]. The window
//! polls it every frame; inbound frames decode into [`NetEvent`]s and the
//! caller applies world data through the scene. Frames are bounded by the
//! same limits the headless client uses.

use torse_codec::CodecLimits;
use torse_codec::wire;
use torse_net::{ReplicaMirror, Transport};
use torse_protocol::PROTOCOL_VERSION;
use torse_sim::replica::{ReplicaChanges, ReplicaSnapshot};
use torse_types::{
    Admission, BlockPos, Compatibility, ContentKey, Fault, OutputBudget, RejectCode, RequestSeq,
    SessionId,
};

/// The concrete message shapes this demo exchanges; the type parameters all
/// live in shared crates, so the client binds them itself.
pub type ClientMessage = torse_protocol::ClientMessage<torse_sim::durable::Call>;
pub type ServerMessage =
    torse_protocol::ServerMessage<torse_sim::durable::Reply, ReplicaSnapshot, ReplicaChanges>;

/// One decoded server message the window acts on.
#[derive(Debug)]
pub enum NetEvent {
    /// Handshake finished; the session is live.
    Welcomed { session: SessionId },
    /// The authorized baseline arrived; the mirror now holds a full snapshot.
    Baseline(ReplicaSnapshot),
    /// One increment was validated and applied to the mirror.
    Increment(ReplicaChanges),
    /// Admission for a submitted request.
    Admitted {
        seq: RequestSeq,
        rejected: Option<RejectCode>,
    },
    /// A request result arrived.
    Finished { seq: RequestSeq, committed: bool },
    /// The server closed the connection with a reason.
    Closed(String),
}

/// Live client connection to a standalone server.
pub struct Connection<T: Transport> {
    transport: T,
    mirror: Option<ReplicaMirror>,
    request_seq: u64,
    budget: OutputBudget,
    limits: CodecLimits,
}

impl<T: Transport> Connection<T> {
    /// Wrap a connected transport with the demo frame budget.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            mirror: None,
            request_seq: 0,
            budget: OutputBudget {
                max_items: 200_000,
                max_bytes: 8_388_608,
            },
            limits: codec_limits(),
        }
    }

    fn send(&mut self, message: ClientMessage) -> Result<(), Fault> {
        let mut bytes = Vec::new();
        wire::encode(&message, &mut bytes, self.budget, self.limits)?;
        self.transport.send(bytes)
    }

    /// Start the handshake with the shared content compatibility.
    pub fn hello(&mut self, compatibility: Compatibility) -> Result<(), Fault> {
        self.send(ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            compatibility,
        })
    }

    /// Subscribe to a fixed section set the host already approved.
    pub fn subscribe(&mut self, sections: Vec<torse_types::SectionPos>) -> Result<(), Fault> {
        self.send(ClientMessage::Subscribe { sections })
    }

    /// Submit one action call; sequence numbers start at one and never repeat.
    pub fn request(&mut self, call: torse_sim::durable::Call) -> Result<RequestSeq, Fault> {
        self.request_seq += 1;
        let seq = RequestSeq(self.request_seq);
        self.send(ClientMessage::Request { seq, call })?;
        Ok(seq)
    }

    /// Poll the transport and translate every completed frame.
    pub fn poll(&mut self) -> Result<Vec<NetEvent>, Fault> {
        self.transport.poll()?;
        let mut events = Vec::new();
        while let Some(bytes) = self.transport.receive()? {
            let message: ServerMessage = wire::decode(&bytes, self.budget, self.limits)?;
            match self.handle(message)? {
                Some(event) => events.push(event),
                None => continue,
            }
        }
        Ok(events)
    }

    fn handle(&mut self, message: ServerMessage) -> Result<Option<NetEvent>, Fault> {
        match message {
            ServerMessage::Welcome { session, .. } => Ok(Some(NetEvent::Welcomed { session })),
            ServerMessage::Seed(seed) => {
                // Clone the snapshot first; from_seed consumes the envelope.
                let snapshot = seed.state.clone();
                let mirror = ReplicaMirror::from_seed(seed)?;
                self.mirror = Some(mirror);
                Ok(Some(NetEvent::Baseline(snapshot)))
            }
            ServerMessage::Batch(batch) => {
                let seq = batch.seq;
                let changes = batch.changes.clone();
                let mirror = self
                    .mirror
                    .as_mut()
                    .ok_or_else(|| Fault::invalid_data("batch before baseline"))?;
                mirror.apply(batch)?;
                let _ = seq;
                Ok(Some(NetEvent::Increment(changes)))
            }
            ServerMessage::Admission { seq, admission } => {
                let rejected = match admission {
                    Admission::Queued { .. } | Admission::Duplicate => None,
                    Admission::Rejected(code) => Some(code),
                };
                Ok(Some(NetEvent::Admitted { seq, rejected }))
            }
            ServerMessage::Result { seq, reply } => {
                // A rejected reply is a completed request the caller must see
                // as denied, not silently dropped. ReadOnly results are a
                // legitimate query outcome, not a rejection.
                use torse_sim::durable::ReplyStatus;
                let committed = !matches!(reply.status, ReplyStatus::Rejected(_));
                Ok(Some(NetEvent::Finished { seq, committed }))
            }
            ServerMessage::Rejected(reason) => {
                Ok(Some(NetEvent::Closed(format!("rejected: {reason:?}"))))
            }
        }
    }
}

impl Connection<torse_net::TcpTransport> {
    /// Connect over TCP to a standalone server.
    pub fn connect(address: &str) -> Result<Connection<torse_net::TcpTransport>, Fault> {
        let stream = std::net::TcpStream::connect(address)
            .map_err(|error| Fault::invalid_data(error.to_string()))?;
        let transport = torse_net::TcpTransport::new(
            stream,
            torse_net::TransportLimits {
                frame: 8_388_608,
                queued_frames: 32,
                queued_bytes: 16_777_216,
                poll_bytes: 262_144,
            },
        )?;
        Ok(Connection::new(transport))
    }
}

/// Build the same place or break call the local scene commits, for the
/// network request path.
pub fn edit_call(place: bool, block: &ContentKey, target: BlockPos) -> torse_sim::durable::Call {
    let action = if place {
        "world:action/place"
    } else {
        "world:action/break"
    };
    let mut parameters = torse_world::ComponentFields::new();
    if place {
        parameters.insert(
            ContentKey::parse("world:field/block").expect("static key"),
            torse_world::ComponentValue::Content(block.clone()),
        );
    }
    torse_sim::durable::Call {
        action: ContentKey::parse(action).expect("static key"),
        version: 1,
        target: torse_sim::action::ActionTarget::Block(target),
        parameters,
    }
}

fn codec_limits() -> CodecLimits {
    CodecLimits {
        max_frame_bytes: 8_388_608,
        max_field_bytes: 262_144,
        max_depth: 12,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torse_net::{MemoryTransport, TransportLimits};

    #[test]
    fn request_sequences_start_at_one_and_increase() {
        let (client, _server) = MemoryTransport::pair(default_limits()).expect("pair");
        let mut connection = Connection::new(client);
        let call = edit_call(
            false,
            &ContentKey::parse("base:stone").expect("key"),
            BlockPos::new(1, 1, 1),
        );
        let first = connection.request(call.clone()).expect("send");
        let second = connection.request(call).expect("send");
        assert_eq!(first.0, 1);
        assert_eq!(second.0, 2);
    }

    #[test]
    fn read_only_results_are_not_rejections() {
        use torse_sim::durable::{Reply, ReplyStatus};
        use torse_types::{CommitCursor, CommitSeq, OutputBudget, WorldId};

        let (client, mut server) = MemoryTransport::pair(default_limits()).expect("pair");
        let mut connection = Connection::new(client);
        let reply = Reply {
            status: ReplyStatus::ReadOnly,
            fields: Default::default(),
            at: CommitCursor {
                world: WorldId(1),
                seq: CommitSeq(1),
            },
        };
        let frame = {
            let mut bytes = Vec::new();
            wire::encode(
                &ServerMessage::Result {
                    seq: RequestSeq(1),
                    reply,
                },
                &mut bytes,
                OutputBudget {
                    max_items: 1000,
                    max_bytes: 65_536,
                },
                codec_limits(),
            )
            .expect("encode");
            bytes
        };
        server.send(frame).expect("send");
        let events = connection.poll().expect("poll");
        match &events[..] {
            [NetEvent::Finished { seq, committed }] => {
                assert_eq!(seq.0, 1);
                assert!(committed, "read-only is a legitimate outcome");
            }
            other => panic!("unexpected events {other:?}"),
        }
    }

    fn default_limits() -> TransportLimits {
        TransportLimits {
            frame: 8_388_608,
            queued_frames: 32,
            queued_bytes: 16_777_216,
            poll_bytes: 262_144,
        }
    }
}
