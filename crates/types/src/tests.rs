//! Unit tests for validation and basic type behavior.

use super::*;

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
