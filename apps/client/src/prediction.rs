//! Detached walking prediction. Replica state is only ever read.
use std::collections::VecDeque;
use torse_protocol::PlayerUpdate;
use torse_sim::{
    WorldAuthority,
    player::{self, MoveInput},
};
use torse_types::{Fault, SessionId};
use torse_world::{EntityCollider, PlayerMotion, Position};

const MAX_PENDING: usize = 64;
#[derive(Default)]
pub(crate) struct Prediction {
    session: Option<SessionId>,
    update: Option<PlayerUpdate>,
    incoming: Option<PlayerUpdate>,
    sequence: u64,
    pending: VecDeque<MoveInput>,
    position: Option<Position>,
    presented: Option<Position>,
    motion: PlayerMotion,
}

impl Prediction {
    pub fn welcome(&mut self, session: SessionId) {
        *self = Self {
            session: Some(session),
            ..Default::default()
        };
    }
    pub fn position(&self, world: &WorldAuthority) -> Option<Position> {
        self.body(world)?;
        self.presented
    }
    pub fn entity(&self) -> Option<torse_types::EntityId> {
        self.update.map(|u| u.entity)
    }
    pub fn receive(&mut self, mut update: PlayerUpdate) {
        if self.session == Some(update.session)
            && self
                .incoming
                .is_none_or(|old| (update.tick, update.input_seq) >= (old.tick, old.input_seq))
        {
            update.reset |= self.incoming.is_some_and(|old| old.reset);
            self.incoming = Some(update);
        }
    }
    fn body(&self, world: &WorldAuthority) -> Option<[f64; 3]> {
        let id = self.update?.entity;
        let entity = world.entities().get(world.entities().handle_of(id)?)?;
        let section = entity.position.section(world.store().side())?;
        world.store().section(section)?;
        EntityCollider::of(entity).ok()??.half()
    }
    pub fn update(&mut self, update: PlayerUpdate, world: &WorldAuthority) -> Result<(), Fault> {
        if self.session != Some(update.session) {
            return Ok(());
        }
        if self
            .update
            .is_some_and(|old| update.tick < old.tick || update.input_seq < old.input_seq)
        {
            return Ok(());
        }
        let position = Position::from_bits(update.position_bits);
        let motion = PlayerMotion {
            vertical_micros: update.vertical_micros,
            grounded: update.grounded,
        };
        if update.entity.0 == 0
            || !position.is_finite()
            || update.input_seq > self.sequence
            || PlayerMotion::from_fields(&motion.fields()).is_err()
        {
            return Err(Fault::invalid_data("invalid authoritative player state"));
        }
        if update.reset || self.update.is_some_and(|old| old.entity != update.entity) {
            self.pending.clear();
        }
        self.update = Some(PlayerUpdate {
            reset: false,
            ..update
        });
        while self
            .pending
            .front()
            .is_some_and(|input| input.seq <= update.input_seq)
        {
            self.pending.pop_front();
        }
        self.motion = motion;
        self.position = Some(position);
        let Some(half) = self.body(world) else {
            self.position = None;
            self.presented = None;
            return Ok(());
        };
        let mut candidate = position;
        for input in &self.pending {
            match player::predict_step(world, candidate, half, self.motion, *input) {
                Ok((next, motion)) => {
                    candidate = next;
                    self.motion = motion;
                }
                Err(_) => {
                    self.position = None;
                    self.presented = None;
                    return Ok(());
                }
            }
        }
        self.position = Some(candidate);
        self.presented = Some(match self.presented {
            Some(previous) if !update.reset => {
                match player::reconcile(previous, candidate, player::RECONCILE_TOLERANCE) {
                    player::Correction::Keep { .. } => previous,
                    player::Correction::Snap(next) => next,
                }
            }
            _ => candidate,
        });
        Ok(())
    }
    pub fn refresh(&mut self, world: &WorldAuthority) -> Result<(), Fault> {
        if let Some(update) = self.incoming.take().or(self.update) {
            self.update(update, world)?;
        }
        Ok(())
    }
    pub fn step(
        &mut self,
        world: &WorldAuthority,
        wish: [i8; 2],
        jump: bool,
    ) -> Result<Option<(SessionId, MoveInput)>, Fault> {
        if self.pending.len() >= MAX_PENDING {
            return Ok(None);
        }
        let (Some(session), Some(position), Some(half)) =
            (self.session, self.position, self.body(world))
        else {
            return Ok(None);
        };
        let seq = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Fault::resource_limit("movement sequence exhausted"))?;
        let input = MoveInput {
            seq,
            wish: [wish[0], 0, wish[1]],
            jump,
        };
        let Ok((next, motion)) = player::predict_step(world, position, half, self.motion, input)
        else {
            self.position = None;
            self.presented = None;
            return Ok(None);
        };
        self.position = Some(next);
        self.presented = Some(next);
        self.motion = motion;
        self.sequence = seq;
        self.pending.push_back(input);
        Ok(Some((session, input)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torse_content::{compile, m0_fixture_pack};
    use torse_types::*;
    use torse_world::*;
    fn world() -> (WorldAuthority, EntityId) {
        let content = compile(m0_fixture_pack()).unwrap();
        let registry = BlockStateRegistry::build(&content).unwrap();
        let schemas = ComponentSchemas::build(&content);
        let mut world = WorldAuthority::new(
            WorldId(1),
            registry,
            schemas.clone(),
            16,
            WorldBounds::new(0, 32).unwrap(),
            Compatibility {
                authority_schema: 1,
                rules_digest: content.authority_digest,
                content_digest: content.presentation_digest,
            },
        )
        .unwrap();
        let stone = world
            .registry()
            .default_state(&ContentKey::parse("base:stone").unwrap())
            .unwrap();
        let mut section = Section::empty(16).unwrap();
        for x in 0..16 {
            for z in 0..16 {
                section.set(LocalPos { x, y: 0, z }, stone).unwrap();
            }
        }
        world.store_mut().request(SectionPos::ZERO).unwrap();
        world
            .store_mut()
            .install(SectionPos::ZERO, section)
            .unwrap();
        world.store_mut().finish_load(SectionPos::ZERO).unwrap();
        let handle = world
            .entities_mut()
            .spawn(Position::new(8.0, 1.900001, 8.0))
            .unwrap();
        world
            .entities_mut()
            .components_mut(handle)
            .unwrap()
            .insert(
                &schemas,
                &collider_schema(),
                EntityCollider::Box {
                    half: [300_000, 900_000, 300_000],
                }
                .fields(),
            )
            .unwrap();
        let id = world.entities().get(handle).unwrap().id;
        (world, id)
    }
    fn update(id: EntityId) -> PlayerUpdate {
        PlayerUpdate {
            session: SessionId(1),
            entity: id,
            input_seq: 0,
            tick: Tick(0),
            position_bits: Position::new(8.0, 1.900001, 8.0).to_bits(),
            vertical_micros: 0,
            grounded: true,
            reset: false,
        }
    }
    #[test]
    fn delayed_confirmation_replays_pending_intent_without_mutating_the_mirror() {
        let (world, id) = world();
        let meta = torse_sim::streaming::page_meta(&world);
        let snapshot = world.capture_snapshot(meta).unwrap();
        let mut prediction = Prediction::default();
        prediction.welcome(SessionId(1));
        prediction.update(update(id), &world).unwrap();
        for _ in 0..3 {
            prediction.step(&world, [127, 0], false).unwrap().unwrap();
        }
        let expected = prediction.position(&world).unwrap();
        prediction
            .update(
                PlayerUpdate {
                    input_seq: 1,
                    tick: Tick(1),
                    position_bits: Position::new(8.215, 1.900001, 8.0).to_bits(),
                    ..update(id)
                },
                &world,
            )
            .unwrap();
        assert_eq!(prediction.pending.len(), 2);
        assert!((prediction.position(&world).unwrap().x - expected.x).abs() < 1e-8);
        prediction
            .update(
                PlayerUpdate {
                    input_seq: 1,
                    tick: Tick(2),
                    reset: true,
                    position_bits: Position::new(5.0, 1.900001, 8.0).to_bits(),
                    ..update(id)
                },
                &world,
            )
            .unwrap();
        assert!(prediction.pending.is_empty());
        assert_eq!(prediction.position(&world).unwrap().x, 5.0);
        assert_eq!(
            prediction
                .step(&world, [127, 0], false)
                .unwrap()
                .unwrap()
                .1
                .seq,
            4
        );
        assert_eq!(world.capture_snapshot(meta).unwrap(), snapshot);
    }
    #[test]
    fn prediction_history_is_bounded_and_unsubscribed_or_stale_players_cannot_advance() {
        let (mut world, id) = world();
        let mut prediction = Prediction::default();
        prediction.welcome(SessionId(1));
        prediction.update(update(id), &world).unwrap();
        for _ in 0..MAX_PENDING {
            assert!(prediction.step(&world, [0, 0], false).unwrap().is_some());
        }
        assert!(prediction.step(&world, [0, 0], false).unwrap().is_none());
        assert_eq!(prediction.pending.len(), MAX_PENDING);
        let confirmed = PlayerUpdate {
            input_seq: MAX_PENDING as u64,
            tick: Tick(4),
            ..update(id)
        };
        prediction.update(confirmed, &world).unwrap();
        prediction
            .update(
                PlayerUpdate {
                    tick: Tick(3),
                    position_bits: Position::new(2.0, 2.0, 2.0).to_bits(),
                    ..update(id)
                },
                &world,
            )
            .unwrap();
        assert_eq!(prediction.update.unwrap().tick, Tick(4));
        world.store_mut().unload(SectionPos::ZERO).unwrap();
        assert!(prediction.position(&world).is_none());
        assert!(prediction.step(&world, [127, 0], true).unwrap().is_none());
        prediction.welcome(SessionId(2));
        prediction.receive(confirmed);
        prediction.refresh(&world).unwrap();
        assert!(prediction.update.is_none());
        assert_eq!(prediction.sequence, 0);
    }
}
