//! Section lifecycle and authoritative terrain queries.
//!
//! The store owns every section that is present in memory and the state machine
//! that produced it. Queries never guess: a caller either receives an
//! authoritative state, or is told that the data is not loaded, or that the
//! coordinate is outside the supported world. Returning air for an unloaded
//! region is the error this API exists to prevent.
//!
//! Lifecycle transitions are deliberately explicit rather than automatic. A
//! caller that requests a section must also install data, finish loading, and
//! release its tickets, which keeps async work and eviction visible in the
//! call graph instead of hiding behind a background thread.

use std::collections::BTreeMap;

use torse_types::{BlockPos, Revision, SectionPos};

use crate::registry::BlockStateId;
use crate::section::{Section, SectionError};

/// Vertical extent of a world instance.
///
/// `min_y` is inclusive and `max_y` is exclusive, matching the range convention
/// used by Rust slices. Horizontal coordinates are unbounded for M1 because the
/// generator and the collision service derive their own limits from it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldBounds {
    /// Lowest supported block layer.
    pub min_y: i64,
    /// First unsupported block layer above the world.
    pub max_y: i64,
}

impl WorldBounds {
    /// Build a vertical range and reject empty or absurd extents.
    pub fn new(min_y: i64, max_y: i64) -> Result<Self, StoreError> {
        if min_y >= max_y {
            return Err(StoreError::InvalidBounds { min_y, max_y });
        }
        Ok(Self { min_y, max_y })
    }

    /// Return whether a block layer belongs to this world.
    pub const fn contains_y(&self, y: i64) -> bool {
        y >= self.min_y && y < self.max_y
    }
}

/// Result of a terrain query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerrainQuery {
    /// The authoritative state of a loaded cell.
    Ready(BlockStateId),
    /// The section exists conceptually but has no authoritative data in memory.
    NotLoaded,
    /// The coordinate is outside the supported world extent.
    OutOfBounds,
}

impl TerrainQuery {
    /// Return the state if the cell is loaded.
    pub const fn ready(self) -> Option<BlockStateId> {
        match self {
            Self::Ready(state) => Some(state),
            Self::NotLoaded | Self::OutOfBounds => None,
        }
    }
}

/// Lifecycle phase of one section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionPhase {
    /// A load has been requested but no data is present.
    Requested,
    /// Data is being installed or generated.
    Loading,
    /// Authoritative data is readable but the section holds no simulation ticket.
    Ready,
    /// Authoritative data is readable and at least one ticket is held.
    Active,
}

/// Why a caller holds a section in memory.
///
/// Ticket classes are separate because their quotas and eviction rules differ:
/// a player ticket should outrank a background prefetch, and a transaction ticket
/// must keep a cross-section commit from losing its target mid-flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TicketKind {
    /// A player or camera keeps the section visible.
    Player,
    /// Generation, lighting, or meshing work.
    Background,
    /// A commit that reads or writes the section.
    Transaction,
    /// Replication to a connected client.
    Network,
}

impl TicketKind {
    /// All ticket classes in a stable order.
    pub const ALL: [Self; 4] = [
        Self::Player,
        Self::Background,
        Self::Transaction,
        Self::Network,
    ];

    /// Position of this class in the per-section ticket counters.
    pub const fn index(self) -> usize {
        match self {
            Self::Player => 0,
            Self::Background => 1,
            Self::Transaction => 2,
            Self::Network => 3,
        }
    }
}

/// Maximum tickets of one class per section.
pub const TICKET_QUOTA_PER_KIND: u16 = 8;

/// Proof that a section is held for a stated purpose.
///
/// The handle is a value, not a borrow, so callers can hold tickets across
/// function boundaries without borrowing the whole store. Releasing is explicit
/// and checked, which makes double release a reported error instead of an
/// underflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TicketHandle {
    pos: SectionPos,
    kind: TicketKind,
}

