//! Single-coordinator commit protocol with explicit prepare and publish steps.
//!
//! Processing a transaction follows the contract's order:
//!
//! 1. [`CommitCoordinator::prepare`] revalidates every recorded read and every
//!    write target, then returns a [`PreparedChange`]. The world is unchanged,
//!    no sequence number is assigned, and the record carries the metadata the
//!    storage side needs to stage it.
//! 2. Storage decides whether it can take the record. A rejected or dropped
//!    preparation changes nothing: no sequence, no durable identity, no cells.
//! 3. [`CommitCoordinator::publish`] applies the prepared edits and moves the
//!    commit position. It cannot fail for business reasons because every check
//!    already happened in step 1; a failure here is an engine invariant
//!    violation and stops the world instead of rejecting one transaction.
//!
//! [`CommitCoordinator::commit`] keeps the one-call form for tests and for hosts
//! without a journal. It performs prepare and publish back to back.

use torse_types::{
    BlockPos, CommitCursor, CommitMeta, CommitSeq, Compatibility, ContentKey, Digest, EntityHandle,
    EntityId, RejectCode, ResumePoint, Revision, SectionPos, Tick, TransactionId,
    TransactionOutcome, TransactionResult, WorldId,
};
use torse_world::{
    BlockEdit, BlockStateRegistry, ComponentBatch, ComponentEdit, ComponentSchemas, EntityError,
    EntityLifecycle, EntityMove, EntityStore, LifecycleBatch, SectionStore, StoreError,
    TerrainQuery,
};

use crate::phase::Phase;
use crate::transaction::Transaction;

/// A change that passed every check but has not affected the world yet.
///
/// The record is what the journal receives before publication. Dropping it is a
/// valid outcome and consumes nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedChange {
    meta: CommitMeta,
    observed_resume: ResumePoint,
    transaction: TransactionId,
    source: ContentKey,
    phase: Phase,
    edits: Vec<BlockEdit>,
    touched: Vec<SectionPos>,
    entity_moves: Vec<EntityMove>,
    movement_reads: Vec<(BlockPos, TerrainQuery)>,
    movement_handles: Vec<(EntityHandle, EntityId)>,
    component_edits: Vec<ComponentEdit>,
    component_batch: ComponentBatch,
    lifecycle_batch: Option<LifecycleBatch>,
}

impl PreparedChange {
    /// Complete creation/removal records and proposed allocator state, before publication.
    pub fn entity_lifecycle(&self) -> Option<&EntityLifecycle> {
        self.lifecycle_batch.as_ref().map(LifecycleBatch::change)
    }
    /// Exact component replacements available before journal reservation.
    pub fn component_edits(&self) -> &[ComponentEdit] {
        &self.component_edits
    }
    /// Exact translations for staging before the authority publishes them.
    pub fn entity_moves(&self) -> &[EntityMove] {
        &self.entity_moves
    }

    /// Metadata that will appear in the commit record.
    pub const fn meta(&self) -> CommitMeta {
        self.meta
    }

    /// Transaction that produced this change.
    pub const fn transaction(&self) -> TransactionId {
        self.transaction
    }

    /// Module or system that produced the transaction.
    pub fn source(&self) -> &ContentKey {
        &self.source
    }

    /// Phase in which the transaction was produced.
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Writes that publication would apply, in commit order.
    pub fn edits(&self) -> &[BlockEdit] {
        &self.edits
    }

    /// Sections the change touches, in coordinate order.
    pub fn touched(&self) -> &[SectionPos] {
        &self.touched
    }
}

/// Result of a successful publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publication {
    /// Subscriptions whose queued data must be discarded before any further send.
    pub invalidated_replicas: Vec<torse_types::SubscriptionId>,
    /// Commit position that became visible.
    pub cursor: CommitCursor,
    /// Sections whose revisions changed, in coordinate order.
    pub touched: Vec<(SectionPos, Revision)>,
    /// Persistent identities moved by this publication, in transaction order.
    pub moved: Vec<EntityId>,
    /// Component owners and schemas changed, in transaction order.
    pub components: Vec<(EntityId, ContentKey)>,
}

/// Changes produced by one committed transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Committed {
    /// Metadata of the committed record.
    pub meta: CommitMeta,
    /// Ordered writes that were applied.
    pub edits: Vec<BlockEdit>,
    /// Collision-resolved entity translations suitable for deterministic replay.
    pub entity_moves: Vec<EntityMove>,
    /// Protected component replacements with stable identity and expected fields.
    pub component_edits: Vec<ComponentEdit>,
    /// Complete creation/removal effects and the committed allocation watermark.
    pub entity_lifecycle: Option<EntityLifecycle>,
    /// Sections touched by the commit, in coordinate order.
    pub touched: Vec<(SectionPos, Revision)>,
}

