//! Unit tests for validation and basic type behavior.

use super::*;

#[test]
fn content_key_parses_namespaced_value() {
    // The accessors must preserve the namespace/path split rather than only
    // validating the combined display form.
    let key = ContentKey::parse("base:blocks/stone").unwrap();
    assert_eq!(key.namespace(), "base");
    assert_eq!(key.path(), "blocks/stone");
    assert_eq!(key.to_string(), "base:blocks/stone");
}

#[test]
fn content_key_rejects_invalid_names() {
    // Uppercase names and path normalization hazards must be rejected at the
    // boundary, not silently interpreted by later filesystems or registries.
    assert_eq!(
        ContentKey::parse("Base:stone"),
        Err(ContentKeyError::InvalidNamespace)
    );
    assert_eq!(
        ContentKey::parse("base:stone//rough"),
        Err(ContentKeyError::InvalidPath)
    );
    assert_eq!(
        ContentKey::parse("stone"),
        Err(ContentKeyError::MissingNamespace)
    );
}

#[test]
fn content_key_enforces_lengths() {
    // Length checks are part of the persistent identity contract, so the exact
    // failing component is reported by a dedicated error.
    let long_namespace = format!("{}:stone", "a".repeat(65));
    assert!(matches!(
        ContentKey::parse(&long_namespace),
        Err(ContentKeyError::NamespaceTooLong { .. })
    ));
}

#[test]
fn negative_coordinates_use_floor_sections_and_nonnegative_local_values() {
    // This locks down the negative-coordinate convention shared by all systems:
    // floor section indices plus non-negative local coordinates.
    let (section, local) = BlockPos::new(-1, -32, 31).to_section(32).unwrap();
    assert_eq!(section, SectionPos::new(-1, -1, 0));
    assert_eq!(local, LocalPos { x: 31, y: 0, z: 31 });
}

#[test]
fn negative_coordinate_chunks_survive_roundtrip() {
    // Round-tripping across section boundaries catches a common off-by-one bug
    // caused by truncating division or signed remainders.
    for position in [
        BlockPos::new(-1, -1, -1),
        BlockPos::new(-32, -33, -64),
        BlockPos::new(0, 31, 32),
        BlockPos::new(1_000_000, -2_000_000, 3_000_000),
    ] {
        let (section, local) = position.to_section(32).unwrap();
        assert_eq!(section.to_block(local, 32).unwrap(), position);
    }
}

#[test]
fn coordinate_arithmetic_rejects_overflow() {
    // Wrapped coordinates could alias unrelated world locations, so overflow is
    // a hard error rather than a debug-only assertion.
    assert_eq!(
        BlockPos::new(i64::MAX, 0, 0).checked_add(1, 0, 0),
        Err(SpatialError::CoordinateOverflow)
    );
}

#[test]
fn local_coordinate_must_be_inside_section() {
    // Local indices are unsigned but still need an explicit upper bound.
    assert_eq!(
        SectionPos::ZERO.to_block(LocalPos { x: 32, y: 0, z: 0 }, 32),
        Err(SpatialError::LocalCoordinateOutOfRange { side: 32 })
    );
}

#[test]
fn revisions_and_ids_enforce_nonzero_start() {
    // Zero is reserved by the ID contracts; revisions advance monotonically and
    // do not wrap when exhausted.
    assert_eq!(Revision::INITIAL.next(), Some(Revision(1)));
    assert!(EntityId(1).validate().is_ok());
    assert!(EntityId(0).validate().is_err());
    assert!(EventId(0).validate().is_err());
    assert!(TransactionId(0).validate().is_err());
}

#[test]
fn transaction_and_event_results_carry_stable_results() {
    // Results retain their transaction/event identity and include the commit
    // cursor or target tick needed by replay and diagnostics.
    let transaction = TransactionId(7);
    let result = TransactionResult {
        transaction,
        outcome: TransactionOutcome::Committed {
            at: CommitCursor::initial(WorldId(3)),
            value: "ok",
        },
    };
    assert_eq!(result.transaction, transaction);

    let event = ScheduledEvent {
        id: EventId(9),
        target_tick: Tick(42),
        input: "work",
    };
    assert_eq!(event.target_tick, Tick(42));
}

