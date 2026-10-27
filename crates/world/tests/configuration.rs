use std::collections::BTreeMap;
use torse_content::{
    CompiledContent, ComponentField, ComponentFieldType, ComponentSchema, PersistencePolicy,
    compile, m0_fixture_pack,
};
use torse_types::{Compatibility, ContentKey, Digest, EntityId, ReplicationDeclaration};
use torse_world::{
    BlockDefault, ComponentConfiguration, ComponentError, ComponentFields, ComponentSchemas,
    ComponentValue, PersistenceSelection,
};

fn key(id: &str) -> ContentKey {
    ContentKey::parse(id).unwrap()
}
fn content() -> CompiledContent {
    let mut pack = m0_fixture_pack();
    let state = pack
        .state_schemas
        .iter_mut()
        .find(|s| s.value.id == key("base:state/container"))
        .unwrap();
    for id in ["test:a", "test:b"] {
        state.value.component_schemas.push(ComponentSchema {
            id: key(id),
            schema_version: 1,
            owner_module: key("base:inventory"),
            fields: if id == "test:a" {
                vec![ComponentField {
                    id: key("test:value"),
                    field_type: ComponentFieldType::Record,
                    required: true,
                }]
            } else {
                vec![]
            },
            persistence: PersistencePolicy::OwnerDefined,
            replication: ReplicationDeclaration::SERVER_PRIVATE,
            migration_binding: None,
        });
    }
    pack.blocks
        .iter_mut()
        .find(|b| b.value.id == key("base:stone"))
        .unwrap()
        .value
        .component_defaults = vec![key("test:a"), key("test:b")];
    compile(pack).unwrap()
}
fn base() -> Compatibility {
    Compatibility {
        authority_schema: 4,
        rules_digest: Digest([1; 32]),
        content_digest: Digest([2; 32]),
    }
}
fn fields(value: ComponentValue) -> ComponentFields {
    [(
        key("test:value"),
        ComponentValue::Record([(key("test:inner"), value)].into()),
    )]
    .into()
}
fn configuration() -> ComponentConfiguration {
    ComponentConfiguration {
        persistence: vec![
            PersistenceSelection {
                owner: key("base:inventory"),
                schema: key("test:a"),
                persist: false,
            },
            PersistenceSelection {
                owner: key("base:inventory"),
                schema: key("test:b"),
                persist: true,
            },
        ],
        block_defaults: vec![
            BlockDefault {
                owner: key("base:inventory"),
                block: key("base:stone"),
                schema: key("test:a"),
                fields: fields(ComponentValue::U32(7)),
            },
            BlockDefault {
                owner: key("base:inventory"),
                block: key("base:stone"),
                schema: key("test:b"),
                fields: BTreeMap::new(),
            },
        ],
    }
}

#[test]
fn assembly_is_canonical_and_freezes_owner_choices() {
    let content = content();
    let config = configuration();
    let (mut schemas, compatibility) =
        ComponentSchemas::assemble(&content, config.clone(), base()).unwrap();
    assert!(!schemas.persists(&key("test:a")).unwrap());
    assert!(schemas.persists(&key("test:b")).unwrap());
    assert_eq!(
        schemas.block_default(&key("base:stone"), &key("test:a")),
        Some(&fields(ComponentValue::U32(7)))
    );
    assert_ne!(compatibility.rules_digest, base().rules_digest);
    assert_eq!(compatibility.content_digest, base().content_digest);
    assert_eq!(compatibility.authority_schema, base().authority_schema);
    let mut reversed = config.clone();
    reversed.persistence.reverse();
    reversed.block_defaults.reverse();
    assert_eq!(
        ComponentSchemas::assemble(&content, reversed, base())
            .unwrap()
            .1,
        compatibility
    );
    let mut implicit = config;
    implicit.persistence.pop();
    implicit.block_defaults.pop();
    assert_eq!(
        ComponentSchemas::assemble(&content, implicit, base())
            .unwrap()
            .1,
        compatibility
    );
    assert_eq!(
        schemas.resolve_persistence(&key("base:inventory"), &key("test:a"), true),
        Err(ComponentError::ConfigurationFrozen)
    );
    assert_eq!(
        schemas.set_block_default(
            &key("base:inventory"),
            key("base:stone"),
            key("test:a"),
            fields(ComponentValue::U32(9))
        ),
        Err(ComponentError::ConfigurationFrozen)
    );
    assert_eq!(
        schemas.clone().bind_compatibility(compatibility).unwrap().1,
        compatibility
    );
    assert!(schemas.bind_compatibility(base()).is_err());
}

