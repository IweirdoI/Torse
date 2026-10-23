//! Bounded instance components for entities and complex blocks.
//!
//! Component data is owned by one module and validated against a schema declared
//! in content. The store here does not decide game rules; it enforces the
//! declared shape and limits so a malformed or oversized value cannot reach
//! storage, replication, or a save file.
//!
//! Values are deliberately a small closed set. Registering an arbitrary Rust
//! type would make the schema advisory, and the schema is what migration and
//! encoding need to reason about.

use std::collections::BTreeMap;

use torse_content::{CompiledContent, ComponentFieldType, ComponentSchema};
use torse_types::{ContentKey, EntityId};

/// Maximum nesting depth accepted inside a list or record value.
pub const MAX_VALUE_DEPTH: u32 = 8;

/// One component field value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComponentValue {
    /// Boolean flag.
    Bool(bool),
    /// Unsigned 32-bit number.
    U32(u32),
    /// Signed 64-bit number.
    I64(i64),
    /// Reference to content.
    Content(ContentKey),
    /// Reference to a persistent entity identity.
    Entity(EntityId),
    /// Bounded UTF-8 text.
    Text(String),
    /// Bounded ordered list.
    List(Vec<Self>),
    /// Bounded record keyed by field identity.
    Record(BTreeMap<ContentKey, Self>),
}

/// Component schemas available to the world, indexed by identity.
///
/// The set is built once from compiled content and never changes at runtime,
/// which is what lets storage trust a schema identity without re-validating it.
#[derive(Clone, Debug, Default)]
pub struct ComponentSchemas {
    schemas: BTreeMap<ContentKey, ComponentSchema>,
}

impl ComponentSchemas {
    /// Collect every component schema declared by the given state schemas.
    pub fn build(content: &CompiledContent) -> Self {
        let mut schemas = BTreeMap::new();
        for state in content.state_schemas.values() {
            for schema in &state.component_schemas {
                schemas.insert(schema.id.clone(), schema.clone());
            }
        }
        Self { schemas }
    }

    /// Return a schema by identity.
    pub fn get(&self, id: &ContentKey) -> Option<&ComponentSchema> {
        self.schemas.get(id)
    }

    /// Number of registered component schemas.
    pub fn len(&self) -> usize {
        self.schemas.len()
    }

    /// Return whether no component schema is registered.
    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }
}

/// Bounded component instances attached to one object.
///
/// A set may hold several components, each keyed by its schema identity. Field
/// ownership and mutation rules belong to the owning module; this type only
/// guarantees that whatever is stored matches the declared schema.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComponentSet {
    components: BTreeMap<ContentKey, BTreeMap<ContentKey, ComponentValue>>,
}

impl ComponentSet {
    /// Create an empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate and insert one component instance.
    ///
    /// Replaces an existing instance of the same schema. Nothing is written when
    /// validation fails.
    pub fn insert(
        &mut self,
        schemas: &ComponentSchemas,
        id: &ContentKey,
        fields: BTreeMap<ContentKey, ComponentValue>,
    ) -> Result<(), ComponentError> {
        let schema = schemas
            .get(id)
            .ok_or_else(|| ComponentError::UnknownSchema { id: id.clone() })?;

        for (field, value) in &fields {
            let declared = schema
                .fields
                .iter()
                .find(|candidate| &candidate.id == field)
                .ok_or_else(|| ComponentError::UnknownField {
                    schema: id.clone(),
                    field: field.clone(),
                })?;
            validate_value(field, declared.field_type, value, 0)?;
        }
        for declared in schema.fields.iter().filter(|field| field.required) {
            if !fields.contains_key(&declared.id) {
                return Err(ComponentError::MissingRequiredField {
                    schema: id.clone(),
                    field: declared.id.clone(),
                });
            }
        }

        self.components.insert(id.clone(), fields);
        Ok(())
    }

    /// Borrow one component instance.
    pub fn component(&self, id: &ContentKey) -> Option<&BTreeMap<ContentKey, ComponentValue>> {
        self.components.get(id)
    }

    /// Remove one component instance.
    pub fn remove(&mut self, id: &ContentKey) -> bool {
        self.components.remove(id).is_some()
    }

    /// Number of component instances in the set.
    pub fn len(&self) -> usize {
        self.components.len()
    }

    /// Return whether the set holds no component.
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    /// Identities of the components in the set, in canonical order.
    pub fn ids(&self) -> impl Iterator<Item = &ContentKey> {
        self.components.keys()
    }
}

