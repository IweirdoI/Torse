//! Independent native movement tasks shared with semantic regression tests.
use torse_server::{bootstrap as base, execution::*};
use torse_sim::{OrderingKey, Phase, SystemSpec, Transaction, WorldAuthority};
use torse_types::*;
use torse_world::Position;

#[derive(Clone, Copy)]
pub struct Motion {
    pub tasks: usize,
    pub half: [f64; 3],
    pub contended: bool,
    pub mixed: bool,
    pub deny_plan: bool,
}
impl Motion {
    pub fn world(self) -> WorldAuthority {
        let (mut world, _) = base::world().unwrap();
        base::fill_floor(&mut world).unwrap();
        for _ in 0..self.tasks {
            world
                .entities_mut()
                .spawn(Position::new(14.0, 8.0, 8.0))
                .unwrap();
        }
        world
    }
    pub fn rules(self) -> Vec<Box<dyn DurableRule>> {
        vec![Box::new(self)]
    }
}
impl DurableRule for Motion {
    fn spec(&self) -> SystemSpec {
        SystemSpec {
            id: base::key("movement:baseline"),
            phase: Phase::Simulation,
            rule_priority: 0,
            before: vec![],
            after: vec![],
        }
    }
    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        assert!(!self.deny_plan, "recovery must consume frozen tasks");
        // The bootstrap's two inventory entities are outside this motion load.
        let entities: Vec<_> = world.entities().iter().skip(2).collect();
        let mut plans = vec![];
        for index in 0..self.tasks {
            let (handle, entity) = entities[if self.contended { index / 2 } else { index }];
            let mut tx = Transaction::new(
                TransactionId(u128::from(tick.0) * 256 + index as u128 + 1),
                self.spec().id,
                OrderingKey {
                    phase: Phase::Simulation,
                    rule_priority: 0,
                    stable_task_key: index as u64,
                    local_sequence: 0,
                },
            );
            tx.move_entity(
                handle,
                entity.id,
                entity.position,
                [if tick.0.is_multiple_of(2) { 4.0 } else { -4.0 }, -4.0, 0.0],
                self.half,
            );
            if self.mixed && index == self.tasks / 2 {
                let pos = BlockPos::new(10, 1, 8);
                let previous = world.store().query(pos).ready().unwrap();
                let stone = world
                    .registry()
                    .default_state(&base::key("base:stone"))
                    .unwrap();
                tx.read_block(pos, previous).write_block(pos, stone);
            }
            plans.push(WorkPlan::transaction(tx));
        }
        Ok(plans)
    }
}
