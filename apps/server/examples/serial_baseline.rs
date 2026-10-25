//! Reproducible serial baseline through the real durable execution coordinator.
#[path = "serial_baseline/workload.rs"]
mod workload;

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use torse_codec::StorageCodec;
use torse_diagnostics::{Profile, SerialProfiler, Stage};
use torse_server::{bootstrap as base, execution::*, *};
use torse_sim::action::ActorContext;
use torse_storage::*;
use torse_types::*;
use workload::Workload;

struct Options {
    output: PathBuf,
    ticks: u32,
    warmup: u32,
    backend: String,
    profiled: bool,
    full_runtime: bool,
    batch: u32,
}

fn options() -> Result<Options, Box<dyn Error>> {
    let mut options = Options {
        output: PathBuf::from(".work/serial-baseline"),
        ticks: 30,
        warmup: 5,
        backend: "all".into(),
        profiled: true,
        full_runtime: false,
        batch: 1,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => options.output = args.next().ok_or("missing output")?.into(),
            "--ticks" => options.ticks = args.next().ok_or("missing ticks")?.parse()?,
            "--warmup" => options.warmup = args.next().ok_or("missing warmup")?.parse()?,
            "--backend" => options.backend = args.next().ok_or("missing backend")?,
            "--unprofiled" => options.profiled = false,
            "--full-runtime" => options.full_runtime = true,
            "--batch" => options.batch = args.next().ok_or("missing batch")?.parse()?,
            _ => return Err(format!("unknown option: {arg}").into()),
        }
    }
    if options.ticks == 0 || options.ticks > 1000 || options.warmup > 100 {
        return Err("ticks must be 1..=1000 and warmup 0..=100".into());
    }
    if !(1..=1024).contains(&options.batch) {
        return Err("batch must be 1..1024".into());
    }
    if !matches!(
        options.backend.as_str(),
        "sink" | "memory" | "file" | "both" | "all"
    ) {
        return Err("backend must be sink, memory, file, both, or all".into());
    }
    Ok(options)
}

struct ResultRow {
    name: String,
    tick_ns: Vec<u64>,
    drive_ns: Vec<u64>,
    steps: u64,
    batches: u64,
    state_crc: u32,
    profile: Profile,
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = options()?;
    // Refuse to mix runs or overwrite measurements/worlds from an earlier run.
    fs::create_dir_all(options.output.parent().unwrap_or(Path::new(".")))?;
    fs::create_dir(&options.output)?;
    let mut rows = Vec::new();
    for backend in ["sink", "memory", "file"] {
        if options.backend != "all"
            && options.backend != backend
            && !(options.backend == "both" && backend != "sink")
        {
            continue;
        }
        for (tasks, subscribers, contended) in [
            (0, 0, false),
            (16, 0, false),
            (64, 0, false),
            (128, 0, false),
            (64, 2, false),
            (64, 8, false),
            (64, 2, true),
        ] {
            let load = Workload { tasks, contended };
            let name = format!("{backend}-{}-subscribers-{subscribers}", load.name());
            let path = options.output.join(&name);
            fs::create_dir(&path)?;
            let row = if backend == "file" {
                file_case(&options, &path, load, subscribers, &name)?
            } else {
                memory_case(
                    &options,
                    &path,
                    load,
                    subscribers,
                    &name,
                    backend == "memory",
                )?
            };
            println!(
                "case={} tick_p50_us={} tick_p95_us={} steps={} crc32c={:08x}",
                row.name,
                percentile(&row.tick_ns, 50) / 1000,
                percentile(&row.tick_ns, 95) / 1000,
                row.steps,
                row.state_crc,
            );
            rows.push(row);
        }
    }
    for group in rows.chunks(7).skip(1) {
        for (reference, current) in rows.iter().zip(group) {
            assert_eq!(reference.state_crc, current.state_crc, "backends diverged");
        }
    }
    write_results(&options, &rows)?;
    Ok(())
}

fn file_case(
    options: &Options,
    path: &Path,
    load: Workload,
    subscribers: usize,
    name: &str,
) -> Result<ResultRow, Box<dyn Error>> {
    let (world, actions) = base::world()?;
    let compatibility = world.coordinator().compatibility();
    let mut server = base::Files::create_file(
        WorldDirectory::acquire(path, base::WORLD, 1_048_576)?,
        compatibility,
        base::Codec::new(base::codec_limits())?,
        {
            let mut policy = base::file_policy();
            policy.journal.max_pending_records = options.batch;
            policy
        },
        |binding| {
            ExecutionHost::new(
                binding,
                world,
                actions,
                load.rules(),
                base::execution_limits(),
            )
        },
    )?;
    let profiler = recorder(options);
    server.set_serial_profiler(profiler.clone());
    let row = measure(&mut server, &profiler, options, load, subscribers, name)?;
    let expected = workload::state_bytes(server.authority()?)?;
    drop(server);
    // Replay from the initial checkpoint, so this validates the measured log.
    let (world, actions) = base::world()?;
    let recovered = base::Files::open_file(
        WorldDirectory::acquire(path, base::WORLD, 1_048_576)?,
        compatibility,
        base::Codec::new(base::codec_limits())?,
        base::file_policy(),
        |binding| {
            Ok(ExecutionRecovery::new(
                binding,
                world.registry(),
                world.schemas(),
                actions,
                load.rules(),
                base::execution_limits(),
            ))
        },
    )?;
    assert_eq!(workload::state_bytes(recovered.authority()?)?, expected);
    Ok(row)
}

