//! Cross-module atomic commits: one task, several sources, one commit.
//!
//! Two fake modules own one component schema each, standing in for a machine
//! module and an inventory module. The tests pin the contract semantics:
//! members keep their own ownership, writes stay disjoint, one group consumes
//! one sequence number, and any member rejection leaves the world untouched.

use torse_content::{
    ComponentField, ComponentFieldType, ComponentSchema, ContentPack, PersistencePolicy, compile,
    m0_fixture_pack,
};
use torse_sim::action::register_action_module;
use torse_sim::{OrderingKey, Phase, Transaction, TransactionGroup, WorldAuthority};
use torse_types::{
    BlockPos, Compatibility, ContentKey, Digest, EntityId, RejectCode, ReplicationDeclaration,
    SectionPos, TransactionId, TransactionOutcome, WorldId,
};
use torse_world::{
    BlockStateRegistry, ComponentEdit, ComponentFields, ComponentSchemas, ComponentValue, Position,
    Section, WorldBounds,
};

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).unwrap_or_else(|error| panic!("key {value}: {error:?}"))
}

fn schema(owner: &str, name: &str) -> ComponentSchema {
    ComponentSchema {
        id: key(&format!("{owner}:schema/{name}")),
        schema_version: 1,
        owner_module: key(&format!("{owner}:module")),
        fields: vec![ComponentField {
            id: key(&format!("{owner}:field/{name}")),
            field_type: ComponentFieldType::Unsigned32,
            required: true,
        }],
        persistence: PersistencePolicy::Always,
        replication: ReplicationDeclaration::SERVER_PRIVATE,
        migration_binding: None,
    }
}

fn setup() -> (WorldAuthority, ComponentSchemas, EntityId, EntityId) {
    let mut pack: ContentPack = m0_fixture_pack();
    register_action_module(
        &mut pack,
        key("machine:module"),
        key("machine:state"),
        vec![schema("machine", "progress")],
        Vec::new(),
        Vec::new(),
    );
    register_action_module(
        &mut pack,
        key("store:module"),
        key("store:state"),
        vec![schema("store", "stock")],
        Vec::new(),
        Vec::new(),
    );
    let content = compile(pack).expect("content");
    let registry = BlockStateRegistry::build(&content).expect("registry");
    let schemas = ComponentSchemas::build(&content);
    let mut world = WorldAuthority::new(
        WorldId(1),
        registry,
        schemas.clone(),
        16,
        WorldBounds::new(0, 32).expect("bounds"),
        Compatibility {
            authority_schema: 1,
            rules_digest: Digest([1; 32]),
            content_digest: content.presentation_digest,
        },
    )
    .expect("authority");
    for pos in [SectionPos::ZERO, SectionPos::new(1, 0, 0)] {
        world.store_mut().request(pos).expect("request");
        world
            .store_mut()
            .install(pos, Section::empty(16).expect("section"))
            .expect("install");
        world.store_mut().finish_load(pos).expect("finish");
    }
    let machine = spawn_with(&mut world, &schemas, "machine", "progress", 0);
    let store = spawn_with(&mut world, &schemas, "store", "stock", 1);
    (world, schemas, machine, store)
}

fn spawn_with(
    world: &mut WorldAuthority,
    schemas: &ComponentSchemas,
    owner: &str,
    name: &str,
    value: u32,
) -> EntityId {
    let handle = world
        .entities_mut()
        .spawn(Position::new(2.0, 2.0, 2.0))
        .expect("spawn");
    let id = world.entities().get(handle).expect("entity").id;
    let mut fields = ComponentFields::new();
    fields.insert(
        key(&format!("{owner}:field/{name}")),
        ComponentValue::U32(value),
    );
    world
        .entities_mut()
        .components_mut(handle)
        .expect("entity")
        .insert(schemas, &key(&format!("{owner}:schema/{name}")), fields)
        .expect("component");
    id
}

fn member(source: &str, id: u64, edits: Vec<ComponentEdit>) -> Transaction {
    let mut tx = Transaction::new(
        TransactionId(u128::from(id)),
        key(&format!("{source}:module")),
        OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: 42,
            local_sequence: 0,
        },
    );
    tx.components = edits;
    tx
}

fn progress_edit(machine: EntityId, previous: u32, next: u32) -> ComponentEdit {
    ComponentEdit {
        id: machine,
        schema: key("machine:schema/progress"),
        previous: Some(fields("machine", "progress", previous)),
        fields: Some(fields("machine", "progress", next)),
    }
}

fn stock_edit(store: EntityId, previous: u32, next: u32) -> ComponentEdit {
    ComponentEdit {
        id: store,
        schema: key("store:schema/stock"),
        previous: Some(fields("store", "stock", previous)),
        fields: Some(fields("store", "stock", next)),
    }
}