/// Why preparation did not produce a record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrepareError {
    /// The transaction is rejected for a recoverable reason.
    Rejected(RejectCode),
    /// The commit sequence cannot advance without wrapping.
    SequenceExhausted,
}

/// Terminal failure that stops the world instead of rejecting one transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitFault {
    /// The commit sequence cannot advance without wrapping.
    SequenceExhausted,
    /// A prepared record no longer matches the coordinator or the world.
    Invariant(&'static str),
}

impl std::fmt::Display for CommitFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SequenceExhausted => write!(f, "commit sequence exhausted"),
            Self::Invariant(detail) => write!(f, "commit invariant violated: {detail}"),
        }
    }
}

impl std::error::Error for CommitFault {}

/// Owner of the world's commit order.
#[derive(Clone, Copy, Debug)]
pub struct CommitCoordinator {
    world: WorldId,
    seq: CommitSeq,
    compatibility: Compatibility,
    resume: ResumePoint,
}

impl CommitCoordinator {
    /// Create a coordinator at the baseline commit position.
    ///
    /// Compatibility digests start at zero until the host supplies the compiled
    /// content and rule summaries through [`Self::set_compatibility`].
    pub fn new(world: WorldId) -> Self {
        Self {
            world,
            seq: CommitSeq::INITIAL,
            compatibility: Compatibility {
                authority_schema: 0,
                rules_digest: Digest::default(),
                content_digest: Digest::default(),
            },
            resume: ResumePoint {
                tick: Tick(0),
                next_phase: Phase::Input.index_u16(),
                next_step: 0,
            },
        }
    }

    /// Last committed position.
    pub const fn seq(&self) -> CommitSeq {
        self.seq
    }

    /// Move the coordinator to a recovered position.
    ///
    /// Recovery uses this to continue numbering after a journal tail; the next
    /// prepared change must receive `seq + 1`.
    pub const fn resumed_at(mut self, seq: CommitSeq) -> Self {
        self.seq = seq;
        self
    }

    /// World identity used in commit cursors.
    pub const fn world(&self) -> WorldId {
        self.world
    }

    /// Compatibility data recorded in every commit.
    pub const fn compatibility(&self) -> Compatibility {
        self.compatibility
    }

    /// Replace the compatibility data, for example after content compilation.
    pub fn set_compatibility(&mut self, compatibility: Compatibility) {
        self.compatibility = compatibility;
    }

    /// Resume position recorded in every commit.
    pub const fn resume_point(&self) -> ResumePoint {
        self.resume
    }

    /// Update the resume position.
    ///
    /// The tick driver sets this before each phase and after each tick.
    pub fn set_resume_point(&mut self, resume: ResumePoint) {
        self.resume = resume;
    }

    /// Prepare a durable execution-state barrier without changing the phase.
    pub fn prepare_barrier(&self) -> Result<PreparedChange, CommitFault> {
        let mut prepared = self.prepare_empty(self.resume)?;
        prepared.source = ContentKey::parse("torse:execution").expect("static key");
        Ok(prepared)
    }

    fn prepare_empty(&self, resume_at: ResumePoint) -> Result<PreparedChange, CommitFault> {
        let seq = self.seq.next().ok_or(CommitFault::SequenceExhausted)?;
        let phase = Phase::from_index(self.resume.next_phase)
            .ok_or(CommitFault::Invariant("invalid phase"))?;
        Ok(PreparedChange {
            meta: CommitMeta {
                cursor: CommitCursor {
                    world: self.world,
                    seq,
                },
                previous: self.seq,
                compatibility: self.compatibility,
                resume_at,
            },
            observed_resume: self.resume,
            transaction: TransactionId(u128::from(seq.0)),
            source: ContentKey::parse("torse:phase").expect("static key"),
            phase,
            edits: Vec::new(),
            touched: Vec::new(),
            entity_moves: Vec::new(),
            movement_reads: Vec::new(),
            movement_handles: Vec::new(),
            component_edits: Vec::new(),
            component_batch: ComponentBatch::default(),
            lifecycle_batch: None,
        })
    }

    /// Prepare a journaled whole-phase completion without publishing progress.
    /// The trusted driver must have settled all work in the current phase first.
    pub fn prepare_phase_advance(&self) -> Result<PreparedChange, CommitFault> {
        let resume_at = crate::phase::next_resume_point(self.resume).ok_or(
            CommitFault::Invariant("phase progress is invalid or exhausted"),
        )?;
        self.prepare_empty(resume_at)
    }

