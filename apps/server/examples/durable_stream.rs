//! Explicit persistent section residency using the normal execution host and files.
use std::path::PathBuf;
use torse_server::{bootstrap as base, execution::*, section_pages::SectionPages, *};
use torse_types::*;
use torse_world::*;

fn run() -> Result<(), Fault> {
    let mut args = std::env::args().skip(1);
    let root =
        PathBuf::from(args.next().ok_or_else(|| {
            Fault::invalid_data("usage: durable_stream <directory> park|load|grow")
        })?);
    let operation = args.next().unwrap_or_else(|| "park".into());
    std::fs::create_dir_all(&root).map_err(|e| Fault::invalid_data(e.to_string()))?;
    let directory = WorldDirectory::acquire(&root, base::WORLD, 8192)?;
    let page_root = root.join("sections");
    std::fs::create_dir_all(&page_root).map_err(|e| Fault::invalid_data(e.to_string()))?;
    let pages = SectionPages::new(page_root, base::budget(), base::codec_limits(), 128)?;
    let (world, actions) = base::world()?;
    let compatibility = world.coordinator().compatibility();
    let codec = base::Codec::new(base::codec_limits())?;
    let mut server = if root.join("journal.log").exists() {
        base::Files::open_file(
            directory,
            compatibility,
            codec,
            base::file_policy(),
            |binding| {
                Ok(ExecutionRecovery::new(
                    binding,
                    world.registry(),
                    world.schemas(),
                    actions,
                    Vec::new(),
                    base::execution_limits(),
                )
                .with_section_pages(pages.clone()))
            },
        )?
    } else {
        base::Files::create_file(
            directory,
            compatibility,
            codec,
            base::file_policy(),
            |binding| {
                ExecutionHost::new(
                    binding,
                    world,
                    actions,
                    Vec::new(),
                    base::execution_limits(),
                )
            },
        )?
    };
    let result = match operation.as_str() {
        "park" => server.unload_section(&pages, SectionPos::ZERO, &mut NoReplicas)?,
        "load" => server.reload_section(&pages, SectionPos::ZERO, &mut NoReplicas)?,
        "grow" => {
            let world = server.authority()?.world();
            let page = Snapshot {
                meta: torse_sim::streaming::page_meta(world),
                side: world.store().side(),
                bounds: world.store().bounds(),
                states: Vec::new(),
                sections: vec![SnapshotSection {
                    pos: SectionPos::new(2, 0, 0),
                    cells: SnapshotCells::Uniform(BlockStateId::EMPTY),
                }],
                entities: Vec::new(),
                next_entity_id: world.entities().next_entity_id(),
            };
            server.install_section(&pages, page, &mut NoReplicas)?
        }
        _ => return Err(Fault::invalid_data("operation must be park, load, or grow")),
    };
    if !matches!(result, CommitStep::Durable(_)) {
        return Err(Fault::resource_limit(
            "section commit blocked; retry operation",
        ));
    }
    server.drive_tick(&mut NoReplicas, 1024)?;
    server.checkpoint()?;
    let host = server.authority()?;
    println!(
        "seq={} tick={} resident={} parked={} entities={} next_entity={}",
        host.cursor().seq.0,
        host.resume_point().tick.0,
        host.world().store().len(),
        host.parked_sections().len(),
        host.world().entities().len(),
        host.world().entities().next_entity_id()
    );
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
