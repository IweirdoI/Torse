//! Trusted player bindings and durable fixed-tick movement planning.
use super::*;
use torse_sim::action::ActionTarget;
use torse_sim::player::{MoveInput, plan_step};
use torse_world::{ComponentValue, PlayerMotion, Position};

pub(super) struct PlayerSession {
    pub parent: SessionId,
    pub entity: EntityId,
    pub last_tick: Option<Tick>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerState {
    pub entity: EntityId,
    pub position: Position,
    pub motion: PlayerMotion,
    pub tick: Tick,
}
fn key(name: &str) -> ContentKey {
    ContentKey::parse(name).expect("static player control key")
}
pub fn movement_action() -> ContentKey {
    key("core:action/player-step")
}

/// Only the host-created movement session may submit this reserved action.
pub fn movement_call(entity: EntityId, input: MoveInput) -> Call {
    Call {
        action: movement_action(),
        version: 1,
        target: ActionTarget::Entity(entity),
        parameters: [
            (
                key("core:field/input-seq"),
                ComponentValue::I64(input.seq as i64),
            ),
            (
                key("core:field/wish-x"),
                ComponentValue::I64(i64::from(input.wish[0])),
            ),
            (
                key("core:field/wish-z"),
                ComponentValue::I64(i64::from(input.wish[2])),
            ),
            (key("core:field/jump"), ComponentValue::Bool(input.jump)),
        ]
        .into(),
    }
}
impl ExecutionHost {
    /// Resident players pin their durable sections independently of interest
    /// admission. Unbound players remain available for a later connection.
    pub(crate) fn player_sections(&self) -> std::collections::BTreeSet<SectionPos> {
        self.world()
            .entities()
            .iter()
            .filter_map(|(_, entity)| {
                PlayerMotion::of(entity).ok()??;
                entity.position.section(self.world().store().side())
            })
            .collect()
    }

    /// Bind an existing host-created player to a parent session. The private child
    /// session carries durable receipts without consuming the parent's request sequence.
    pub fn bind_player(
        &mut self,
        parent: SessionId,
        viewer: Viewer,
        entity: EntityId,
    ) -> Result<(), Fault> {
        if self
            .players
            .values()
            .any(|p| p.entity == entity || p.parent == parent)
            || self.player_state_at(parent, entity).is_none()
        {
            return Err(Fault::invalid_data("player binding unavailable"));
        }
        let actor = ActorContext::new(key("core:actor/player-control"));
        self.register_session(
            viewer,
            actor,
            ReplicaScope {
                sections: Default::default(),
            },
        )?;
        self.players.insert(
            viewer.session.0,
            PlayerSession {
                parent,
                entity,
                last_tick: None,
            },
        );
        Ok(())
    }
    pub fn player_state(&self, parent: SessionId) -> Option<PlayerState> {
        let player = self.players.values().find(|p| p.parent == parent)?;
        self.player_state_at(parent, player.entity)
    }
    fn player_state_at(&self, parent: SessionId, id: EntityId) -> Option<PlayerState> {
        let live = self.sessions.get(&parent.0)?;
        let handle = self.world().entities().handle_of(id)?;
        let entity = self.world().entities().get(handle)?;
        let section = entity.position.section(self.world().store().side())?;
        if !live.scope.sections.contains(&section)
            || torse_world::EntityCollider::of(entity)
                .ok()??
                .half()
                .is_none()
        {
            return None;
        }
        Some(PlayerState {
            entity: id,
            position: entity.position,
            motion: PlayerMotion::of(entity).ok()??,
            tick: self.resume_point().tick,
        })
    }
    fn movement_owner(
        &self,
        id: RequestId,
        target: ActionTarget,
    ) -> Result<&PlayerSession, RejectCode> {
        if id.epoch != self.binding.epoch {
            return Err(RejectCode::StaleSession);
        }
        let player = self
            .players
            .get(&id.session.0)
            .ok_or(RejectCode::StaleSession)?;
        if target != ActionTarget::Entity(player.entity)
            || !self.sessions.contains_key(&player.parent.0)
        {
            return Err(RejectCode::PermissionDenied);
        }
        Ok(player)
    }
    fn check_player_scope(
        &self,
        player: &PlayerSession,
        tx: &Transaction,
    ) -> Result<(), RejectCode> {
        let scope = &self
            .sessions
            .get(&player.parent.0)
            .ok_or(RejectCode::StaleSession)?
            .scope
            .sections;
        let side = self.world().store().side();
        if tx.preconditions.iter().any(|read| {
            read.pos
                .to_section(side)
                .map_or(true, |(pos, _)| !scope.contains(&pos))
        }) || tx.movements.iter().any(|m| {
            let previous = Position::from_bits(m.previous_bits);
            let target = previous.translated(m.delta_bits.map(f64::from_bits));
            [previous, target]
                .iter()
                .any(|p| p.section(side).is_none_or(|s| !scope.contains(&s)))
        }) {
            return Err(RejectCode::PermissionDenied);
        }
        Ok(())
    }
    pub(super) fn plan_movement(
        &self,
        request: &WorldRequest<Call>,
    ) -> Result<Transaction, RejectCode> {
        let player = self.movement_owner(request.id, request.input.target)?;
        let fields = &request.input.parameters;
        let signed = |name| match fields.get(&key(name)) {
            Some(ComponentValue::I64(value)) => Ok(*value),
            _ => Err(RejectCode::InvalidData),
        };
        let Some(ComponentValue::Bool(jump)) = fields.get(&key("core:field/jump")) else {
            return Err(RejectCode::InvalidData);
        };
        if request.input.version != 1 || fields.len() != 4 {
            return Err(RejectCode::InvalidData);
        }
        let input = MoveInput {
            seq: signed("core:field/input-seq")? as u64,
            wish: [
                i8::try_from(signed("core:field/wish-x")?).map_err(|_| RejectCode::InvalidData)?,
                0,
                i8::try_from(signed("core:field/wish-z")?).map_err(|_| RejectCode::InvalidData)?,
            ],
            jump: *jump,
        };
        let handle = self
            .world()
            .entities()
            .handle_of(player.entity)
            .ok_or(RejectCode::MissingTarget)?;
        let tx = plan_step(
            self.world(),
            handle,
            input,
            TransactionId(request.id.seq.0 as u128),
            torse_sim::OrderingKey {
                phase: Phase::Simulation,
                rule_priority: 0,
                stable_task_key: request.id.session.0 as u64,
                local_sequence: request.id.seq.0 as u32,
            },
        )?;
        self.check_player_scope(player, &tx)?;
        Ok(tx)
    }
    pub(super) fn check_frozen_movement(&self, task: &FrozenTask) -> Result<(), RejectCode> {
        let Some(request) = task
            .request
            .as_ref()
            .filter(|r| r.input.action == movement_action())
        else {
            return Ok(());
        };
        let player = self.movement_owner(request.id, request.input.target)?;
        if let Some(stored) = &task.transaction {
            for tx in stored.restore(self.world())?.members {
                self.check_player_scope(player, &tx)?;
            }
        }
        Ok(())
    }
}
