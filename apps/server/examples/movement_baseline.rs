//! Full durable execution path with optional bounded collision prevalidation.
#[allow(dead_code)]
#[path = "serial_baseline/workload.rs"]
mod serial;
#[path = "movement_baseline/workload.rs"]
mod workload;
use std::{
    error::Error,
    fs::{self, File},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};
use torse_diagnostics::{SerialProfiler, Stage};
use torse_server::{bootstrap as base, execution::*, *};
use torse_storage::*;
use torse_types::*;
use workload::Motion;

// In-memory control includes the real journal encoder and ordered append path.
// Retain every byte so the benchmark checks equality outside the timed loop.
struct Retain(Arc<Mutex<Vec<u8>>>);
impl JournalBackend for Retain {
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: movement_baseline <new-directory>")?,
    );
    fs::create_dir(&output)?;
    let mut raw = File::create(output.join("ticks.csv"))?;
    let mut summary = File::create(output.join("summary.csv"))?;
    writeln!(
        raw,
        "scenario,tasks,workers,tick,drive_ns,preparation_ns,planning_ns"
    )?;
    writeln!(
        summary,
        "scenario,tasks,workers,drive_p50_ns,drive_p95_ns,preparation_mean_ns,prepared,hits,peak_jobs,peak_reserved_bytes,peak_cached_bytes,state_crc32c,journal_crc32c"
    )?;
    for (scenario, half) in [("actor", [0.3, 0.9, 0.3]), ("large", [4.0; 3])] {
        for tasks in [16, 64] {
            let mut reference = None;
            for workers in [0, 1, 4] {
                let load = Motion {
                    tasks,
                    half,
                    contended: false,
                    mixed: false,
                    deny_plan: false,
                };
                let world = load.world();
                let (_, actions) = base::world()?;
                let path = output.join(format!("{scenario}-{tasks}-{workers}"));
                fs::create_dir(&path)?;
                let directory = WorldDirectory::acquire(path, base::WORLD, 65536)?;
                let binding = Binding {
                    world: base::WORLD,
                    epoch: directory.epoch(),
                    compatibility: world.coordinator().compatibility(),
                };
                let mut policy = base::file_policy();
                policy.journal.max_pending_records = 16;
                let codec = base::Codec::new(base::codec_limits())?;
                let log = Arc::new(Mutex::new(Vec::with_capacity(4 * 1024 * 1024)));
                let journal = Journal::with_backend(
                    binding,
                    CommitCursor::initial(base::WORLD),
                    codec,
                    policy.journal,
                    Retain(Arc::clone(&log)),
                )?;
                let cp = Checkpoints::open(
                    binding,
                    codec,
                    policy.checkpoint,
                    MemoryCheckpointBackend::new(16_777_216),
                )?;
                let host = ExecutionHost::new(
                    binding,
                    world,
                    actions,
                    load.rules(),
                    base::execution_limits(),
                )?;
                let mut server = Server::initialize(directory, host, journal, cp, policy.server)?;
                server.set_commit_batch_limit(16)?;
                if workers > 0 {
                    server.set_prevalidation(Some(PrevalidationConfig {
                        workers,
                        ..Default::default()
                    }))?;
                }
                let profiler = SerialProfiler::new(0);
                server.set_serial_profiler(profiler.clone());
                let mut times = vec![];
                let mut preparation = 0;
                for tick in 0..25 {
                    profiler.clear();
                    let start = Instant::now();
                    server.drive_tick(&mut NoReplicas, 256)?;
                    let elapsed = start.elapsed().as_nanos() as u64;
                    let totals = profiler.totals();
                    let prepare = totals[Stage::Preparation as usize].total_ns as u64;
                    let planning = totals[Stage::Planning as usize].total_ns as u64;
                    if tick >= 5 {
                        times.push(elapsed);
                        preparation += prepare;
                        writeln!(
                            raw,
                            "{scenario},{tasks},{workers},{tick},{elapsed},{prepare},{planning}"
                        )?;
                    }
                }
                let state = serial::state_bytes(server.authority()?)?;
                let log = log.lock().unwrap();
                let state_crc = crc32c::crc32c(&state);
                let log_crc = crc32c::crc32c(&log);
                if let Some((expected_state, expected_log)) = &reference {
                    assert_eq!(&state, expected_state);
                    assert_eq!(&*log, expected_log);
                } else {
                    reference = Some((state, log.clone()));
                }
                let stats = server.authority()?.prevalidation_stats();
                if scenario == "large" && workers > 0 {
                    assert_eq!(stats.prepared, 25 * tasks as u64);
                    assert!(stats.hits >= stats.prepared);
                }
                if scenario == "actor" {
                    assert_eq!(stats.prepared, 0);
                }
                times.sort_unstable();
                let p50 = times[9];
                let p95 = times[18];
                writeln!(
                    summary,
                    "{scenario},{tasks},{workers},{p50},{p95},{},{},{},{},{},{},{state_crc:08x},{log_crc:08x}",
                    preparation / 20,
                    stats.prepared,
                    stats.hits,
                    stats.peak_jobs,
                    stats.peak_reserved_bytes,
                    stats.peak_cached_bytes
                )?;
                println!(
                    "{scenario} tasks={tasks} workers={workers} p50_us={} p95_us={}",
                    p50 / 1000,
                    p95 / 1000
                );
            }
        }
    }
    Ok(())
}