fn memory_case(
    options: &Options,
    path: &Path,
    load: Workload,
    subscribers: usize,
    name: &str,
    retain: bool,
) -> Result<ResultRow, Box<dyn Error>> {
    let (world, actions) = base::world()?;
    let directory = WorldDirectory::acquire(path, base::WORLD, 1_048_576)?;
    let binding = Binding {
        world: base::WORLD,
        epoch: directory.epoch(),
        compatibility: world.coordinator().compatibility(),
    };
    let mut config = base::file_policy();
    config.journal.max_pending_records = options.batch;
    let codec = base::Codec::new(base::codec_limits())?;
    let journal = Journal::with_backend(
        binding,
        CommitCursor::initial(base::WORLD),
        codec,
        config.journal,
        ControlBackend {
            retained: retain.then(|| MemoryBackend::new(2_147_483_648)),
        },
    )?;
    let checkpoints = Checkpoints::open(
        binding,
        codec,
        config.checkpoint,
        MemoryCheckpointBackend::new(16_777_216),
    )?;
    let host = ExecutionHost::new(
        binding,
        world,
        actions,
        load.rules(),
        base::execution_limits(),
    )?;
    let mut server = Server::initialize(directory, host, journal, checkpoints, config.server)?;
    let profiler = recorder(options);
    server.set_serial_profiler(profiler.clone());
    measure(&mut server, &profiler, options, load, subscribers, name)
}

/// The sink retains no bytes and promises no persistence. Both controls use
/// the production Journal encoder, reservation queue, and durability protocol.
struct ControlBackend {
    retained: Option<MemoryBackend>,
}

impl JournalBackend for ControlBackend {
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(backend) = &mut self.retained {
            backend.append(bytes)
        } else {
            std::hint::black_box(bytes);
            Ok(())
        }
    }

    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn recorder(options: &Options) -> SerialProfiler {
    if options.profiled {
        // Worst case: 128 successes, eight projections each, four spans per
        // commit, and six planning spans. Warm-up is cleared before sampling.
        SerialProfiler::new((options.ticks.max(options.warmup) as usize) * 2048)
    } else {
        SerialProfiler::default()
    }
}

fn measure<C, B, S>(
    server: &mut Server<ExecutionHost, Journal<ExecutionModel, C, B>, S>,
    profiler: &SerialProfiler,
    options: &Options,
    load: Workload,
    subscribers: usize,
    name: &str,
) -> Result<ResultRow, Box<dyn Error>>
where
    C: StorageCodec<ExecutionModel>,
    B: JournalBackend,
    S: HostCheckpointStore<ExecutionModel>,
{
    server.set_runtime_optimization(!options.full_runtime)?;
    server.set_commit_batch_limit(options.batch)?;
    let mut subscriptions = Vec::new();
    for id in 1..=subscribers as u128 {
        server.register_session(
            Viewer {
                session: SessionId(id),
                actor: ActorContextId(id),
            },
            ActorContext::new(base::key("baseline:viewer")),
            base::scope(),
        )?;
        subscriptions.push(
            server
                .open_replica(SessionId(id), base::scope(), base::budget())?
                .subscription,
        );
    }
    let mut ticks = Vec::new();
    let mut drives = Vec::new();
    let mut steps = 0;
    let mut batches = 0;
    for tick in 0..options.warmup + options.ticks {
        if tick == options.warmup {
            profiler.clear();
        }
        let start = Instant::now();
        let mut drive_ns = 0;
        let mut tick_batches = 0;
        let mut remaining = load.tasks + 12;
        while remaining != 0 {
            let started = Instant::now();
            let batch = server.drive_batch(&mut NoReplicas, options.batch.min(remaining as u32))?;
            drive_ns += started.elapsed().as_nanos() as u64;
            assert!(batch.blocked.is_none() && !batch.idle && batch.committed != 0);
            remaining -= batch.committed as usize;
            for &subscription in &subscriptions {
                tick_batches += server.take_replica(subscription, base::budget())?.len() as u64;
            }
        }
        let tick_ns = start.elapsed().as_nanos() as u64;
        let host = server.authority()?;
        assert!(host.at_tick_boundary());
        assert_eq!(host.resume_point().tick, Tick(u64::from(tick + 1)));
        assert_eq!(
            host.cursor().seq.0,
            u64::from(tick + 1) * (load.tasks as u64 + 12)
        );
        assert_eq!(tick_batches, (load.successes() * subscribers) as u64);
        load.verify(host.world(), u64::from(tick + 1));
        if tick >= options.warmup {
            ticks.push(tick_ns);
            drives.push(drive_ns);
            steps += load.tasks as u64 + 12;
            batches += tick_batches;
        }
    }
    let profile = profiler.profile();
    assert_eq!(profile.discarded_samples, 0, "sample capacity exhausted");
    if options.profiled {
        assert_eq!(
            profile.totals[Stage::Planning as usize].calls,
            u64::from(options.ticks) * 6
        );
        for stage in [Stage::Preparation, Stage::Encoding, Stage::Publication] {
            assert_eq!(profile.totals[stage as usize].calls, steps);
        }
        assert_eq!(
            profile.totals[Stage::Flush as usize].calls,
            ((load.tasks as u64 + 12).div_ceil(u64::from(options.batch)))
                * u64::from(options.ticks)
        );
        assert_eq!(profile.totals[Stage::Projection as usize].calls, batches);
        assert!(
            profile.totals.iter().map(|t| t.total_ns).sum::<u128>()
                <= drives.iter().map(|&n| u128::from(n)).sum()
        );
    }
    Ok(ResultRow {
        name: name.into(),
        tick_ns: ticks,
        drive_ns: drives,
        steps,
        batches,
        state_crc: crc32c::crc32c(&workload::state_bytes(server.authority()?)?),
        profile,
    })
}

