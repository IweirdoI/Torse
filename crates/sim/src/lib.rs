//! Authoritative simulation: phases, systems, transactions, and commit order.
//!
//! M1 delivered editable world storage. M2 begins here with the execution model
//! that turns storage into an authority: a fixed-step clock, an ordered phase
//! graph, typed read/write transactions, a single coordinator that assigns the
//! world's commit sequence, and a tick driver that runs systems in that order.
//!
//! The crate deliberately has no thread pool yet. A serial reference executor is
//! required before parallel execution, because parallel results are only
//! acceptable if they match the serial arbitration order exactly.

#![forbid(unsafe_code)]

pub mod clock;
pub mod coordinator;
pub mod payload;
pub mod phase;
pub mod simulation;
pub mod system;
pub mod transaction;

pub use clock::{TickClock, TickReport};
pub use coordinator::{
    CommitCoordinator, CommitFault, Committed, PrepareError, PreparedChange, Publication,
};
pub use payload::changes_from_committed;
pub use phase::{OrderError, Phase, SystemSpec, order_phase};
pub use simulation::{
    DEFAULT_PHASE_TRANSACTION_LIMIT, Simulation, SimulationError, SimulationLimits, TickOutcome,
};
pub use system::{System, SystemContext};
pub use transaction::{BlockPrecondition, Mutation, OrderingKey, Transaction};