#[test]
fn defaults_policy_and_base_rules_each_change_compatibility() {
    let content = content();
    let original = ComponentSchemas::assemble(&content, configuration(), base())
        .unwrap()
        .1;
    let mut defaults = configuration();
    defaults.block_defaults[0].fields = fields(ComponentValue::U32(8));
    assert_ne!(
        ComponentSchemas::assemble(&content, defaults, base())
            .unwrap()
            .1,
        original
    );
    let mut policy = configuration();
    policy.persistence[0].persist = true;
    assert_ne!(
        ComponentSchemas::assemble(&content, policy, base())
            .unwrap()
            .1,
        original
    );
    assert_ne!(
        ComponentSchemas::assemble(
            &content,
            configuration(),
            Compatibility {
                rules_digest: Digest([3; 32]),
                ..base()
            }
        )
        .unwrap()
        .1,
        original
    );
    let (schema, _) = ComponentSchemas::assemble(&content, configuration(), base()).unwrap();
    assert_eq!(
        schema.validate_compatibility(base()),
        Err(ComponentError::ConfigurationMismatch)
    );
}

#[test]
fn typed_nested_values_have_unambiguous_rule_identity() {
    let content = content();
    let mut digests = vec![];
    for value in [
        ComponentValue::Bool(true),
        ComponentValue::U32(1),
        ComponentValue::I64(1),
        ComponentValue::Content(key("test:item")),
        ComponentValue::Entity(EntityId(1)),
        ComponentValue::Text("test:item".into()),
        ComponentValue::List(vec![ComponentValue::Bool(true)]),
        ComponentValue::Record([(key("test:k"), ComponentValue::Bool(true))].into()),
    ] {
        let mut config = configuration();
        config.block_defaults[0].fields = fields(value);
        let digest = ComponentSchemas::assemble(&content, config, base())
            .unwrap()
            .1
            .rules_digest;
        assert!(!digests.contains(&digest));
        digests.push(digest);
    }
    let mut split = configuration();
    split.block_defaults[0].fields = fields(ComponentValue::List(vec![
        ComponentValue::Text("ab".into()),
        ComponentValue::Text("c".into()),
    ]));
    let first = ComponentSchemas::assemble(&content, split.clone(), base())
        .unwrap()
        .1;
    split.block_defaults[0].fields = fields(ComponentValue::List(vec![
        ComponentValue::Text("a".into()),
        ComponentValue::Text("bc".into()),
    ]));
    assert_ne!(
        ComponentSchemas::assemble(&content, split, base())
            .unwrap()
            .1,
        first
    );
}

#[test]
fn invalid_owners_declarations_duplicates_and_required_fields_fail_startup() {
    let content = content();
    let mut invalid = vec![];
    let mut config = configuration();
    config.persistence[0].owner = key("other:owner");
    invalid.push(config);
    let mut config = configuration();
    config.persistence[0].schema = key("base:schema/inventory_content");
    invalid.push(config);
    let mut config = configuration();
    config.block_defaults[0].owner = key("other:owner");
    invalid.push(config);
    let mut config = configuration();
    config.block_defaults[0].block = key("test:unknown");
    invalid.push(config);
    let mut config = configuration();
    config.block_defaults[0].block = key("base:glass");
    invalid.push(config);
    let mut config = configuration();
    config.persistence.push(config.persistence[0].clone());
    invalid.push(config);
    let mut config = configuration();
    config.block_defaults.push(config.block_defaults[0].clone());
    invalid.push(config);
    let mut config = configuration();
    config.block_defaults.remove(0);
    invalid.push(config);
    let mut config = configuration();
    config.block_defaults[0].fields = [(key("test:value"), ComponentValue::U32(7))].into();
    invalid.push(config);
    for config in invalid {
        assert!(ComponentSchemas::assemble(&content, config, base()).is_err());
    }
    assert!(ComponentSchemas::assemble(&content, configuration(), base()).is_ok());
}

#[test]
fn legacy_incremental_configuration_is_hashed_before_world_use() {
    let content = content();
    let config = configuration();
    let mut legacy = ComponentSchemas::build(&content);
    for p in config.persistence {
        legacy
            .resolve_persistence(&p.owner, &p.schema, p.persist)
            .unwrap();
    }
    for d in config.block_defaults {
        legacy
            .set_block_default(&d.owner, d.block, d.schema, d.fields)
            .unwrap();
    }
    assert!(legacy.validate_compatibility(base()).is_err());
    assert_eq!(
        legacy.bind_compatibility(base()).unwrap().1,
        ComponentSchemas::assemble(&content, configuration(), base())
            .unwrap()
            .1
    );
}

#[test]
fn default_configuration_keeps_legacy_metadata_and_limits_are_enforced() {
    let content = compile(m0_fixture_pack()).unwrap();
    let (schemas, compatibility) =
        ComponentSchemas::assemble(&content, ComponentConfiguration::default(), base()).unwrap();
    assert_eq!(compatibility, base());
    assert!(
        schemas
            .validate_compatibility(Compatibility {
                rules_digest: Digest([9; 32]),
                ..base()
            })
            .is_err()
    );
    let mut config = configuration();
    config.persistence = vec![config.persistence[0].clone(); 4097];
    assert!(matches!(
        ComponentSchemas::assemble(&self::content(), config, base()),
        Err(ComponentError::BudgetExceeded)
    ));
    let mut config = configuration();
    config.block_defaults[0].fields = fields(ComponentValue::Text("x".repeat(65_536)));
    assert!(matches!(
        ComponentSchemas::assemble(&self::content(), config, base()),
        Err(ComponentError::BudgetExceeded)
    ));
}
