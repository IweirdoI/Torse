//! Scenario runner for executing test scenarios with metrics.

use super::metrics::MetricsCollector;
use super::scenarios::Scenario;

/// Result of running a scenario.
#[derive(Debug)]
pub struct ScenarioResult {
    pub name: String,
    pub success: bool,
    pub error: Option<String>,
    pub metrics: MetricsCollector,
}

impl ScenarioResult {
    pub fn success(name: String, metrics: MetricsCollector) -> Self {
        Self {
            name,
            success: true,
            error: None,
            metrics,
        }
    }

    pub fn failure(name: String, error: String, metrics: MetricsCollector) -> Self {
        Self {
            name,
            success: false,
            error: Some(error),
            metrics,
        }
    }

    pub fn print(&self) {
        println!("\n=== Scenario: {} ===", self.name);
        if self.success {
            println!("Status: PASSED");
        } else {
            println!("Status: FAILED");
            if let Some(err) = &self.error {
                println!("Error: {}", err);
            }
        }
        self.metrics.print();
    }
}

/// Scenario runner for executing test scenarios.
pub struct ScenarioRunner {
    verbose: bool,
}

impl ScenarioRunner {
    pub fn new() -> Self {
        Self { verbose: false }
    }

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Run a single scenario and return the result.
    pub fn run<S: Scenario>(&self, scenario: S) -> ScenarioResult {
        let name = scenario.name().to_string();
        let mut metrics = MetricsCollector::new();

        if self.verbose {
            println!("Running scenario: {}", name);
            println!("Description: {}", scenario.description());
        }

        metrics.start_timer("scenario_total");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scenario.run(&mut metrics)
        }));

        metrics.stop_timer("scenario_total");

        match result {
            Ok(Ok(())) => ScenarioResult::success(name, metrics),
            Ok(Err(e)) => ScenarioResult::failure(name, e, metrics),
            Err(panic) => {
                let msg = if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "unknown panic".to_string()
                };
                ScenarioResult::failure(name, format!("panic: {}", msg), metrics)
            }
        }
    }

    /// Run multiple scenarios and return all results.
    pub fn run_all<S: Scenario>(&self, scenarios: Vec<S>) -> Vec<ScenarioResult> {
        scenarios.into_iter().map(|s| self.run(s)).collect()
    }

    /// Run multiple scenarios and print summary.
    pub fn run_and_report<S: Scenario>(&self, scenarios: Vec<S>) {
        let results = self.run_all(scenarios);
        self.print_summary(&results);
    }

    /// Print summary of all results.
    pub fn print_summary(&self, results: &[ScenarioResult]) {
        let total = results.len();
        let passed = results.iter().filter(|r| r.success).count();
        let failed = total - passed;

        println!("\n============================================");
        println!("Scenario Summary");
        println!("============================================");
        println!("Total:  {}", total);
        println!("Passed: {}", passed);
        println!("Failed: {}", failed);
        println!("============================================\n");

        if self.verbose {
            for result in results {
                result.print();
            }
        } else {
            for result in results.iter().filter(|r| !r.success) {
                result.print();
            }
        }
    }
}

impl Default for ScenarioRunner {
    fn default() -> Self {
        Self::new()
    }
}