    /// Validate a transaction and build its record without touching the world.
    pub fn prepare(
        &self,
        store: &SectionStore,
        transaction: &Transaction,
    ) -> Result<PreparedChange, PrepareError> {
        if !transaction.movements.is_empty()
            || !transaction.components.is_empty()
            || !transaction.spawns.is_empty()
            || !transaction.despawns.is_empty()
        {
            return Err(PrepareError::Rejected(RejectCode::InvalidData));
        }
        self.prepare_blocks(store, transaction)
    }

    /// Resolve terrain writes and entity translations without publishing either.
    pub fn prepare_with_entities(
        &self,
        store: &SectionStore,
        entities: &EntityStore,
        registry: &BlockStateRegistry,
        transaction: &Transaction,
    ) -> Result<PreparedChange, PrepareError> {
        if !transaction.components.is_empty()
            || !transaction.spawns.is_empty()
            || !transaction.despawns.is_empty()
        {
            return Err(PrepareError::Rejected(RejectCode::InvalidData));
        }
        self.prepare_world(store, entities, registry, transaction)
    }

    /// Validate a mixed group, enforcing generic component ownership and shape.
    ///
    /// Item conservation and other gameplay rules remain module responsibilities;
    /// the kernel checks ownership, identity, source fields, and schema limits.
    pub fn prepare_with_components(
        &self,
        store: &SectionStore,
        entities: &EntityStore,
        registry: &BlockStateRegistry,
        schemas: &ComponentSchemas,
        transaction: &Transaction,
    ) -> Result<PreparedChange, PrepareError> {
        let lifecycle = crate::lifecycle::prepare_lifecycle(store, entities, schemas, transaction)
            .map_err(PrepareError::Rejected)?;
        if transaction.components.len() > torse_world::MAX_COMPONENT_EDITS {
            return Err(PrepareError::Rejected(RejectCode::QuotaExceeded));
        }
        for edit in &transaction.components {
            let schema = schemas
                .get(&edit.schema)
                .ok_or(PrepareError::Rejected(RejectCode::InvalidData))?;
            if schema.owner_module != transaction.source {
                return Err(PrepareError::Rejected(RejectCode::PermissionDenied));
            }
        }
        let batch = entities
            .prepare_components(&transaction.components, schemas)
            .map_err(|error| {
                PrepareError::Rejected(match error {
                    EntityError::StaleHandle => RejectCode::MissingTarget,
                    EntityError::ComponentConflict => RejectCode::Conflict,
                    _ => RejectCode::InvalidData,
                })
            })?;
        let mut prepared = self.prepare_world(store, entities, registry, transaction)?;
        prepared.component_edits = transaction.components.clone();
        prepared.component_batch = batch;
        prepared.lifecycle_batch = lifecycle;
        Ok(prepared)
    }

    fn prepare_world(
        &self,
        store: &SectionStore,
        entities: &EntityStore,
        registry: &BlockStateRegistry,
        transaction: &Transaction,
    ) -> Result<PreparedChange, PrepareError> {
        let mut prepared = self.prepare_blocks(store, transaction)?;
        let resolved = crate::movement::prepare_movements(
            store,
            entities,
            registry,
            &prepared.edits,
            &transaction.movements,
        )
        .map_err(PrepareError::Rejected)?;
        prepared.entity_moves = resolved.moves;
        prepared.movement_reads = resolved.reads;
        prepared.movement_handles = transaction
            .movements
            .iter()
            .map(|request| (request.handle, request.id))
            .collect();
        Ok(prepared)
    }

