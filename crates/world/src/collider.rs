//! Persistent axis-aligned entity bodies, expressed in millionths of a block.
use crate::{ComponentError, ComponentFields, ComponentValue, Entity};
use torse_content::{ComponentField, ComponentFieldType, ComponentSchema, PersistencePolicy};
use torse_types::{ContentKey, ReplicationDeclaration, ReplicationVisibility};

pub const COLLIDER_SCALE: u32 = 1_000_000;
pub const MAX_COLLIDER_EXTENT: u32 = 4 * COLLIDER_SCALE;
fn key(s: &str) -> ContentKey {
    ContentKey::parse(s).expect("static collider key")
}
pub fn collider_schema() -> ContentKey {
    key("core:schema/collider")
}
pub fn collider_owner() -> ContentKey {
    key("core:module/physics")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityCollider {
    Disabled,
    Box { half: [u32; 3] },
}
impl EntityCollider {
    pub fn fields(self) -> ComponentFields {
        let (enabled, half) = match self {
            Self::Disabled => (false, [0; 3]),
            Self::Box { half } => (true, half),
        };
        [
            (key("core:field/enabled"), ComponentValue::Bool(enabled)),
            (key("core:field/half_x"), ComponentValue::U32(half[0])),
            (key("core:field/half_y"), ComponentValue::U32(half[1])),
            (key("core:field/half_z"), ComponentValue::U32(half[2])),
        ]
        .into()
    }
    pub fn from_fields(fields: &ComponentFields) -> Result<Self, ComponentError> {
        let invalid = || ComponentError::InvalidConfiguration {
            id: collider_schema(),
        };
        let Some(ComponentValue::Bool(enabled)) = fields.get(&key("core:field/enabled")) else {
            return Err(invalid());
        };
        let mut half = [0; 3];
        for (axis, name) in ["half_x", "half_y", "half_z"].into_iter().enumerate() {
            let Some(ComponentValue::U32(value)) = fields.get(&key(&format!("core:field/{name}")))
            else {
                return Err(invalid());
            };
            half[axis] = *value;
        }
        if fields.len() != 4
            || (*enabled && half.iter().any(|h| *h == 0 || *h > MAX_COLLIDER_EXTENT))
            || (!enabled && half != [0; 3])
        {
            return Err(invalid());
        }
        Ok(if *enabled {
            Self::Box { half }
        } else {
            Self::Disabled
        })
    }
    pub fn of(entity: &Entity) -> Result<Option<Self>, ComponentError> {
        entity
            .components
            .component(&collider_schema())
            .map(Self::from_fields)
            .transpose()
    }
    pub fn half(self) -> Option<[f64; 3]> {
        match self {
            Self::Disabled => None,
            Self::Box { half } => Some(half.map(|h| f64::from(h) / f64::from(COLLIDER_SCALE))),
        }
    }
}
pub(crate) fn schema() -> ComponentSchema {
    ComponentSchema {
        id: collider_schema(),
        schema_version: 1,
        owner_module: collider_owner(),
        fields: ["enabled", "half_x", "half_y", "half_z"]
            .into_iter()
            .map(|name| ComponentField {
                id: key(&format!("core:field/{name}")),
                field_type: if name == "enabled" {
                    ComponentFieldType::Bool
                } else {
                    ComponentFieldType::Unsigned32
                },
                required: true,
            })
            .collect(),
        persistence: PersistencePolicy::Always,
        replication: ReplicationDeclaration {
            visibility: ReplicationVisibility::Public,
            ..ReplicationDeclaration::SERVER_PRIVATE
        },
        migration_binding: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_bodies_have_canonical_bounded_fields() {
        for body in [
            EntityCollider::Disabled,
            EntityCollider::Box {
                half: [1, COLLIDER_SCALE, MAX_COLLIDER_EXTENT],
            },
        ] {
            assert_eq!(EntityCollider::from_fields(&body.fields()), Ok(body));
        }
        for half in [[0, 1, 1], [1, MAX_COLLIDER_EXTENT + 1, 1]] {
            assert!(EntityCollider::from_fields(&EntityCollider::Box { half }.fields()).is_err());
        }
        let mut fields = EntityCollider::Disabled.fields();
        fields.insert(key("core:field/half_x"), ComponentValue::U32(1));
        assert!(EntityCollider::from_fields(&fields).is_err());
        fields = EntityCollider::Disabled.fields();
        fields.insert(key("test:extra"), ComponentValue::Bool(false));
        assert!(EntityCollider::from_fields(&fields).is_err());
    }
}
