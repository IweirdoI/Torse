//! Headless scenario runner with readable metric output.

#![allow(dead_code)]

use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

/// One recorded observation.
#[derive(Clone, Debug)]
pub enum Metric {
    Count(u64),
    Bytes(u64),
    Elapsed(Duration),
}

/// Metrics recorded by one scenario, kept in insertion order.
#[derive(Debug, Default)]
pub struct Metrics {
    entries: Vec<(String, Metric)>,
}

impl Metrics {
    pub fn count(&mut self, name: &str, value: u64) {
        self.entries.push((name.to_owned(), Metric::Count(value)));
    }

    pub fn bytes(&mut self, name: &str, value: u64) {
        self.entries.push((name.to_owned(), Metric::Bytes(value)));
    }

    /// Run `work` and record its elapsed time under `name`.
    pub fn time<T>(&mut self, name: &str, work: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let value = work();
        self.entries
            .push((name.to_owned(), Metric::Elapsed(start.elapsed())));
        value
    }

    pub fn entries(&self) -> &[(String, Metric)] {
        &self.entries
    }

    pub fn print(&self) {
        for (name, metric) in &self.entries {
            match metric {
                Metric::Count(value) => println!("    {name}: {value}"),
                Metric::Bytes(value) => println!("    {name}: {value} bytes"),
                Metric::Elapsed(value) => println!("    {name}: {value:?}"),
            }
        }
    }
}

/// A scenario that checks behavior and records metrics.
pub trait Scenario {
    fn name(&self) -> &str;
    fn run(&self, metrics: &mut Metrics) -> Result<(), String>;
}

/// Outcome of one scenario run.
#[derive(Debug)]
pub struct Report {
    pub name: String,
    pub error: Option<String>,
    pub metrics: Metrics,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.error.is_none()
    }
}

pub fn run(scenario: &dyn Scenario) -> Report {
    let mut metrics = Metrics::default();
    let result = panic::catch_unwind(AssertUnwindSafe(|| scenario.run(&mut metrics)));
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(message)) => Some(message),
        Err(payload) => Some(match payload.downcast_ref::<&str>() {
            Some(message) => format!("panic: {message}"),
            None => match payload.downcast_ref::<String>() {
                Some(message) => format!("panic: {message}"),
                None => "panic with non-string payload".to_owned(),
            },
        }),
    };
    Report {
        name: scenario.name().to_owned(),
        error,
        metrics,
    }
}

/// Run every scenario, print each report, and panic if any failed.
pub fn run_all(scenarios: &[&dyn Scenario]) {
    let reports: Vec<Report> = scenarios.iter().map(|scenario| run(*scenario)).collect();
    for report in &reports {
        match &report.error {
            None => println!("PASS {}", report.name),
            Some(error) => println!("FAIL {}: {error}", report.name),
        }
        report.metrics.print();
    }
    let failed: Vec<&str> = reports
        .iter()
        .filter(|report| !report.passed())
        .map(|report| report.name.as_str())
        .collect();
    assert!(failed.is_empty(), "failed scenarios: {failed:?}");
}
