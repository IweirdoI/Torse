#[allow(dead_code)]
mod support;

use support::*;
use torse_inventory::{ItemCatalog, Transfer, plan_transfers};
use torse_types::{Compatibility, Digest, TransactionId};

#[test]
fn catalog_identity_covers_membership_limits_and_canonical_order() {
    let a = ItemCatalog::new([(key("base:stone"), 64), (key("base:glass"), 16)]).unwrap();
    let b = ItemCatalog::new([(key("base:glass"), 16), (key("base:stone"), 64)]).unwrap();
    let c = ItemCatalog::new([(key("base:stone"), 63), (key("base:glass"), 16)]).unwrap();
    let d = ItemCatalog::new([(key("base:stone"), 64)]).unwrap();
    assert_eq!(a.rules_digest(), b.rules_digest());
    assert_ne!(a.rules_digest(), c.rules_digest());
    assert_ne!(a.rules_digest(), d.rules_digest());
    let base = Compatibility {
        authority_schema: 7,
        rules_digest: Digest([2; 32]),
        content_digest: Digest([3; 32]),
    };
    let bound = a.bind_compatibility(base);
    assert_eq!(bound.authority_schema, base.authority_schema);
    assert_eq!(bound.content_digest, base.content_digest);
    assert_ne!(bound.rules_digest, base.rules_digest);
    assert_eq!(bound, b.bind_compatibility(base));
    assert_ne!(bound, c.bind_compatibility(base));
}

#[test]
fn shared_endpoint_batch_conserves_items_and_late_failure_changes_nothing() {
    let (mut world, _, catalog, ids) = setup();
    let grant = actor(&ids);
    let ordering = request(ids[0], ids[1], 1).ordering;
    let transfers = [
        Transfer {
            source: ids[0],
            source_slot: 0,
            destination: ids[1],
            destination_slot: 1,
            count: 1,
        },
        Transfer {
            source: ids[1],
            source_slot: 1,
            destination: ids[2],
            destination_slot: 0,
            count: 1,
        },
    ];
    let before = world.capture_snapshot(meta(&world)).unwrap();
    let mut bad = transfers;
    bad[1].count = 2;
    assert!(
        plan_transfers(
            &grant.view(&world),
            &catalog,
            &bad,
            TransactionId(1),
            ordering
        )
        .is_err()
    );
    assert_eq!(world.capture_snapshot(meta(&world)).unwrap(), before);
    let tx = plan_transfers(
        &grant.view(&world),
        &catalog,
        &transfers,
        TransactionId(2),
        ordering,
    )
    .unwrap();
    assert_eq!(tx.components.len(), 2, "unchanged intermediary is omitted");
    world.commit(&tx).unwrap();
    assert_eq!(total(&world, &ids, &catalog), 1);
    assert!(
        torse_inventory::Inventory::from_fields(fields(&world, ids[0]), &catalog)
            .unwrap()
            .slots()[0]
            .is_none()
    );
    assert_eq!(
        torse_inventory::Inventory::from_fields(fields(&world, ids[2]), &catalog)
            .unwrap()
            .slots()[0]
            .as_ref()
            .unwrap()
            .count,
        1
    );
    assert!(
        plan_transfers(
            &grant.view(&world),
            &catalog,
            &transfers,
            TransactionId(3),
            ordering
        )
        .is_err()
    );
    assert!(
        plan_transfers(
            &actor(&ids[..1]).view(&world),
            &catalog,
            &transfers,
            TransactionId(4),
            ordering
        )
        .is_err()
    );
}
