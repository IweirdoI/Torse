//! Metrics collection for test scenarios.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A single metric observation.
#[derive(Clone, Debug)]
pub enum Metric {
    Counter { name: String, value: u64 },
    Gauge { name: String, value: f64 },
    Duration { name: String, value: Duration },
    Bytes { name: String, value: u64 },
}

impl Metric {
    pub fn name(&self) -> &str {
        match self {
            Metric::Counter { name, .. } => name,
            Metric::Gauge { name, .. } => name,
            Metric::Duration { name, .. } => name,
            Metric::Bytes { name, .. } => name,
        }
    }
}

/// Metrics collector for test scenarios.
#[derive(Debug, Default)]
pub struct MetricsCollector {
    metrics: Vec<Metric>,
    timers: HashMap<String, Instant>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a counter metric.
    pub fn count(&mut self, name: impl Into<String>, value: u64) {
        self.metrics.push(Metric::Counter {
            name: name.into(),
            value,
        });
    }

    /// Record a gauge metric.
    pub fn gauge(&mut self, name: impl Into<String>, value: f64) {
        self.metrics.push(Metric::Gauge {
            name: name.into(),
            value,
        });
    }

    /// Record a duration metric.
    pub fn duration(&mut self, name: impl Into<String>, value: Duration) {
        self.metrics.push(Metric::Duration {
            name: name.into(),
            value,
        });
    }

    /// Record a bytes metric.
    pub fn bytes(&mut self, name: impl Into<String>, value: u64) {
        self.metrics.push(Metric::Bytes {
            name: name.into(),
            value,
        });
    }

    /// Start a named timer.
    pub fn start_timer(&mut self, name: impl Into<String>) {
        self.timers.insert(name.into(), Instant::now());
    }

    /// Stop a named timer and record the duration.
    pub fn stop_timer(&mut self, name: impl Into<String>) -> Option<Duration> {
        let name_str = name.into();
        if let Some(start) = self.timers.remove(&name_str) {
            let duration = start.elapsed();
            self.duration(&name_str, duration);
            Some(duration)
        } else {
            None
        }
    }

    /// Get all collected metrics.
    pub fn metrics(&self) -> &[Metric] {
        &self.metrics
    }

    /// Clear all collected metrics.
    pub fn clear(&mut self) {
        self.metrics.clear();
        self.timers.clear();
    }

    /// Print metrics in human-readable format.
    pub fn print(&self) {
        println!("\n=== Metrics ===");
        for metric in &self.metrics {
            match metric {
                Metric::Counter { name, value } => {
                    println!("  {}: {} (counter)", name, value);
                }
                Metric::Gauge { name, value } => {
                    println!("  {}: {:.2} (gauge)", name, value);
                }
                Metric::Duration { name, value } => {
                    println!("  {}: {:?} (duration)", name, value);
                }
                Metric::Bytes { name, value } => {
                    println!("  {}: {} bytes", name, value);
                }
            }
        }
        println!("===============\n");
    }

    /// Export metrics as JSON string.
    pub fn to_json(&self) -> String {
        let mut entries = Vec::new();
        for metric in &self.metrics {
            let entry = match metric {
                Metric::Counter { name, value } => {
                    format!(r#"{{"name":"{}","type":"counter","value":{}}}"#, name, value)
                }
                Metric::Gauge { name, value } => {
                    format!(r#"{{"name":"{}","type":"gauge","value":{}}}"#, name, value)
                }
                Metric::Duration { name, value } => {
                    format!(
                        r#"{{"name":"{}","type":"duration","value_ms":{}}}"#,
                        name,
                        value.as_millis()
                    )
                }
                Metric::Bytes { name, value } => {
                    format!(r#"{{"name":"{}","type":"bytes","value":{}}}"#, name, value)
                }
            };
            entries.push(entry);
        }
        format!("[{}]", entries.join(","))
    }
}
