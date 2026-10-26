//! A stationary entity attached to one logical block cell.

use torse_content::{ComponentField, ComponentFieldType, ComponentSchema, PersistencePolicy};
use torse_types::{BlockPos, ContentKey, EntityHandle, ReplicationDeclaration};

use crate::{ComponentFields, ComponentValue, Entity, EntityError, EntityStore, Position};

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("static block instance key")
}

/// Reserved component identity managed by the kernel, never by action payloads.
pub fn block_instance_schema() -> ContentKey {
    key("core:schema/block_instance")
}

pub(crate) fn binding_schema() -> ComponentSchema {
    ComponentSchema {
        id: block_instance_schema(),
        schema_version: 1,
        owner_module: key("core:module"),
        fields: ["x", "y", "z", "block"]
            .into_iter()
            .map(|name| ComponentField {
                id: key(&format!("core:field/{name}")),
                field_type: if name == "block" {
                    ComponentFieldType::ContentReference
                } else {
                    ComponentFieldType::Signed64
                },
                required: true,
            })
            .collect(),
        persistence: PersistencePolicy::Always,
        replication: ReplicationDeclaration {
            visibility: torse_types::ReplicationVisibility::Public,
            ..ReplicationDeclaration::SERVER_PRIVATE
        },
        migration_binding: None,
    }
}

/// Stable link between an entity and its owning block. Finite state changes of
/// the same block retain this identity; replacement creates a new identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockInstance {
    pub pos: BlockPos,
    pub block: ContentKey,
}

impl BlockInstance {
    /// Canonical binding fields stored alongside the owner's components.
    pub fn fields(&self) -> ComponentFields {
        [
            (key("core:field/x"), ComponentValue::I64(self.pos.x)),
            (key("core:field/y"), ComponentValue::I64(self.pos.y)),
            (key("core:field/z"), ComponentValue::I64(self.pos.z)),
            (
                key("core:field/block"),
                ComponentValue::Content(self.block.clone()),
            ),
        ]
        .into_iter()
        .collect()
    }

    /// The owning cell's center. An attached entity has no implied collider.
    pub fn position(&self) -> Position {
        Position::new(
            self.pos.x as f64 + 0.5,
            self.pos.y as f64 + 0.5,
            self.pos.z as f64 + 0.5,
        )
    }

    /// Read and validate the complete binding, including its spatial index position.
    pub fn of(entity: &Entity) -> Result<Option<Self>, EntityError> {
        let Some(fields) = entity.components.component(&block_instance_schema()) else {
            return Ok(None);
        };
        let coordinate = |name: &str| match fields.get(&key(name)) {
            Some(ComponentValue::I64(value)) => Ok(*value),
            _ => Err(EntityError::InvalidComponents),
        };
        let Some(ComponentValue::Content(block)) = fields.get(&key("core:field/block")) else {
            return Err(EntityError::InvalidComponents);
        };
        let instance = Self {
            pos: BlockPos::new(
                coordinate("core:field/x")?,
                coordinate("core:field/y")?,
                coordinate("core:field/z")?,
            ),
            block: block.clone(),
        };
        if fields.len() != 4 || instance.position().to_bits() != entity.position.to_bits() {
            return Err(EntityError::InvalidPosition);
        }
        Ok(Some(instance))
    }
}

impl EntityStore {
    /// Look up a block's attached entity through the section spatial index.
    /// Duplicate or malformed bindings fail instead of choosing an arbitrary one.
    pub fn block_instance_at(
        &self,
        pos: BlockPos,
    ) -> Result<Option<(EntityHandle, &Entity)>, EntityError> {
        let (section, _) = pos
            .to_section(self.side())
            .map_err(|_| EntityError::InvalidPosition)?;
        let mut found = None;
        for handle in self.entities_in_section(section) {
            let entity = self.get(handle).ok_or(EntityError::StaleHandle)?;
            if BlockInstance::of(entity)?.is_some_and(|instance| instance.pos == pos) {
                if found.is_some() {
                    return Err(EntityError::InvalidLifecycle);
                }
                found = Some((handle, entity));
            }
        }
        Ok(found)
    }
}
