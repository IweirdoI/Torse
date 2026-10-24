//! Section-grant authorization for block actions.
//!
//! A host can grant every cell of a section instead of one cell at a time,
//! which is what network sessions need over their subscribed scope. These
//! tests pin the boundary: inside the section everything the per-cell grant
//! allows stays allowed, outside it nothing is, and entity access is never
//! widened.

use torse_content::{compile, m0_fixture_pack};
use torse_sim::WorldAuthority;
use torse_sim::action::{ActionOrigin, ActionRequest, ActionService, ActionTarget, ActorContext};
use torse_sim::{OrderingKey, Phase, Transaction, block_actions};
use torse_types::{
    BlockPos, Compatibility, ContentKey, Digest, EntityId, RejectCode, SectionPos, TransactionId,
    WorldId,
};
use torse_world::{ComponentFields, ComponentSchemas, ComponentValue, Section, WorldBounds};

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("key")
}

fn setup() -> (WorldAuthority, ActionService) {
    let mut pack = m0_fixture_pack();
    block_actions::register_content(&mut pack);
    let content = compile(pack).expect("content");
    let registry = torse_world::BlockStateRegistry::build(&content).expect("registry");
    let schemas = ComponentSchemas::build(&content);
    let service = ActionService::bind(
        block_actions::descriptors(),
        block_actions::handlers(),
        &schemas,
    )
    .expect("service");
    let mut world = WorldAuthority::new(
        WorldId(1),
        registry,
        schemas,
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
    (world, service)
}

fn editor() -> ActorContext {
    ActorContext::new(key("test:editor"))
        .permission(key("world:permission/edit"))
        .section(SectionPos::ZERO)
}

fn request(place: bool, pos: BlockPos, seq: u64) -> ActionRequest {
    let mut parameters = ComponentFields::new();
    if place {
        parameters.insert(
            key("world:field/block"),
            ComponentValue::Content(key("base:stone")),
        );
    }
    ActionRequest {
        action: key(if place {
            "world:action/place"
        } else {
            "world:action/break"
        }),
        api_version: 1,
        origin: ActionOrigin::Network,
        target: ActionTarget::Block(pos),
        parameters,
        transaction: TransactionId(u128::from(seq)),
        ordering: OrderingKey {
            phase: Phase::Interaction,
            rule_priority: 0,
            stable_task_key: seq,
            local_sequence: 0,
        },
    }
}

fn mutation(
    service: &ActionService,
    world: &WorldAuthority,
    actor: &ActorContext,
    req: &ActionRequest,
) -> Result<Transaction, RejectCode> {
    match service.plan(world, actor, Phase::Interaction, req) {
        Ok(torse_sim::action::ActionOutput::Mutation(tx)) => Ok(*tx),
        Ok(_) => Err(RejectCode::InvalidData),
        Err(code) => Err(code),
    }
}

#[test]
fn section_grant_covers_every_cell_inside() {
    let (mut world, service) = setup();
    let actor = editor();
    for pos in [
        BlockPos::new(0, 0, 0),
        BlockPos::new(15, 15, 15),
        BlockPos::new(7, 5, 9),
    ] {
        let tx = mutation(&service, &world, &actor, &request(true, pos, 1)).expect("place");
        world.commit(&tx).expect("commit");
        assert!(!matches!(
            world.store().query(pos),
            torse_world::TerrainQuery::Ready(state) if state.is_empty()
        ));
    }
    let tx = mutation(
        &service,
        &world,
        &actor,
        &request(false, BlockPos::new(7, 5, 9), 2),
    )
    .expect("break");
    world.commit(&tx).expect("commit");
    assert!(matches!(
        world.store().query(BlockPos::new(7, 5, 9)),
        torse_world::TerrainQuery::Ready(state) if state.is_empty()
    ));
}

#[test]
fn section_grant_stops_at_the_boundary() {
    let (world, service) = setup();
    let actor = editor();
    // Cell in the neighbouring section is outside the grant.
    for pos in [
        BlockPos::new(16, 1, 1),
        BlockPos::new(-1, 1, 1),
        BlockPos::new(1, 1, 16),
    ] {
        assert_eq!(
            mutation(&service, &world, &actor, &request(true, pos, 1)),
            Err(RejectCode::PermissionDenied)
        );
    }
    // The terrain view enforces the same boundary.
    let view = actor.view(&world);
    assert!(view.terrain(BlockPos::new(1, 1, 1)).is_ok());
    assert_eq!(
        view.terrain(BlockPos::new(16, 1, 1)),
        Err(RejectCode::PermissionDenied)
    );
}

#[test]
fn section_grants_do_not_widen_entity_or_permission_access() {
    let (world, service) = setup();
    // No edit permission: the section alone must not authorize place.
    let no_permission = ActorContext::new(key("test:editor")).section(SectionPos::ZERO);
    assert_eq!(
        mutation(
            &service,
            &world,
            &no_permission,
            &request(true, BlockPos::new(1, 1, 1), 1)
        ),
        Err(RejectCode::PermissionDenied)
    );
    // Entity access is unchanged by section grants.
    let actor = editor();
    assert!(!actor.allows(ActionTarget::Entity(EntityId(1))));
    // Exact-cell lookup does not consult sections; only allows_block does.
    assert!(!actor.allows(ActionTarget::Block(BlockPos::new(1, 1, 1))));
    assert!(actor.allows_block(BlockPos::new(1, 1, 1), 16));
}

#[test]
fn section_grants_have_a_registration_budget() {
    let mut actor = editor();
    let mut index = 1i64;
    while actor.section_count() < torse_sim::action::MAX_ACTOR_SECTIONS {
        actor = actor.section(SectionPos::new(index, 0, 0));
        index += 1;
    }
    assert_eq!(actor.section_count(), torse_sim::action::MAX_ACTOR_SECTIONS);
    // The builder stays infallible; registration is where the bound bites.
    let overflowing = actor.section(SectionPos::new(999, 0, 0));
    assert!(overflowing.section_count() > torse_sim::action::MAX_ACTOR_SECTIONS);
}

#[test]
fn exact_cell_grants_keep_working_alongside_sections() {
    let (world, service) = setup();
    let mixed = ActorContext::new(key("test:editor"))
        .permission(key("world:permission/edit"))
        .section(SectionPos::ZERO)
        .target(ActionTarget::Block(BlockPos::new(20, 1, 1)));
    let tx = mutation(
        &service,
        &world,
        &mixed,
        &request(true, BlockPos::new(20, 1, 1), 1),
    )
    .expect("cell grant");
    drop(tx);
    // Inside the granted section; (19, 1, 1) is in neither grant.
    let tx = mutation(
        &service,
        &world,
        &mixed,
        &request(true, BlockPos::new(1, 1, 1), 2),
    )
    .expect("section grant");
    drop(tx);
    assert_eq!(
        mutation(
            &service,
            &world,
            &mixed,
            &request(true, BlockPos::new(19, 1, 1), 3)
        ),
        Err(RejectCode::PermissionDenied)
    );
}

#[test]
fn section_grant_map_covers_all_axes() {
    let (_world, _service) = setup();
    let actor = editor();
    assert!(actor.allows_block(BlockPos::new(0, 0, 0), 16));
    assert!(actor.allows_block(BlockPos::new(15, 15, 15), 16));
    // The section covers one layer; the cell above y=15 is in another one.
    assert!(!actor.allows_block(BlockPos::new(0, 16, 0), 16));
    assert!(!actor.allows_block(BlockPos::new(-1, 0, 0), 16));
}
