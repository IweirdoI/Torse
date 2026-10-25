//! Assembled authoritative world and its recovery path.
//!
//! This is the concrete A-side object a host drives: it owns the world store,
//! the entity store, the compiled registry, and the commit coordinator. Storage
//! hands it prepared changes, snapshots travel through the codec, and recovery
//! rebuilds an authority from a snapshot plus its journal tail.
//!
//! Recovery is a builder, not a world: nothing is usable until `finish`. A
//! snapshot that does not match the expected world, or a journal tail that does
//! not continue the snapshot position, fails the builder and no half-restored
//! authority escapes.

use torse_types::{
    CommitCursor, CommitSeq, Compatibility, ResumePoint, SnapshotMeta, TransactionOutcome,
    TransactionResult, WorldId,
};
use torse_world::{
    BlockStateRegistry, ChangeSet, ComponentSchemas, EntityStore, ExchangeError, RestoredWorld,
    SectionStore, Snapshot, WorldBounds,
};

use crate::coordinator::{
    CommitCoordinator, CommitFault, Committed, PrepareError, PreparedChange, Publication,
};
use crate::transaction::Transaction;

/// Failure produced while assembling an authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityError {
    /// The section side or bounds are unsupported.
    World(ExchangeError),
    /// The entity store rejected its configuration.
    Store(String),
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::World(error) => write!(f, "{error}"),
            Self::Store(detail) => write!(f, "entity store: {detail}"),
        }
    }
}

impl std::error::Error for AuthorityError {}

/// Failure produced while restoring from a snapshot and journal tail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryError {
    /// No snapshot has been applied yet.
    MissingSnapshot,
    /// The snapshot or change belongs to another world.
    WrongWorld {
        /// Expected world identity.
        expected: WorldId,
        /// Identified by the payload.
        actual: WorldId,
    },
    /// A change set does not continue the current position.
    UnexpectedPosition {
        /// Position the change set claims as previous.
        previous: CommitSeq,
        /// Position the recovery currently holds.
        expected: CommitSeq,
    },
    /// A change set jumps past the next commit position.
    NonConsecutive {
        /// Position the change set would assign.
        cursor: CommitSeq,
        /// Position expected after the current one.
        expected: CommitSeq,
    },
    /// The payload could not be applied.
    Payload(ExchangeError),
    /// The restored world cannot be assembled.
    Authority(AuthorityError),
    /// The commit sequence cannot advance.
    SequenceExhausted,
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSnapshot => write!(f, "recovery has no snapshot"),
            Self::WrongWorld { expected, actual } => {
                write!(
                    f,
                    "payload belongs to world {}, expected {}",
                    actual.0, expected.0
                )
            }
            Self::UnexpectedPosition { previous, expected } => write!(
                f,
                "change closes {} but recovery is at {}",
                previous.0, expected.0
            ),
            Self::NonConsecutive { cursor, expected } => {
                write!(f, "change assigns {} but {} is next", cursor.0, expected.0)
            }
            Self::Payload(error) => write!(f, "{error}"),
            Self::Authority(error) => write!(f, "{error}"),
            Self::SequenceExhausted => write!(f, "commit sequence exhausted"),
        }
    }
}

impl std::error::Error for RecoveryError {}

/// The assembled authoritative world.
pub struct WorldAuthority {
    world: WorldId,
    registry: BlockStateRegistry,
    schemas: ComponentSchemas,
    store: SectionStore,
    entities: EntityStore,
    coordinator: CommitCoordinator,
}

impl WorldAuthority {
    pub fn prevalidate_components(
        &self,
        group: &crate::TransactionGroup,
    ) -> Result<crate::ComponentPrevalidation, PrepareError> {
        self.coordinator.prevalidate_components(
            &self.store,
            &self.entities,
            &self.registry,
            &self.schemas,
            group,
        )
    }
    pub fn refresh_components(
        &self,
        token: crate::ComponentPrevalidation,
    ) -> Result<PreparedChange, PrepareError> {
        self.coordinator.refresh_components(&self.entities, token)
    }

    /// Compute a pure movement on a borrowed world. It still requires serial
    /// refresh and the usual reservation/publication/durability path.
    pub fn prevalidate_movement(
        &self,
        group: &crate::TransactionGroup,
    ) -> Result<crate::MovementPrevalidation, PrepareError> {
        self.coordinator.prevalidate_movement(
            &self.store,
            &self.entities,
            &self.registry,
            &self.schemas,
            group,
        )
    }

