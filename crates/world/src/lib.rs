//! Authoritative world storage and lifecycle primitives.
//!
//! M1 covers compact block-state identity, section storage, section lifecycle
//! with load tickets, and authoritative terrain queries. Simulation phases,
//! commit transactions, entity storage, and persistence belong to later
//! milestones, so this crate deliberately exposes no "world" facade yet.
//!
//! Two invariants shape the API:
//!
//! - An unloaded cell is never reported as air. [`TerrainQuery`] distinguishes
//!   `Ready`, `NotLoaded`, and `OutOfBounds`.
//! - Editing requires readable data. A write to an absent or loading section
//!   fails instead of materializing a section with the wrong contents.

#![forbid(unsafe_code)]

mod handles;
mod registry;
mod section;
mod store;

pub use handles::HandleTable;
pub use registry::{BlockStateId, BlockStateRegistry, EMPTY_BLOCK_KEY, RegistryError};
pub use section::{
    DEFAULT_SECTION_SIDE, MAX_SECTION_SIDE, Section, SectionError, SectionStorage, linear_index,
};
pub use store::{
    BlockEdit, SectionPhase, SectionStore, StoreError, StoreStats, TICKET_QUOTA_PER_KIND,
    TerrainQuery, TicketHandle, TicketKind, WorldBounds,
};