impl TicketHandle {
    /// Section kept alive by this ticket.
    pub const fn section(self) -> SectionPos {
        self.pos
    }

    /// Purpose recorded for this ticket.
    pub const fn kind(self) -> TicketKind {
        self.kind
    }
}

#[derive(Debug)]
struct Entry {
    section: Section,
    phase: SectionPhase,
    tickets: [u16; 4],
}

impl Entry {
    const fn ticket_total(&self) -> u32 {
        let mut total = 0u32;
        let mut index = 0usize;
        while index < self.tickets.len() {
            total += self.tickets[index] as u32;
            index += 1;
        }
        total
    }
}

/// In-memory terrain keyed by section coordinate.
#[derive(Debug)]
pub struct SectionStore {
    side: u32,
    bounds: WorldBounds,
    sections: BTreeMap<SectionPos, Entry>,
}

impl SectionStore {
    /// Create an empty store.
    pub fn new(side: u32, bounds: WorldBounds) -> Result<Self, SectionError> {
        let _ = Section::empty(side)?;
        Ok(Self {
            side,
            bounds,
            sections: BTreeMap::new(),
        })
    }

    /// Section side length in cells.
    pub const fn side(&self) -> u32 {
        self.side
    }

    /// Supported vertical extent.
    pub const fn bounds(&self) -> WorldBounds {
        self.bounds
    }

    /// Current phase of a section, or `None` when it is absent.
    pub fn phase(&self, pos: SectionPos) -> Option<SectionPhase> {
        self.sections.get(&pos).map(|entry| entry.phase)
    }

    /// Record a load request without providing data yet.
    pub fn request(&mut self, pos: SectionPos) -> Result<SectionPhase, StoreError> {
        match self.sections.get_mut(&pos) {
            None => {
                let section = Section::empty(self.side)?;
                self.sections.insert(
                    pos,
                    Entry {
                        section,
                        phase: SectionPhase::Requested,
                        tickets: [0; 4],
                    },
                );
                Ok(SectionPhase::Requested)
            }
            Some(entry) => match entry.phase {
                SectionPhase::Requested
                | SectionPhase::Loading
                | SectionPhase::Ready
                | SectionPhase::Active => Ok(entry.phase),
            },
        }
    }

    /// Install generated or read data for a requested section.
    pub fn install(&mut self, pos: SectionPos, section: Section) -> Result<(), StoreError> {
        if section.side() != self.side {
            return Err(StoreError::SectionSideMismatch {
                expected: self.side,
                actual: section.side(),
            });
        }
        let entry = self
            .sections
            .get_mut(&pos)
            .ok_or(StoreError::UnknownSection { pos })?;
        match entry.phase {
            SectionPhase::Requested | SectionPhase::Loading => {
                entry.section = section;
                entry.phase = SectionPhase::Loading;
                Ok(())
            }
            phase => Err(StoreError::WrongPhase {
                pos,
                phase,
                expected: "requested or loading",
            }),
        }
    }

    /// Mark installed data as readable.
    pub fn finish_load(&mut self, pos: SectionPos) -> Result<(), StoreError> {
        let entry = self
            .sections
            .get_mut(&pos)
            .ok_or(StoreError::UnknownSection { pos })?;
        match entry.phase {
            SectionPhase::Loading => {
                entry.phase = SectionPhase::Ready;
                Ok(())
            }
            phase => Err(StoreError::WrongPhase {
                pos,
                phase,
                expected: "loading",
            }),
        }
    }