    fn prepare_blocks(
        &self,
        store: &SectionStore,
        transaction: &Transaction,
    ) -> Result<PreparedChange, PrepareError> {
        if (transaction.mutations.is_empty()
            && transaction.movements.is_empty()
            && transaction.components.is_empty()
            && transaction.spawns.is_empty()
            && transaction.despawns.is_empty())
            || has_duplicate_targets(transaction)
        {
            return Err(PrepareError::Rejected(RejectCode::InvalidData));
        }

        for precondition in &transaction.preconditions {
            match store.query(precondition.pos) {
                TerrainQuery::Ready(current) if current == precondition.expected => {}
                TerrainQuery::Ready(_) => {
                    return Err(PrepareError::Rejected(RejectCode::Conflict));
                }
                TerrainQuery::NotLoaded => {
                    return Err(PrepareError::Rejected(RejectCode::NotLoaded));
                }
                TerrainQuery::OutOfBounds => {
                    return Err(PrepareError::Rejected(RejectCode::MissingTarget));
                }
            }
        }

        let edits: Vec<BlockEdit> = transaction
            .mutations
            .iter()
            .map(|mutation| BlockEdit {
                pos: mutation.pos,
                state: mutation.state,
            })
            .collect();
        let touched = store
            .validate_edits(&edits)
            .map_err(|error| PrepareError::Rejected(code_for(&error)))?;

        let Some(next) = self.seq.next() else {
            return Err(PrepareError::SequenceExhausted);
        };
        let cursor = CommitCursor {
            world: self.world,
            seq: next,
        };
        Ok(PreparedChange {
            meta: CommitMeta {
                cursor,
                previous: self.seq,
                compatibility: self.compatibility,
                resume_at: self.resume,
            },
            observed_resume: self.resume,
            transaction: transaction.id,
            source: transaction.source.clone(),
            phase: transaction.ordering.phase,
            edits,
            touched,
            entity_moves: Vec::new(),
            movement_reads: Vec::new(),
            movement_handles: Vec::new(),
            component_edits: Vec::new(),
            component_batch: ComponentBatch::default(),
            lifecycle_batch: None,
        })
    }

    /// Apply a prepared record and advance the commit position.
    ///
    /// The record must still sit at the current position. Anything else means a
    /// second record was published in between, which the single-coordinator loop
    /// does not allow.
    pub fn publish(
        &mut self,
        store: &mut SectionStore,
        prepared: PreparedChange,
    ) -> Result<Publication, CommitFault> {
        if !prepared.entity_moves.is_empty()
            || !prepared.component_edits.is_empty()
            || prepared.lifecycle_batch.is_some()
        {
            return Err(CommitFault::Invariant(
                "entity movements require entity-aware publication",
            ));
        }
        self.publish_blocks(store, prepared)
    }

    /// Publish an already resolved mixed group under exclusive authority access.
    ///
    /// All entity targets and collision reads are checked before terrain writes.
    /// No rule or collision callback executes after the first authoritative write.
    pub fn publish_with_entities(
        &mut self,
        store: &mut SectionStore,
        entities: &mut EntityStore,
        prepared: PreparedChange,
    ) -> Result<Publication, CommitFault> {
        if let Some(batch) = &prepared.lifecycle_batch {
            batch
                .validate(entities)
                .map_err(|_| CommitFault::Invariant("prepared lifecycle observations changed"))?;
            batch
                .change()
                .validate_world(store)
                .map_err(|_| CommitFault::Invariant("prepared creation terrain changed"))?;
        }
        prepared
            .component_batch
            .validate(entities)
            .map_err(|_| CommitFault::Invariant("prepared component observations changed"))?;
        if prepared
            .movement_reads
            .iter()
            .any(|(pos, expected)| store.query(*pos) != *expected)
            || prepared
                .movement_handles
                .iter()
                .any(|(handle, id)| entities.get(*handle).is_none_or(|entity| entity.id != *id))
        {
            return Err(CommitFault::Invariant(
                "prepared movement observations changed",
            ));
        }
        entities
            .validate_moves(&prepared.entity_moves)
            .map_err(|_| CommitFault::Invariant("prepared entity targets changed"))?;
        let entity_moves = prepared.entity_moves.clone();
        let components = prepared
            .component_edits
            .iter()
            .map(|edit| (edit.id, edit.schema.clone()))
            .collect();
        let component_batch = prepared.component_batch.clone();
        let lifecycle_batch = prepared.lifecycle_batch.clone();
        let mut publication = self.publish_blocks(store, prepared)?;
        entities
            .apply_moves(&entity_moves)
            .map_err(|_| CommitFault::Invariant("prevalidated entity moves no longer apply"))?;
        publication.moved = entity_moves.iter().map(|movement| movement.id).collect();
        entities.publish_components(component_batch).map_err(|_| {
            CommitFault::Invariant("prevalidated component group no longer applies")
        })?;
        publication.components = components;
        if let Some(batch) = lifecycle_batch {
            entities.publish_lifecycle(batch).map_err(|_| {
                CommitFault::Invariant("prevalidated entity lifecycle no longer applies")
            })?;
        }
        Ok(publication)
    }

