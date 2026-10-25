//! Isolated upper-bound experiment; no prepared change is reused for publication.
#[allow(dead_code)]
#[path = "serial_baseline/workload.rs"]
mod workload;
use std::error::Error;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;
use torse_runtime::{ExecutorConfig, JobBudget, JobOutput, run_scoped};
use torse_server::{HostAuthority, bootstrap as base, execution::*};
use torse_sim::Phase;
use torse_types::*;

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: prevalidation_baseline <new-output.csv>")?;
    let mut out = OpenOptions::new().write(true).create_new(true).open(path)?;
    writeln!(out, "tasks,contended,workers,round,elapsed_ns")?;
    for tasks in [16, 64, 128] {
        for contended in [false, true] {
            let (world, actions) = base::world()?;
            let binding = Binding {
                world: base::WORLD,
                epoch: WorldEpoch(1),
                compatibility: world.coordinator().compatibility(),
            };
            let mut host = ExecutionHost::new(
                binding,
                world,
                actions,
                workload::Workload { tasks, contended }.rules(),
                base::execution_limits(),
            )?;
            while usize::from(host.resume_point().next_phase) != Phase::Simulation.index()
                || host.runtime().work.is_none()
            {
                let prepared = host.prepare_next()?.unwrap();
                host.publish(prepared)?;
            }
            let frozen = &host.runtime().work.as_ref().unwrap().tasks;
            let validate = |index: usize| {
                let tx = frozen[index]
                    .transaction
                    .as_ref()
                    .unwrap()
                    .restore(host.world())
                    .unwrap();
                host.world().prepare_group(&tx).is_ok()
            };
            // Capture only thread-safe world/table references, never the host.
            let world = host.world();
            let parallel = |index: usize| {
                let tx = frozen[index]
                    .transaction
                    .as_ref()
                    .unwrap()
                    .restore(world)
                    .unwrap();
                Ok(JobOutput {
                    value: world.prepare_group(&tx).is_ok(),
                    bytes: 1,
                })
            };
            let reference: Vec<_> = (0..tasks).map(validate).collect();
            for workers in [0, 1, 4] {
                let mut times = vec![];
                for round in 0..110 {
                    let started = Instant::now();
                    let result = if workers == 0 {
                        (0..tasks).map(validate).collect::<Vec<_>>()
                    } else {
                        run_scoped(
                            ExecutorConfig {
                                workers,
                                max_jobs: tasks,
                                max_input_bytes: tasks * 8,
                                max_output_bytes: tasks * 65536,
                            },
                            JobBudget {
                                input_bytes: 8,
                                output_bytes: 65536,
                            },
                            tasks,
                            parallel,
                        )?
                        .into_iter()
                        .map(Result::unwrap)
                        .collect()
                    };
                    let elapsed = started.elapsed().as_nanos() as u64;
                    assert_eq!(result, reference);
                    if round >= 10 {
                        writeln!(
                            out,
                            "{tasks},{contended},{workers},{},{elapsed}",
                            round - 10
                        )?;
                        times.push(elapsed);
                    }
                }
                times.sort_unstable();
                println!(
                    "tasks={tasks} contended={contended} workers={workers} p50_ns={} p95_ns={}",
                    times[49], times[94]
                );
            }
        }
    }
    Ok(())
}
