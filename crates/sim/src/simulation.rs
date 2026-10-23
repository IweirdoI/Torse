//! Tick driver: ordered systems, one commit boundary per phase.
//!
//! The driver deliberately runs systems serially in M1. Every system sees the
//! state committed at the end of the previous phase, and its transactions are
//! committed before the next phase starts. That is a coarser visibility rule than
//! the final design allows, but it is a correct one, and it gives parallel
//! execution something to compare against later.

use torse_types::{
    CommitSeq, ContentKey, ResumePoint, Tick, TransactionOutcome, TransactionResult, WorldId,
};
use torse_world::{EntityStore, SectionStore};

use crate::clock::{TickClock, TickReport};
use crate::coordinator::{CommitCoordinator, CommitFault, Committed};
use crate::phase::{OrderError, Phase, SystemSpec, order_phase};
use crate::system::{System, SystemContext};
use crate::transaction::Transaction;

/// Per-phase transaction limit.
pub const DEFAULT_PHASE_TRANSACTION_LIMIT: usize = 4_096;

/// Execution policy.
#[derive(Clone, Copy, Debug)]
pub struct SimulationLimits {
    /// Maximum transactions a single phase may produce.
    pub phase_transaction_limit: usize,
}

impl Default for SimulationLimits {
    fn default() -> Self {
        Self {
            phase_transaction_limit: DEFAULT_PHASE_TRANSACTION_LIMIT,
        }
    }
}

/// Result of one tick.
#[derive(Clone, Debug)]
pub struct TickOutcome {
    /// Tick that was executed.
    pub tick: Tick,
    /// Transactions that committed.
    pub committed: usize,
    /// Transactions that were rejected.
    pub rejected: usize,
    /// Per-transaction results in commit order.
    pub results: Vec<TransactionResult<Committed>>,
    /// Clock measurement and overrun state.
    pub report: TickReport,
}

/// Failure produced while building or running the simulation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SimulationError {
    /// System declarations could not be ordered.
    Order(OrderError),
    /// A system emitted a transaction for another phase.
    PhaseMismatch {
        /// Declared system.
        system: ContentKey,
        /// Phase the driver was executing.
        expected: Phase,
        /// Phase recorded on the transaction.
        actual: Phase,
    },
    /// A phase produced more transactions than the limit allows.
    TransactionQuota {
        /// Phase that exceeded its limit.
        phase: Phase,
        /// Configured limit.
        limit: usize,
    },
    /// The commit coordinator reported a terminal fault.
    Commit(CommitFault),
    /// The tick counter is exhausted.
    TickExhausted,
}

impl std::fmt::Display for SimulationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Order(error) => write!(f, "{error}"),
            Self::PhaseMismatch {
                system,
                expected,
                actual,
            } => write!(
                f,
                "system {system} emitted a {actual:?} transaction during {expected:?}"
            ),
            Self::TransactionQuota { phase, limit } => {
                write!(f, "phase {phase:?} exceeded {limit} transactions")
            }
            Self::Commit(fault) => write!(f, "{fault}"),
            Self::TickExhausted => write!(f, "tick counter exhausted"),
        }
    }
}

impl std::error::Error for SimulationError {}

/// Ordered systems and the commit boundary that follows them.
pub struct Simulation {
    world: WorldId,
    clock: TickClock,
    coordinator: CommitCoordinator,
    systems: Vec<Box<dyn System>>,
    by_phase: [Vec<usize>; 6],
    limits: SimulationLimits,
}

impl Simulation {
    /// Create a simulation with no systems.
    pub fn new(world: WorldId, budget_ms: u64, limits: SimulationLimits) -> Self {
        Self {
            world,
            clock: TickClock::new(budget_ms),
            coordinator: CommitCoordinator::new(world),
            systems: Vec::new(),
            by_phase: [const { Vec::new() }; 6],
            limits,
        }
    }

    /// World identity of this simulation.
    pub const fn world(&self) -> WorldId {
        self.world
    }

    /// Clock state.
    pub const fn clock(&self) -> &TickClock {
        &self.clock
    }