    /// Refresh only when the phase, terrain, identity generations and exact
    /// source positions still match; otherwise callers must prepare normally.
    pub fn refresh_movement(
        &self,
        token: &crate::MovementPrevalidation,
    ) -> Result<PreparedChange, PrepareError> {
        self.coordinator
            .refresh_movement(&self.store, &self.entities, token)
    }
    /// Prepare an empty world change carrying an atomic host execution-state update.
    pub fn prepare_barrier(&self) -> Result<PreparedChange, CommitFault> {
        self.coordinator.prepare_barrier()
    }
    /// Prepare a durable whole-phase completion after the driver settles its work.
    pub fn prepare_phase_advance(&self) -> Result<PreparedChange, CommitFault> {
        self.coordinator.prepare_phase_advance()
    }

    /// Assemble an empty world.
    pub fn new(
        world: WorldId,
        registry: BlockStateRegistry,
        schemas: ComponentSchemas,
        side: u32,
        bounds: WorldBounds,
        compatibility: Compatibility,
    ) -> Result<Self, AuthorityError> {
        let store = SectionStore::new(side, bounds).map_err(|error| {
            AuthorityError::World(ExchangeError::Store {
                detail: error.to_string(),
            })
        })?;
        let entities =
            EntityStore::new(side).map_err(|error| AuthorityError::Store(error.to_string()))?;
        let mut coordinator = CommitCoordinator::new(world);
        coordinator.set_compatibility(compatibility);
        Ok(Self {
            world,
            registry,
            schemas,
            store,
            entities,
            coordinator,
        })
    }

    /// World identity.
    pub const fn world(&self) -> WorldId {
        self.world
    }

    /// Last committed position.
    pub const fn seq(&self) -> CommitSeq {
        self.coordinator.seq()
    }

    /// Terrain store.
    pub const fn store(&self) -> &SectionStore {
        &self.store
    }

    /// Mutable terrain access for load and generation paths.
    pub fn store_mut(&mut self) -> &mut SectionStore {
        &mut self.store
    }

    /// Entity store.
    pub const fn entities(&self) -> &EntityStore {
        &self.entities
    }

    /// Mutable entity access for initialization and load paths.
    ///
    /// Authoritative movement should use transactions so journal reservation,
    /// conflict checks, and recovery observe the same publication boundary.
    pub fn entities_mut(&mut self) -> &mut EntityStore {
        &mut self.entities
    }

    /// Compiled registry.
    pub const fn registry(&self) -> &BlockStateRegistry {
        &self.registry
    }

    /// Component schemas.
    pub const fn schemas(&self) -> &ComponentSchemas {
        &self.schemas
    }

    /// Commit coordinator.
    pub const fn coordinator(&self) -> &CommitCoordinator {
        &self.coordinator
    }

    /// Mutable coordinator access for compatibility and resume updates.
    pub fn coordinator_mut(&mut self) -> &mut CommitCoordinator {
        &mut self.coordinator
    }

    /// Validate a transaction without changing the world.
    pub fn prepare(&self, transaction: &Transaction) -> Result<PreparedChange, PrepareError> {
        self.coordinator.prepare_with_components(
            &self.store,
            &self.entities,
            &self.registry,
            &self.schemas,
            transaction,
        )
    }

    /// Prepare one multi-source atomic group; see the cross-module contract.
    pub fn prepare_group(
        &self,
        group: &crate::TransactionGroup,
    ) -> Result<PreparedChange, PrepareError> {
        self.coordinator.prepare_group(
            &self.store,
            &self.entities,
            &self.registry,
            &self.schemas,
            group,
        )
    }

    /// Publish a prepared change.
    pub fn publish(&mut self, prepared: PreparedChange) -> Result<Publication, CommitFault> {
        self.coordinator
            .publish_with_entities(&mut self.store, &mut self.entities, prepared)
    }

    /// Prepare and publish one transaction.
    pub fn commit(
        &mut self,
        transaction: &Transaction,
    ) -> Result<TransactionResult<Committed>, CommitFault> {
        self.coordinator.commit_with_components(
            &mut self.store,
            &mut self.entities,
            &self.registry,
            &self.schemas,
            transaction,
        )
    }