    /// Take a ticket that prevents eviction.
    ///
    /// A ready section becomes active while at least one ticket is held.
    pub fn acquire_ticket(
        &mut self,
        pos: SectionPos,
        kind: TicketKind,
    ) -> Result<TicketHandle, StoreError> {
        let entry = self
            .sections
            .get_mut(&pos)
            .ok_or(StoreError::UnknownSection { pos })?;
        match entry.phase {
            SectionPhase::Ready | SectionPhase::Active => {}
            phase => {
                return Err(StoreError::WrongPhase {
                    pos,
                    phase,
                    expected: "ready or active",
                });
            }
        }
        let counter = &mut entry.tickets[kind.index()];
        if *counter >= TICKET_QUOTA_PER_KIND {
            return Err(StoreError::TicketQuota {
                pos,
                kind,
                quota: TICKET_QUOTA_PER_KIND,
            });
        }
        *counter += 1;
        entry.phase = SectionPhase::Active;
        Ok(TicketHandle { pos, kind })
    }

    /// Release a ticket obtained from [`Self::acquire_ticket`].
    pub fn release_ticket(&mut self, handle: TicketHandle) -> Result<(), StoreError> {
        let entry = self
            .sections
            .get_mut(&handle.pos)
            .ok_or(StoreError::TicketNotHeld { handle })?;
        let counter = &mut entry.tickets[handle.kind.index()];
        if *counter == 0 {
            return Err(StoreError::TicketNotHeld { handle });
        }
        *counter -= 1;
        if entry.ticket_total() == 0 && entry.phase == SectionPhase::Active {
            entry.phase = SectionPhase::Ready;
        }
        Ok(())
    }

    /// Read an authoritative cell.
    pub fn query(&self, pos: BlockPos) -> TerrainQuery {
        if !self.bounds.contains_y(pos.y) {
            return TerrainQuery::OutOfBounds;
        }
        let (section_pos, local) = match pos.to_section(self.side) {
            Ok(converted) => converted,
            Err(_) => return TerrainQuery::OutOfBounds,
        };
        let entry = match self.sections.get(&section_pos) {
            Some(entry) => entry,
            None => return TerrainQuery::NotLoaded,
        };
        match entry.phase {
            SectionPhase::Ready | SectionPhase::Active => match entry.section.get(local) {
                Ok(state) => TerrainQuery::Ready(state),
                Err(_) => TerrainQuery::OutOfBounds,
            },
            SectionPhase::Requested | SectionPhase::Loading => TerrainQuery::NotLoaded,
        }
    }

    /// Write one authoritative cell.
    ///
    /// Only readable sections accept edits. Loading or absent data returns an
    /// error so a caller cannot turn a missing section into air by writing.
    pub fn edit(&mut self, pos: BlockPos, state: BlockStateId) -> Result<Revision, StoreError> {
        if !self.bounds.contains_y(pos.y) {
            return Err(StoreError::OutOfBounds { pos });
        }
        let (section_pos, local) = pos
            .to_section(self.side)
            .map_err(|_| StoreError::OutOfBounds { pos })?;
        let entry = match self.sections.get_mut(&section_pos) {
            Some(entry) => entry,
            None => return Err(StoreError::UnknownSection { pos: section_pos }),
        };
        match entry.phase {
            SectionPhase::Ready | SectionPhase::Active => {}
            phase => {
                return Err(StoreError::NotEditable {
                    pos: section_pos,
                    phase,
                });
            }
        }
        entry.section.set(local, state)?;
        Ok(entry.section.revision())
    }

    /// Terrain revision of a section, if it has data.
    pub fn revision(&self, pos: SectionPos) -> Option<Revision> {
        self.sections
            .get(&pos)
            .map(|entry| entry.section.revision())
    }

    /// Borrow a section's data.
    pub fn section(&self, pos: SectionPos) -> Option<&Section> {
        self.sections.get(&pos).map(|entry| &entry.section)
    }

    /// Remove a section that no caller is using.
    pub fn unload(&mut self, pos: SectionPos) -> Result<(), StoreError> {
        let entry = self
            .sections
            .get(&pos)
            .ok_or(StoreError::UnknownSection { pos })?;
        if entry.ticket_total() > 0 {
            return Err(StoreError::TicketsHeld {
                pos,
                tickets: entry.ticket_total(),
            });
        }
        match entry.phase {
            SectionPhase::Ready => {
                self.sections.remove(&pos);
                Ok(())
            }
            phase => Err(StoreError::WrongPhase {
                pos,
                phase,
                expected: "ready",
            }),
        }
    }

