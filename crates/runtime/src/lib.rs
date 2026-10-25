//! CPU task execution, I/O, and bounded experiments.

#![forbid(unsafe_code)]

mod executor;
pub use executor::{
    Cancellation, Executor, ExecutorConfig, Job, JobBudget, JobError, JobOutput, Priority,
    Reservation, ReserveError, Usage,
};

#[cfg(feature = "ecs-probe")]
mod ecs_probe;

#[cfg(feature = "ecs-probe")]
pub use ecs_probe::{EcsProbeReport, run_ecs_probe};