    /// Relocate a wedged entity in one hop. The transaction carries a move
    /// whose start overlaps terrain; the destination must be free. Returns
    /// `Ok(false)` when the move was rejected.
    pub fn commit_escape(&mut self, transaction: &crate::Transaction) -> Result<bool, CommitFault> {
        let result = self.coordinator.commit_escape(
            &mut self.store,
            &mut self.entities,
            &self.registry,
            &self.schemas,
            transaction,
        )?;
        Ok(matches!(
            result.outcome,
            TransactionOutcome::Committed { .. }
        ))
    }

    pub fn commit_group(
        &mut self,
        group: &crate::TransactionGroup,
    ) -> Result<TransactionResult<Committed>, CommitFault> {
        self.coordinator.commit_group(
            &mut self.store,
            &mut self.entities,
            &self.registry,
            &self.schemas,
            group,
        )
    }

    /// Capture the whole world at its current commit position.
    pub fn capture_snapshot(&self, meta: SnapshotMeta) -> Result<Snapshot, ExchangeError> {
        Snapshot::capture(meta, &self.store, &self.entities, &self.registry)
    }

    /// Check the complete owned output budget before allocating a world snapshot.
    pub fn capture_snapshot_with_budget(
        &self,
        meta: SnapshotMeta,
        budget: torse_types::OutputBudget,
    ) -> Result<Snapshot, torse_types::Fault> {
        Snapshot::capture_with_budget(meta, &self.store, &self.entities, &self.registry, budget)
    }

    /// Start building a recovered world.
    pub fn begin_restore(&self) -> Recovery<'_> {
        Recovery::new(self.world, &self.registry, &self.schemas)
    }

    /// Consume the authority into its parts.
    pub fn into_parts(
        self,
    ) -> (
        SectionStore,
        EntityStore,
        BlockStateRegistry,
        ComponentSchemas,
        CommitCoordinator,
    ) {
        (
            self.store,
            self.entities,
            self.registry,
            self.schemas,
            self.coordinator,
        )
    }
}

/// Builder that turns a snapshot and its journal tail into an authority.
pub struct Recovery<'a> {
    world: WorldId,
    registry: &'a BlockStateRegistry,
    schemas: &'a ComponentSchemas,
    restored: Option<RestoredWorld>,
    seq: CommitSeq,
    compatibility: Compatibility,
    resume: ResumePoint,
}

impl<'a> Recovery<'a> {
    /// Create a recovery bound to one world's registry.
    pub fn new(
        world: WorldId,
        registry: &'a BlockStateRegistry,
        schemas: &'a ComponentSchemas,
    ) -> Self {
        Self {
            world,
            registry,
            schemas,
            restored: None,
            seq: CommitSeq::INITIAL,
            compatibility: Compatibility {
                authority_schema: 0,
                rules_digest: torse_types::Digest::default(),
                content_digest: torse_types::Digest::default(),
            },
            resume: ResumePoint {
                tick: torse_types::Tick(0),
                next_phase: 0,
                next_step: 0,
            },
        }
    }

