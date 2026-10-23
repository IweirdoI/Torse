//! Signed world coordinates and section conversion rules.

use core::fmt;

/// Authoritative block coordinate using signed 64-bit components.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlockPos {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

impl BlockPos {
    pub const ZERO: Self = Self::new(0, 0, 0);

    pub const fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }

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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SectionPos {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

impl SectionPos {
    pub const ZERO: Self = Self::new(0, 0, 0);

    pub const fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }

    /// Convert a section-local coordinate back into an authoritative block coordinate.
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct LocalPos {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialError {
    ZeroSectionSide,
    CoordinateOverflow,
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