fn validate_value(
    field: &ContentKey,
    field_type: ComponentFieldType,
    value: &ComponentValue,
    depth: u32,
) -> Result<(), ComponentError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(ComponentError::DepthExceeded {
            field: field.clone(),
            limit: MAX_VALUE_DEPTH,
        });
    }
    match (field_type, value) {
        (ComponentFieldType::Bool, ComponentValue::Bool(_))
        | (ComponentFieldType::Unsigned32, ComponentValue::U32(_))
        | (ComponentFieldType::Signed64, ComponentValue::I64(_))
        | (ComponentFieldType::ContentReference, ComponentValue::Content(_))
        | (ComponentFieldType::EntityReference, ComponentValue::Entity(_)) => Ok(()),
        (ComponentFieldType::String { max_bytes }, ComponentValue::Text(text)) => {
            let size = u32::try_from(text.len()).unwrap_or(u32::MAX);
            if size > max_bytes {
                return Err(ComponentError::LimitExceeded {
                    field: field.clone(),
                    limit: u64::from(max_bytes),
                    actual: u64::from(size),
                });
            }
            Ok(())
        }
        (ComponentFieldType::List { max_items }, ComponentValue::List(items)) => {
            let size = u32::try_from(items.len()).unwrap_or(u32::MAX);
            if size > max_items {
                return Err(ComponentError::LimitExceeded {
                    field: field.clone(),
                    limit: u64::from(max_items),
                    actual: u64::from(size),
                });
            }
            for item in items {
                validate_element(field, item, depth + 1)?;
            }
            Ok(())
        }
        (ComponentFieldType::Record, ComponentValue::Record(entries)) => {
            for (key, entry) in entries {
                if key.path().is_empty() {
                    return Err(ComponentError::UnknownField {
                        schema: field.clone(),
                        field: key.clone(),
                    });
                }
                validate_element(key, entry, depth + 1)?;
            }
            Ok(())
        }
        (expected, _) => Err(ComponentError::TypeMismatch {
            field: field.clone(),
            expected: format!("{expected:?}"),
        }),
    }
}

/// Validate a nested container element, which has no declared scalar type.
///
/// Lists and records do not carry element schemas in M1, so nested values are
/// checked for depth and shape only. A future schema revision can add element
/// types without changing the accepted surface.
fn validate_element(
    field: &ContentKey,
    value: &ComponentValue,
    depth: u32,
) -> Result<(), ComponentError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(ComponentError::DepthExceeded {
            field: field.clone(),
            limit: MAX_VALUE_DEPTH,
        });
    }
    match value {
        ComponentValue::Bool(_)
        | ComponentValue::U32(_)
        | ComponentValue::I64(_)
        | ComponentValue::Content(_)
        | ComponentValue::Entity(_)
        | ComponentValue::Text(_) => Ok(()),
        ComponentValue::List(items) => {
            for item in items {
                validate_element(field, item, depth + 1)?;
            }
            Ok(())
        }
        ComponentValue::Record(entries) => {
            for (key, entry) in entries {
                validate_element(key, entry, depth + 1)?;
            }
            Ok(())
        }
    }
}
/// Failure produced by component validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComponentError {
    /// The schema identity is not registered.
    UnknownSchema {
        /// Missing schema.
        id: ContentKey,
    },
    /// A field is not declared by the schema.
    UnknownField {
        /// Schema or owning field.
        schema: ContentKey,
        /// Offending field.
        field: ContentKey,
    },
    /// A required field was omitted.
    MissingRequiredField {
        /// Schema identity.
        schema: ContentKey,
        /// Missing field.
        field: ContentKey,
    },
    /// The value kind does not match the declared field type.
    TypeMismatch {
        /// Field being written.
        field: ContentKey,
        /// Declared type description.
        expected: String,
    },
    /// A size limit declared by the field type was exceeded.
    LimitExceeded {
        /// Field being written.
        field: ContentKey,
        /// Declared limit.
        limit: u64,
        /// Observed size.
        actual: u64,
    },
    /// Nested containers exceeded the accepted depth.
    DepthExceeded {
        /// Field being written.
        field: ContentKey,
        /// Accepted depth.
        limit: u32,
    },
}

impl std::fmt::Display for ComponentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSchema { id } => write!(f, "component schema {id} is not registered"),
            Self::UnknownField { schema, field } => {
                write!(f, "component {schema} has no field {field}")
            }
            Self::MissingRequiredField { schema, field } => {
                write!(f, "component {schema} requires field {field}")
            }
            Self::TypeMismatch { field, expected } => {
                write!(f, "field {field} expects {expected}")
            }
            Self::LimitExceeded {
                field,
                limit,
                actual,
            } => write!(f, "field {field} allows at most {limit}, got {actual}"),
            Self::DepthExceeded { field, limit } => {
                write!(f, "field {field} exceeds nesting depth {limit}")
            }
        }
    }
}

impl std::error::Error for ComponentError {}

