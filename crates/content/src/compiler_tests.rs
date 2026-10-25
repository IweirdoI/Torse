use crate::{
    AlphaMode, BoxBounds, Face, FaceOverlay, Shape, SourceRef, Sourced, compile, m0_fixture_pack,
};

#[test]
fn builtin_fixture_compiles() {
    // The fixture is the minimum proof that all four contracts can coexist and
    // that authority and presentation summaries are independent values.
    let compiled = compile(m0_fixture_pack()).unwrap();
    assert_eq!(compiled.blocks.len(), 4);
    assert!(compiled.blocks.contains_key(&key("base:stone")));
    assert!(compiled.blocks.contains_key(&key("base:glass")));
    // The upper slab is a state of base:glass_slab, not its own block.
    assert!(compiled.blocks.contains_key(&key("base:glass_slab")));
    assert_ne!(compiled.authority_digest, compiled.presentation_digest);
}

#[test]
fn pack_order_does_not_change_digest() {
    // Reversing top-level vectors and reordering legal finite values must not
    // affect compiled identity. This protects content hashes from filesystem and
    // loader enumeration order.
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
    // Geometry failures must identify the owning content and field so an author
    // can fix the source without searching the entire pack.
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
    // Duplicate stable IDs are never resolved by load order; the second source
    // location must be reported explicitly.
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
    // A block cannot compile against an undeclared state model.
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
    // Two individually small fields can produce a large Cartesian product, so
    // the compiler must enforce the combined state limit.
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
    // Module registration cannot claim an action that is absent from the pack.
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
fn partial_occlusion_claims_are_rejected() {
    // Mesh culling removes whole neighbour faces, so a slab claiming a side
    // boundary would punch a see-through hole above its midline.
    let mut pack = m0_fixture_pack();
    let slab = pack
        .blocks
        .iter_mut()
        .find(|block| block.value.id.to_string() == "base:glass_slab")
        .unwrap();
    slab.value.occlusion_profile.face_mask = 0b110100;
    let errors = compile(pack).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.field == "occlusion_profile.face_mask"
            && error.message.contains("does not fully cover")
    }));
}

#[test]
fn transparent_materials_may_not_claim_occlusion() {
    // Neighbour faces stay visible through cutout and blended blocks, so
    // claiming any occlusion would delete a face the player can still see.
    let mut pack = m0_fixture_pack();
    let glass = pack
        .blocks
        .iter_mut()
        .find(|block| block.value.id.to_string() == "base:glass")
        .unwrap();
    glass.value.occlusion_profile.face_mask = 0b11_1111;
    let errors = compile(pack).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "occlusion_profile.face_mask"
                && error.message.contains("may not claim occlusion")),
        "got {errors:?}"
    );
}

#[test]
fn completion_target_must_be_a_registered_block() {
    let mut pack = m0_fixture_pack();
    let slab = pack
        .blocks
        .iter_mut()
        .find(|block| block.value.id.to_string() == "base:glass_slab")
        .unwrap();
    slab.value.interaction_profile.completes_to = Some(key("base:missing"));
    let errors = compile(pack).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.field == "interaction_profile.completes_to" && error.message.contains("base:missing")
    }));
}

#[test]
fn capability_binding_and_host_permission_are_independent() {
    // Binding a capability to an object proves only that the interface exists;
    // the module's requested host permission remains a separate declaration.
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

#[test]
fn face_materials_affect_presentation_only() {
    // A per-face surface change is a presentation decision: clients may see a
    // different image while the authoritative content identity stays stable.
    let base = compile(m0_fixture_pack()).unwrap();

    let mut pack = m0_fixture_pack();
    let stone = pack
        .blocks
        .iter_mut()
        .find(|block| block.value.id.to_string() == "base:stone")
        .unwrap();
    stone
        .value
        .render_profile
        .face_materials
        .set(Face::PosY, key("base:material/stone_top"));

    let changed = compile(pack).unwrap();
    assert_eq!(base.authority_digest, changed.authority_digest);
    assert_ne!(base.presentation_digest, changed.presentation_digest);

    let stone = &changed.blocks[&key("base:stone")];
    assert_eq!(
        stone
            .render_profile
            .face_materials
            .get(Face::PosY)
            .map(|material| material.path()),
        Some("material/stone_top")
    );
    assert_eq!(
        stone
            .render_profile
            .face_materials
            .resolve(Face::NegY, &stone.render_profile.material)
            .path(),
        "material/stone"
    );
}

#[test]
fn face_overlays_affect_presentation_only() {
    let base = compile(m0_fixture_pack()).unwrap();
    let mut pack = m0_fixture_pack();
    let stone = pack
        .blocks
        .iter_mut()
        .find(|block| block.value.id.to_string() == "base:stone")
        .unwrap();
    stone.value.render_profile.face_overlays.set(
        Face::NegX,
        FaceOverlay {
            material: key("base:material/stone_overlay"),
            alpha_mode: AlphaMode::Mask,
        },
    );

    let changed = compile(pack).unwrap();
    assert_eq!(base.authority_digest, changed.authority_digest);
    assert_ne!(base.presentation_digest, changed.presentation_digest);
}

#[test]
fn same_face_material_change_keeps_authority_digest() {
    // Setting an override equal to the block material must still be a
    // presentation change only, never an authority change.
    let base = compile(m0_fixture_pack()).unwrap();

    let mut pack = m0_fixture_pack();
    let stone = pack
        .blocks
        .iter_mut()
        .find(|block| block.value.id.to_string() == "base:stone")
        .unwrap();
    let material = stone.value.render_profile.material.clone();
    stone
        .value
        .render_profile
        .face_materials
        .set(Face::NegX, material);

    let changed = compile(pack).unwrap();
    assert_eq!(base.authority_digest, changed.authority_digest);
    assert_ne!(base.presentation_digest, changed.presentation_digest);
}

fn key(value: &str) -> torse_types::ContentKey {
    torse_types::ContentKey::parse(value).unwrap()
}
