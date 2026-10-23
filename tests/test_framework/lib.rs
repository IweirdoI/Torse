//! Test scenario framework for B-side modules.
//!
//! Provides infrastructure for running codec, storage, and network scenarios
//! with metrics collection and failure injection.

#![forbid(unsafe_code)]

pub mod metrics;
pub mod runner;
pub mod scenarios;

pub use metrics::{Metric, MetricsCollector};
pub use runner::{ScenarioResult, ScenarioRunner};
pub use scenarios::Scenario;
