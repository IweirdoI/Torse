//! Authoritative world storage and lifecycle primitives.
//!
//! The crate covers compact block-state identity, section storage and lifecycle,
//! entity storage with a section index, bounded component instances, and the
//! exchange payloads that cross the authority boundary. Simulation phases and
//! commit transactions live in `torse-sim`; encoding lives in `torse-codec`.
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

mod archive;
mod block_instance;
mod collider;
mod component;
mod entity;
mod exchange;
mod handles;
mod lifecycle;
mod palette;
mod persistence;
mod registry;
mod section;
mod snapshot_budget;
mod store;

pub use archive::{
    ArchiveIssue, ArchiveManifest, BlockStamp, ComponentStamp, MigrateFields, MigrationPlan,
    SnapshotArchive,
};
pub use block_instance::{BlockInstance, block_instance_schema};
pub use collider::{
    COLLIDER_SCALE, EntityCollider, MAX_COLLIDER_EXTENT, collider_owner, collider_schema,
};
pub use component::{
    ComponentError, ComponentFields, ComponentSchemas, ComponentSet, ComponentValue,
    MAX_COMPONENT_BYTES, MAX_COMPONENT_INSTANCES, MAX_COMPONENT_NODES, MAX_VALUE_DEPTH,
    component_size,
};
pub use entity::{
    ComponentBatch, ComponentEdit, Entity, EntityError, EntityMove, EntityStore,
    MAX_COMPONENT_EDITS, Position,
};
pub use exchange::{
    CellContent, ChangeEdit, ChangeSet, ExchangeError, RestoredWorld, Snapshot, SnapshotCells,
    SnapshotComponent, SnapshotEntity, SnapshotSection, SnapshotValue, StateEntry,
};
pub use handles::HandleTable;
pub use lifecycle::{
    EntityLifecycle, EntityState, LifecycleBatch, MAX_ENTITY_LIFECYCLE, MAX_ENTITY_LIFECYCLE_BYTES,
    MAX_SPAWN_COORDINATE,
};
pub use palette::{MAX_PALETTE_STATES, PaletteCells};
pub use registry::{BlockStateId, BlockStateRegistry, EMPTY_BLOCK_KEY, RegistryError};
pub use section::{
    DEFAULT_SECTION_SIDE, MAX_SECTION_SIDE, Section, SectionError, SectionStorage, linear_index,
};
pub use store::{
    BlockEdit, SectionPhase, SectionStore, StoreError, StoreStats, TICKET_QUOTA_PER_KIND,
    TerrainQuery, TicketHandle, TicketKind, WorldBounds,
};
