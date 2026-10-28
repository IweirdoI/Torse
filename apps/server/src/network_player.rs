//! Bounded transport movement input, independently sequenced from actions.
use super::*;
use torse_sim::durable::ReplyStatus;
use torse_sim::player::{MoveInput, MoveInputBuffer};

pub(super) struct Control {
    pub entity: EntityId,
    pub viewer: Option<Viewer>,
    buffer: MoveInputBuffer,
    next_request: u64,
    pending: Option<(RequestId, u64)>,
    scheduled: Option<Tick>,
    confirmed: u64,
    received_tick: Tick,
    received: u32,
}
impl Control {
    pub(super) fn new(entity: EntityId, capacity: usize) -> Self {
        Self {
            entity,
            viewer: None,
            buffer: MoveInputBuffer::new(capacity),
            next_request: 1,
            pending: None,
            scheduled: None,
            confirmed: 0,
            received_tick: Tick(0),
            received: 0,
        }
    }
}
impl<T: Transport> NetworkHub<T> {
    /// Assign a host-created player before Hello. Client input can never select an entity.
    pub fn assign_player(&mut self, peer: u128, entity: EntityId) -> Result<(), Fault> {
        if self.authentication.is_some() {
            return Err(Fault::invalid_data(
                "authenticated players are assigned by user policy",
            ));
        }
        if entity.0 == 0
            || self
                .peers
                .values()
                .any(|p| p.player.as_ref().is_some_and(|p| p.entity == entity))
        {
            return Err(Fault::invalid_data("player assignment unavailable"));
        }
        let peer = self
            .peers
            .get_mut(&peer)
            .ok_or_else(|| Fault::invalid_data("unknown peer"))?;
        if peer.viewer.is_some() || peer.player.is_some() {
            return Err(Fault::invalid_data(
                "player assignment requires a fresh peer",
            ));
        }
        peer.player = Some(Control::new(entity, self.limits.movement_inputs));
        Ok(())
    }
    pub(super) fn send_player<
        J: CommitJournal<ExecutionModel>,
        S: HostCheckpointStore<ExecutionModel>,
    >(
        peer: &mut Peer<T>,
        server: &Server<ExecutionHost, J, S>,
        reset: bool,
        limits: NetworkLimits,
    ) -> Result<(), Fault> {
        let viewer = peer
            .viewer
            .ok_or_else(|| Fault::invalid_data("handshake required"))?;
        let state = server
            .authority()?
            .player_state(viewer.session)
            .ok_or_else(|| Fault::invalid_data("player outside approved residency"))?;
        let confirmed = peer.player.as_ref().expect("assigned player").confirmed;
        Self::send(
            peer,
            ServerMessage::Player(torse_protocol::PlayerUpdate {
                session: viewer.session,
                entity: state.entity,
                input_seq: confirmed,
                tick: state.tick,
                position_bits: state.position.to_bits(),
                vertical_micros: state.motion.vertical_micros,
                grounded: state.motion.grounded,
                reset,
            }),
            limits,
        )
    }
    pub(super) fn receive_movement<
        J: CommitJournal<ExecutionModel>,
        S: HostCheckpointStore<ExecutionModel>,
    >(
        peer: &mut Peer<T>,
        server: &Server<ExecutionHost, J, S>,
        input: torse_protocol::MoveCommand,
        limits: NetworkLimits,
    ) -> Result<(), Fault> {
        let viewer = peer
            .viewer
            .ok_or_else(|| Fault::invalid_data("handshake required"))?;
        let control = peer
            .player
            .as_mut()
            .ok_or_else(|| Fault::invalid_data("no player binding"))?;
        if input.session != viewer.session || input.seq == 0 {
            return Err(Fault::invalid_data("stale movement session or sequence"));
        }
        let tick = server.authority()?.resume_point().tick;
        if control.received_tick != tick {
            control.received_tick = tick;
            control.received = 0;
        }
        if control.received >= limits.requests_per_tick {
            return Self::send_player(peer, server, true, limits);
        }
        control.received += 1;
        let stale = input.seq <= control.buffer.watermark();
        control.buffer.push(MoveInput {
            seq: input.seq,
            wish: [input.wish[0], 0, input.wish[1]],
            jump: input.jump,
        });
        let reset = control.buffer.take_resync();
        if stale || reset {
            Self::send_player(peer, server, reset, limits)?;
        }
        Ok(())
    }
    pub(super) fn poll_player<
        J: CommitJournal<ExecutionModel>,
        S: HostCheckpointStore<ExecutionModel>,
    >(
        peer: &mut Peer<T>,
        server: &mut Server<ExecutionHost, J, S>,
        limits: NetworkLimits,
    ) -> Result<(), Fault> {
        let Some(control) = peer.player.as_mut() else {
            return Ok(());
        };
        if let Some((id, seq)) = control.pending {
            let status = server
                .receipt(id)?
                .map(|r| matches!(r.reply.status, ReplyStatus::Rejected(_)));
            let expired = server.authority()?.expired(id);
            if status.is_none() && !expired {
                return Ok(());
            }
            control.pending = None;
            control.confirmed = control.confirmed.max(seq);
            Self::send_player(peer, server, status.unwrap_or(true), limits)?;
        }
        let control = peer.player.as_mut().expect("assigned player");
        let authority = server.authority()?;
        // Admission at a partial tick is deferred by the host; only enqueue at
        // a boundary so polling frequency cannot manufacture extra physics steps.
        if !authority.at_tick_boundary() {
            return Ok(());
        }
        let tick = authority.resume_point().tick;
        if control.scheduled == Some(tick) {
            return Ok(());
        }
        let input = control.buffer.peek().unwrap_or(MoveInput {
            seq: control.confirmed,
            wish: [0; 3],
            jump: false,
        });
        let child = control
            .viewer
            .ok_or_else(|| Fault::invalid_data("missing movement session"))?;
        let next = control
            .next_request
            .checked_add(1)
            .ok_or_else(|| Fault::resource_limit("movement sequence exhausted"))?;
        let id = RequestId {
            world: server.binding().world,
            epoch: server.binding().epoch,
            session: child.session,
            seq: RequestSeq(control.next_request),
        };
        let admission = server.submit(WorldRequest {
            id,
            actor: child.actor,
            input: crate::execution::movement_call(control.entity, input),
        });
        if let Admission::Queued { target_tick } = admission {
            if control.buffer.peek().is_some() {
                control.buffer.settle(1);
            }
            control.next_request = next;
            control.pending = Some((id, input.seq));
            control.scheduled = Some(target_tick);
        }
        Ok(())
    }
}
