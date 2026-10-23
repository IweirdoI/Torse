use crate::{BoxBounds, Shape, SourceRef, Sourced, compile, m0_fixture_pack};

#[test]
fn builtin_fixture_compiles() {
    let compiled = compile(m0_fixture_pack()).unwrap();
    assert_eq!(compiled.blocks.len(), 3);
    assert!(compiled.blocks.contains_key(&key("base:stone")));
    assert_ne!(compiled.authority_digest, compiled.presentation_digest);
}

#[test]
fn pack_order_does_not_change_digest() {
    let mut first = m0_fixture_pack();
    let mut second = m0_fixture_pack();
    first.state_schemas[1].value.finite_fields[0]
        .values
        .reverse();
    first.blocks.reverse();
    first.state_schemas.reverse();
    first.actions.reverse();
    second.blocks.rotate_left(1);
    second.state_schemas.rotate_left(1);
    second.actions.rotate_left(1);

    let first = compile(first).unwrap();
    let second = compile(second).unwrap();
    assert_eq!(first.authority_digest, second.authority_digest);
    assert_eq!(first.presentation_digest, second.presentation_digest);
}

#[test]
fn invalid_shape_is_reported_with_content_location() {
    let mut pack = m0_fixture_pack();
    let block = pack
        .blocks
        .iter_mut()
        .find(|block| block.value.id.to_string() == "base:stone")
        .unwrap();
    block.value.render_profile.shape = Shape::Box(BoxBounds {
        min: [0.0, 0.0, 0.0],
        max: [1.5, 1.0, 1.0],
    });

    let errors = compile(pack).unwrap_err();
    assert!(errors.iter().any(|error| {
        error
            .content_id
            .as_ref()
            .is_some_and(|id| id.to_string() == "base:stone")
            && error.field == "render_profile.shape"
            && error.source.source == "fixtures/m0/stone.json"
    }));
}

#[test]
fn duplicate_definition_is_rejected() {
    let mut pack = m0_fixture_pack();
    let duplicate = pack.blocks[0].clone();
    pack.blocks.push(Sourced::new(
        SourceRef::new("fixtures/m0/duplicate.json", 7),
        duplicate.value,
    ));
    let errors = compile(pack).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.field == "id"
            && error.message.contains("duplicate")
            && error.source.source == "fixtures/m0/duplicate.json"
    }));
}

#[test]
fn missing_state_schema_is_rejected() {
    let mut pack = m0_fixture_pack();
    pack.blocks[0].value.state_schema = key("missing:state");
    let errors = compile(pack).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "state_schema" && error.message.contains("missing:state"))
    );
}

#[test]
fn finite_state_explosion_is_rejected() {
    let mut pack = m0_fixture_pack();
    let schema = pack
        .state_schemas
        .iter_mut()
        .find(|schema| schema.value.id.to_string() == "base:state/plant_growth")
        .unwrap();
    schema.value.finite_fields[0].values = (0..100).collect();
    schema.value.finite_fields.push(crate::FiniteField {
        id: key("base:field/age"),
        default_value: 0,
        values: (0..100).collect(),
    });
    let errors = compile(pack).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "finite_fields" && error.message.contains("exceed limit"))
    );
}

#[test]
fn module_action_references_are_resolved() {
    let mut pack = m0_fixture_pack();
    pack.modules[0]
        .value
        .registered_actions
        .push(key("base:action/missing"));
    let errors = compile(pack).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.field == "registered_actions" && error.message.contains("base:action/missing")
    }));
}

#[test]
fn capability_binding_and_host_permission_are_independent() {
    let mut pack = m0_fixture_pack();
    pack.blocks[0]
        .value
        .capability_bindings
        .push(crate::CapabilityBinding {
            capability: key("base:capability/item_port"),
            face_mask: 0b11_1111,
        });
    let compiled = compile(pack).unwrap();
    let stone = &compiled.blocks[&key("base:stone")];
    assert_eq!(stone.capability_bindings.len(), 1);
    assert_eq!(
        compiled.modules[&key("base:core")]
            .requested_permissions
            .len(),
        1
    );

    let mut invalid = m0_fixture_pack();
    invalid.blocks[0]
        .value
        .capability_bindings
        .push(crate::CapabilityBinding {
            capability: key("missing:capability"),
            face_mask: 0,
        });
    let errors = compile(invalid).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.field == "capability_bindings" && error.message.contains("missing:capability")
    }));
}

fn key(value: &str) -> torse_types::ContentKey {
    torse_types::ContentKey::parse(value).unwrap()
}
