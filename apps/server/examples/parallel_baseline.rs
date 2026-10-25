//! Synthetic planning cost through ExecutionHost and the ordinary sink journal.
#[allow(dead_code)]
#[path = "serial_baseline/workload.rs"]
mod reference;

use std::error::Error;
use std::fs::{self, File};
use std::hint::black_box;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use torse_diagnostics::{SerialProfiler, Stage};
use torse_server::{bootstrap as base, execution::*, *};
use torse_sim::{OrderingKey, Phase, SystemSpec, Transaction, WorldAuthority};
use torse_storage::*;
use torse_types::*;
use torse_world::BlockStateId;

struct Planner {
    index: usize,
    rounds: usize,
    contended: bool,
}
impl DurableRule for Planner {
    fn spec(&self) -> SystemSpec {
        SystemSpec {
            id: base::key(&format!("planning:r{}", self.index)),
            phase: Phase::Simulation,
            rule_priority: 0,
            before: vec![],
            after: vec![],
        }
    }
    fn parallel_planner(&self) -> Option<&(dyn DurableRule + Sync)> {
        Some(self)
    }
    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        // Controlled CPU work, not a claim about production rule complexity.
        let mut checksum = self.index as u64;
        for i in 0..self.rounds {
            let query = black_box(world)
                .store()
                .query(BlockPos::new((i % 32) as i64, 3, 1));
            checksum =
                checksum.rotate_left(7) ^ u64::from(black_box(query).ready().unwrap().is_empty());
        }
        black_box(checksum);
        let pos = BlockPos::new(
            if self.contended {
                self.index / 2
            } else {
                self.index
            } as i64,
            1,
            1,
        );
        let previous = world.store().query(pos).ready().unwrap();
        let stone = world
            .registry()
            .default_state(&base::key("base:stone"))
            .unwrap();
        let mut tx = Transaction::new(
            TransactionId(u128::from(tick.0) * 16 + self.index as u128 + 1),
            self.spec().id,
            OrderingKey {
                phase: Phase::Simulation,
                rule_priority: 0,
                stable_task_key: self.index as u64,
                local_sequence: 0,
            },
        );
        tx.read_block(pos, previous).write_block(
            pos,
            if previous.is_empty() {
                stone
            } else {
                BlockStateId::EMPTY
            },
        );
        Ok(vec![WorkPlan::transaction(tx)])
    }
}

