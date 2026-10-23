//! Authoritative simulation: phases, clock, transactions, and commit order.
//!
//! M1 delivered editable world storage. M2 begins here with the execution model
//! that turns storage into an authority: a fixed-step clock, an ordered phase
//! graph, typed read/write transactions, and a single coordinator that assigns
//! the world's commit sequence.
//!
//! The crate deliberately has no thread pool yet. A serial reference executor is
//! required before parallel execution, because parallel results are only
//! acceptable if they match the serial arbitration order exactly.

#![forbid(unsafe_code)]

pub mod clock;
pub mod coordinator;
pub mod phase;
pub mod transaction;

pub use clock::{TickClock, TickReport};
pub use coordinator::{CommitCoordinator, CommitFault, Committed};
pub use phase::{OrderError, Phase, SystemSpec, order_phase};
pub use transaction::{BlockPrecondition, Mutation, OrderingKey, Transaction};
