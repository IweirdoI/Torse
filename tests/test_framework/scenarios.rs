//! Scenario trait and common scenario types.

use super::metrics::MetricsCollector;

/// A test scenario that can be executed with metrics collection.
pub trait Scenario {
    /// Name of the scenario.
    fn name(&self) -> &str;

    /// Description of what this scenario tests.
    fn description(&self) -> &str;

    /// Run the scenario, collecting metrics.
    fn run(&self, metrics: &mut MetricsCollector) -> Result<(), String>;
}

/// Helper macro to define a simple scenario from a closure.
#[macro_export]
macro_rules! scenario {
    ($name:expr, $desc:expr, $body:expr) => {{
        struct SimpleScenario;
        impl $crate::Scenario for SimpleScenario {
            fn name(&self) -> &str {
                $name
            }
            fn description(&self) -> &str {
                $desc
            }
            fn run(&self, metrics: &mut $crate::MetricsCollector) -> Result<(), String> {
                $body(metrics)
            }
        }
        SimpleScenario
    }};
}
