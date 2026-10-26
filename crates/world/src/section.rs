//! Section storage for authoritative block cells.
//!
//! A section is a fixed-size cube of cells. The M1 prototype starts with the
//! simplest correct representation and keeps the layout details private, so
//! choosing palette compression or cold/hot splitting later does not change any
//! public query.
//!
//! Two representations exist today:
//!
//! - `Uniform`: every cell holds the same state. An untouched or fully empty
//!   section costs one state identifier and no per-cell array.
//! - `Dense`: one identifier per cell. Sections switch to this form on the first
//!   edit and stay dense, even if later edits make them uniform again. Returning
//!   to `Uniform` would save memory but adds a second transition that M1 does not
//!   need yet.
//!
//! Linear indexing follows the architecture note: `x + side * (z + side * y)`.
//! The encoding is internal, but fixing it here keeps tests and future
//! serialization from inventing a second order.

use torse_types::{LocalPos, Revision, SpatialError};

use crate::registry::BlockStateId;

/// Section side used by the M1 prototype.
///
/// The public API takes the side explicitly so the 16-versus-32 experiment can
/// still change the default without changing types.
pub const DEFAULT_SECTION_SIDE: u32 = 32;

/// Largest supported side that keeps `side^3` inside `u32` and under a million
/// cells, which bounds any single allocation made by storage.
pub const MAX_SECTION_SIDE: u32 = 64;

/// Internal cell layout of a section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SectionStorage {
    /// Every cell holds the same state.
    Uniform(BlockStateId),
    /// One state identifier per cell, in linear index order.
    Dense(Vec<BlockStateId>),
}

/// A fixed-size cube of authoritative block cells.
///
/// The revision advances whenever a cell changes value. Derived work such as
/// meshing captures the revision and refuses to publish a result computed from
/// an older one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    side: u32,
    revision: Revision,
    storage: SectionStorage,
}

impl Section {
    pub(crate) fn resume_revision(&mut self, after: Revision) -> Result<(), SectionError> {
        self.revision = Revision(
            self.revision
                .0
                .max(after.0)
                .checked_add(1)
                .ok_or(SectionError::RevisionExhausted)?,
        );
        Ok(())
    }
    /// Create a section filled with one state.
    pub fn new(side: u32, fill: BlockStateId) -> Result<Self, SectionError> {
        let _ = checked_cell_count(side)?;
        Ok(Self {
            side,
            revision: Revision::INITIAL,
            storage: SectionStorage::Uniform(fill),
        })
    }

    /// Create an empty section whose cells contain no block.
    pub fn empty(side: u32) -> Result<Self, SectionError> {
        Self::new(side, BlockStateId::EMPTY)
    }

    /// Create a section from an explicit cell layout.
    ///
    /// Used by restore, which receives storage that was already validated by the
    /// snapshot but still must match the side length of this run.
    pub fn from_storage(side: u32, storage: SectionStorage) -> Result<Self, SectionError> {
        let expected = checked_cell_count(side)?;
        if let SectionStorage::Dense(cells) = &storage
            && cells.len() != expected
        {
            return Err(SectionError::CellCountMismatch {
                expected,
                actual: cells.len(),
            });
        }
        Ok(Self {
            side,
            revision: Revision::INITIAL,
            storage,
        })
    }

    /// Section side length in cells.
    pub const fn side(&self) -> u32 {
        self.side
    }

    /// Current terrain revision of this section.
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Current internal representation.
    pub const fn storage(&self) -> &SectionStorage {
        &self.storage
    }

    /// Number of allocated cells, or zero for a uniform section.
    pub fn allocated_cells(&self) -> usize {
        match &self.storage {
            SectionStorage::Uniform(_) => 0,
            SectionStorage::Dense(cells) => cells.len(),
        }
    }

    /// Read one cell.
    pub fn get(&self, local: LocalPos) -> Result<BlockStateId, SectionError> {
        let index = self.index(local)?;
        Ok(match &self.storage {
            SectionStorage::Uniform(value) => *value,
            SectionStorage::Dense(cells) => cells[index],
        })
    }

