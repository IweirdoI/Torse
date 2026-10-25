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
    if std::env::args().nth(2).as_deref() == Some("--movement") {
        return movement_baseline(&mut out);
    }
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

fn movement_baseline(out: &mut impl Write) -> Result<(), Box<dyn Error>> {
    use torse_sim::{OrderingKey, Transaction, TransactionGroup};
    use torse_world::Position;
    writeln!(out, "scenario,tasks,workers,round,elapsed_ns")?;
    for (name, half) in [("actor", [0.3, 0.9, 0.3]), ("large", [4.0, 4.0, 4.0])] {
        for tasks in [16, 64] {
            let (mut world, _) = base::world()?;
            base::fill_floor(&mut world)?;
            let mut groups = vec![];
            for index in 0..tasks {
                let handle = world.entities_mut().spawn(Position::new(14.0, 8.0, 8.0))?;
                let entity = world.entities().get(handle).unwrap();
                let ordering = OrderingKey {
                    phase: Phase::Simulation,
                    rule_priority: 0,
                    stable_task_key: index as u64,
                    local_sequence: 0,
                };
                let mut tx = Transaction::new(
                    TransactionId(index as u128 + 1),
                    base::key("movement:baseline"),
                    ordering,
                );
                tx.move_entity(handle, entity.id, entity.position, [4.0, -4.0, 0.0], half);
                groups.push(TransactionGroup::new(tx.id, ordering, vec![tx]).unwrap());
            }
            let validate = |index: usize| {
                let prepared = world
                    .prepare_group(&groups[index])
                    .expect("movement is within loaded bounds");
                prepared.entity_moves().to_vec()
            };
            let reference: Vec<_> = (0..tasks).map(validate).collect();
            assert!(reference.iter().all(|moves| moves.len() == 1));
            for workers in [0, 1, 4] {
                let mut times = vec![];
                for round in 0..25 {
                    let started = Instant::now();
                    let result = if workers == 0 {
                        (0..tasks).map(validate).collect::<Vec<_>>()
                    } else {
                        let reservation = 8 * 1024 * 1024;
                        run_scoped(
                            ExecutorConfig {
                                workers,
                                max_jobs: tasks,
                                max_input_bytes: tasks * 8,
                                max_output_bytes: tasks * reservation,
                            },
                            JobBudget {
                                input_bytes: 8,
                                output_bytes: reservation,
                            },
                            tasks,
                            |index| {
                                let value = validate(index);
                                let bytes = size_of::<Vec<torse_world::EntityMove>>()
                                    + std::mem::size_of_val(value.as_slice());
                                Ok(JobOutput { value, bytes })
                            },
                        )?
                        .into_iter()
                        .map(Result::unwrap)
                        .collect()
                    };
                    let elapsed = started.elapsed().as_nanos() as u64;
                    assert_eq!(result, reference);
                    if round >= 5 {
                        writeln!(out, "{name},{tasks},{workers},{},{elapsed}", round - 5)?;
                        times.push(elapsed);
                    }
                }
                times.sort_unstable();
                println!(
                    "scenario={name} tasks={tasks} workers={workers} p50_ns={} p95_ns={}",
                    times[9], times[18]
                );
            }
        }
    }
    Ok(())
}
