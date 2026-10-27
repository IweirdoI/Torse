//! Persistent integration state for a host-owned walking body.
use crate::{ComponentError, ComponentFields, ComponentValue, Entity};
use torse_content::{ComponentField, ComponentFieldType, ComponentSchema, PersistencePolicy};
use torse_types::{ContentKey, ReplicationDeclaration, ReplicationVisibility};

fn key(name: &str) -> ContentKey {
    ContentKey::parse(name).expect("static player motion key")
}
pub fn player_motion_schema() -> ContentKey {
    key("core:schema/player-motion")
}

/// Vertical velocity uses millionths of a block per second, with a terminal speed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlayerMotion {
    pub vertical_micros: i64,
    pub grounded: bool,
}
impl PlayerMotion {
    pub const MAX_SPEED: i64 = 64_000_000;
    pub fn fields(self) -> ComponentFields {
        [
            (
                key("core:field/vertical-speed"),
                ComponentValue::I64(self.vertical_micros),
            ),
            (
                key("core:field/grounded"),
                ComponentValue::Bool(self.grounded),
            ),
        ]
        .into()
    }
    pub fn from_fields(fields: &ComponentFields) -> Result<Self, ComponentError> {
        let invalid = || ComponentError::InvalidConfiguration {
            id: player_motion_schema(),
        };
        let Some(ComponentValue::I64(speed)) = fields.get(&key("core:field/vertical-speed")) else {
            return Err(invalid());
        };
        let Some(ComponentValue::Bool(grounded)) = fields.get(&key("core:field/grounded")) else {
            return Err(invalid());
        };
        if fields.len() != 2
            || !(-Self::MAX_SPEED..=Self::MAX_SPEED).contains(speed)
            || (*grounded && *speed != 0)
        {
            return Err(invalid());
        }
        Ok(Self {
            vertical_micros: *speed,
            grounded: *grounded,
        })
    }
    pub fn of(entity: &Entity) -> Result<Option<Self>, ComponentError> {
        entity
            .components
            .component(&player_motion_schema())
            .map(Self::from_fields)
            .transpose()
    }
}
pub(crate) fn schema() -> ComponentSchema {
    ComponentSchema {
        id: player_motion_schema(),
        schema_version: 1,
        owner_module: crate::collider_owner(),
        fields: vec![
            ComponentField {
                id: key("core:field/vertical-speed"),
                field_type: ComponentFieldType::Signed64,
                required: true,
            },
            ComponentField {
                id: key("core:field/grounded"),
                field_type: ComponentFieldType::Bool,
                required: true,
            },
        ],
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
    fn persistent_motion_has_canonical_finite_bounded_fields() {
        for motion in [
            PlayerMotion::default(),
            PlayerMotion {
                vertical_micros: -PlayerMotion::MAX_SPEED,
                grounded: false,
            },
            PlayerMotion {
                vertical_micros: 0,
                grounded: true,
            },
        ] {
            assert_eq!(PlayerMotion::from_fields(&motion.fields()), Ok(motion));
        }
        for motion in [
            PlayerMotion {
                vertical_micros: i64::MIN,
                grounded: false,
            },
            PlayerMotion {
                vertical_micros: i64::MAX,
                grounded: false,
            },
            PlayerMotion {
                vertical_micros: 1,
                grounded: true,
            },
        ] {
            assert!(PlayerMotion::from_fields(&motion.fields()).is_err());
        }
    }
}