    /// Apply the base snapshot exactly once.
    pub fn restore_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), RecoveryError> {
        if self.restored.is_some() {
            return Err(RecoveryError::Payload(ExchangeError::Store {
                detail: "snapshot already applied".to_owned(),
            }));
        }
        if snapshot.meta.cursor.world != self.world {
            return Err(RecoveryError::WrongWorld {
                expected: self.world,
                actual: snapshot.meta.cursor.world,
            });
        }
        let restored = snapshot
            .restore(self.registry, self.schemas)
            .map_err(RecoveryError::Payload)?;
        self.seq = snapshot.meta.cursor.seq;
        self.compatibility = snapshot.meta.compatibility;
        self.resume = snapshot.meta.resume_at;
        self.restored = Some(restored);
        Ok(())
    }

    /// Apply one committed change set onto the restored world.
    ///
    /// The change must continue the current commit position exactly. Applying a
    /// gap or a duplicate is rejected before anything is written.
    pub fn replay(&mut self, changes: &ChangeSet) -> Result<CommitSeq, RecoveryError> {
        let restored = self
            .restored
            .as_mut()
            .ok_or(RecoveryError::MissingSnapshot)?;
        if changes.meta.cursor.world != self.world {
            return Err(RecoveryError::WrongWorld {
                expected: self.world,
                actual: changes.meta.cursor.world,
            });
        }
        if changes.meta.previous != self.seq {
            return Err(RecoveryError::UnexpectedPosition {
                previous: changes.meta.previous,
                expected: self.seq,
            });
        }
        let expected = self.seq.next().ok_or(RecoveryError::SequenceExhausted)?;
        if changes.meta.cursor.seq != expected {
            return Err(RecoveryError::NonConsecutive {
                cursor: changes.meta.cursor.seq,
                expected,
            });
        }
        changes
            .apply_with_components(
                &mut restored.store,
                &mut restored.entities,
                self.registry,
                self.schemas,
            )
            .map_err(RecoveryError::Payload)?;
        self.seq = expected;
        self.compatibility = changes.meta.compatibility;
        self.resume = changes.meta.resume_at;
        Ok(self.seq)
    }

    /// Position the recovery currently holds.
    pub const fn seq(&self) -> CommitSeq {
        self.seq
    }

    /// Finish the builder and produce a usable authority.
    pub fn finish(self) -> Result<WorldAuthority, RecoveryError> {
        let restored = self.restored.ok_or(RecoveryError::MissingSnapshot)?;
        let mut coordinator = CommitCoordinator::new(self.world);
        coordinator.set_compatibility(self.compatibility);
        coordinator.set_resume_point(self.resume);
        // The coordinator starts at the recovered position, so the next prepared
        // change receives exactly the following sequence number.
        let coordinator = coordinator.resumed_at(self.seq);
        Ok(WorldAuthority {
            world: self.world,
            registry: self.registry.clone(),
            schemas: self.schemas.clone(),
            store: restored.store,
            entities: restored.entities,
            coordinator,
        })
    }
}

impl Recovery<'_> {
    /// Commit cursor of the recovered position.
    pub fn cursor(&self) -> CommitCursor {
        CommitCursor {
            world: self.world,
            seq: self.seq,
        }
    }

    /// Abort the builder without producing a world.
    ///
    /// Dropping the value has the same effect; the method exists so a caller can
    /// make the intent explicit at the failure site.
    pub fn abort(self) {}
}
#[cfg(test)]
mod tests {
    use super::*;
    use torse_content::{compile, m0_fixture_pack};
    use torse_types::SectionPos;
    use torse_types::{
        BlockPos, CommitCursor, ContentKey, Digest, Tick, TransactionId, TransactionOutcome,
    };
    use torse_world::{BlockStateId, Section, TerrainQuery};

