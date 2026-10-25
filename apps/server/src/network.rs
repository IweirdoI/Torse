//! Authenticated session/application assembly. Polling does bounded work outside
//! the commit path. Only durable reads, seeds, results and deltas enter transports.
use crate::execution::{ExecutionHost, ExecutionModel};
use crate::{HostAuthority, HostCheckpointStore, InvalidationSink, Server};
use std::collections::{BTreeMap, BTreeSet};
use torse_codec::{CodecLimits, wire};
use torse_net::{SessionRegistry, Transport};
use torse_protocol::PROTOCOL_VERSION;
use torse_sim::action::ActorContext;
use torse_sim::durable::Receipt;
use torse_sim::replica::{ReplicaScope, WorldReplicaBatch, WorldReplicaSeed};
use torse_storage::CommitJournal;
use torse_types::*;

pub type ClientMessage = torse_protocol::ClientMessage<torse_sim::durable::Call>;
pub type ServerMessage = torse_protocol::ServerMessage<
    torse_sim::durable::Reply,
    torse_sim::replica::ReplicaSnapshot,
    torse_sim::replica::ReplicaChanges,
>;

impl<J: CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>
    Server<ExecutionHost, J, S>
{
    pub fn register_session(
        &mut self,
        viewer: Viewer,
        actor: ActorContext,
        scope: ReplicaScope,
    ) -> Result<(), Fault> {
        self.authority()?;
        self.authority.register_session(viewer, actor, scope)
    }
    pub fn open_replica(
        &mut self,
        session: SessionId,
        scope: ReplicaScope,
        budget: OutputBudget,
    ) -> Result<WorldReplicaSeed, Fault> {
        self.authority()?;
        let seed = self.authority.open_replica(session, scope, budget)?;
        if seed.binding != self.binding() || seed.baseline.seq.0 > self.durability().through.0 {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "undurable replica seed",
            ));
        }
        Ok(seed)
    }
    pub fn take_replica(
        &mut self,
        id: SubscriptionId,
        budget: OutputBudget,
    ) -> Result<Vec<WorldReplicaBatch>, Fault> {
        self.authority()?;
        let batches = self.authority.take_replica(id, budget)?;
        if batches
            .iter()
            .any(|b| b.binding != self.binding() || b.through.seq.0 > self.durability().through.0)
        {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "undurable replica batch",
            ));
        }
        Ok(batches)
    }
    pub fn close_replica(&mut self, id: SubscriptionId) -> Result<(), Fault> {
        self.authority()?;
        self.authority.close_replica(id)
    }
    pub fn disconnect(&mut self, session: SessionId) -> Result<Vec<SubscriptionId>, Fault> {
        self.authority()?;
        let ids = self.authority.revoke_session(session);
        for id in &ids {
            self.authority.close_replica(*id)?;
        }
        Ok(ids)
    }
    pub fn receipt(&self, id: RequestId) -> Result<Option<&Receipt>, Fault> {
        let receipt = self.authority()?.receipt(id);
        if receipt.is_some_and(|r| r.reply.at.seq.0 > self.durability().through.0) {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "undurable request result",
            ));
        }
        Ok(receipt)
    }
    pub fn set_runtime_optimization(&mut self, enabled: bool) -> Result<(), Fault> {
        self.authority()?;
        self.authority.set_runtime_optimization(enabled);
        Ok(())
    }
    pub fn set_projection_optimization(&mut self, enabled: bool) -> Result<(), Fault> {
        self.authority()?;
        self.authority.set_projection_optimization(enabled);
        Ok(())
    }
    /// Finish one logical tick, including a recovered partial tick, within an
    /// explicit work budget. This never skips a tick or invokes future events.
    pub fn drive_tick(
        &mut self,
        sink: &mut impl InvalidationSink,
        max_steps: u32,
    ) -> Result<u32, Fault> {
        let target = self
            .authority()?
            .resume_point()
            .tick
            .0
            .checked_add(1)
            .ok_or_else(|| Fault::invalid_data("tick exhausted"))?;
        let mut steps = 0;
        while steps < max_steps {
            let batch = self.drive_batch(sink, self.commit_batch_limit.min(max_steps - steps))?;
            steps += batch.committed;
            if batch.idle {
                return Ok(steps);
            }
            if let Some(error) = batch.blocked
                && (!matches!(error, StageError::Busy) || batch.committed == 0)
            {
                return Err(Fault::resource_limit(
                    "journal blocked; tick remains recoverable",
                ));
            }
            let authority = self.authority()?;
            if authority.at_tick_boundary() && authority.resume_point().tick.0 == target {
                return Ok(steps);
            }
        }
        Err(Fault::resource_limit("tick step budget exhausted"))
    }
}