#[cfg(test)]
mod tests {
    use super::*;
    use torse_content::{
        ComponentField, ComponentFieldType, PersistencePolicy, compile, m0_fixture_pack,
    };
    use torse_types::ReplicationDeclaration;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key is valid")
    }

    fn schemas() -> (ComponentSchemas, ContentKey) {
        let content = compile(m0_fixture_pack()).expect("fixture content compiles");
        let schemas = ComponentSchemas::build(&content);
        (schemas, key("base:schema/inventory_content"))
    }

    fn fields(entries: &[(ContentKey, ComponentValue)]) -> BTreeMap<ContentKey, ComponentValue> {
        entries.iter().cloned().collect()
    }

    #[test]
    fn known_schema_is_registered_from_compiled_content() {
        let (schemas, id) = schemas();
        assert_eq!(schemas.len(), 1);
        assert!(schemas.get(&id).is_some());
    }

    #[test]
    fn unknown_schema_and_field_are_rejected() {
        let (schemas, id) = schemas();
        let mut set = ComponentSet::new();
        assert!(matches!(
            set.insert(&schemas, &key("base:missing"), BTreeMap::new()),
            Err(ComponentError::UnknownSchema { .. })
        ));
        assert!(matches!(
            set.insert(
                &schemas,
                &id,
                fields(&[(key("base:field/x"), ComponentValue::Bool(true))])
            ),
            Err(ComponentError::UnknownField { .. })
        ));
    }

    #[test]
    fn required_fields_must_be_present() {
        let content = compile(m0_fixture_pack()).expect("fixture content compiles");
        let schemas = ComponentSchemas::build(&content);
        let id = key("base:schema/inventory_content");
        let schema = schemas.get(&id).expect("schema exists").clone();
        let required = ComponentSchema {
            fields: vec![ComponentField {
                id: key("base:field/count"),
                field_type: ComponentFieldType::Unsigned32,
                required: true,
            }],
            ..schema
        };
        let mut custom = ComponentSchemas::default();
        custom.schemas.insert(id.clone(), required);
        let mut set = ComponentSet::new();
        assert!(matches!(
            set.insert(&custom, &id, BTreeMap::new()),
            Err(ComponentError::MissingRequiredField { .. })
        ));
        set.insert(
            &custom,
            &id,
            fields(&[(key("base:field/count"), ComponentValue::U32(3))]),
        )
        .expect("required field is present");
    }

    #[test]
    fn limits_and_types_are_enforced() {
        let content = compile(m0_fixture_pack()).expect("fixture content compiles");
        let schemas = ComponentSchemas::build(&content);
        let id = key("base:schema/inventory_content");
        let schema = ComponentSchema {
            fields: vec![
                ComponentField {
                    id: key("base:field/name"),
                    field_type: ComponentFieldType::String { max_bytes: 4 },
                    required: false,
                },
                ComponentField {
                    id: key("base:field/items"),
                    field_type: ComponentFieldType::List { max_items: 2 },
                    required: false,
                },
            ],
            ..schemas.get(&id).expect("schema exists").clone()
        };
        let mut custom = ComponentSchemas::default();
        custom.schemas.insert(id.clone(), schema);
        let mut set = ComponentSet::new();
        assert!(matches!(
            set.insert(
                &custom,
                &id,
                fields(&[(
                    key("base:field/name"),
                    ComponentValue::Text("toolong".to_owned())
                )])
            ),
            Err(ComponentError::LimitExceeded { .. })
        ));
        assert!(matches!(
            set.insert(
                &custom,
                &id,
                fields(&[(
                    key("base:field/items"),
                    ComponentValue::List(vec![
                        ComponentValue::U32(1),
                        ComponentValue::U32(2),
                        ComponentValue::U32(3),
                    ])
                )])
            ),
            Err(ComponentError::LimitExceeded { .. })
        ));
        assert!(matches!(
            set.insert(
                &custom,
                &id,
                fields(&[(key("base:field/name"), ComponentValue::U32(1))])
            ),
            Err(ComponentError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn replacing_and_removing_components_works() {
        let content = compile(m0_fixture_pack()).expect("fixture content compiles");
        let schemas = ComponentSchemas::build(&content);
        let id = key("base:schema/inventory_content");
        let mut set = ComponentSet::new();
        set.insert(&schemas, &id, BTreeMap::new())
            .expect("empty instance is valid");
        assert_eq!(set.len(), 1);
        assert_eq!(set.ids().count(), 1);
        assert!(set.remove(&id));
        assert!(!set.remove(&id));
        assert!(set.is_empty());
        let _ = PersistencePolicy::Always;
        let _ = ReplicationDeclaration::SERVER_PRIVATE;
    }
}