fn fields(owner: &str, name: &str, value: u32) -> ComponentFields {
    let mut fields = ComponentFields::new();
    fields.insert(
        key(&format!("{owner}:field/{name}")),
        ComponentValue::U32(value),
    );
    fields
}

fn grinding_group(machine: EntityId, store: EntityId, id: u64) -> TransactionGroup {
    TransactionGroup::new(
        TransactionId(u128::from(id)),
        OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: 42,
            local_sequence: 0,
        },
        vec![
            member("store", id * 10 + 1, vec![stock_edit(store, 1, 0)]),
            member("machine", id * 10 + 2, vec![progress_edit(machine, 0, 1)]),
        ],
    )
    .expect("group")
}

fn committed(
    world: &mut WorldAuthority,
    group: &TransactionGroup,
) -> TransactionOutcome<torse_sim::Committed> {
    world.commit_group(group).expect("no fault").outcome
}

fn component_of(world: &WorldAuthority, id: EntityId, schema: &ContentKey) -> u32 {
    let handle = world.entities().handle_of(id).expect("entity");
    match world
        .entities()
        .get(handle)
        .expect("entity")
        .components
        .component(schema)
        .expect("component")
        .get(&key("store:field/stock"))
        .or_else(|| {
            world
                .entities()
                .get(handle)
                .expect("entity")
                .components
                .component(schema)
                .expect("component")
                .get(&key("machine:field/progress"))
        })
        .expect("field")
    {
        ComponentValue::U32(value) => *value,
        _ => panic!("u32 field"),
    }
}

#[test]
fn group_commits_both_sources_and_consumes_one_sequence() {
    let (mut world, schemas, machine, store) = setup();
    let before = world.seq().0;
    let outcome = committed(&mut world, &grinding_group(machine, store, 1));
    let TransactionOutcome::Committed { value, .. } = &outcome else {
        panic!("expected commit, got {outcome:?}");
    };
    // The merged payload carries both sources' edits in one change set.
    let changes = torse_sim::changes_from_committed(world.registry(), value).expect("changes");
    assert_eq!(changes.component_edits.len(), 2);
    assert_eq!(changes.component_edits[0].id, machine);
    assert_eq!(changes.component_edits[1].id, store);
    let _ = schemas;
    assert_eq!(world.seq().0, before + 1, "one sequence per group");
    assert_eq!(
        component_of(&world, machine, &key("machine:schema/progress")),
        1
    );
    assert_eq!(component_of(&world, store, &key("store:schema/stock")), 0);
}

#[test]
fn member_ownership_violation_rejects_the_whole_group() {
    let (mut world, _schemas, machine, store) = setup();
    let before = world.seq().0;
    // The machine member tries to write the store-owned stock component.
    let group = TransactionGroup::new(
        TransactionId(2),
        OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: 42,
            local_sequence: 0,
        },
        vec![
            member("store", 21, vec![stock_edit(store, 1, 0)]),
            member(
                "machine",
                22,
                vec![progress_edit(machine, 0, 1), stock_edit(machine, 9, 0)],
            ),
        ],
    )
    .expect("group");
    assert_eq!(
        committed(&mut world, &group),
        TransactionOutcome::Rejected(RejectCode::PermissionDenied)
    );
    assert_eq!(world.seq().0, before, "rejection consumes nothing");
    assert_eq!(
        component_of(&world, machine, &key("machine:schema/progress")),
        0
    );
    assert_eq!(component_of(&world, store, &key("store:schema/stock")), 1);
}

#[test]
fn stale_member_observation_rejects_the_whole_group() {
    let (mut world, _schemas, machine, store) = setup();
    let first = grinding_group(machine, store, 1);
    assert!(matches!(
        committed(&mut world, &first),
        TransactionOutcome::Committed { .. }
    ));
    // A second identical task still expects the consumed stock; the whole
    // group must fail without advancing the machine.
    let before = world.seq().0;
    let second = grinding_group(machine, store, 2);
    assert_eq!(
        committed(&mut world, &second),
        TransactionOutcome::Rejected(RejectCode::Conflict)
    );
    assert_eq!(world.seq().0, before);
    assert_eq!(
        component_of(&world, machine, &key("machine:schema/progress")),
        1
    );
}