    /// Immediately forget a section regardless of phase, for shutdown or tests.
    ///
    /// Out-of-order teardown is explicit: normal operation goes through
    /// `release_ticket` and `unload`, which enforce the lifecycle rules.
    pub fn discard(&mut self, pos: SectionPos) -> Option<Section> {
        self.sections.remove(&pos).map(|entry| entry.section)
    }

    /// Number of sections tracked by the store, in any phase.
    pub fn len(&self) -> usize {
        self.sections.len()
    }

    /// Return whether no section is tracked.
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Memory accounting used by the acceptance checks.
    pub fn stats(&self) -> StoreStats {
        let mut stats = StoreStats::default();
        for entry in self.sections.values() {
            match entry.phase {
                SectionPhase::Ready | SectionPhase::Active => {
                    stats.loaded_sections += 1;
                    stats.dense_cells += entry.section.allocated_cells();
                    if entry.section.allocated_cells() == 0 {
                        stats.uniform_sections += 1;
                    }
                }
                SectionPhase::Requested | SectionPhase::Loading => stats.pending_sections += 1,
            }
            stats.tickets += entry.ticket_total();
        }
        stats
    }
}

/// Snapshot of store occupancy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StoreStats {
    /// Sections with readable data.
    pub loaded_sections: usize,
    /// Sections waiting for data.
    pub pending_sections: usize,
    /// Loaded sections that still use the uniform representation.
    pub uniform_sections: usize,
    /// Allocated cell identifiers across dense sections.
    pub dense_cells: usize,
    /// Tickets currently held by callers.
    pub tickets: u32,
}

