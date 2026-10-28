//! Body transitions are planned by the physics owner, not by visibility.
use torse_content::{compile, m0_fixture_pack};
use torse_sim::action::{ActionTarget, ActorContext};
use torse_sim::{OrderingKey, Phase, WorldAuthority};
use torse_types::*;
use torse_world::*;

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("key")
}

fn world() -> WorldAuthority {
    let content = compile(m0_fixture_pack()).expect("content");
    let registry = BlockStateRegistry::build(&content).expect("registry");
    let schemas = ComponentSchemas::build(&content);
    WorldAuthority::new(
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
    .expect("authority")
}

fn load(world: &mut WorldAuthority) {
    world.store_mut().request(SectionPos::ZERO).unwrap();
    world
        .store_mut()
        .install(SectionPos::ZERO, Section::empty(16).unwrap())
        .unwrap();
    world.store_mut().finish_load(SectionPos::ZERO).unwrap();
}

fn spawn(world: &mut WorldAuthority, body: EntityCollider) -> EntityId {
    let handle = world
        .entities_mut()
        .spawn(Position {
            x: 0.5,
            y: 0.5,
            z: 0.5,
        })
        .unwrap();
    let schemas = world.schemas().clone();
    world
        .entities_mut()
        .components_mut(handle)
        .unwrap()
        .insert(&schemas, &collider_schema(), body.fields())
        .unwrap();
    world.entities().get(handle).unwrap().id
}

fn ordering() -> OrderingKey {
    OrderingKey {
        phase: Phase::Interaction,
        rule_priority: 0,
        stable_task_key: 1,
        local_sequence: 0,
    }
}

fn grant(entity: EntityId, permission: bool) -> ActorContext {
    let mut actor = ActorContext::new(key("test:game")).target(ActionTarget::Entity(entity));
    if permission {
        actor = actor.permission(collider_permission());
    }
    actor
}

#[test]
fn planning_requires_an_explicit_grant_and_does_not_publish() {
    let mut world = world();
    load(&mut world);
    let body = EntityCollider::Box { half: [200_000; 3] };
    let id = spawn(&mut world, body);
    let denied = grant(id, false);
    let view = denied.view(&world);
    assert_eq!(
        view.plan_collider_transition(
            id,
            body,
            EntityCollider::Disabled,
            TransactionId(1),
            ordering()
        ),
        Err(RejectCode::PermissionDenied)
    );
    let hidden = ActorContext::new(key("test:game"))
        .permission(key("world:permission/edit"))
        .permission(collider_permission());
    assert_eq!(
        hidden.view(&world).plan_collider_transition(
            id,
            body,
            EntityCollider::Disabled,
            TransactionId(1),
            ordering()
        ),
        Err(RejectCode::PermissionDenied)
    );
    let before = EntityCollider::of(
        world
            .entities()
            .get(world.entities().handle_of(id).unwrap())
            .unwrap(),
    )
    .unwrap();
    let planned = grant(id, true)
        .view(&world)
        .plan_collider_transition(
            id,
            body,
            EntityCollider::Disabled,
            TransactionId(7),
            ordering(),
        )
        .unwrap();
    assert_eq!(planned.source, collider_owner());
    assert_eq!(planned.id, TransactionId(7));
    assert_eq!(planned.components.len(), 1);
    assert_eq!(planned.components[0].previous, Some(body.fields()));
    assert_eq!(
        planned.components[0].fields,
        Some(EntityCollider::Disabled.fields())
    );
    assert_eq!(
        EntityCollider::of(
            world
                .entities()
                .get(world.entities().handle_of(id).unwrap())
                .unwrap()
        )
        .unwrap(),
        before
    );
    assert_eq!(
        grant(id, true).view(&world).plan_collider_transition(
            id,
            EntityCollider::Disabled,
            EntityCollider::Disabled,
            TransactionId(8),
            ordering()
        ),
        Err(RejectCode::Conflict)
    );
}

#[test]
fn enabling_inside_terrain_is_rejected_and_open_air_is_planned() {
    let mut world = world();
    load(&mut world);
    let disabled = EntityCollider::Disabled;
    let body = EntityCollider::Box { half: [200_000; 3] };
    let id = spawn(&mut world, disabled);
    let stone = world
        .registry()
        .default_state(&key("base:stone"))
        .expect("stone");
    world
        .store_mut()
        .edit(BlockPos::new(0, 0, 0), stone)
        .unwrap();
    let granted = grant(id, true);
    let blocked = granted.view(&world);
    assert_eq!(
        blocked.plan_collider_transition(id, disabled, body, TransactionId(1), ordering()),
        Err(RejectCode::Conflict)
    );
    world
        .store_mut()
        .edit(BlockPos::new(0, 0, 0), BlockStateId::EMPTY)
        .unwrap();
    let open = granted.view(&world);
    let planned = open
        .plan_collider_transition(id, disabled, body, TransactionId(2), ordering())
        .unwrap();
    assert_eq!(planned.source, collider_owner());
    assert!(!planned.preconditions.is_empty());
    let far = world
        .entities_mut()
        .spawn(Position {
            x: 64.5,
            y: 0.5,
            z: 0.5,
        })
        .unwrap();
    let schemas = world.schemas().clone();
    world
        .entities_mut()
        .components_mut(far)
        .unwrap()
        .insert(&schemas, &collider_schema(), disabled.fields())
        .unwrap();
    let far_id = world.entities().get(far).unwrap().id;
    let far_grant = grant(far_id, true);
    assert_eq!(
        far_grant.view(&world).plan_collider_transition(
            far_id,
            disabled,
            body,
            TransactionId(3),
            ordering()
        ),
        Err(RejectCode::NotLoaded)
    );
}

#[test]
fn planning_rejects_an_invalid_collider_before_returning_a_transaction() {
    let mut world = world();
    load(&mut world);
    let disabled = EntityCollider::Disabled;
    let id = spawn(&mut world, disabled);
    assert_eq!(
        grant(id, true).view(&world).plan_collider_transition(
            id,
            disabled,
            EntityCollider::Box { half: [0, 1, 1] },
            TransactionId(4),
            ordering(),
        ),
        Err(RejectCode::InvalidData)
    );
}