fn percentile(values: &[u64], percent: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)]
}

fn write_results(options: &Options, rows: &[ResultRow]) -> Result<(), Box<dyn Error>> {
    let mut summary = BufWriter::new(File::create(options.output.join("summary.csv"))?);
    let mut samples = BufWriter::new(File::create(options.output.join("samples.csv"))?);
    let mut ticks = BufWriter::new(File::create(options.output.join("ticks.csv"))?);
    writeln!(
        summary,
        "case,stage,calls,total_ns,p50_ns,p95_ns,p99_ns,max_ns,bytes"
    )?;
    writeln!(samples, "case,sample,stage,elapsed_ns,bytes")?;
    writeln!(
        ticks,
        "case,tick,wall_ns,drive_ns,steps,batches,state_crc32c"
    )?;
    for row in rows {
        // The digest identifies the completed case; it is not a per-tick snapshot.
        for (index, (&wall, &drive)) in row.tick_ns.iter().zip(&row.drive_ns).enumerate() {
            writeln!(
                ticks,
                "{},{index},{wall},{drive},{},{},{:08x}",
                row.name,
                row.steps / u64::from(options.ticks),
                row.batches / u64::from(options.ticks),
                row.state_crc
            )?;
        }
        for (index, sample) in row.profile.samples.iter().enumerate() {
            writeln!(
                samples,
                "{},{index},{},{},{}",
                row.name,
                sample.stage.name(),
                sample.elapsed_ns,
                sample.bytes
            )?;
        }
        for stage in Stage::ALL {
            let total = row.profile.totals[stage as usize];
            let values: Vec<_> = row
                .profile
                .samples
                .iter()
                .filter(|s| s.stage == stage)
                .map(|s| s.elapsed_ns)
                .collect();
            writeln!(
                summary,
                "{},{},{},{},{},{},{},{},{}",
                row.name,
                stage.name(),
                total.calls,
                total.total_ns,
                percentile(&values, 50),
                percentile(&values, 95),
                percentile(&values, 99),
                total.max_ns,
                total.bytes
            )?;
        }
    }
    summary.flush()?;
    samples.flush()?;
    ticks.flush()?;
    let mut metadata = BufWriter::new(File::create(options.output.join("run.txt"))?);
    writeln!(
        metadata,
        "format=1\nthreads=1\nprofiled={}\nticks={}\nwarmup={}\nbackend={}\nos={}\narch={}",
        options.profiled,
        options.ticks,
        options.warmup,
        options.backend,
        std::env::consts::OS,
        std::env::consts::ARCH
    )?;
    writeln!(metadata, "debug_assertions={}", cfg!(debug_assertions))?;
    writeln!(
        metadata,
        "runtime_optimization={}\ncommit_batch={}",
        !options.full_runtime, options.batch
    )?;
    writeln!(
        metadata,
        "logical_cpus={}",
        std::thread::available_parallelism()?
    )?;
    for (label, command, args) in [
        ("rustc", "rustc", vec!["--version"]),
        ("revision", "git", vec!["rev-parse", "HEAD"]),
        ("worktree", "git", vec!["status", "--porcelain"]),
    ] {
        if let Ok(output) = std::process::Command::new(command).args(args).output()
            && output.status.success()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            writeln!(metadata, "{label}={}", text.trim().replace('\n', ";"))?;
        }
    }
    metadata.flush()?;
    Ok(())
}
