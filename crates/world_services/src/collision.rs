//! Axis-aligned collision against authoritative block shapes.
//!
//! Collision runs on the authority side and, in the same form, on the client for
//! prediction. It therefore must not depend on rendering or on unloaded data
//! being treated as air: an unloaded cell blocks movement until the authority
//! says otherwise, which is what prevents a player from falling through a world
//! that has not loaded yet.
//!
//! Movement is resolved one axis at a time with fixed substeps. Splitting a large
//! delta into steps is not a physical model; it keeps a fast-moving collider from
//! tunnelling through a one-block wall while the prototype has no continuous
//! sweep yet. The step size and the contact epsilon are part of the contract
//! because tests and future prediction code must agree on them.

use torse_types::BlockPos;

/// Distance kept between a collider and a surface after a blocked move.
pub const CONTACT_EPSILON: f64 = 1e-6;

/// Largest translation considered in one collision substep.
pub const MAX_SUBSTEP: f64 = 0.25;

/// Axis-aligned box in world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    /// Lower corner.
    pub min: [f64; 3],
    /// Upper corner.
    pub max: [f64; 3],
}

impl Aabb {
    /// Build a box from two corners, normalizing their order.
    pub fn new(a: [f64; 3], b: [f64; 3]) -> Self {
        let mut min = [0.0; 3];
        let mut max = [0.0; 3];
        for axis in 0..3 {
            min[axis] = a[axis].min(b[axis]);
            max[axis] = a[axis].max(b[axis]);
        }
        Self { min, max }
    }

    /// Build a box from a center and half extents.
    pub fn from_center(center: [f64; 3], half: [f64; 3]) -> Self {
        Self {
            min: [
                center[0] - half[0],
                center[1] - half[1],
                center[2] - half[2],
            ],
            max: [
                center[0] + half[0],
                center[1] + half[1],
                center[2] + half[2],
            ],
        }
    }

    /// Translate the box.
    pub fn translated(self, delta: [f64; 3]) -> Self {
        Self {
            min: [
                self.min[0] + delta[0],
                self.min[1] + delta[1],
                self.min[2] + delta[2],
            ],
            max: [
                self.max[0] + delta[0],
                self.max[1] + delta[1],
                self.max[2] + delta[2],
            ],
        }
    }

    /// Center of the box.
    pub fn center(self) -> [f64; 3] {
        [
            (self.min[0] + self.max[0]) / 2.0,
            (self.min[1] + self.max[1]) / 2.0,
            (self.min[2] + self.max[2]) / 2.0,
        ]
    }

    /// Return whether two boxes overlap by more than [`CONTACT_EPSILON`].
    ///
    /// Touching faces do not count as an overlap, which lets a collider rest on
    /// a surface without being pushed back out every step.
    pub fn overlaps(&self, other: &Aabb) -> bool {
        for axis in 0..3 {
            if self.min[axis] >= other.max[axis] - CONTACT_EPSILON
                || self.max[axis] <= other.min[axis] + CONTACT_EPSILON
            {
                return false;
            }
        }
        true
    }
}

/// One solid box expressed in the local unit cell of a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalBox {
    /// Lower corner in cell-local coordinates, each in `0.0..=1.0`.
    pub min: [f64; 3],
    /// Upper corner in cell-local coordinates, each in `0.0..=1.0`.
    pub max: [f64; 3],
}

impl LocalBox {
    /// A box covering the whole cell.
    pub const FULL_CUBE: Self = Self {
        min: [0.0, 0.0, 0.0],
        max: [1.0, 1.0, 1.0],
    };

    /// Place this local box at a block coordinate.
    pub fn at(&self, pos: BlockPos) -> Aabb {
        Aabb {
            min: [
                pos.x as f64 + self.min[0],
                pos.y as f64 + self.min[1],
                pos.z as f64 + self.min[2],
            ],
            max: [
                pos.x as f64 + self.max[0],
                pos.y as f64 + self.max[1],
                pos.z as f64 + self.max[2],
            ],
        }
    }
}

/// Collision content of one block cell.
#[derive(Clone, Debug, PartialEq)]
pub enum CollisionCell {
    /// The cell contributes no collision surface.
    Empty,
    /// The cell contributes one or more solid boxes.
    Solid(Vec<LocalBox>),
    /// The authority has no data for the cell, so movement is blocked.
    NotLoaded,
    /// The cell is outside the supported world extent, so movement is blocked.
    OutOfBounds,
}

/// Read-only access to block collision data.
pub trait CollisionSource {
    /// Extra owner-cell search distance on the negative and positive side.
    /// Each component is bounded to one cell; default preserves unit-cell queries.
    fn shape_reach(&self) -> ([u8; 3], [u8; 3]) {
        ([0; 3], [0; 3])
    }

    /// Collision content of one block cell.
    fn collision(&self, pos: BlockPos) -> CollisionCell;
}