/// Failure produced by the section store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    /// The vertical range is empty or inverted.
    InvalidBounds {
        /// Requested minimum layer.
        min_y: i64,
        /// Requested maximum layer.
        max_y: i64,
    },
    /// The store has no record of the section.
    UnknownSection {
        /// Missing section.
        pos: SectionPos,
    },
    /// A transition was attempted from an incompatible phase.
    WrongPhase {
        /// Section being transitioned.
        pos: SectionPos,
        /// Observed phase.
        phase: SectionPhase,
        /// Required phase description.
        expected: &'static str,
    },
    /// Installed data used a different side length than the store.
    SectionSideMismatch {
        /// Store side length.
        expected: u32,
        /// Provided section side length.
        actual: u32,
    },
    /// The coordinate is outside the supported vertical extent.
    OutOfBounds {
        /// Rejected coordinate.
        pos: BlockPos,
    },
    /// The section has no readable data, so it cannot accept edits.
    NotEditable {
        /// Section that rejected the edit.
        pos: SectionPos,
        /// Observed phase.
        phase: SectionPhase,
    },
    /// One ticket class reached its per-section quota.
    TicketQuota {
        /// Section at the quota.
        pos: SectionPos,
        /// Exhausted class.
        kind: TicketKind,
        /// Configured quota.
        quota: u16,
    },
    /// A ticket was released that the store did not issue, or was released twice.
    TicketNotHeld {
        /// Rejected handle.
        handle: TicketHandle,
    },
    /// Unloading was attempted while tickets are still held.
    TicketsHeld {
        /// Section that is still in use.
        pos: SectionPos,
        /// Number of outstanding tickets.
        tickets: u32,
    },
    /// Section storage rejected the operation.
    Section(SectionError),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBounds { min_y, max_y } => {
                write!(f, "world bounds [{min_y}, {max_y}) are empty")
            }
            Self::UnknownSection { pos } => write!(
                f,
                "section [{}, {}, {}] is not tracked",
                pos.x, pos.y, pos.z
            ),
            Self::WrongPhase {
                pos,
                phase,
                expected,
            } => write!(
                f,
                "section [{}, {}, {}] is {phase:?} but {expected} is required",
                pos.x, pos.y, pos.z
            ),
            Self::SectionSideMismatch { expected, actual } => {
                write!(
                    f,
                    "section side {actual} does not match store side {expected}"
                )
            }
            Self::OutOfBounds { pos } => write!(
                f,
                "block [{}, {}, {}] is outside the world",
                pos.x, pos.y, pos.z
            ),
            Self::NotEditable { pos, phase } => write!(
                f,
                "section [{}, {}, {}] is {phase:?} and cannot be edited",
                pos.x, pos.y, pos.z
            ),
            Self::TicketQuota { pos, kind, quota } => write!(
                f,
                "section [{}, {}, {}] already holds {quota} {kind:?} tickets",
                pos.x, pos.y, pos.z
            ),
            Self::TicketNotHeld { handle } => write!(
                f,
                "no {:?} ticket is held for section [{}, {}, {}]",
                handle.kind, handle.pos.x, handle.pos.y, handle.pos.z
            ),
            Self::TicketsHeld { pos, tickets } => write!(
                f,
                "section [{}, {}, {}] still holds {tickets} tickets",
                pos.x, pos.y, pos.z
            ),
            Self::Section(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<SectionError> for StoreError {
    fn from(error: SectionError) -> Self {
        Self::Section(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::Section;
    use torse_types::LocalPos;

    fn store() -> SectionStore {
        let bounds = WorldBounds::new(0, 256).expect("bounds are valid");
        SectionStore::new(16, bounds).expect("side is valid")
    }

    fn ready(store: &mut SectionStore, section: SectionPos, fill: BlockStateId) {
        store.request(section).expect("request is accepted");
        store
            .install(section, Section::new(16, fill).expect("side is valid"))
            .expect("install is accepted");
        store.finish_load(section).expect("load finishes");
    }

    #[test]
    fn absent_section_is_not_loaded_not_air() {
        let store = store();
        assert_eq!(
            store.query(BlockPos::new(0, 10, 0)),
            TerrainQuery::NotLoaded
        );
        assert_eq!(
            store.query(BlockPos::new(0, 300, 0)),
            TerrainQuery::OutOfBounds
        );
        assert_eq!(store.phase(SectionPos::ZERO), None);
    }

    #[test]
    fn loading_section_still_reports_not_loaded() {
        let mut store = store();
        store
            .request(SectionPos::ZERO)
            .expect("request is accepted");
        assert_eq!(store.query(BlockPos::ZERO), TerrainQuery::NotLoaded);
        store
            .install(SectionPos::ZERO, Section::empty(16).expect("side is valid"))
            .expect("install is accepted");
        assert_eq!(store.query(BlockPos::ZERO), TerrainQuery::NotLoaded);
        store.finish_load(SectionPos::ZERO).expect("load finishes");
        assert_eq!(
            store.query(BlockPos::ZERO),
            TerrainQuery::Ready(BlockStateId::EMPTY)
        );
    }

    #[test]
    fn negative_coordinates_land_in_their_own_section() {
        let mut store = store();
        let section = SectionPos::new(-1, 0, -1);
        ready(&mut store, section, BlockStateId(4));
        assert_eq!(
            store.query(BlockPos::new(-1, 0, -1)),
            TerrainQuery::Ready(BlockStateId(4))
        );
        assert_eq!(store.query(BlockPos::new(0, 0, 0)), TerrainQuery::NotLoaded);
    }

    #[test]
    fn edit_requires_readable_section_and_bumps_revision() {
        let mut store = store();
        assert!(matches!(
            store.edit(BlockPos::ZERO, BlockStateId(1)),
            Err(StoreError::UnknownSection { .. })
        ));
        ready(&mut store, SectionPos::ZERO, BlockStateId::EMPTY);
        let before = store
            .revision(SectionPos::ZERO)
            .expect("section has a revision");
        let after = store
            .edit(BlockPos::ZERO, BlockStateId(1))
            .expect("edit is accepted");
        assert!(after > before);
        assert_eq!(
            store.query(BlockPos::ZERO),
            TerrainQuery::Ready(BlockStateId(1))
        );
    }

    #[test]
    fn tickets_block_eviction_and_phase_tracks_them() {
        let mut store = store();
        ready(&mut store, SectionPos::ZERO, BlockStateId(1));
        assert_eq!(store.phase(SectionPos::ZERO), Some(SectionPhase::Ready));
        let ticket = store
            .acquire_ticket(SectionPos::ZERO, TicketKind::Player)
            .expect("ticket is granted");
        assert_eq!(store.phase(SectionPos::ZERO), Some(SectionPhase::Active));
        assert!(matches!(
            store.unload(SectionPos::ZERO),
            Err(StoreError::TicketsHeld { .. })
        ));
        store.release_ticket(ticket).expect("ticket is released");
        assert_eq!(store.phase(SectionPos::ZERO), Some(SectionPhase::Ready));
        store.unload(SectionPos::ZERO).expect("unload is accepted");
        assert_eq!(store.phase(SectionPos::ZERO), None);
    }

    #[test]
    fn double_release_and_quota_are_reported() {
        let mut store = store();
        ready(&mut store, SectionPos::ZERO, BlockStateId(1));
        let ticket = store
            .acquire_ticket(SectionPos::ZERO, TicketKind::Background)
            .expect("ticket is granted");
        store.release_ticket(ticket).expect("ticket is released");
        assert!(matches!(
            store.release_ticket(ticket),
            Err(StoreError::TicketNotHeld { .. })
        ));

        let mut tickets = Vec::new();
        for _ in 0..TICKET_QUOTA_PER_KIND {
            tickets.push(
                store
                    .acquire_ticket(SectionPos::ZERO, TicketKind::Network)
                    .expect("quota allows the configured number"),
            );
        }
        assert!(matches!(
            store.acquire_ticket(SectionPos::ZERO, TicketKind::Network),
            Err(StoreError::TicketQuota { .. })
        ));
        for ticket in tickets {
            store.release_ticket(ticket).expect("ticket is released");
        }
    }

    #[test]
    fn stats_separate_uniform_from_dense_sections() {
        let mut store = store();
        ready(&mut store, SectionPos::ZERO, BlockStateId(1));
        ready(&mut store, SectionPos::new(1, 0, 0), BlockStateId::EMPTY);
        let stats = store.stats();
        assert_eq!(stats.loaded_sections, 2);
        assert_eq!(stats.uniform_sections, 2);
        assert_eq!(stats.dense_cells, 0);
        store
            .edit(BlockPos::new(1, 2, 3), BlockStateId(2))
            .expect("edit makes the section dense");
        let stats = store.stats();
        assert_eq!(stats.uniform_sections, 1);
        assert_eq!(stats.dense_cells, 16usize.pow(3));
    }

    #[test]
    fn section_side_mismatch_is_rejected() {
        let mut store = store();
        store
            .request(SectionPos::ZERO)
            .expect("request is accepted");
        let wrong = Section::empty(32).expect("side is valid");
        assert!(matches!(
            store.install(SectionPos::ZERO, wrong),
            Err(StoreError::SectionSideMismatch { .. })
        ));
    }

    #[test]
    fn local_query_helper_roundtrips() {
        // Guard against a section-local conversion regression at a negative
        // boundary, which is the classic chunk indexing failure.
        let (section, local) = torse_types::BlockPos::new(-1, 5, -17)
            .to_section(16)
            .expect("conversion succeeds");
        assert_eq!(section, SectionPos::new(-1, 0, -2));
        assert_eq!(local, LocalPos { x: 15, y: 5, z: 15 });
    }
}