    fn publish_blocks(
        &mut self,
        store: &mut SectionStore,
        prepared: PreparedChange,
    ) -> Result<Publication, CommitFault> {
        let expected = self.seq.next();
        if prepared.meta.previous != self.seq
            || prepared.meta.cursor.world != self.world
            || Some(prepared.meta.cursor.seq) != expected
            || prepared.observed_resume != self.resume
            || prepared.meta.compatibility != self.compatibility
        {
            return Err(CommitFault::Invariant(
                "prepared change is not at the current commit position",
            ));
        }
        let touched = store
            .apply_edits(&prepared.edits)
            .map_err(|_| CommitFault::Invariant("prepared change no longer applies"))?;
        self.seq = prepared.meta.cursor.seq;
        self.resume = prepared.meta.resume_at;
        Ok(Publication {
            invalidated_replicas: Vec::new(),
            cursor: prepared.meta.cursor,
            touched,
            moved: Vec::new(),
            components: Vec::new(),
        })
    }

    /// Prepare and publish one transaction in a single call.
    pub fn commit(
        &mut self,
        store: &mut SectionStore,
        transaction: &Transaction,
    ) -> Result<TransactionResult<Committed>, CommitFault> {
        let prepared = match self.prepare(store, transaction) {
            Ok(prepared) => prepared,
            Err(PrepareError::Rejected(code)) => return Ok(rejected(transaction.id, code)),
            Err(PrepareError::SequenceExhausted) => {
                return Err(CommitFault::SequenceExhausted);
            }
        };
        let meta = prepared.meta;
        let edits = prepared.edits.clone();
        let publication = self.publish(store, prepared)?;
        Ok(TransactionResult {
            transaction: transaction.id,
            outcome: TransactionOutcome::Committed {
                at: publication.cursor,
                value: Committed {
                    meta,
                    edits,
                    entity_moves: Vec::new(),
                    component_edits: Vec::new(),
                    entity_lifecycle: None,
                    touched: publication.touched,
                },
            },
        })
    }

    /// Prepare and publish a mixed group, for hosts without a journal.
    pub fn commit_with_entities(
        &mut self,
        store: &mut SectionStore,
        entities: &mut EntityStore,
        registry: &BlockStateRegistry,
        transaction: &Transaction,
    ) -> Result<TransactionResult<Committed>, CommitFault> {
        let prepared = self.prepare_with_entities(store, entities, registry, transaction);
        self.finish_commit(store, entities, transaction.id, prepared)
    }

    /// Prepare and publish a schema-aware group for hosts without a journal.
    pub fn commit_with_components(
        &mut self,
        store: &mut SectionStore,
        entities: &mut EntityStore,
        registry: &BlockStateRegistry,
        schemas: &ComponentSchemas,
        transaction: &Transaction,
    ) -> Result<TransactionResult<Committed>, CommitFault> {
        let prepared =
            self.prepare_with_components(store, entities, registry, schemas, transaction);
        self.finish_commit(store, entities, transaction.id, prepared)
    }

    fn finish_commit(
        &mut self,
        store: &mut SectionStore,
        entities: &mut EntityStore,
        transaction: TransactionId,
        prepared: Result<PreparedChange, PrepareError>,
    ) -> Result<TransactionResult<Committed>, CommitFault> {
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(PrepareError::Rejected(code)) => return Ok(rejected(transaction, code)),
            Err(PrepareError::SequenceExhausted) => return Err(CommitFault::SequenceExhausted),
        };
        let meta = prepared.meta;
        let edits = prepared.edits.clone();
        let entity_moves = prepared.entity_moves.clone();
        let component_edits = prepared.component_edits.clone();
        let entity_lifecycle = prepared.entity_lifecycle().cloned();
        let publication = self.publish_with_entities(store, entities, prepared)?;
        Ok(TransactionResult {
            transaction,
            outcome: TransactionOutcome::Committed {
                at: publication.cursor,
                value: Committed {
                    meta,
                    edits,
                    entity_moves,
                    component_edits,
                    entity_lifecycle,
                    touched: publication.touched,
                },
            },
        })
    }

    /// Commit transactions in deterministic order.
    ///
    /// Each transaction is prepared and published before the next one is
    /// prepared, so a prepared record never outlives its own step and the
    /// arbitration order is exactly the sorted order.
    pub fn commit_ordered(
        &mut self,
        store: &mut SectionStore,
        transactions: Vec<Transaction>,
    ) -> Result<Vec<TransactionResult<Committed>>, CommitFault> {
        let mut results = Vec::with_capacity(transactions.len());
        for transaction in Transaction::order(transactions) {
            results.push(self.commit(store, &transaction)?);
        }
        Ok(results)
    }
}

