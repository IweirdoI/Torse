//! Toggle one block in a fully resident world, save, and recover it on the next run.

use std::error::Error;
use std::path::PathBuf;

use torse_codec::{CodecLimits, WorldCodec};
use torse_content::{compile, m0_fixture_pack};
use torse_server::{
    FileServer, FileServerConfig, HostAuthority, LocalCommit, LocalWorldModel, NoReplicas,
    ServerConfig, WorldDirectory, WorldHost, WorldHostRecovery,
};
use torse_sim::{OrderingKey, Phase, Transaction, WorldAuthority};
use torse_storage::{CheckpointConfig, JournalConfig};
use torse_types::{
    BlockPos, Compatibility, ContentKey, OutputBudget, SectionPos, Tick, TransactionId, WorldId,
};
use torse_world::{
    BlockStateId, BlockStateRegistry, ComponentSchemas, Position, Section, TerrainQuery,
    WorldBounds,
};

const WORLD: WorldId = WorldId(7);
type Codec = WorldCodec<LocalWorldModel>;
type Files = FileServer<WorldHost, Codec>;

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: durable_world <world-directory>")?;
    std::fs::create_dir_all(&path)?;
    let content = compile(m0_fixture_pack())
        .map_err(|errors| std::io::Error::other(format!("content compile failed: {errors:?}")))?;
    let registry = BlockStateRegistry::build(&content)?;
    let schemas = ComponentSchemas::build(&content);
    let compatibility = Compatibility {
        authority_schema: 1,
        rules_digest: content.authority_digest,
        content_digest: content.presentation_digest,
    };
    let config = policy();
    let codec = Codec::new(config.journal.codec_limits)?;
    let directory = WorldDirectory::acquire(&path, WORLD, 65_536)?;
    let mut server = if path.join("checkpoints/manifest.log").try_exists()? {
        Files::open_file(directory, compatibility, codec, config, |binding| {
            Ok(WorldHostRecovery::new(binding, &registry, &schemas))
        })?
    } else {
        Files::create_file(directory, compatibility, codec, config, |binding| {
            let mut world = WorldAuthority::new(
                WORLD,
                registry,
                schemas,
                16,
                WorldBounds::new(0, 16).unwrap(),
                compatibility,
            )
            .map_err(|error| torse_types::Fault::invalid_data(error.to_string()))?;
            let store = world.store_mut();
            store.request(SectionPos::ZERO).unwrap();
            store
                .install(SectionPos::ZERO, Section::empty(16).unwrap())
                .unwrap();
            store.finish_load(SectionPos::ZERO).unwrap();
            WorldHost::new(binding, world)
        })?
    };
    let world = server.authority()?.world();
    let pos = BlockPos::new(1, 1, 1);
    let TerrainQuery::Ready(previous) = world.store().query(pos) else {
        return Err("example block is not loaded".into());
    };
    let next = if previous.is_empty() {
        world
            .registry()
            .default_state(&ContentKey::parse("base:stone")?)
            .unwrap()
    } else {
        BlockStateId::EMPTY
    };
    let sequence = world.seq().next().ok_or("commit sequence exhausted")?;
    let mut transaction = Transaction::new(
        TransactionId(u128::from(sequence.0)),
        ContentKey::parse("base:example")?,
        OrderingKey {
            phase: Phase::Interaction,
            rule_priority: 0,
            stable_task_key: 0,
            local_sequence: 0,
        },
    );
    transaction.read_block(pos, previous).write_block(pos, next);
    if next.is_empty() {
        for (handle, entity) in world.entities().iter() {
            transaction.despawn_entity(handle, entity);
        }
    } else {
        transaction.spawn_entity(Position::new(2.5, 2.0, 2.5), Default::default());
    }
    match server.commit_transaction(&transaction)? {
        LocalCommit::Durable(result) => println!(
            concat!(
                "epoch={} commit_seq={} durable_seq={} previous={} current={}",
                " entities={} next_entity_id={}",
            ),
            server.binding().epoch.0,
            result.publication.at.seq.0,
            result.durability.through.0,
            previous.0,
            next.0,
            server.authority()?.world().entities().len(),
            server.authority()?.world().entities().next_entity_id(),
        ),
        other => return Err(format!("local transaction did not commit: {other:?}").into()),
    }
    let deadline = server.authority()?.resume_point().tick;
    server.shutdown(Tick(deadline.0), &mut NoReplicas)?;
    Ok(())
}

fn policy() -> FileServerConfig {
    let limits = CodecLimits {
        max_frame_bytes: 1_048_576,
        max_field_bytes: 65_536,
        max_depth: 12,
    };
    let budget = OutputBudget::new(8192, 1_048_576);
    FileServerConfig {
        server: ServerConfig {
            snapshot_budget: budget,
            max_invalidations: 0,
            shutdown_max_steps: 8,
            checkpoint_predecessors: 1,
        },
        journal: JournalConfig {
            max_pending_records: 2,
            max_pending_bytes: 2_097_152,
            codec_budget: budget,
            codec_limits: limits,
        },
        checkpoint: CheckpointConfig {
            codec_limits: limits,
            codec_budget: budget,
            max_manifest_bytes: 65_536,
            max_snapshot_files: 4,
        },
    }
}