    /// Last committed sequence.
    pub const fn seq(&self) -> CommitSeq {
        self.coordinator.seq()
    }

    /// Register a system and rebuild the execution order.
    ///
    /// A rejected registration leaves the previous order intact.
    pub fn register(&mut self, system: Box<dyn System>) -> Result<(), SimulationError> {
        self.systems.push(system);
        match self.rebuild_order() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.systems.pop();
                let _ = self.rebuild_order();
                Err(SimulationError::Order(error))
            }
        }
    }

    /// Execute one tick and commit its transactions.
    pub fn tick(
        &mut self,
        store: &mut SectionStore,
        entities: &EntityStore,
        elapsed_ms: u64,
    ) -> Result<TickOutcome, SimulationError> {
        let tick = self.clock.current();
        let mut results = Vec::new();
        let mut rejected = 0usize;

        for phase in Phase::ALL {
            let order = self.by_phase[phase.index()].clone();
            let mut collected: Vec<(ContentKey, Transaction)> = Vec::new();
            for index in order {
                let system = &mut self.systems[index];
                let system_id = system.spec().id.clone();
                let mut context =
                    SystemContext::new(tick, phase, system_id.clone(), store, entities);
                system.run(&mut context);
                for transaction in context.into_transactions() {
                    collected.push((system_id.clone(), transaction));
                }
            }

            if collected.len() > self.limits.phase_transaction_limit {
                return Err(SimulationError::TransactionQuota {
                    phase,
                    limit: self.limits.phase_transaction_limit,
                });
            }

            let mut phase_transactions = Vec::with_capacity(collected.len());
            for (system, transaction) in collected {
                if transaction.ordering.phase != phase {
                    return Err(SimulationError::PhaseMismatch {
                        system,
                        expected: phase,
                        actual: transaction.ordering.phase,
                    });
                }
                phase_transactions.push(transaction);
            }

            // The resume point recorded by these commits is "this phase, first
            // step": after a crash the world replays this phase from the state
            // committed at the end of the previous phase.
            self.coordinator.set_resume_point(ResumePoint {
                tick,
                next_phase: phase.index_u16(),
                next_step: 0,
            });

            let phase_results = self
                .coordinator
                .commit_ordered(store, phase_transactions)
                .map_err(SimulationError::Commit)?;
            for result in &phase_results {
                if matches!(result.outcome, TransactionOutcome::Rejected(_)) {
                    rejected += 1;
                }
            }
            results.extend(phase_results);
        }

        let report = self.clock.finish(elapsed_ms);
        let next_tick = self.clock.advance().ok_or(SimulationError::TickExhausted)?;
        // After the publish phase the next authority work is the input phase of
        // the following tick.
        self.coordinator.set_resume_point(ResumePoint {
            tick: next_tick,
            next_phase: Phase::Input.index_u16(),
            next_step: 0,
        });
        Ok(TickOutcome {
            tick,
            committed: results.len() - rejected,
            rejected,
            results,
            report,
        })
    }

    /// Execution order of registered systems as indices.
    pub fn order(&self) -> Vec<ContentKey> {
        let mut order = Vec::with_capacity(self.systems.len());
        for phase in Phase::ALL {
            for index in &self.by_phase[phase.index()] {
                order.push(self.systems[*index].spec().id.clone());
            }
        }
        order
    }

    fn rebuild_order(&mut self) -> Result<(), OrderError> {
        let mut by_phase: [Vec<usize>; 6] = [const { Vec::new() }; 6];
        for phase in Phase::ALL {
            let specs: Vec<SystemSpec> = self
                .systems
                .iter()
                .filter(|system| system.spec().phase == phase)
                .map(|system| system.spec().clone())
                .collect();
            let ordered = order_phase(&specs)?;
            for id in ordered {
                if let Some(index) = self
                    .systems
                    .iter()
                    .position(|system| system.spec().id == id)
                {
                    by_phase[phase.index()].push(index);
                }
            }
        }
        self.by_phase = by_phase;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use torse_types::SectionPos;
    use torse_types::{BlockPos, TransactionId};
    use torse_world::{BlockStateId, Section, TerrainQuery, WorldBounds};

    use super::*;
    use crate::transaction::OrderingKey;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key is valid")
    }

    fn store() -> SectionStore {
        let bounds = WorldBounds::new(0, 64).expect("bounds are valid");
        let mut store = SectionStore::new(16, bounds).expect("side is valid");
        store
            .request(SectionPos::ZERO)
            .expect("request is accepted");
        store
            .install(SectionPos::ZERO, Section::empty(16).expect("side is valid"))
            .expect("install is accepted");
        store.finish_load(SectionPos::ZERO).expect("load finishes");
        store
    }

    fn entities() -> EntityStore {
        EntityStore::new(16).expect("side is valid")
    }

    /// System that appends its identity to a shared log and optionally writes a
    /// block when the target still holds the expected state.
    struct Writer {
        spec: SystemSpec,
        log: Rc<RefCell<Vec<String>>>,
        target: BlockPos,
        expected: BlockStateId,
        write: Option<BlockStateId>,
        id: u128,
    }

    impl System for Writer {
        fn spec(&self) -> &SystemSpec {
            &self.spec
        }

        fn run(&mut self, context: &mut SystemContext<'_>) {
            self.log.borrow_mut().push(self.spec.id.to_string());
            let current = context.query(self.target);
            let matching = matches!(current, TerrainQuery::Ready(state) if state == self.expected);
            if !matching {
                return;
            }
            let Some(state) = self.write else {
                return;
            };
            let ordering = OrderingKey {
                phase: self.spec.phase,
                rule_priority: self.spec.rule_priority,
                stable_task_key: 0,
                local_sequence: 0,
            };
            let mut transaction =
                Transaction::new(TransactionId(self.id), self.spec.id.clone(), ordering);
            transaction
                .read_block(self.target, self.expected)
                .write_block(self.target, state);
            context.push(transaction);
        }
    }

    fn writer(
        id: &str,
        priority: i32,
        log: &Rc<RefCell<Vec<String>>>,
        target: BlockPos,
        write: Option<BlockStateId>,
        transaction_id: u128,
    ) -> Box<dyn System> {
        Box::new(Writer {
            spec: SystemSpec {
                id: key(id),
                phase: Phase::Simulation,
                rule_priority: priority,
                before: Vec::new(),
                after: Vec::new(),
            },
            log: Rc::clone(log),
            target,
            expected: BlockStateId::EMPTY,
            write,
            id: transaction_id,
        })
    }

    #[test]
    fn systems_run_in_priority_order_and_commit_the_writes() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut simulation = Simulation::new(WorldId(1), 50, SimulationLimits::default());
        simulation
            .register(writer(
                "base:second",
                5,
                &log,
                BlockPos::new(1, 1, 1),
                Some(BlockStateId(4)),
                2,
            ))
            .expect("registration succeeds");
        simulation
            .register(writer(
                "base:first",
                1,
                &log,
                BlockPos::new(2, 1, 1),
                Some(BlockStateId(3)),
                1,
            ))
            .expect("registration succeeds");

        let mut store = store();
        let entities = entities();
        let outcome = simulation
            .tick(&mut store, &entities, 10)
            .expect("tick runs");
        assert_eq!(outcome.committed, 2);
        assert_eq!(outcome.rejected, 0);
        assert_eq!(outcome.tick, Tick(0));
        assert_eq!(simulation.seq(), CommitSeq(2));
        assert_eq!(
            log.borrow().as_slice(),
            &["base:first".to_owned(), "base:second".to_owned()]
        );
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId(4))
        );
    }

    #[test]
    fn two_systems_contending_for_one_cell_commit_once() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut simulation = Simulation::new(WorldId(1), 50, SimulationLimits::default());
        simulation
            .register(writer(
                "base:a",
                1,
                &log,
                BlockPos::new(3, 1, 3),
                Some(BlockStateId(5)),
                1,
            ))
            .expect("registration succeeds");
        simulation
            .register(writer(
                "base:b",
                2,
                &log,
                BlockPos::new(3, 1, 3),
                Some(BlockStateId(7)),
                2,
            ))
            .expect("registration succeeds");

        let mut store = store();
        let entities = entities();
        let outcome = simulation
            .tick(&mut store, &entities, 10)
            .expect("tick runs");
        assert_eq!(outcome.committed, 1);
        assert_eq!(outcome.rejected, 1);
        assert_eq!(simulation.seq(), CommitSeq(1));
        assert_eq!(
            store.query(BlockPos::new(3, 1, 3)),
            TerrainQuery::Ready(BlockStateId(5))
        );

        // The next tick sees a filled cell, so neither system proposes a write.
        let outcome = simulation
            .tick(&mut store, &entities, 10)
            .expect("tick runs");
        assert_eq!(outcome.committed, 0);
        assert_eq!(outcome.rejected, 0);
        assert_eq!(outcome.tick, Tick(1));
    }

    #[test]
    fn overruns_are_reported_and_ticks_still_advance() {
        let mut simulation = Simulation::new(WorldId(1), 50, SimulationLimits::default());
        let mut store = store();
        let entities = entities();
        let outcome = simulation
            .tick(&mut store, &entities, 80)
            .expect("tick runs");
        assert_eq!(outcome.report.overrun_ms, 30);
        assert_eq!(simulation.clock().current(), Tick(1));
        assert_eq!(simulation.clock().overruns(), 1);
    }

    #[test]
    fn phase_mismatch_is_reported() {
        struct WrongPhase {
            spec: SystemSpec,
        }
        impl System for WrongPhase {
            fn spec(&self) -> &SystemSpec {
                &self.spec
            }
            fn run(&mut self, context: &mut SystemContext<'_>) {
                let ordering = OrderingKey {
                    phase: Phase::Publish,
                    rule_priority: 0,
                    stable_task_key: 0,
                    local_sequence: 0,
                };
                let mut transaction =
                    Transaction::new(TransactionId(1), self.spec.id.clone(), ordering);
                transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
                context.push(transaction);
            }
        }
        let mut simulation = Simulation::new(WorldId(1), 50, SimulationLimits::default());
        simulation
            .register(Box::new(WrongPhase {
                spec: SystemSpec {
                    id: key("base:wrong"),
                    phase: Phase::Simulation,
                    rule_priority: 0,
                    before: Vec::new(),
                    after: Vec::new(),
                },
            }))
            .expect("registration succeeds");
        let mut store = store();
        let entities = entities();
        let error = simulation
            .tick(&mut store, &entities, 10)
            .expect_err("mismatch is rejected");
        assert!(matches!(error, SimulationError::PhaseMismatch { .. }));
        assert_eq!(simulation.seq(), CommitSeq::INITIAL);
    }

    #[test]
    fn phase_transaction_quota_is_enforced() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut simulation = Simulation::new(
            WorldId(1),
            50,
            SimulationLimits {
                phase_transaction_limit: 0,
            },
        );
        simulation
            .register(writer(
                "base:a",
                1,
                &log,
                BlockPos::new(1, 1, 1),
                Some(BlockStateId(3)),
                1,
            ))
            .expect("registration succeeds");
        let mut store = store();
        let entities = entities();
        let error = simulation
            .tick(&mut store, &entities, 10)
            .expect_err("quota is enforced");
        assert!(matches!(error, SimulationError::TransactionQuota { .. }));
    }

    #[test]
    fn duplicate_registration_is_rejected_and_keeps_the_original() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut simulation = Simulation::new(WorldId(1), 50, SimulationLimits::default());
        simulation
            .register(writer(
                "base:a",
                1,
                &log,
                BlockPos::new(1, 1, 1),
                Some(BlockStateId(3)),
                1,
            ))
            .expect("registration succeeds");
        let error = simulation
            .register(writer(
                "base:a",
                2,
                &log,
                BlockPos::new(2, 1, 1),
                Some(BlockStateId(4)),
                2,
            ))
            .expect_err("duplicate is rejected");
        assert!(matches!(error, SimulationError::Order(_)));
        assert_eq!(simulation.order(), vec![key("base:a")]);
    }
}
