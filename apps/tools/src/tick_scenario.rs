//! Headless tick scenario: systems, contention, entities, and overrun reporting.
//!
//! The scenario registers two systems that both want to fill the same cell and
//! runs several ticks through [`torse_sim::Simulation`]. It also exercises the
//! entity store by spawning an entity, moving it across a section boundary, and
//! checking the spatial index.
//!
//! This is the first scenario that uses the tick driver instead of calling the
//! coordinator directly, so it covers registration order, per-phase collection,
//! commit accounting, and clock advances together.

use std::cell::RefCell;
use std::rc::Rc;

use torse_content::{compile, m0_fixture_pack};
use torse_sim::{
    OrderingKey, Phase, Simulation, SimulationLimits, System, SystemContext, SystemSpec,
    Transaction,
};
use torse_types::{BlockPos, ContentKey, SectionPos, TransactionId, WorldId};
use torse_world::{
    BlockStateId, BlockStateRegistry, EntityStore, Position, Section, SectionStore, TerrainQuery,
    WorldBounds,
};

/// Result counters reported by the scenario.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickScenarioReport {
    /// Ticks executed.
    pub ticks: u64,
    /// Transactions that committed across all ticks.
    pub committed: usize,
    /// Transactions that were rejected.
    pub rejected: usize,
    /// Final commit sequence.
    pub commit_seq: u64,
    /// Ticks whose measured duration exceeded the budget.
    pub overruns: u64,
    /// Live entities after the scenario.
    pub entities: usize,
    /// Entities indexed in the destination section after the move.
    pub moved_section_entities: usize,
    /// State of the contested cell after the run.
    pub contested_state: u16,
}

/// Failure produced by the scenario.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickScenarioError {
    /// Human-readable explanation.
    pub detail: String,
}

impl TickScenarioError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for TickScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for TickScenarioError {}

/// System that fills one cell when it still holds the expected state.
struct Contender {
    spec: SystemSpec,
    target: BlockPos,
    expected: BlockStateId,
    write: BlockStateId,
    transaction: u128,
    log: Rc<RefCell<Vec<String>>>,
}

impl System for Contender {
    fn spec(&self) -> &SystemSpec {
        &self.spec
    }

    fn run(&mut self, context: &mut SystemContext<'_>) {
        self.log.borrow_mut().push(self.spec.id.to_string());
        let matching = matches!(context.query(self.target), TerrainQuery::Ready(state) if state == self.expected);
        if !matching {
            return;
        }
        let ordering = OrderingKey {
            phase: self.spec.phase,
            rule_priority: self.spec.rule_priority,
            stable_task_key: 0,
            local_sequence: 0,
        };
        let mut transaction = Transaction::new(
            TransactionId(self.transaction),
            self.spec.id.clone(),
            ordering,
        );
        transaction
            .read_block(self.target, self.expected)
            .write_block(self.target, self.write);
        context.push(transaction);
    }
}

