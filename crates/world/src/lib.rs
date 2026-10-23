//! Authoritative world storage and lifecycle primitives.
//!
//! The crate covers compact block-state identity, section storage and lifecycle,
//! entity storage with a section index, and bounded component instances.
//! Simulation phases and commit transactions live in `torse-sim`; persistence
//! lives behind the storage contract. This crate deliberately exposes no "world"
//! facade yet.
//!
//! Three invariants shape the API:
//!
//! - An unloaded cell is never reported as air. [`TerrainQuery`] distinguishes
//!   `Ready`, `NotLoaded`, and `OutOfBounds`.
//! - Editing requires readable data. A write to an absent or loading section
//!   fails instead of materializing a section with the wrong contents.
//! - A stale entity handle cannot reach a replacement. Slot reuse advances the
//!   generation, and the spatial index is maintained inside the store.

#![forbid(unsafe_code)]

mod component;
mod entity;
mod handles;
mod registry;
mod section;
mod store;

pub use component::{
    ComponentError, ComponentSchemas, ComponentSet, ComponentValue, MAX_VALUE_DEPTH,
};
pub use entity::{Entity, EntityError, EntityStore, Position};
pub use handles::HandleTable;
pub use registry::{BlockStateId, BlockStateRegistry, EMPTY_BLOCK_KEY, RegistryError};
pub use section::{
    DEFAULT_SECTION_SIDE, MAX_SECTION_SIDE, Section, SectionError, SectionStorage, linear_index,
};
pub use store::{
    BlockEdit, SectionPhase, SectionStore, StoreError, StoreStats, TICKET_QUOTA_PER_KIND,
    TerrainQuery, TicketHandle, TicketKind, WorldBounds,
};