// ============================================================================================
// CommitSeq Tests
// ============================================================================================

#[test]
fn commit_seq_initial_is_zero() {
    assert_eq!(CommitSeq::INITIAL.0, 0);
}

#[test]
fn commit_seq_follows_validates_consecutive() {
    let prev = CommitSeq(5);
    let next = CommitSeq(6);
    assert!(next.follows(prev).is_ok());
}

#[test]
fn commit_seq_follows_rejects_gap() {
    let prev = CommitSeq(5);
    let next = CommitSeq(7);
    assert!(matches!(
        next.follows(prev),
        Err(ValidationError::NonConsecutiveCommit { .. })
    ));
}

#[test]
fn commit_seq_follows_rejects_duplicate() {
    let prev = CommitSeq(5);
    let next = CommitSeq(5);
    assert!(matches!(
        next.follows(prev),
        Err(ValidationError::NonConsecutiveCommit { .. })
    ));
}

#[test]
fn commit_seq_next_increments() {
    let seq = CommitSeq(42);
    assert_eq!(seq.next(), Some(CommitSeq(43)));
}

#[test]
fn commit_seq_next_detects_overflow() {
    let seq = CommitSeq(u64::MAX);
    assert_eq!(seq.next(), None);
}

#[test]
fn commit_seq_follows_rejects_wrap_after_max() {
    let prev = CommitSeq(u64::MAX);
    let next = CommitSeq(0);
    assert_eq!(next.follows(prev), Err(ValidationError::CommitSeqOverflow));
}

// ============================================================================================
// DurableSeq Tests
// ============================================================================================

#[test]
fn durable_seq_initial_is_zero() {
    assert_eq!(DurableSeq::INITIAL.0, 0);
}

#[test]
fn durable_seq_within_accepts_equal() {
    let durable = DurableSeq(10);
    let commit = CommitSeq(10);
    assert!(durable.within(commit).is_ok());
}

#[test]
fn durable_seq_within_accepts_less() {
    let durable = DurableSeq(5);
    let commit = CommitSeq(10);
    assert!(durable.within(commit).is_ok());
}

#[test]
fn durable_seq_within_rejects_greater() {
    let durable = DurableSeq(15);
    let commit = CommitSeq(10);
    assert!(matches!(
        durable.within(commit),
        Err(ValidationError::DurableExceedsCommit { .. })
    ));
}

// ============================================================================================
// RequestSeq Tests
// ============================================================================================

#[test]
fn request_seq_validates_nonzero() {
    let seq = RequestSeq(1);
    assert!(seq.validate().is_ok());
}

#[test]
fn request_seq_rejects_zero() {
    let seq = RequestSeq(0);
    assert!(matches!(
        seq.validate(),
        Err(ValidationError::RequestSeqZero)
    ));
}

// ============================================================================================
// ReplicaSeq Tests
// ============================================================================================

#[test]
fn replica_seq_validates_nonzero() {
    let seq = ReplicaSeq(1);
    assert!(seq.validate().is_ok());
}

#[test]
fn replica_seq_rejects_zero() {
    let seq = ReplicaSeq(0);
    assert!(matches!(
        seq.validate(),
        Err(ValidationError::ReplicaSeqZero)
    ));
}

// ============================================================================================
// CommitCursor Tests
// ============================================================================================

#[test]
fn commit_cursor_initial_creates_baseline() {
    let world = WorldId(123);
    let cursor = CommitCursor::initial(world);
    assert_eq!(cursor.world, world);
    assert_eq!(cursor.seq, CommitSeq::INITIAL);
}

// ============================================================================================
// CommitMeta Tests
// ============================================================================================

#[test]
fn commit_meta_validates_consecutive() {
    let meta = CommitMeta {
        cursor: CommitCursor {
            world: WorldId(1),
            seq: CommitSeq(6),
        },
        previous: CommitSeq(5),
        compatibility: Compatibility {
            authority_schema: 1,
            rules_digest: Digest([0; 32]),
            content_digest: Digest([0; 32]),
        },
        resume_at: ResumePoint {
            tick: Tick(100),
            next_phase: 0,
            next_step: 0,
        },
    };
    assert!(meta.validate().is_ok());
}

