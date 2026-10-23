//! Reproducible A4 acceptance: bounded movement, journal staging, and replay.
//!
//! This exercises the same concrete authority used by world recovery. The
//! exchange payload is captured before publication, as a real journal requires;
//! no file codec, transport, graphics context, or disk state is involved.

use torse_content::{compile, m0_fixture_pack};
use torse_sim::{OrderingKey, Phase, Transaction, WorldAuthority, changes_from_prepared};
use torse_types::{
    BlockPos, CommitCursor, Compatibility, ContentKey, Digest, EntityHandle, SectionPos,
    SnapshotMeta, TransactionId, TransactionOutcome, WorldId,
};
use torse_world::{BlockStateRegistry, ComponentSchemas, Position, Section, WorldBounds};

/// Acceptance counters printed by the command-line runner.
#[derive(Debug)]
pub struct MovementReport {
    pub moved_entities: usize,
    pub discarded_preparations: usize,
    pub negative_x: f64,
    pub wall_x: f64,
    pub unloaded_x: f64,
    pub recovered_seq: u64,
    pub continued_seq: u64,
    pub indexed_entities: usize,
    pub replayed_equal: bool,
}

type ScenarioResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Run the complete scenario and fail on every acceptance regression.
pub fn run() -> ScenarioResult<MovementReport> {
    let content = compile(m0_fixture_pack()).map_err(|errors| format!("content: {errors:?}"))?;
    let registry = BlockStateRegistry::build(&content)?;
    let stone = registry
        .default_state(&key("base:stone"))
        .ok_or("stone is missing")?;
    let mut authority = WorldAuthority::new(
        WorldId(29),
        registry,
        ComponentSchemas::build(&content),
        16,
        WorldBounds::new(0, 32)?,
        Compatibility {
            authority_schema: 1,
            rules_digest: Digest([7; 32]),
            content_digest: content.presentation_digest,
        },
    )?;
    for x in [-1, 0] {
        let pos = SectionPos::new(x, 0, 0);
        authority.store_mut().request(pos)?;
        authority.store_mut().install(pos, Section::empty(16)?)?;
        authority.store_mut().finish_load(pos)?;
    }
    for y in 1..=2 {
        authority.store_mut().edit(BlockPos::new(4, y, 2), stone)?;
    }
    let mut handles = Vec::new();
    for x in [0.5, 2.0, 14.0] {
        handles.push(authority.entities_mut().spawn(Position::new(x, 2.0, 2.0))?);
    }
    let baseline = authority.capture_snapshot(meta(&authority))?;
    let mut transaction = transaction(1);
    for (handle, delta) in handles
        .iter()
        .zip([[-1.0, 0.0, 0.0], [12.0, 0.0, 0.0], [4.0, 0.0, 0.0]])
    {
        let entity = authority.entities().get(*handle).ok_or("missing entity")?;
        transaction.move_entity(*handle, entity.id, entity.position, delta, [0.3, 0.9, 0.3]);
    }
    let discarded = authority
        .prepare(&transaction)
        .map_err(|e| format!("prepare: {e:?}"))?;
    drop(discarded);
    if authority.capture_snapshot(meta(&authority))? != baseline {
        return Err("discarding a preparation changed the authority".into());
    }
    let prepared = authority
        .prepare(&transaction)
        .map_err(|e| format!("prepare: {e:?}"))?;
    let tail = changes_from_prepared(authority.registry(), &prepared)?;
    let publication = authority.publish(prepared)?;
    let negative_x = x(&authority, handles[0])?;
    let wall_x = x(&authority, handles[1])?;
    let unloaded_x = x(&authority, handles[2])?;
    if negative_x != -0.5 || (wall_x - 3.7).abs() > 0.001 || (unloaded_x - 15.7).abs() > 0.001 {
        return Err("negative seam, wall, or unloaded boundary acceptance failed".into());
    }
    if authority
        .entities()
        .entities_in_section(SectionPos::new(-1, 0, 0))
        .count()
        != 1
    {
        return Err("negative section index was not updated".into());
    }
    let mut recovery = authority.begin_restore();
    recovery.restore_snapshot(&baseline)?;
    recovery.replay(&tail)?;
    let mut recovered = recovery.finish()?;
    let recovered_seq = recovered.seq().0;
    let replayed_equal = recovered.capture_snapshot(meta(&recovered))?
        == authority.capture_snapshot(meta(&authority))?;
    if !replayed_equal {
        return Err("entity replay differs from the live world".into());
    }
    let first_id = tail.entity_moves[0].id;
    let handle = recovered
        .entities()
        .handle_of(first_id)
        .ok_or("recovered identity is missing")?;
    let entity = recovered
        .entities()
        .get(handle)
        .ok_or("recovered handle is stale")?;
    let mut next = self::transaction(2);
    next.move_entity(
        handle,
        entity.id,
        entity.position,
        [-0.5, 0.0, 0.0],
        [0.3, 0.9, 0.3],
    );
    if !matches!(
        recovered.commit(&next)?.outcome,
        TransactionOutcome::Committed { .. }
    ) {
        return Err("recovered authority refused a valid continuation".into());
    }
    if recovered.seq().0 != 2 || recovered.entities().indexed_count() != 3 {
        return Err("continuation sequence or entity index is inconsistent".into());
    }
    Ok(MovementReport {
        moved_entities: publication.moved.len(),
        discarded_preparations: 1,
        negative_x,
        wall_x,
        unloaded_x,
        recovered_seq,
        continued_seq: recovered.seq().0,
        indexed_entities: recovered.entities().indexed_count(),
        replayed_equal,
    })
}

fn meta(authority: &WorldAuthority) -> SnapshotMeta {
    SnapshotMeta {
        cursor: CommitCursor {
            world: authority.world(),
            seq: authority.seq(),
        },
        compatibility: authority.coordinator().compatibility(),
        resume_at: authority.coordinator().resume_point(),
    }
}

fn transaction(id: u128) -> Transaction {
    Transaction::new(
        TransactionId(id),
        key("base:motion"),
        OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: id as u64,
            local_sequence: 0,
        },
    )
}

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("scenario key")
}

fn x(authority: &WorldAuthority, handle: EntityHandle) -> ScenarioResult<f64> {
    Ok(authority
        .entities()
        .get(handle)
        .ok_or("entity is missing")?
        .position
        .x)
}

#[cfg(test)]
mod tests {
    #[test]
    fn movement_scenario_holds_acceptance_properties() {
        let report = super::run().expect("movement scenario");
        assert_eq!(report.moved_entities, 3);
        assert_eq!(report.discarded_preparations, 1);
        assert!(report.replayed_equal);
        assert_eq!(
            (
                report.recovered_seq,
                report.continued_seq,
                report.indexed_entities
            ),
            (1, 2, 3)
        );
    }
}