#[cfg(test)]
mod reach_tests {
    use super::*;
    struct Tall;
    impl CollisionSource for Tall {
        fn shape_reach(&self) -> ([u8; 3], [u8; 3]) {
            ([0, 1, 0], [0; 3])
        }
        fn collision(&self, pos: BlockPos) -> CollisionCell {
            if pos == BlockPos::new(0, 0, 0) {
                CollisionCell::Solid(vec![LocalBox {
                    min: [0.25, 0.0, 0.25],
                    max: [0.75, 1.5, 0.75],
                }])
            } else {
                CollisionCell::Empty
            }
        }
    }
    #[test]
    fn overhanging_owner_is_found_from_the_adjacent_cell() {
        assert!(collides(
            &Tall,
            &Aabb::from_center([0.5, 1.25, 0.5], [0.1; 3])
        ));
        assert!(!collides(
            &Tall,
            &Aabb::from_center([0.5, 1.75, 0.5], [0.1; 3])
        ));
        let moved = sweep(
            &Tall,
            Aabb::from_center([1.2, 1.25, 0.5], [0.1; 3]),
            [-1.0, 0.0, 0.0],
        );
        assert!(moved.blocked[0]);
        assert!(moved.aabb.min[0] >= 0.75 - 2.0 * CONTACT_EPSILON);
    }
}

/// Result of a swept move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SweepResult {
    /// Final collider position after running the move.
    pub aabb: Aabb,
    /// Axis mask of the directions that were blocked during the move.
    pub blocked: [bool; 3],
}

impl SweepResult {
    /// Return whether any axis was blocked.
    pub fn is_blocked(&self) -> bool {
        self.blocked.iter().any(|value| *value)
    }
}

/// Return whether the collider overlaps any solid cell.
///
/// Unloaded and out-of-bounds cells count as solid. A collider that starts
/// inside such a cell is reported as colliding, which is the correct answer for
/// movement even though the server will also refuse to edit the region.
pub fn collides(source: &impl CollisionSource, collider: &Aabb) -> bool {
    let (negative, positive) = source.shape_reach();
    let lower = |axis: usize| {
        (collider.min[axis].floor() as i64).saturating_sub(i64::from(negative[axis].min(1)))
    };
    let upper = |axis: usize| {
        ((collider.max[axis] - CONTACT_EPSILON).floor() as i64)
            .saturating_add(i64::from(positive[axis].min(1)))
    };
    let min_x = lower(0);
    let min_y = lower(1);
    let min_z = lower(2);
    let max_x = upper(0);
    let max_y = upper(1);
    let max_z = upper(2);

    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                match source.collision(BlockPos::new(x, y, z)) {
                    CollisionCell::Empty => {}
                    CollisionCell::Solid(boxes) => {
                        let pos = BlockPos::new(x, y, z);
                        if boxes.iter().any(|local| local.at(pos).overlaps(collider)) {
                            return true;
                        }
                    }
                    CollisionCell::NotLoaded | CollisionCell::OutOfBounds => return true,
                }
            }
        }
    }
    false
}

/// Move a collider, stopping at the first blocking surface per axis.
///
/// Axes are resolved in x, z, y order so gravity is applied last. That order
/// keeps a walking collider from being pushed sideways after it lands.
///
/// A blocked axis is refined by bisection between the last safe position and the
/// rejected step, so the collider comes to rest against the surface instead of up
/// to [`MAX_SUBSTEP`] away from it. Refinement costs a fixed number of overlap
/// tests and only runs on the blocked axis.
pub fn sweep(source: &impl CollisionSource, collider: Aabb, delta: [f64; 3]) -> SweepResult {
    sweep_steps(source, collider, delta, MAX_SUBSTEP)
}

/// Adapt the step to a thin body, while bounding the complete axis work.
pub fn sweep_bounded(
    source: &impl CollisionSource,
    collider: Aabb,
    delta: [f64; 3],
) -> Option<SweepResult> {
    let step = (0..3)
        .map(|axis| (collider.max[axis] - collider.min[axis]) * 0.25)
        .fold(MAX_SUBSTEP, f64::min);
    if !step.is_finite()
        || step <= 0.0
        || delta.iter().any(|d| !d.is_finite())
        || delta.iter().map(|d| (d.abs() / step).ceil()).sum::<f64>() > 512.0
    {
        return None;
    }
    Some(sweep_steps(source, collider, delta, step))
}

fn sweep_steps(
    source: &impl CollisionSource,
    collider: Aabb,
    delta: [f64; 3],
    limit: f64,
) -> SweepResult {
    let mut current = collider;
    let mut blocked = [false; 3];
    for axis in [0usize, 2, 1] {
        let mut remaining = delta[axis];
        while remaining.abs() > f64::EPSILON {
            let step = remaining.clamp(-limit, limit);
            let mut candidate = current;
            candidate.min[axis] += step;
            candidate.max[axis] += step;
            if collides(source, &candidate) {
                blocked[axis] = true;
                let safe = refine(source, current, axis, step);
                current.min[axis] += safe;
                current.max[axis] += safe;
                break;
            }
            current = candidate;
            remaining -= step;
        }
    }
    SweepResult {
        aabb: current,
        blocked,
    }
}