#[test]
fn commit_meta_rejects_non_consecutive() {
    let meta = CommitMeta {
        cursor: CommitCursor {
            world: WorldId(1),
            seq: CommitSeq(8),
        },
        previous: CommitSeq(5),
        compatibility: Compatibility {
            authority_schema: 1,
            rules_digest: Digest([0; 32]),
            content_digest: Digest([0; 32]),
        },
        resume_at: ResumePoint {
            tick: Tick(100),
            next_phase: 0,
            next_step: 0,
        },
    };
    assert!(meta.validate().is_err());
}

// ============================================================================================
// Durability Tests
// ============================================================================================

#[test]
fn durability_covers_exact_match() {
    let binding = Binding {
        world: WorldId(1),
        epoch: WorldEpoch(100),
        compatibility: Compatibility {
            authority_schema: 1,
            rules_digest: Digest([0; 32]),
            content_digest: Digest([0; 32]),
        },
    };
    let durability = Durability {
        binding,
        through: DurableSeq(10),
    };
    let target = CommitCursor {
        world: WorldId(1),
        seq: CommitSeq(10),
    };
    assert!(durability.covers(target));
}

#[test]
fn durability_covers_earlier_commit() {
    let binding = Binding {
        world: WorldId(1),
        epoch: WorldEpoch(100),
        compatibility: Compatibility {
            authority_schema: 1,
            rules_digest: Digest([0; 32]),
            content_digest: Digest([0; 32]),
        },
    };
    let durability = Durability {
        binding,
        through: DurableSeq(10),
    };
    let target = CommitCursor {
        world: WorldId(1),
        seq: CommitSeq(5),
    };
    assert!(durability.covers(target));
}

#[test]
fn durability_does_not_cover_later_commit() {
    let binding = Binding {
        world: WorldId(1),
        epoch: WorldEpoch(100),
        compatibility: Compatibility {
            authority_schema: 1,
            rules_digest: Digest([0; 32]),
            content_digest: Digest([0; 32]),
        },
    };
    let durability = Durability {
        binding,
        through: DurableSeq(10),
    };
    let target = CommitCursor {
        world: WorldId(1),
        seq: CommitSeq(15),
    };
    assert!(!durability.covers(target));
}

#[test]
fn durability_does_not_cover_different_world() {
    let binding = Binding {
        world: WorldId(1),
        epoch: WorldEpoch(100),
        compatibility: Compatibility {
            authority_schema: 1,
            rules_digest: Digest([0; 32]),
            content_digest: Digest([0; 32]),
        },
    };
    let durability = Durability {
        binding,
        through: DurableSeq(10),
    };
    let target = CommitCursor {
        world: WorldId(2),
        seq: CommitSeq(5),
    };
    assert!(!durability.covers(target));
}

// ============================================================================================
// OutputBudget Tests
// ============================================================================================

#[test]
fn output_budget_allows_totals_at_limits() {
    let budget = OutputBudget::new(10, 1000);
    assert!(budget.allows(5, 500));
    assert!(budget.allows(10, 1000));
}

#[test]
fn output_budget_rejects_items_over_limit() {
    let budget = OutputBudget::new(10, 1000);
    assert!(!budget.allows(11, 100));
}

#[test]
fn output_budget_rejects_bytes_over_limit() {
    let budget = OutputBudget::new(10, 1000);
    assert!(!budget.allows(5, 1001));
}

// ============================================================================================
// Fault Tests
// ============================================================================================

#[test]
fn fault_creation() {
    let fault = Fault::new(FaultCode::InvalidData, "test error");
    assert_eq!(fault.code, FaultCode::InvalidData);
    assert_eq!(fault.context, "test error");
}

#[test]
fn fault_from_validation_error() {
    let validation_err = ValidationError::RequestSeqZero;
    let fault: Fault = validation_err.into();
    assert_eq!(fault.code, FaultCode::InvalidData);
    assert!(fault.context.contains("request sequence"));
}

#[test]
fn fault_display_includes_code_and_context() {
    let fault = Fault::resource_limit("queue full");
    assert_eq!(fault.to_string(), "ResourceLimit: queue full");
}