    use crate::phase::Phase;
    use crate::transaction::OrderingKey;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key is valid")
    }

    fn compatibility() -> Compatibility {
        Compatibility {
            authority_schema: 1,
            rules_digest: Digest([1; 32]),
            content_digest: Digest([2; 32]),
        }
    }

    fn authority() -> WorldAuthority {
        let content = compile(m0_fixture_pack()).expect("fixture content compiles");
        let registry = BlockStateRegistry::build(&content).expect("registry builds");
        let schemas = ComponentSchemas::build(&content);
        let mut authority = WorldAuthority::new(
            WorldId(5),
            registry,
            schemas,
            16,
            WorldBounds::new(0, 64).expect("bounds are valid"),
            compatibility(),
        )
        .expect("authority assembles");
        let store = authority.store_mut();
        store.request(SectionPos::ZERO).expect("request works");
        store
            .install(SectionPos::ZERO, Section::empty(16).expect("side is valid"))
            .expect("install works");
        store.finish_load(SectionPos::ZERO).expect("finish works");
        authority
    }

    fn transaction(id: u128, pos: BlockPos, state: BlockStateId) -> Transaction {
        let mut transaction = Transaction::new(
            TransactionId(id),
            key("base:test"),
            OrderingKey {
                phase: Phase::Interaction,
                rule_priority: 0,
                stable_task_key: id as u64,
                local_sequence: 0,
            },
        );
        transaction
            .read_block(pos, BlockStateId::EMPTY)
            .write_block(pos, state);
        transaction
    }

    fn snapshot_meta(authority: &WorldAuthority) -> SnapshotMeta {
        SnapshotMeta {
            cursor: CommitCursor {
                world: authority.world(),
                seq: authority.seq(),
            },
            compatibility: authority.coordinator().compatibility(),
            resume_at: ResumePoint {
                tick: Tick(1),
                next_phase: 3,
                next_step: 0,
            },
        }
    }

    #[test]
    fn snapshot_plus_journal_tail_recovers_the_same_world() {
        let mut authority = authority();
        let stone = authority
            .registry()
            .default_state(&key("base:stone"))
            .expect("stone exists");
        let glass = authority
            .registry()
            .default_state(&key("base:glass_slab"))
            .expect("glass exists");

        authority
            .commit(&transaction(1, BlockPos::new(1, 1, 1), stone))
            .expect("commit runs");
        let snapshot = authority
            .capture_snapshot(snapshot_meta(&authority))
            .expect("snapshot captures");
        assert_eq!(snapshot.meta.cursor.seq, CommitSeq(1));

        let committed = authority
            .commit(&transaction(2, BlockPos::new(2, 1, 1), glass))
            .expect("commit runs");
        let change = match &committed.outcome {
            TransactionOutcome::Committed { value, .. } => {
                crate::payload::changes_from_committed(authority.registry(), value)
                    .expect("payload converts")
            }
            TransactionOutcome::Rejected(code) => panic!("unexpected rejection {code:?}"),
        };
        assert_eq!(change.meta.cursor.seq, CommitSeq(2));

        let mut recovery = authority.begin_restore();
        recovery
            .restore_snapshot(&snapshot)
            .expect("snapshot applies");
        recovery.replay(&change).expect("journal tail applies");
        assert_eq!(recovery.seq(), CommitSeq(2));
        let mut recovered = recovery.finish().expect("finish works");

        assert_eq!(recovered.seq(), CommitSeq(2));
        assert_eq!(
            recovered.store().query(BlockPos::new(1, 1, 1)),
            TerrainQuery::Ready(stone)
        );
        assert_eq!(
            recovered.store().query(BlockPos::new(2, 1, 1)),
            TerrainQuery::Ready(glass)
        );
        assert_eq!(
            recovered.coordinator().compatibility(),
            compatibility(),
            "compatibility survives recovery"
        );

        // The recovered authority continues numbering where the journal ended.
        recovered
            .commit(&transaction(3, BlockPos::new(3, 1, 1), stone))
            .expect("commit runs");
        assert_eq!(recovered.seq(), CommitSeq(3));
    }

    #[test]
    fn replay_rejects_a_gap_in_the_journal() {
        let mut authority = authority();
        let stone = authority
            .registry()
            .default_state(&key("base:stone"))
            .expect("stone exists");
        authority
            .commit(&transaction(4, BlockPos::new(1, 1, 1), stone))
            .expect("commit runs");
        let snapshot = authority
            .capture_snapshot(snapshot_meta(&authority))
            .expect("snapshot captures");

        let mut recovery = authority.begin_restore();
        recovery
            .restore_snapshot(&snapshot)
            .expect("snapshot applies");
        let change = ChangeSet {
            entity_lifecycle: None,
            component_edits: Vec::new(),
            entity_moves: Vec::new(),
            meta: torse_types::CommitMeta {
                cursor: CommitCursor {
                    world: WorldId(5),
                    seq: CommitSeq(2),
                },
                previous: CommitSeq(0),
                compatibility: compatibility(),
                resume_at: ResumePoint {
                    tick: Tick(2),
                    next_phase: 0,
                    next_step: 0,
                },
            },
            edits: Vec::new(),
        };
        let error = recovery.replay(&change).expect_err("gap is rejected");
        assert!(matches!(error, RecoveryError::UnexpectedPosition { .. }));
        assert_eq!(recovery.seq(), CommitSeq(1));
    }

    #[test]
    fn payload_from_another_world_is_rejected() {
        let authority = authority();
        let mut recovery = authority.begin_restore();
        let mut snapshot = authority
            .capture_snapshot(SnapshotMeta {
                cursor: CommitCursor {
                    world: WorldId(9),
                    seq: CommitSeq::INITIAL,
                },
                compatibility: compatibility(),
                resume_at: ResumePoint {
                    tick: Tick(0),
                    next_phase: 0,
                    next_step: 0,
                },
            })
            .expect("snapshot captures");
        let error = recovery
            .restore_snapshot(&snapshot)
            .expect_err("foreign world is rejected");
        assert!(matches!(error, RecoveryError::WrongWorld { .. }));
        snapshot.meta.cursor.world = WorldId(5);
        recovery
            .restore_snapshot(&snapshot)
            .expect("own world applies");
    }

    #[test]
    fn finish_requires_a_snapshot() {
        let authority = authority();
        let recovery = authority.begin_restore();
        assert!(matches!(
            recovery.finish(),
            Err(RecoveryError::MissingSnapshot)
        ));
    }
}