struct Sink(Arc<Mutex<u32>>);
impl JournalBackend for Sink {
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut crc = self.0.lock().unwrap();
        *crc = crc32c::crc32c_append(*crc, bytes);
        Ok(())
    }
    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output = PathBuf::from(".work/parallel-baseline");
    let mut ticks = 20u32;
    let mut warmup = 3u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().ok_or("missing output")?.into(),
            "--ticks" => ticks = args.next().ok_or("missing ticks")?.parse()?,
            "--warmup" => warmup = args.next().ok_or("missing warmup")?.parse()?,
            _ => return Err(format!("unknown option: {arg}").into()),
        }
    }
    if ticks == 0 || ticks > 1000 || warmup > 100 {
        return Err("ticks must be 1..1000, warmup 0..100".into());
    }
    fs::create_dir_all(output.parent().unwrap_or(std::path::Path::new(".")))?;
    fs::create_dir(&output)?;
    let mut raw = File::create(output.join("ticks.csv"))?;
    let mut summary = File::create(output.join("summary.csv"))?;
    writeln!(raw, "scenario,workers,tick,planning_ns,drive_ns")?;
    writeln!(
        summary,
        "scenario,workers,planning_mean_ns,planning_p50_ns,planning_p95_ns,drive_mean_ns,state_crc32c,journal_crc32c"
    )?;
    for rounds in [0, 200_000] {
        for contended in [false, true] {
            let name = format!(
                "{}-{}",
                if rounds == 0 { "light" } else { "cpu" },
                if contended {
                    "contended"
                } else {
                    "independent"
                }
            );
            let mut expected = None;
            for workers in [0, 1, 4] {
                let path = output.join(format!("{name}-{workers}"));
                fs::create_dir(&path)?;
                let (world, actions) = base::world()?;
                let directory = WorldDirectory::acquire(&path, base::WORLD, 65_536)?;
                let binding = Binding {
                    world: base::WORLD,
                    epoch: directory.epoch(),
                    compatibility: world.coordinator().compatibility(),
                };
                let rules = (0..8)
                    .map(|index| {
                        Box::new(Planner {
                            index,
                            rounds,
                            contended,
                        }) as Box<dyn DurableRule>
                    })
                    .collect();
                let host =
                    ExecutionHost::new(binding, world, actions, rules, base::execution_limits())?;
                let config = base::file_policy();
                let codec = base::Codec::new(base::codec_limits())?;
                let journal_crc = Arc::new(Mutex::new(0));
                let journal = Journal::with_backend(
                    binding,
                    CommitCursor::initial(base::WORLD),
                    codec,
                    config.journal,
                    Sink(Arc::clone(&journal_crc)),
                )?;
                let checkpoints = Checkpoints::open(
                    binding,
                    codec,
                    config.checkpoint,
                    MemoryCheckpointBackend::new(16_777_216),
                )?;
                let mut server =
                    Server::initialize(directory, host, journal, checkpoints, config.server)?;
                if workers != 0 {
                    server.set_parallel_planning(Some(PlanningConfig {
                        workers,
                        max_in_flight: 8,
                    }))?;
                }
                let profiler = SerialProfiler::new(0);
                server.set_serial_profiler(profiler.clone());
                let mut planning = vec![];
                let mut drive = vec![];
                for tick in 0..warmup + ticks {
                    profiler.clear();
                    let started = Instant::now();
                    assert_eq!(server.drive_tick(&mut NoReplicas, 64)?, 20);
                    let elapsed = started.elapsed().as_nanos() as u64;
                    assert!(server.authority()?.at_tick_boundary());
                    if tick >= warmup {
                        let plan = profiler.totals()[Stage::Planning as usize].total_ns as u64;
                        writeln!(raw, "{name},{workers},{tick},{plan},{elapsed}")?;
                        planning.push(plan);
                        drive.push(elapsed);
                    }
                }
                let state = reference::state_bytes(server.authority()?)?;
                let journal = *journal_crc.lock().unwrap();
                if let Some((previous, crc)) = &expected {
                    assert_eq!(&state, previous);
                    assert_eq!(&journal, crc);
                }
                let digest = crc32c::crc32c(&state);
                let mean = planning.iter().sum::<u64>() / u64::from(ticks);
                let drive_mean = drive.iter().sum::<u64>() / u64::from(ticks);
                planning.sort_unstable();
                let p50 = planning[(planning.len() * 50).div_ceil(100) - 1];
                let p95 = planning[(planning.len() * 95).div_ceil(100) - 1];
                writeln!(
                    summary,
                    "{name},{workers},{mean},{p50},{p95},{drive_mean},{digest:08x},{journal:08x}"
                )?;
                println!(
                    "case={name} workers={workers} planning_mean_us={} drive_mean_us={} state={digest:08x} journal={journal:08x}",
                    mean / 1000,
                    drive_mean / 1000
                );
                expected = Some((state, journal));
            }
        }
    }
    let toolchain = std::process::Command::new("rustc")
        .arg("--version")
        .output()?;
    fs::write(
        output.join("run.txt"),
        format!(
            "ticks={ticks}\nwarmup={warmup}\nrules=8\nmax_in_flight=8\nsynthetic_rounds=0,200000\nbackend=sink-with-streaming-crc32c\nlogical_cpus={}\n{}",
            std::thread::available_parallelism()?.get(),
            String::from_utf8(toolchain.stdout)?
        ),
    )?;
    Ok(())
}
