//! Signed world coordinates and section conversion rules.
//!
//! Torse uses Euclidean division for section conversion. This is the crucial
//! invariant for negative coordinates: block `-1` belongs to section `-1` with
//! local coordinate `side - 1`, not to section `0` with a negative local index.
//! The same rule must be used by storage, replication, collision, and hot-path
//! indexing so serialized coordinates never change interpretation.

use core::fmt;

/// Authoritative block coordinate using signed 64-bit components.
///
/// The range is deliberately wider than the world-generation range. A broad
/// representation keeps arithmetic and future dimensions flexible, but callers
/// must still reject unsupported values before allocation or generation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlockPos {
    /// Signed x coordinate in block-space units.
    pub x: i64,
    /// Signed y coordinate in block-space units.
    pub y: i64,
    /// Signed z coordinate in block-space units.
    pub z: i64,
}

impl BlockPos {
    /// Origin block coordinate.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// Construct a coordinate without performing any world-specific bounds check.
    pub const fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }

    /// Add a signed offset and reject overflow on any axis.
    ///
    /// This is preferred over raw arithmetic at trust boundaries because a
    /// wrapped coordinate can alias a completely unrelated location.
    pub fn checked_add(self, x: i64, y: i64, z: i64) -> Result<Self, SpatialError> {
        Ok(Self {
            x: self
                .x
                .checked_add(x)
                .ok_or(SpatialError::CoordinateOverflow)?,
            y: self
                .y
                .checked_add(y)
                .ok_or(SpatialError::CoordinateOverflow)?,
            z: self
                .z
                .checked_add(z)
                .ok_or(SpatialError::CoordinateOverflow)?,
        })
    }

    /// Convert to a section identity and non-negative local coordinate.
    ///
    /// Negative values use Euclidean division and Euclidean remainder. As a
    /// result, `local` is always in `0..side`, which lets section storage use
    /// unsigned indices without special cases for negative worlds.
    pub fn to_section(self, side: u32) -> Result<(SectionPos, LocalPos), SpatialError> {
        let side = checked_side(side)?;
        Ok((
            SectionPos {
                x: self.x.div_euclid(side),
                y: self.y.div_euclid(side),
                z: self.z.div_euclid(side),
            },
            LocalPos {
                x: self.x.rem_euclid(side) as u32,
                y: self.y.rem_euclid(side) as u32,
                z: self.z.rem_euclid(side) as u32,
            },
        ))
    }
}

/// Section coordinate. One section contains `side^3` cells.
///
/// `SectionPos` is a storage and scheduling index, not an authoritative block
/// position. The section side length is passed explicitly so internal defaults
/// remain replaceable without changing the public coordinate model.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SectionPos {
    /// Signed section index on the x axis.
    pub x: i64,
    /// Signed section index on the y axis.
    pub y: i64,
    /// Signed section index on the z axis.
    pub z: i64,
}

impl SectionPos {
    /// Section containing the block-space origin.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// Construct a section index directly.
    pub const fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }

    /// Convert a section-local coordinate back into an authoritative block coordinate.
    ///
    /// Both multiplication and addition are checked because a very large section
    /// index can overflow `i64` even when each input is individually valid.
    pub fn to_block(self, local: LocalPos, side: u32) -> Result<BlockPos, SpatialError> {
        let section_side = checked_side(side)?;
        if local.x >= side || local.y >= side || local.z >= side {
            return Err(SpatialError::LocalCoordinateOutOfRange { side });
        }

        let x = self
            .x
            .checked_mul(section_side)
            .and_then(|base| base.checked_add(i64::from(local.x)))
            .ok_or(SpatialError::CoordinateOverflow)?;
        let y = self
            .y
            .checked_mul(section_side)
            .and_then(|base| base.checked_add(i64::from(local.y)))
            .ok_or(SpatialError::CoordinateOverflow)?;
        let z = self
            .z
            .checked_mul(section_side)
            .and_then(|base| base.checked_add(i64::from(local.z)))
            .ok_or(SpatialError::CoordinateOverflow)?;
        Ok(BlockPos { x, y, z })
    }
}

/// Non-negative position inside a section.
///
/// Values are validated against the section side by [`SectionPos::to_block`].
/// Keeping this type unsigned makes negative local indices impossible to
/// represent, which removes an entire class of chunk-index bugs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct LocalPos {
    /// Non-negative local x index inside a section.
    pub x: u32,
    /// Non-negative local y index inside a section.
    pub y: u32,
    /// Non-negative local z index inside a section.
    pub z: u32,
}

/// Errors produced by arithmetic or section conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialError {
    /// A section side of zero would make division undefined.
    ZeroSectionSide,
    /// At least one coordinate operation exceeded the signed 64-bit range.
    CoordinateOverflow,
    /// A local coordinate was not less than the provided section side.
    LocalCoordinateOutOfRange { side: u32 },
}

impl fmt::Display for SpatialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSectionSide => write!(f, "section side must be greater than zero"),
            Self::CoordinateOverflow => write!(f, "coordinate arithmetic overflow"),
            Self::LocalCoordinateOutOfRange { side } => {
                write!(f, "local coordinate must be less than section side {side}")
            }
        }
    }
}

impl std::error::Error for SpatialError {}

fn checked_side(side: u32) -> Result<i64, SpatialError> {
    if side == 0 {
        Err(SpatialError::ZeroSectionSide)
    } else {
        Ok(i64::from(side))
    }
}
