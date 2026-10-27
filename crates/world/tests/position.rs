use torse_types::{BlockPos, EntityId, SectionPos};
use torse_world::{ComponentSet, EntityError, EntityMove, EntityStore, Position};

#[test]
fn checked_positions_reject_nonfinite_and_saturating_cast_aliases() {
    let lower = i64::MIN as f64;
    let upper = -lower;
    let last = f64::from_bits(upper.to_bits() - 1);
    for coordinate in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        -f64::MAX,
        upper,
        f64::from_bits(lower.to_bits() + 1),
    ] {
        for position in [
            Position::new(coordinate, 0.0, 0.0),
            Position::new(0.0, coordinate, 0.0),
            Position::new(0.0, 0.0, coordinate),
        ] {
            assert_eq!(position.checked_block(), None);
            assert_eq!(position.section(16), None);
        }
    }
    assert_eq!(
        Position::new(lower, 0.0, last).checked_block(),
        Some(BlockPos::new(i64::MIN, 0, last as i64))
    );
    assert_eq!(
        Position::new(-0.001, -16.001, 15.999).checked_block(),
        Some(BlockPos::new(-1, -17, 15))
    );
    assert_eq!(
        Position::new(-0.001, -16.001, 15.999).section(16),
        Some(SectionPos::new(-1, -2, 0))
    );
    assert_eq!(Position::new(0.0, 0.0, 0.0).section(0), None);
}

#[test]
fn invalid_spawns_and_restores_do_not_consume_identity_or_free_slots() {
    let mut store = EntityStore::new(16).unwrap();
    let first = store.spawn(Position::new(0.5, 0.5, 0.5)).unwrap();
    store.despawn(first).unwrap();
    let next = store.next_entity_id();
    for coordinate in [f64::NAN, f64::INFINITY, f64::MAX, -(i64::MIN as f64)] {
        let position = Position::new(coordinate, 0.0, 0.0);
        assert_eq!(store.spawn(position), Err(EntityError::InvalidPosition));
        assert_eq!(
            store.insert_restored(EntityId(50), position, ComponentSet::new()),
            Err(EntityError::InvalidPosition)
        );
        assert_eq!(store.next_entity_id(), next);
        assert!(store.is_empty());
        assert_eq!(store.indexed_count(), 0);
        assert!(store.handle_of(EntityId(50)).is_none());
    }
    let second = store.spawn(Position::new(0.5, 0.5, 0.5)).unwrap();
    assert_eq!(second.slot, first.slot);
    assert_eq!(second.generation, first.generation + 1);
    assert_eq!(store.get(second).unwrap().id, EntityId(next));
}

#[test]
fn invalid_direct_and_replayed_moves_leave_positions_and_indices_unchanged() {
    let mut store = EntityStore::new(16).unwrap();
    let position = Position::new(-0.5, 0.5, 0.5);
    let handle = store.spawn(position).unwrap();
    let id = store.get(handle).unwrap().id;
    for coordinate in [f64::NAN, f64::NEG_INFINITY, f64::MAX, -(i64::MIN as f64)] {
        let invalid = Position::new(0.0, 0.0, coordinate);
        assert_eq!(
            store.move_to(handle, invalid),
            Err(EntityError::InvalidPosition)
        );
        let moves = [EntityMove {
            id,
            previous_bits: position.to_bits(),
            position_bits: invalid.to_bits(),
        }];
        assert_eq!(store.apply_moves(&moves), Err(EntityError::InvalidPosition));
        assert_eq!(store.get(handle).unwrap().position, position);
        assert_eq!(store.indexed_count(), 1);
        assert_eq!(
            store.entities_in_section(SectionPos::new(-1, 0, 0)).count(),
            1
        );
    }
    store
        .move_to(handle, Position::new(16.5, 0.5, 0.5))
        .unwrap();
    assert_eq!(
        store.entities_in_section(SectionPos::new(-1, 0, 0)).count(),
        0
    );
    assert_eq!(
        store.entities_in_section(SectionPos::new(1, 0, 0)).count(),
        1
    );
}

#[test]
fn representable_extreme_coordinates_remain_indexable_and_exhaustion_is_atomic() {
    for value in [
        i64::MIN as f64,
        f64::from_bits((-(i64::MIN as f64)).to_bits() - 1),
    ] {
        let mut store = EntityStore::new(16).unwrap();
        let position = Position::new(value, 0.0, 0.0);
        let handle = store.spawn(position).unwrap();
        assert_eq!(
            store
                .entities_in_section(position.section(16).unwrap())
                .count(),
            1
        );
        assert_eq!(store.get(handle).unwrap().position, position);
        store.restore_next_entity_id(u128::MAX).unwrap();
        assert_eq!(store.spawn(position), Err(EntityError::IdExhausted));
        assert_eq!(store.len(), 1);
        assert_eq!(store.indexed_count(), 1);
        assert_eq!(store.next_entity_id(), u128::MAX);
    }
}
