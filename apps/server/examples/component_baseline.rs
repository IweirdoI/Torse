//! Read-only component preparation experiment; no prepared output is published.
#[allow(dead_code)]
#[path = "component_baseline/workload.rs"]
mod workload;
use std::{error::Error, fs::OpenOptions, io::Write, time::Instant};
use torse_runtime::{ExecutorConfig, JobBudget, JobOutput, run_scoped};
use torse_types::*;
use workload::fixture;
fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: component_baseline <new-output.csv>")?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    writeln!(output, "records,tasks,workers,round,prepare_ns")?;
    for records in [0, 32, 128, 512] {
        for tasks in [16, 64] {
            let (world, groups) = fixture(tasks, records)?;
            for workers in [0, 1, 4] {
                let mut times = vec![];
                for round in 0..23 {
                    let started = Instant::now();
                    let result = if workers == 0 {
                        groups
                            .iter()
                            .map(|g| world.prepare_group(g).unwrap())
                            .collect::<Vec<_>>()
                    } else {
                        run_scoped(
                            ExecutorConfig {
                                workers,
                                max_jobs: tasks,
                                max_input_bytes: tasks * 8,
                                max_output_bytes: tasks * 8 * 1024 * 1024,
                            },
                            JobBudget {
                                input_bytes: 8,
                                output_bytes: 8 * 1024 * 1024,
                            },
                            tasks,
                            |index| {
                                Ok(JobOutput {
                                    value: world.prepare_group(&groups[index]).unwrap(),
                                    bytes: 8 * 1024 * 1024,
                                })
                            },
                        )?
                        .into_iter()
                        .map(Result::unwrap)
                        .collect()
                    };
                    let elapsed = started.elapsed().as_nanos() as u64;
                    for (prepared, group) in result.iter().zip(&groups) {
                        assert_eq!(prepared.component_edits(), group.members[0].components);
                        assert_eq!(prepared.transaction(), group.id);
                        assert_eq!(prepared.meta().previous, CommitSeq::INITIAL);
                    }
                    if round >= 3 {
                        writeln!(
                            output,
                            "{records},{tasks},{workers},{},{elapsed}",
                            round - 3
                        )?;
                        times.push(elapsed);
                    }
                }
                times.sort_unstable();
                println!(
                    "records={records} tasks={tasks} workers={workers} p50_us={} p95_us={}",
                    times[9] / 1000,
                    times[18] / 1000
                );
            }
        }
    }
    Ok(())
}