/// Largest safe offset along one axis between a safe position and a blocked step.
fn refine(source: &impl CollisionSource, from: Aabb, axis: usize, step: f64) -> f64 {
    const ITERATIONS: usize = 24;
    let mut safe = 0.0f64;
    let mut blocked = step;
    for _ in 0..ITERATIONS {
        let mid = (safe + blocked) / 2.0;
        let mut probe = from;
        probe.min[axis] += mid;
        probe.max[axis] += mid;
        if collides(source, &probe) {
            blocked = mid;
        } else {
            safe = mid;
        }
    }
    safe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_sweep_stops_a_small_body_at_a_thin_surface() {
        struct Thin;
        impl CollisionSource for Thin {
            fn collision(&self, pos: BlockPos) -> CollisionCell {
                if pos == BlockPos::new(1, 0, 0) {
                    CollisionCell::Solid(vec![LocalBox {
                        min: [0.11, 0.0, 0.0],
                        max: [0.12, 1.0, 1.0],
                    }])
                } else {
                    CollisionCell::Empty
                }
            }
        }
        let start = Aabb::from_center([0.95, 0.5, 0.5], [0.01; 3]);
        let result = sweep_bounded(&Thin, start, [0.5, 0.0, 0.0]).unwrap();
        assert!(result.blocked[0]);
        assert!((result.aabb.max[0] - 1.11).abs() < 1e-5);
        assert!(!collides(&Thin, &result.aabb));
        assert!(
            sweep_bounded(
                &Thin,
                Aabb::from_center([0.5; 3], [0.000_001; 3]),
                [1.0, 0.0, 0.0]
            )
            .is_none()
        );
    }

    struct Grid {
        occupied: Vec<(i64, i64, i64)>,
        unloaded: bool,
    }

    impl Grid {
        fn with_floor(y: i64) -> Self {
            let mut occupied = Vec::new();
            for x in -8..8 {
                for z in -8..8 {
                    occupied.push((x, y, z));
                }
            }
            Self {
                occupied,
                unloaded: false,
            }
        }
    }

    impl CollisionSource for Grid {
        fn collision(&self, pos: BlockPos) -> CollisionCell {
            if self.unloaded {
                return CollisionCell::NotLoaded;
            }
            if self.occupied.contains(&(pos.x, pos.y, pos.z)) {
                CollisionCell::Solid(vec![LocalBox::FULL_CUBE])
            } else {
                CollisionCell::Empty
            }
        }
    }

    fn player(x: f64, y: f64, z: f64) -> Aabb {
        Aabb::from_center([x, y, z], [0.3, 0.9, 0.3])
    }

    #[test]
    fn falling_lands_on_the_floor() {
        let grid = Grid::with_floor(0);
        let start = player(0.5, 3.0, 0.5);
        let result = sweep(&grid, start, [0.0, -5.0, 0.0]);
        assert!(result.blocked[1]);
        assert!(
            (result.aabb.min[1] - 1.0).abs() < 1e-3,
            "rested at {}",
            result.aabb.min[1]
        );
    }

    #[test]
    fn fast_move_cannot_tunnel_through_a_one_block_wall() {
        let mut grid = Grid::with_floor(0);
        grid.occupied.extend((0..4).map(|y| (2, y, 0)));
        let start = player(0.5, 1.0, 0.5);
        let result = sweep(&grid, start, [4.0, 0.0, 0.0]);
        assert!(result.blocked[0]);
        assert!(result.aabb.max[0] <= 2.0, "stopped before the wall");
    }

    #[test]
    fn unloaded_cells_block_movement() {
        let mut grid = Grid::with_floor(0);
        grid.unloaded = true;
        let start = player(0.5, 3.0, 0.5);
        let result = sweep(&grid, start, [1.0, -1.0, 0.0]);
        assert!(result.is_blocked());
        assert_eq!(result.aabb, start);
    }

    #[test]
    fn touching_surface_is_not_an_overlap() {
        let floor = LocalBox::FULL_CUBE.at(BlockPos::new(0, 0, 0));
        let resting = Aabb::new([0.2, 1.0, 0.2], [0.8, 1.9, 0.8]);
        assert!(!resting.overlaps(&floor));
        let sunk = Aabb::new([0.2, 0.99, 0.2], [0.8, 1.9, 0.8]);
        assert!(sunk.overlaps(&floor));
    }

    #[test]
    fn partial_shapes_only_block_their_volume() {
        // A lower slab must not block a collider passing above its top face.
        struct Slab;

        impl CollisionSource for Slab {
            fn collision(&self, pos: BlockPos) -> CollisionCell {
                if pos.y == 0 {
                    CollisionCell::Solid(vec![LocalBox {
                        min: [0.0, 0.0, 0.0],
                        max: [1.0, 0.5, 1.0],
                    }])
                } else {
                    CollisionCell::Empty
                }
            }
        }

        let high = player(0.5, 1.4, 0.5);
        let result = sweep(&Slab, high, [3.0, 0.0, 0.0]);
        assert!(!result.is_blocked(), "the collider passes above the slab");
        let low = player(0.5, 1.0, 0.5);
        let result = sweep(&Slab, low, [3.0, 0.0, 0.0]);
        assert!(result.is_blocked(), "the collider hits the slab side");
    }
}
