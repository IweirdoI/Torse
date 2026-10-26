//! Durable projections. Lossless exchange and authorized replication stay separate.

use crate::{ChangeSet, ComponentSchemas, EntityState, ExchangeError, Snapshot};
use std::borrow::Cow;

fn needs_projection<'a>(
    schemas: &ComponentSchemas,
    keys: impl Iterator<Item = &'a torse_types::ContentKey>,
) -> Result<bool, ExchangeError> {
    let mut needed = false;
    for key in keys {
        needed |= !schemas.persists(key).map_err(error)?;
    }
    Ok(needed)
}

fn error(error: crate::ComponentError) -> ExchangeError {
    ExchangeError::Store {
        detail: error.to_string(),
    }
}

impl EntityState {
    /// Remove runtime-only attached state without changing identity or position.
    pub fn retain_persistent(&mut self, schemas: &ComponentSchemas) -> Result<(), ExchangeError> {
        for schema in self.components.keys() {
            schemas.persists(schema).map_err(error)?;
        }
        self.components
            .retain(|schema, _| schemas.persists(schema).expect("checked schema"));
        Ok(())
    }
}

impl Snapshot {
    /// Borrow already projected snapshots, including snapshots with no components.
    pub fn persistent_view<'a>(
        &'a self,
        schemas: &ComponentSchemas,
    ) -> Result<Cow<'a, Self>, ExchangeError> {
        if !needs_projection(
            schemas,
            self.entities
                .iter()
                .flat_map(|e| e.components.iter().map(|c| &c.schema)),
        )? {
            return Ok(Cow::Borrowed(self));
        }
        let mut projected = self.clone();
        projected.retain_persistent(schemas)?;
        Ok(Cow::Owned(projected))
    }
    /// Apply save policies to an owned world payload. Entity identities and the
    /// allocator watermark survive even when every component is runtime-only.
    pub fn retain_persistent(&mut self, schemas: &ComponentSchemas) -> Result<(), ExchangeError> {
        for entity in &mut self.entities {
            for component in &entity.components {
                schemas.persists(&component.schema).map_err(error)?;
            }
            entity
                .components
                .retain(|c| schemas.persists(&c.schema).expect("checked schema"));
        }
        Ok(())
    }
}

impl ChangeSet {
    /// Normalize legacy complete records without cloning the normal durable path.
    pub fn persistent_view<'a>(
        &'a self,
        schemas: &ComponentSchemas,
    ) -> Result<Cow<'a, Self>, ExchangeError> {
        let keys = self.component_edits.iter().map(|e| &e.schema).chain(
            self.entity_lifecycle
                .iter()
                .flat_map(|l| l.spawned.iter().chain(&l.despawned))
                .flat_map(|e| e.components.keys()),
        );
        if !needs_projection(schemas, keys)? {
            return Ok(Cow::Borrowed(self));
        }
        let mut projected = self.clone();
        projected.retain_persistent(schemas)?;
        Ok(Cow::Owned(projected))
    }
    /// Project both lifecycle observations and component replacements. Filtering
    /// only snapshots would make later removals fail their exact-state checks.
    pub fn retain_persistent(&mut self, schemas: &ComponentSchemas) -> Result<(), ExchangeError> {
        for edit in &self.component_edits {
            schemas.persists(&edit.schema).map_err(error)?;
        }
        self.component_edits
            .retain(|edit| schemas.persists(&edit.schema).expect("checked schema"));
        if let Some(change) = &mut self.entity_lifecycle {
            for entity in change.spawned.iter_mut().chain(&mut change.despawned) {
                entity.retain_persistent(schemas)?;
            }
        }
        Ok(())
    }
}