#[derive(Clone, Copy)]
pub struct NetworkLimits {
    pub peers: usize,
    pub requests_per_tick: u32,
    pub frames_per_poll: usize,
    pub outstanding: usize,
    pub subscriptions: usize,
    pub idle_ticks: u64,
    pub budget: OutputBudget,
    pub codec: CodecLimits,
}
/// Trusted application policy, constructed before an unauthenticated peer can
/// submit data. Authentication plugins may construct a different policy per peer.
pub struct PeerPolicy {
    pub actor: ActorContext,
    pub scope: ReplicaScope,
}
struct Peer<T> {
    transport: T,
    policy: PeerPolicy,
    viewer: Option<Viewer>,
    subscriptions: BTreeSet<u128>,
    waiting: BTreeSet<u64>,
    closed: bool,
    last_tick: Tick,
}
pub struct NetworkHub<T> {
    peers: BTreeMap<u128, Peer<T>>,
    next: u128,
    sessions: SessionRegistry,
    limits: NetworkLimits,
    binding: Binding,
}
impl<T: Transport> NetworkHub<T> {
    pub fn new(binding: Binding, limits: NetworkLimits) -> Result<Self, Fault> {
        if limits.frames_per_poll == 0
            || limits.outstanding == 0
            || limits.subscriptions == 0
            || limits.idle_ticks == 0
            || limits.budget.max_items == 0
            || limits.budget.max_bytes == 0
        {
            return Err(Fault::invalid_data("network limits"));
        }
        Ok(Self {
            peers: BTreeMap::new(),
            next: 1,
            sessions: SessionRegistry::new(limits.peers, limits.requests_per_tick)?,
            limits,
            binding,
        })
    }
    pub fn add(&mut self, transport: T, policy: PeerPolicy, tick: Tick) -> Result<u128, Fault> {
        if self.peers.len() >= self.limits.peers || self.next == u128::MAX {
            return Err(Fault::resource_limit("peer capacity"));
        }
        let id = self.next;
        self.next += 1;
        self.peers.insert(
            id,
            Peer {
                transport,
                policy,
                viewer: None,
                subscriptions: BTreeSet::new(),
                waiting: BTreeSet::new(),
                closed: false,
                last_tick: tick,
            },
        );
        Ok(id)
    }
    pub fn contains(&self, id: u128) -> bool {
        self.peers.contains_key(&id)
    }
    pub fn revoke<J: CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>(
        &mut self,
        server: &mut Server<ExecutionHost, J, S>,
        id: u128,
    ) -> Result<(), Fault> {
        if let Some(peer) = self.peers.get_mut(&id) {
            peer.closed = true;
            peer.transport.close();
        }
        self.cleanup(server)
    }
    fn send(
        peer: &mut Peer<T>,
        message: ServerMessage,
        limits: NetworkLimits,
    ) -> Result<(), Fault> {
        let mut bytes = Vec::new();
        wire::encode(&message, &mut bytes, limits.budget, limits.codec)?;
        peer.transport.send(bytes)
    }
    fn handle<J: CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>(
        peer: &mut Peer<T>,
        sessions: &mut SessionRegistry,
        server: &mut Server<ExecutionHost, J, S>,
        message: ClientMessage,
        limits: NetworkLimits,
    ) -> Result<(), Fault> {
        match message {
            ClientMessage::Hello {
                version,
                compatibility,
            } => {
                if peer.viewer.is_some()
                    || version != PROTOCOL_VERSION
                    || compatibility != server.binding().compatibility
                {
                    return Err(Fault::incompatible(
                        "protocol or authority content mismatch",
                    ));
                }
                let viewer = sessions.open()?;
                if let Err(e) = server.register_session(
                    viewer,
                    peer.policy.actor.clone(),
                    peer.policy.scope.clone(),
                ) {
                    sessions.close(viewer.session);
                    return Err(e);
                }
                peer.viewer = Some(viewer);
                Self::send(
                    peer,
                    ServerMessage::Welcome {
                        binding: server.binding(),
                        session: viewer.session,
                    },
                    limits,
                )
            }
            ClientMessage::Request { seq, call } => {
                let viewer = peer
                    .viewer
                    .ok_or_else(|| Fault::invalid_data("handshake required"))?;
                let tick = server.authority()?.resume_point().tick;
                let admitted = match sessions.check(viewer.session, seq, tick) {
                    Err(reason) => Admission::Rejected(reason),
                    Ok(())
                        if peer.waiting.len() >= limits.outstanding
                            && !peer.waiting.contains(&seq.0) =>
                    {
                        Admission::Rejected(RejectCode::QuotaExceeded)
                    }
                    Ok(()) => server.submit(WorldRequest {
                        id: RequestId {
                            world: server.binding().world,
                            epoch: server.binding().epoch,
                            session: viewer.session,
                            seq,
                        },
                        actor: viewer.actor,
                        input: call,
                    }),
                };
                if matches!(admitted, Admission::Queued { .. } | Admission::Duplicate) {
                    sessions.admitted(viewer.session, seq);
                    peer.waiting.insert(seq.0);
                }
                Self::send(
                    peer,
                    ServerMessage::Admission {
                        seq,
                        admission: admitted,
                    },
                    limits,
                )
            }
            ClientMessage::Subscribe { sections } => {
                let viewer = peer
                    .viewer
                    .ok_or_else(|| Fault::invalid_data("handshake required"))?;
                if peer.subscriptions.len() >= limits.subscriptions {
                    return Err(Fault::resource_limit("peer subscription capacity"));
                }
                let scope = ReplicaScope {
                    sections: sections.iter().copied().collect(),
                };
                if scope.sections.len() != sections.len()
                    || !scope.sections.is_subset(&peer.policy.scope.sections)
                {
                    return Self::send(
                        peer,
                        ServerMessage::Rejected(RejectCode::PermissionDenied),
                        limits,
                    );
                }
                let seed = server.open_replica(viewer.session, scope, limits.budget)?;
                peer.subscriptions.insert(seed.subscription.0);
                Self::send(peer, ServerMessage::Seed(seed), limits)
            }
            ClientMessage::Unsubscribe { subscription } => {
                if !peer.subscriptions.remove(&subscription.0) {
                    return Self::send(
                        peer,
                        ServerMessage::Rejected(RejectCode::PermissionDenied),
                        limits,
                    );
                }
                server.close_replica(subscription)
            }
            ClientMessage::Close => {
                peer.closed = true;
                peer.transport.close();
                Ok(())
            }
        }
    }
    pub fn poll<J: CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>(
        &mut self,
        server: &mut Server<ExecutionHost, J, S>,
    ) -> Result<(), Fault> {
        if server.binding() != self.binding {
            return Err(Fault::invalid_data("hub binding mismatch"));
        }
        let tick = server.authority()?.resume_point().tick;
        for peer in self.peers.values_mut() {
            if peer.closed {
                continue;
            }
            let result = (|| -> Result<(), Fault> {
                if tick.0.saturating_sub(peer.last_tick.0) > self.limits.idle_ticks {
                    return Err(Fault::invalid_data("idle peer"));
                }
                peer.transport.poll()?;
                for _ in 0..self.limits.frames_per_poll {
                    let Some(bytes) = peer.transport.receive()? else {
                        break;
                    };
                    let message = wire::decode(&bytes, self.limits.budget, self.limits.codec)?;
                    peer.last_tick = tick;
                    Self::handle(peer, &mut self.sessions, server, message, self.limits)?;
                    if peer.closed {
                        return Ok(());
                    }
                }
                if let Some(viewer) = peer.viewer {
                    let waiting: Vec<_> = peer.waiting.iter().copied().collect();
                    for seq in waiting {
                        let id = RequestId {
                            world: self.binding.world,
                            epoch: self.binding.epoch,
                            session: viewer.session,
                            seq: RequestSeq(seq),
                        };
                        if let Some(receipt) = server.receipt(id)? {
                            let reply = receipt.reply.clone();
                            Self::send(
                                peer,
                                ServerMessage::Result {
                                    seq: RequestSeq(seq),
                                    reply,
                                },
                                self.limits,
                            )?;
                            peer.waiting.remove(&seq);
                        } else if server.authority()?.expired(id) {
                            Self::send(
                                peer,
                                ServerMessage::Admission {
                                    seq: RequestSeq(seq),
                                    admission: Admission::Rejected(RejectCode::SequenceExpired),
                                },
                                self.limits,
                            )?;
                            peer.waiting.remove(&seq);
                        }
                    }
                    for id in peer.subscriptions.iter().copied().collect::<Vec<_>>() {
                        for batch in server.take_replica(SubscriptionId(id), self.limits.budget)? {
                            Self::send(peer, ServerMessage::Batch(batch), self.limits)?;
                        }
                    }
                }
                peer.transport.poll()
            })();
            if result.is_err() {
                peer.closed = true;
                peer.transport.close();
            }
        }
        self.cleanup(server)
    }
    fn cleanup<J: CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>(
        &mut self,
        server: &mut Server<ExecutionHost, J, S>,
    ) -> Result<(), Fault> {
        let dead: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, p)| p.closed)
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            let mut peer = self.peers.remove(&id).expect("peer exists");
            peer.transport.close();
            if let Some(viewer) = peer.viewer {
                server.disconnect(viewer.session)?;
                self.sessions.close(viewer.session);
                for subscription in peer.subscriptions {
                    server.close_replica(SubscriptionId(subscription))?;
                }
            }
        }
        Ok(())
    }
}
impl<T: Transport> InvalidationSink for NetworkHub<T> {
    fn invalidate(
        &mut self,
        binding: Binding,
        subscriptions: &[SubscriptionId],
    ) -> Result<(), Fault> {
        if binding != self.binding {
            return Err(Fault::invalid_data("invalidation binding"));
        }
        for peer in self.peers.values_mut() {
            if subscriptions
                .iter()
                .any(|id| peer.subscriptions.contains(&id.0))
            {
                peer.closed = true;
                peer.transport.close();
                peer.waiting.clear();
            }
        }
        Ok(())
    }
}