    /// Write one cell.
    ///
    /// Returns `true` when the value changed and the revision advanced. Writing
    /// the same value is free and does not invalidate derived work.
    pub fn set(&mut self, local: LocalPos, state: BlockStateId) -> Result<bool, SectionError> {
        let index = self.index(local)?;
        if matches!(&self.storage, SectionStorage::Uniform(value) if *value == state) {
            return Ok(false);
        }
        if let SectionStorage::Uniform(value) = &self.storage {
            let fill = *value;
            let cells = vec![fill; checked_cell_count(self.side)?];
            self.storage = SectionStorage::Dense(cells);
        }
        let SectionStorage::Dense(cells) = &mut self.storage else {
            unreachable!("storage was converted to dense above");
        };
        if cells[index] == state {
            return Ok(false);
        }
        cells[index] = state;
        self.revision = self
            .revision
            .next()
            .ok_or(SectionError::RevisionExhausted)?;
        Ok(true)
    }

    /// Visit every cell in linear index order.
    pub fn for_each(&self, mut visit: impl FnMut(LocalPos, BlockStateId)) {
        match &self.storage {
            SectionStorage::Uniform(value) => {
                for y in 0..self.side {
                    for z in 0..self.side {
                        for x in 0..self.side {
                            visit(LocalPos { x, y, z }, *value);
                        }
                    }
                }
            }
            SectionStorage::Dense(cells) => {
                for (index, value) in cells.iter().enumerate() {
                    visit(self.local_of(index), *value);
                }
            }
        }
    }

    fn index(&self, local: LocalPos) -> Result<usize, SectionError> {
        let side = self.side;
        if local.x >= side || local.y >= side || local.z >= side {
            return Err(SectionError::LocalOutOfRange {
                side,
                local: [local.x, local.y, local.z],
            });
        }
        Ok(linear_index(side, local))
    }

    fn local_of(&self, index: usize) -> LocalPos {
        let side = self.side as usize;
        let x = index % side;
        let rest = index / side;
        let z = rest % side;
        let y = rest / side;
        LocalPos {
            x: x as u32,
            y: y as u32,
            z: z as u32,
        }
    }
}

/// Linear cell index inside an untranslated section.
///
/// The order is `x + side * (z + side * y)`, matching the architecture note and
/// the `for_each` traversal above.
pub fn linear_index(side: u32, local: LocalPos) -> usize {
    let side = side as usize;
    local.x as usize + side * (local.z as usize + side * local.y as usize)
}

fn checked_cell_count(side: u32) -> Result<usize, SectionError> {
    if side == 0 || side > MAX_SECTION_SIDE {
        return Err(SectionError::SideOutOfRange {
            side,
            max: MAX_SECTION_SIDE,
        });
    }
    Ok((side as usize).pow(3))
}

/// Failure produced by section storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionError {
    /// The requested side length is zero or larger than the supported maximum.
    SideOutOfRange {
        /// Requested side.
        side: u32,
        /// Largest supported side.
        max: u32,
    },
    /// A local coordinate did not fit inside the section.
    LocalOutOfRange {
        /// Section side.
        side: u32,
        /// Offending coordinate components.
        local: [u32; 3],
    },
    /// The revision counter cannot advance without wrapping.
    RevisionExhausted,
    /// A dense layout did not contain exactly `side^3` cells.
    CellCountMismatch {
        /// Expected number of cells.
        expected: usize,
        /// Observed number of cells.
        actual: usize,
    },
}

impl std::fmt::Display for SectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SideOutOfRange { side, max } => {
                write!(f, "section side {side} must be in 1..={max}")
            }
            Self::LocalOutOfRange { side, local } => write!(
                f,
                "local coordinate [{}, {}, {}] is outside a section of side {side}",
                local[0], local[1], local[2]
            ),
            Self::RevisionExhausted => write!(f, "section revision counter exhausted"),
            Self::CellCountMismatch { expected, actual } => {
                write!(f, "dense section has {actual} cells, expected {expected}")
            }
        }
    }
}

impl std::error::Error for SectionError {}