fn rejected<T>(transaction: TransactionId, code: RejectCode) -> TransactionResult<T> {
    TransactionResult {
        transaction,
        outcome: TransactionOutcome::Rejected(code),
    }
}

fn has_duplicate_targets(transaction: &Transaction) -> bool {
    let mut targets: Vec<BlockPos> = Vec::with_capacity(transaction.mutations.len());
    for mutation in &transaction.mutations {
        if targets.contains(&mutation.pos) {
            return true;
        }
        targets.push(mutation.pos);
    }
    false
}

fn code_for(error: &StoreError) -> RejectCode {
    match error {
        StoreError::NotLoaded { .. }
        | StoreError::UnknownSection { .. }
        | StoreError::NotEditable { .. } => RejectCode::NotLoaded,
        StoreError::OutOfBounds { .. } => RejectCode::InvalidData,
        StoreError::Occupied { .. } | StoreError::EmptyCell { .. } => RejectCode::Conflict,
        _ => RejectCode::InvalidData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torse_types::TransactionOutcome;
    use torse_world::{BlockStateId, Section, WorldBounds};

    use crate::transaction::OrderingKey;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key is valid")
    }

    fn store() -> SectionStore {
        let bounds = WorldBounds::new(0, 64).expect("bounds are valid");
        let mut store = SectionStore::new(16, bounds).expect("side is valid");
        for section in [SectionPos::ZERO, SectionPos::new(1, 0, 0)] {
            store.request(section).expect("request is accepted");
            store
                .install(section, Section::empty(16).expect("side is valid"))
                .expect("install is accepted");
            store.finish_load(section).expect("load finishes");
        }
        store
    }

    fn tx(id: u128) -> Transaction {
        Transaction::new(
            TransactionId(id),
            key("base:test"),
            OrderingKey {
                phase: Phase::Simulation,
                rule_priority: 0,
                stable_task_key: 0,
                local_sequence: 0,
            },
        )
    }

    fn committed(result: &TransactionResult<Committed>) -> &Committed {
        match &result.outcome {
            TransactionOutcome::Committed { value, .. } => value,
            TransactionOutcome::Rejected(code) => panic!("expected commit, got {code:?}"),
        }
    }

    fn rejection(result: &TransactionResult<Committed>) -> RejectCode {
        match result.outcome {
            TransactionOutcome::Rejected(code) => code,
            TransactionOutcome::Committed { .. } => panic!("expected rejection"),
        }
    }

    #[test]
    fn prepare_does_not_touch_the_world() {
        let store = store();
        let coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(1);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(5));
        let prepared = coordinator
            .prepare(&store, &transaction)
            .expect("prepare works");
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId::EMPTY)
        );
        assert_eq!(prepared.edits().len(), 1);
        assert_eq!(prepared.touched(), &[SectionPos::ZERO]);
        assert_eq!(prepared.phase(), Phase::Simulation);
    }

    #[test]
    fn prepared_meta_carries_position_compatibility_and_resume_point() {
        let store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(9));
        let compatibility = Compatibility {
            authority_schema: 3,
            rules_digest: Digest([1; 32]),
            content_digest: Digest([2; 32]),
        };
        coordinator.set_compatibility(compatibility);
        let resume = ResumePoint {
            tick: Tick(4),
            next_phase: Phase::Interaction.index_u16(),
            next_step: 7,
        };
        coordinator.set_resume_point(resume);

        let mut transaction = tx(2);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
        let prepared = coordinator
            .prepare(&store, &transaction)
            .expect("prepare works");
        let meta = prepared.meta();
        assert_eq!(meta.previous, CommitSeq::INITIAL);
        assert_eq!(meta.cursor.world, WorldId(9));
        assert_eq!(meta.cursor.seq, CommitSeq(1));
        assert_eq!(meta.compatibility, compatibility);
        assert_eq!(meta.resume_at, resume);
    }

    #[test]
    fn dropped_preparation_consumes_nothing() {
        let mut store = store();
        let coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(3);
        transaction.write_block(BlockPos::new(2, 1, 2), BlockStateId(4));
        let prepared = coordinator
            .prepare(&store, &transaction)
            .expect("prepare works");
        drop(prepared);
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
        assert_eq!(
            store.query(BlockPos::new(2, 1, 2)),
            TerrainQuery::Ready(BlockStateId::EMPTY),
            "a dropped preparation must not write"
        );
        // The same transaction can be prepared again because nothing changed.
        coordinator
            .prepare(&store, &transaction)
            .expect("prepare works again");
        let _ = &mut store;
    }

    #[test]
    fn phase_preparation_is_inert_and_rechecks_observed_progress() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let initial = coordinator.resume_point();
        let prepared = coordinator.prepare_phase_advance().unwrap();
        assert_eq!(prepared.meta().resume_at.next_phase, 1);
        drop(prepared);
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
        assert_eq!(coordinator.resume_point(), initial);

        let mut transaction = tx(1);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(5));
        let stale = coordinator.prepare(&store, &transaction).unwrap();
        let progress = coordinator.prepare_phase_advance().unwrap();
        coordinator.set_resume_point(ResumePoint {
            next_phase: 2,
            ..initial
        });
        assert!(coordinator.publish(&mut store, progress).is_err());
        assert!(coordinator.publish(&mut store, stale).is_err());
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId::EMPTY)
        );

        let progress = coordinator.prepare_phase_advance().unwrap();
        coordinator.publish(&mut store, progress).unwrap();
        assert_eq!(coordinator.resume_point().next_phase, 3);
        assert_eq!(coordinator.seq(), CommitSeq(1));
    }

    #[test]
    fn phase_preparation_rejects_exhausted_sequence_and_progress_without_mutation() {
        let mut coordinator = CommitCoordinator::new(WorldId(1)).resumed_at(CommitSeq(u64::MAX));
        assert_eq!(
            coordinator.prepare_phase_advance().unwrap_err(),
            CommitFault::SequenceExhausted
        );
        assert_eq!(coordinator.seq(), CommitSeq(u64::MAX));
        coordinator = CommitCoordinator::new(WorldId(1));
        let point = ResumePoint {
            tick: Tick(u64::MAX),
            next_phase: 5,
            next_step: 0,
        };
        coordinator.set_resume_point(point);
        assert!(coordinator.prepare_phase_advance().is_err());
        assert_eq!(coordinator.resume_point(), point);
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
    }

    #[test]
    fn publish_applies_and_advances_the_commit_position() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(4);
        transaction
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(5))
            .write_block(BlockPos::new(17, 1, 1), BlockStateId(6));
        let prepared = coordinator
            .prepare(&store, &transaction)
            .expect("prepare works");
        let publication = coordinator
            .publish(&mut store, prepared)
            .expect("publish works");
        assert_eq!(publication.cursor.seq, CommitSeq(1));
        assert_eq!(publication.touched.len(), 2);
        assert_eq!(coordinator.seq(), CommitSeq(1));
        assert_eq!(
            store.query(BlockPos::new(17, 1, 1)),
            TerrainQuery::Ready(BlockStateId(6))
        );
    }

    #[test]
    fn stale_read_is_rejected_at_prepare_without_consuming_a_sequence() {
        let mut store = store();
        store
            .edit(BlockPos::new(1, 1, 1), BlockStateId(9))
            .expect("setup edit");
        let coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(5);
        transaction
            .read_block(BlockPos::new(1, 1, 1), BlockStateId::EMPTY)
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
        let error = coordinator
            .prepare(&store, &transaction)
            .expect_err("prepare rejects the stale read");
        assert_eq!(error, PrepareError::Rejected(RejectCode::Conflict));
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId(9))
        );
    }

    #[test]
    fn unloaded_write_target_is_rejected_at_prepare() {
        let store = store();
        let coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(6);
        transaction.write_block(BlockPos::new(40, 1, 1), BlockStateId(3));
        let error = coordinator
            .prepare(&store, &transaction)
            .expect_err("prepare rejects the missing section");
        assert_eq!(error, PrepareError::Rejected(RejectCode::NotLoaded));
    }

    #[test]
    fn one_call_commit_still_works() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(7));
        let mut transaction = tx(7);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(5));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        let record = committed(&result);
        assert_eq!(record.meta.cursor.seq, CommitSeq(1));
        assert_eq!(record.meta.previous, CommitSeq::INITIAL);
        assert_eq!(coordinator.seq(), CommitSeq(1));
    }

    #[test]
    fn publishing_out_of_order_is_an_invariant_failure() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut first = tx(8);
        first.write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
        let mut second = tx(9);
        second.write_block(BlockPos::new(2, 1, 1), BlockStateId(4));

        let stale = coordinator.prepare(&store, &first).expect("prepare works");
        coordinator
            .commit(&mut store, &second)
            .expect("commit works");
        // `stale` was prepared at position 0; the coordinator is now at 1.
        let fault = coordinator
            .publish(&mut store, stale)
            .expect_err("out-of-order publication must fail");
        assert!(matches!(fault, CommitFault::Invariant(_)));
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId::EMPTY),
            "the stale record must not be applied"
        );
    }

    #[test]
    fn publishing_after_the_target_disappears_is_an_invariant_failure() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(10);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
        let prepared = coordinator
            .prepare(&store, &transaction)
            .expect("prepare works");
        store.unload(SectionPos::ZERO).expect("section unloads");
        let fault = coordinator
            .publish(&mut store, prepared)
            .expect_err("publication cannot apply");
        assert!(matches!(fault, CommitFault::Invariant(_)));
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
    }

    #[test]
    fn sequence_exhaustion_is_terminal() {
        let store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        coordinator.seq = CommitSeq(u64::MAX);
        let mut transaction = tx(11);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
        assert_eq!(
            coordinator.prepare(&store, &transaction),
            Err(PrepareError::SequenceExhausted)
        );
    }

    #[test]
    fn two_readers_of_one_cell_commit_in_order_and_the_second_conflicts() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut first = tx(20);
        first
            .read_block(BlockPos::new(2, 1, 2), BlockStateId::EMPTY)
            .write_block(BlockPos::new(2, 1, 2), BlockStateId(4));
        let mut second = tx(21);
        second
            .read_block(BlockPos::new(2, 1, 2), BlockStateId::EMPTY)
            .write_block(BlockPos::new(2, 1, 2), BlockStateId(6));

        let results = coordinator
            .commit_ordered(&mut store, vec![second, first])
            .expect("commits run");
        // Identical ordering keys fall back to transaction identity, so the
        // lower identity wins the cell and the later one reports a conflict.
        assert_eq!(committed(&results[0]).meta.cursor.seq, CommitSeq(1));
        assert_eq!(rejection(&results[1]), RejectCode::Conflict);
        assert_eq!(
            store.query(BlockPos::new(2, 1, 2)),
            TerrainQuery::Ready(BlockStateId(4))
        );
    }

    #[test]
    fn different_cells_in_one_section_do_not_conflict() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut first = tx(22);
        first
            .read_block(BlockPos::new(1, 1, 1), BlockStateId::EMPTY)
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(3));
        let mut second = tx(23);
        second
            .read_block(BlockPos::new(2, 1, 1), BlockStateId::EMPTY)
            .write_block(BlockPos::new(2, 1, 1), BlockStateId(4));
        let results = coordinator
            .commit_ordered(&mut store, vec![first, second])
            .expect("commits run");
        assert_eq!(committed(&results[0]).meta.cursor.seq, CommitSeq(1));
        assert_eq!(committed(&results[1]).meta.cursor.seq, CommitSeq(2));
    }

    #[test]
    fn write_into_an_unloaded_section_changes_nothing() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(24);
        transaction
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(3))
            .write_block(BlockPos::new(40, 1, 1), BlockStateId(3));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(rejection(&result), RejectCode::NotLoaded);
        assert_eq!(
            store.query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(BlockStateId::EMPTY)
        );
    }

    #[test]
    fn read_only_transaction_is_rejected_without_a_sequence() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(25);
        transaction.read_block(BlockPos::ZERO, BlockStateId::EMPTY);
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(rejection(&result), RejectCode::InvalidData);
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
    }

    #[test]
    fn duplicate_targets_are_rejected() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(26);
        transaction
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(3))
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(4));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(rejection(&result), RejectCode::InvalidData);
        assert_eq!(coordinator.seq(), CommitSeq::INITIAL);
    }

    #[test]
    fn cursor_carries_world_and_sequence() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(99));
        let mut transaction = tx(27);
        transaction.write_block(BlockPos::new(1, 1, 1), BlockStateId(2));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        assert_eq!(committed(&result).meta.cursor.world, WorldId(99));
        assert_eq!(committed(&result).meta.cursor.seq, CommitSeq(1));
    }

    #[test]
    fn touched_sections_are_reported_for_publishing() {
        let mut store = store();
        let mut coordinator = CommitCoordinator::new(WorldId(1));
        let mut transaction = tx(28);
        transaction
            .write_block(BlockPos::new(1, 1, 1), BlockStateId(2))
            .write_block(BlockPos::new(17, 1, 1), BlockStateId(2));
        let result = coordinator
            .commit(&mut store, &transaction)
            .expect("commit runs");
        let record = committed(&result);
        assert_eq!(record.touched.len(), 2);
        assert_eq!(record.touched[0].0, SectionPos::ZERO);
        assert_eq!(record.touched[1].0, SectionPos::new(1, 0, 0));
    }
}