#[test]
fn block_precondition_of_any_member_rejects_the_group() {
    let (mut world, _schemas, machine, store) = setup();
    // Put stone into a cell the store member still expects to be empty.
    let stone = world
        .registry()
        .default_state(&key("base:stone"))
        .expect("stone");
    let mut fill = member("machine", 51, Vec::new());
    fill.write_block(BlockPos::new(1, 1, 1), stone);
    assert!(matches!(
        world.commit(&fill).expect("no fault").outcome,
        TransactionOutcome::Committed { .. }
    ));

    let before = world.seq().0;
    let machine_member = member("machine", 52, vec![progress_edit(machine, 0, 1)]);
    let mut store_member = member("store", 53, vec![stock_edit(store, 1, 0)]);
    store_member.read_block(BlockPos::new(1, 1, 1), torse_world::BlockStateId::EMPTY);
    let group = TransactionGroup::new(
        TransactionId(5),
        OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: 42,
            local_sequence: 0,
        },
        vec![machine_member, store_member],
    )
    .expect("group");
    assert_eq!(
        committed(&mut world, &group),
        TransactionOutcome::Rejected(RejectCode::Conflict)
    );
    assert_eq!(world.seq().0, before);
    assert_eq!(
        component_of(&world, machine, &key("machine:schema/progress")),
        0
    );
    assert_eq!(component_of(&world, store, &key("store:schema/stock")), 1);
}

#[test]
fn constructor_rejects_structurally_invalid_groups() {
    let (world, _schemas, _machine, store) = setup();
    let ordering = OrderingKey {
        phase: Phase::Simulation,
        rule_priority: 0,
        stable_task_key: 42,
        local_sequence: 0,
    };
    // Duplicate sources.
    assert_eq!(
        TransactionGroup::new(
            TransactionId(1),
            ordering,
            vec![
                member("store", 1, vec![stock_edit(store, 1, 0)]),
                member("store", 2, vec![stock_edit(store, 1, 0)]),
            ],
        )
        .err(),
        Some(RejectCode::InvalidData)
    );
    // Same entity and schema written by two members.
    assert_eq!(
        TransactionGroup::new(
            TransactionId(1),
            ordering,
            vec![
                member("store", 1, vec![stock_edit(store, 1, 0)]),
                member("machine", 2, vec![stock_edit(store, 1, 0)]),
            ],
        )
        .err(),
        Some(RejectCode::Conflict)
    );
    // Two members moving the same entity would yield a change set with a
    // duplicate identity, which publish stops the world for; the constructor
    // must reject it as a normal conflict instead.
    let handle = world.entities().handle_of(store).expect("entity");
    let mut mover_a = member("store", 61, Vec::new());
    mover_a.move_entity(
        handle,
        store,
        Position::new(2.0, 2.0, 2.0),
        [0.1, 0.0, 0.0],
        [0.3, 0.9, 0.3],
    );
    let mut mover_b = member("machine", 62, Vec::new());
    mover_b.move_entity(
        handle,
        store,
        Position::new(2.0, 2.0, 2.0),
        [0.0, 0.0, 0.1],
        [0.3, 0.9, 0.3],
    );
    assert_eq!(
        TransactionGroup::new(TransactionId(1), ordering, vec![mover_a, mover_b]).err(),
        Some(RejectCode::Conflict)
    );

    // Empty and oversized groups.
    assert_eq!(
        TransactionGroup::new(TransactionId(1), ordering, Vec::new()).err(),
        Some(RejectCode::QuotaExceeded)
    );
    let five: Vec<Transaction> = ["a", "b", "c", "d", "e"]
        .into_iter()
        .map(|name| member(&format!("extra{name}"), 1, Vec::new()))
        .collect();
    assert_eq!(
        TransactionGroup::new(TransactionId(1), ordering, five).err(),
        Some(RejectCode::QuotaExceeded)
    );
}

#[test]
fn member_order_is_sorted_by_source() {
    let (_world, _schemas, machine, store) = setup();
    let group = grinding_group(machine, store, 9);
    assert_eq!(group.members[0].source, key("machine:module"));
    assert_eq!(group.members[1].source, key("store:module"));
    assert!(
        group
            .members
            .iter()
            .all(|member| member.ordering == group.ordering)
    );
}

#[test]
fn group_rejects_unresolvable_component_shape() {
    let (mut world, _schemas, machine, store) = setup();
    let mut wrong = member("store", 41, vec![stock_edit(store, 1, 0)]);
    // A field the schema does not declare.
    if let Some(fields) = wrong.components[0].fields.as_mut() {
        fields.insert(key("store:field/unknown"), ComponentValue::U32(1));
    }
    let group = TransactionGroup::new(
        TransactionId(4),
        OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: 42,
            local_sequence: 0,
        },
        vec![
            member("machine", 42, vec![progress_edit(machine, 0, 1)]),
            wrong,
        ],
    )
    .expect("group");
    assert_eq!(
        committed(&mut world, &group),
        TransactionOutcome::Rejected(RejectCode::InvalidData)
    );
    assert_eq!(
        component_of(&world, machine, &key("machine:schema/progress")),
        0
    );
}