/// Run the tick scenario.
pub fn run() -> Result<TickScenarioReport, TickScenarioError> {
    let content = compile(m0_fixture_pack())
        .map_err(|errors| TickScenarioError::new(format!("content failed: {errors:?}")))?;
    let registry = BlockStateRegistry::build(&content)
        .map_err(|error| TickScenarioError::new(format!("registry failed: {error}")))?;
    let stone = registry
        .default_state(&key("base:stone"))
        .ok_or_else(|| TickScenarioError::new("missing base:stone"))?;
    let glass = registry
        .default_state(&key("base:glass_slab"))
        .ok_or_else(|| TickScenarioError::new("missing base:glass_slab"))?;

    let bounds = WorldBounds::new(0, 64)
        .map_err(|error| TickScenarioError::new(format!("bounds failed: {error}")))?;
    let mut store = SectionStore::new(16, bounds)
        .map_err(|error| TickScenarioError::new(format!("store failed: {error}")))?;
    for section in [SectionPos::ZERO, SectionPos::new(1, 0, 0)] {
        store
            .request(section)
            .map_err(|error| TickScenarioError::new(format!("request failed: {error}")))?;
        store
            .install(
                section,
                Section::empty(16)
                    .map_err(|error| TickScenarioError::new(format!("section failed: {error}")))?,
            )
            .map_err(|error| TickScenarioError::new(format!("install failed: {error}")))?;
        store
            .finish_load(section)
            .map_err(|error| TickScenarioError::new(format!("finish failed: {error}")))?;
    }

    let mut entities = EntityStore::new(16)
        .map_err(|error| TickScenarioError::new(format!("entity store failed: {error}")))?;
    let player = entities
        .spawn(Position::new(15.5, 2.0, 0.5))
        .map_err(|error| TickScenarioError::new(format!("spawn failed: {error}")))?;
    entities
        .move_to(player, Position::new(16.5, 2.0, 0.5))
        .map_err(|error| TickScenarioError::new(format!("move failed: {error}")))?;

    let log = Rc::new(RefCell::new(Vec::new()));
    let target = BlockPos::new(3, 1, 3);
    let mut simulation = Simulation::new(WorldId(7), 50, SimulationLimits::default());
    simulation
        .register(Box::new(Contender {
            spec: spec("base:first"),
            target,
            expected: BlockStateId::EMPTY,
            write: stone,
            transaction: 1,
            log: Rc::clone(&log),
        }))
        .map_err(|error| TickScenarioError::new(format!("register failed: {error}")))?;
    simulation
        .register(Box::new(Contender {
            spec: SystemSpec {
                rule_priority: 5,
                ..spec("base:second")
            },
            target,
            expected: BlockStateId::EMPTY,
            write: glass,
            transaction: 2,
            log: Rc::clone(&log),
        }))
        .map_err(|error| TickScenarioError::new(format!("register failed: {error}")))?;

    let mut report = TickScenarioReport::default();
    // Three ticks: the first commits the winner and rejects the loser, the next
    // two ticks see a filled cell and propose nothing.
    for index in 0..3 {
        let elapsed = if index == 1 { 80 } else { 20 };
        let outcome = simulation
            .tick(&mut store, &entities, elapsed)
            .map_err(|error| TickScenarioError::new(format!("tick failed: {error}")))?;
        report.ticks = outcome.tick.0 + 1;
        report.committed += outcome.committed;
        report.rejected += outcome.rejected;
        report.overruns = outcome.report.overruns;
    }

    report.commit_seq = simulation.seq().0;
    report.entities = entities.len();
    report.moved_section_entities = entities
        .entities_in_section(SectionPos::new(1, 0, 0))
        .count();
    report.contested_state = match store.query(target) {
        TerrainQuery::Ready(state) => state.0,
        other => {
            return Err(TickScenarioError::new(format!(
                "contested cell is not readable: {other:?}"
            )));
        }
    };

    if report.ticks != 3 {
        return Err(TickScenarioError::new(
            "the driver must advance one tick per call",
        ));
    }
    if report.committed != 1 {
        return Err(TickScenarioError::new("exactly one contender may commit"));
    }
    if report.rejected != 1 {
        return Err(TickScenarioError::new("the loser must be rejected once"));
    }
    if report.commit_seq != 1 {
        return Err(TickScenarioError::new(
            "rejected work must not consume a sequence",
        ));
    }
    if report.contested_state != stone.0 {
        return Err(TickScenarioError::new("the winner's state must survive"));
    }
    if report.overruns != 1 {
        return Err(TickScenarioError::new("one tick must report an overrun"));
    }
    if report.moved_section_entities != 1 {
        return Err(TickScenarioError::new(
            "the entity move must update the section index",
        ));
    }
    Ok(report)
}

fn spec(id: &str) -> SystemSpec {
    SystemSpec {
        id: key(id),
        phase: Phase::Simulation,
        rule_priority: 1,
        before: Vec::new(),
        after: Vec::new(),
    }
}

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("scenario keys are valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_scenario_counts_are_stable() {
        let report = run().expect("scenario completes");
        assert_eq!(report.ticks, 3);
        assert_eq!(report.committed, 1);
        assert_eq!(report.rejected, 1);
        assert_eq!(report.commit_seq, 1);
        assert_eq!(report.overruns, 1);
        assert_eq!(report.entities, 1);
        assert_eq!(report.moved_section_entities, 1);
    }
}
