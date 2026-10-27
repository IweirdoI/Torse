//! Optional, versioned residency declarations carried by the existing event payload codec.
use crate::durable::EventDraft;
use std::sync::OnceLock;
use torse_types::{ContentKey, EntityId, Fault, SectionPos};
use torse_world::ComponentValue;

pub const MAX_EVENT_DEPENDENCIES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum EventDependency {
    Section(SectionPos),
    Entity(EntityId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventDependencies {
    /// Legacy events and planners that cannot bound their reads keep every resident section.
    Global,
    /// A trusted planner declares every section/entity it may need. Empty is world-independent.
    Scoped(Vec<EventDependency>),
}

fn key() -> &'static ContentKey {
    static KEY: OnceLock<ContentKey> = OnceLock::new();
    KEY.get_or_init(|| {
        ContentKey::parse("torse:event/dependencies-v1").expect("static event metadata key")
    })
}

impl EventDraft {
    /// Attach a canonical declaration without changing the event or execution wire layout.
    /// Rule handlers own the completeness of this declaration; omission remains conservative.
    pub fn with_dependencies(
        mut self,
        mut dependencies: Vec<EventDependency>,
    ) -> Result<Self, Fault> {
        if dependencies.len() > MAX_EVENT_DEPENDENCIES {
            return Err(Fault::resource_limit("event dependency count exceeded"));
        }
        dependencies.sort_unstable();
        dependencies.dedup();
        if dependencies
            .iter()
            .any(|d| matches!(d, EventDependency::Entity(EntityId(0))))
        {
            return Err(Fault::invalid_data("zero event dependency entity"));
        }
        let values = dependencies
            .into_iter()
            .map(|dependency| {
                ComponentValue::List(match dependency {
                    EventDependency::Section(pos) => vec![
                        ComponentValue::U32(0),
                        ComponentValue::I64(pos.x),
                        ComponentValue::I64(pos.y),
                        ComponentValue::I64(pos.z),
                    ],
                    EventDependency::Entity(id) => {
                        vec![ComponentValue::U32(1), ComponentValue::Entity(id)]
                    }
                })
            })
            .collect();
        self.payload
            .insert(key().clone(), ComponentValue::List(values));
        Ok(self)
    }

    pub fn dependencies(&self) -> Result<EventDependencies, Fault> {
        let Some(value) = self.payload.get(key()) else {
            return Ok(EventDependencies::Global);
        };
        let ComponentValue::List(values) = value else {
            return Err(Fault::invalid_data("invalid event dependency declaration"));
        };
        if values.len() > MAX_EVENT_DEPENDENCIES {
            return Err(Fault::resource_limit("event dependency count exceeded"));
        }
        let mut dependencies = Vec::new();
        dependencies
            .try_reserve_exact(values.len())
            .map_err(|_| Fault::resource_limit("event dependency allocation"))?;
        for value in values {
            let ComponentValue::List(fields) = value else {
                return Err(Fault::invalid_data("invalid event dependency entry"));
            };
            let dependency = match fields.as_slice() {
                [
                    ComponentValue::U32(0),
                    ComponentValue::I64(x),
                    ComponentValue::I64(y),
                    ComponentValue::I64(z),
                ] => EventDependency::Section(SectionPos::new(*x, *y, *z)),
                [ComponentValue::U32(1), ComponentValue::Entity(id)] if id.0 != 0 => {
                    EventDependency::Entity(*id)
                }
                _ => return Err(Fault::invalid_data("invalid event dependency entry")),
            };
            if dependencies
                .last()
                .is_some_and(|previous| *previous >= dependency)
            {
                return Err(Fault::invalid_data("noncanonical event dependencies"));
            }
            dependencies.push(dependency);
        }
        Ok(EventDependencies::Scoped(dependencies))
    }

    /// Recovery can check an unload directly against its validated page identities.
    pub fn pins_section(
        &self,
        pos: SectionPos,
        entities: impl Fn(EntityId) -> bool,
    ) -> Result<bool, Fault> {
        Ok(match self.dependencies()? {
            EventDependencies::Global => true,
            EventDependencies::Scoped(dependencies) => dependencies.iter().any(|d| match d {
                EventDependency::Section(section) => *section == pos,
                EventDependency::Entity(id) => entities(*id),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> EventDraft {
        EventDraft {
            tick: torse_types::Tick(10),
            rule: ContentKey::parse("test:rule").unwrap(),
            payload: Default::default(),
        }
    }
    #[test]
    fn legacy_events_remain_global_and_scoped_dependencies_are_canonical() {
        assert_eq!(event().dependencies().unwrap(), EventDependencies::Global);
        let draft = event()
            .with_dependencies(vec![
                EventDependency::Entity(EntityId(1)),
                EventDependency::Section(SectionPos::ZERO),
                EventDependency::Section(SectionPos::ZERO),
            ])
            .unwrap();
        assert!(draft.pins_section(SectionPos::ZERO, |_| false).unwrap());
        assert!(
            draft
                .pins_section(SectionPos::new(1, 0, 0), |id| id == EntityId(1))
                .unwrap()
        );
        assert!(
            !draft
                .pins_section(SectionPos::new(1, 0, 0), |_| false)
                .unwrap()
        );
        assert!(
            !event()
                .with_dependencies(Vec::new())
                .unwrap()
                .pins_section(SectionPos::ZERO, |_| false)
                .unwrap()
        );
        assert!(
            event()
                .with_dependencies(vec![EventDependency::Entity(EntityId(0))])
                .is_err()
        );
        assert!(
            event()
                .with_dependencies(vec![EventDependency::Section(SectionPos::ZERO); 65])
                .is_err()
        );
    }
    #[test]
    fn malformed_metadata_cannot_be_interpreted_as_no_dependencies() {
        let mut draft = event();
        draft.payload.insert(key().clone(), ComponentValue::U32(0));
        assert!(draft.dependencies().is_err());
        let mut draft = event()
            .with_dependencies(vec![
                EventDependency::Entity(EntityId(1)),
                EventDependency::Section(SectionPos::ZERO),
            ])
            .unwrap();
        let ComponentValue::List(values) = draft.payload.get_mut(key()).unwrap() else {
            unreachable!()
        };
        values.reverse();
        assert!(draft.dependencies().is_err());
    }
}