impl From<SpatialError> for SectionError {
    fn from(error: SpatialError) -> Self {
        match error {
            SpatialError::ZeroSectionSide => Self::SideOutOfRange {
                side: 0,
                max: MAX_SECTION_SIDE,
            },
            SpatialError::LocalCoordinateOutOfRange { side } => Self::LocalOutOfRange {
                side,
                local: [u32::MAX, u32::MAX, u32::MAX],
            },
            SpatialError::CoordinateOverflow => Self::LocalOutOfRange {
                side: 0,
                local: [u32::MAX, u32::MAX, u32::MAX],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(x: u32, y: u32, z: u32) -> LocalPos {
        LocalPos { x, y, z }
    }

    #[test]
    fn uniform_section_allocates_nothing_until_first_edit() {
        let mut section = Section::empty(32).expect("valid side");
        assert_eq!(section.allocated_cells(), 0);
        assert_eq!(section.get(local(1, 2, 3)), Ok(BlockStateId::EMPTY));
        assert!(
            !section
                .set(local(1, 2, 3), BlockStateId::EMPTY)
                .expect("in range")
        );
        assert_eq!(section.allocated_cells(), 0);
        assert_eq!(section.revision(), Revision::INITIAL);
        assert!(
            section
                .set(local(1, 2, 3), BlockStateId(7))
                .expect("in range")
        );
        assert_eq!(section.allocated_cells(), 32usize.pow(3));
        assert_eq!(section.get(local(1, 2, 3)), Ok(BlockStateId(7)));
        assert_eq!(section.revision(), Revision(1));
    }

    #[test]
    fn repeated_write_of_same_value_keeps_revision() {
        let mut section = Section::new(8, BlockStateId(3)).expect("valid side");
        assert!(
            !section
                .set(local(7, 7, 7), BlockStateId(3))
                .expect("in range")
        );
        assert_eq!(section.revision(), Revision::INITIAL);
    }

    #[test]
    fn out_of_range_local_coordinate_is_rejected() {
        let section = Section::empty(16).expect("valid side");
        assert!(matches!(
            section.get(local(16, 0, 0)),
            Err(SectionError::LocalOutOfRange { .. })
        ));
        assert!(matches!(
            Section::empty(0),
            Err(SectionError::SideOutOfRange { .. })
        ));
        assert!(matches!(
            Section::empty(MAX_SECTION_SIDE + 1),
            Err(SectionError::SideOutOfRange { .. })
        ));
    }

    #[test]
    fn linear_index_matches_documented_order() {
        // x varies fastest, then z, then y.
        let side = 4;
        assert_eq!(linear_index(side, local(1, 0, 0)), 1);
        assert_eq!(linear_index(side, local(0, 0, 1)), 4);
        assert_eq!(linear_index(side, local(0, 1, 0)), 16);
        assert_eq!(linear_index(side, local(3, 3, 3)), 63);
    }

    #[test]
    fn traversal_covers_each_cell_once() {
        let side = 5;
        let mut section = Section::empty(side).expect("valid side");
        section
            .set(local(4, 3, 2), BlockStateId(9))
            .expect("in range");
        let mut seen = 0usize;
        let mut hits = 0usize;
        section.for_each(|_, value| {
            seen += 1;
            if value == BlockStateId(9) {
                hits += 1;
            }
        });
        assert_eq!(seen, (side as usize).pow(3));
        assert_eq!(hits, 1);
    }

    #[test]
    fn dense_roundtrip_preserves_every_cell() {
        let side = 16;
        let mut section = Section::empty(side).expect("valid side");
        for y in 0..side {
            for z in 0..side {
                for x in 0..side {
                    let value = BlockStateId(((x + y * 3 + z * 7) % 5) as u16);
                    section.set(local(x, y, z), value).expect("in range");
                }
            }
        }
        for y in 0..side {
            for z in 0..side {
                for x in 0..side {
                    let value = BlockStateId(((x + y * 3 + z * 7) % 5) as u16);
                    assert_eq!(section.get(local(x, y, z)), Ok(value));
                }
            }
        }
    }
}
