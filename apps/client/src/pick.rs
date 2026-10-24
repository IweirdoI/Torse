//! Voxel ray picking for the client.
//!
//! Grid traversal follows Amanatides and Woo: advance the ray one boundary at
//! a time, cheapest axis first, until a solid cell or the maximum distance is
//! reached. The returned entry face is the crossed boundary's opposite normal,
//! which is the face a placement attaches to.

use torse_types::BlockPos;

/// One ray hit against the terrain occupancy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickHit {
    /// Cell the ray entered.
    pub block: BlockPos,
    /// Unit normal of the entered face, pointing back toward the ray origin.
    pub face: [i32; 3],
    /// World-space point where the ray crossed that face.
    pub entry: [f32; 3],
    /// Distance from the origin to `entry`.
    pub distance: f32,
}

/// Outcome of one traversal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PickOutcome {
    /// The ray entered a solid cell.
    Hit(PickHit),
    /// The ray left the queryable world or ran out of range.
    Miss,
}

/// Traverse the grid and report the first solid cell.
///
/// `solid` classifies one cell: `true` stops the ray, `false` continues, and
/// `None` terminates the traversal without a hit (unloaded or out of bounds).
/// Direction need not be normalized.
pub fn cast_ray(
    mut solid: impl FnMut(BlockPos) -> Option<bool>,
    origin: [f32; 3],
    direction: [f32; 3],
    max_distance: f32,
) -> PickOutcome {
    let length =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt();
    if length == 0.0 || max_distance <= 0.0 {
        return PickOutcome::Miss;
    }
    let direction = [
        direction[0] / length,
        direction[1] / length,
        direction[2] / length,
    ];

    let mut cell = [
        origin[0].floor() as i64,
        origin[1].floor() as i64,
        origin[2].floor() as i64,
    ];
    let step: [i64; 3] = [
        if direction[0] > 0.0 {
            1
        } else if direction[0] < 0.0 {
            -1
        } else {
            0
        },
        if direction[1] > 0.0 {
            1
        } else if direction[1] < 0.0 {
            -1
        } else {
            0
        },
        if direction[2] > 0.0 {
            1
        } else if direction[2] < 0.0 {
            -1
        } else {
            0
        },
    ];

    // Distance along the ray to the next boundary on each axis, and the ray
    // length needed to cross one full cell. Zero-direction axes never win the
    // minimum comparison because their boundary distance stays infinite.
    let mut boundary = [0.0f32; 3];
    let mut delta = [f32::INFINITY; 3];
    for axis in 0..3 {
        if step[axis] == 0 {
            continue;
        }
        delta[axis] = 1.0 / direction[axis].abs();
        let next = if step[axis] > 0 {
            (cell[axis] + 1) as f32 - origin[axis]
        } else {
            origin[axis] - cell[axis] as f32
        };
        boundary[axis] = next * delta[axis];
    }

    let mut classify = |cell: [i64; 3]| -> Option<bool> {
        // World bounds are the caller's concern; the client only queries its
        // loaded sections, so an out-of-bounds cell is reported as unloaded.
        solid(BlockPos::new(cell[0], cell[1], cell[2]))
    };

    match classify(cell) {
        Some(false) => {}
        Some(true) => {
            // The origin cell itself is solid; report entry at the origin with
            // no crossed face. Placement on a zero face is rejected upstream.
            return PickOutcome::Hit(PickHit {
                block: BlockPos::new(cell[0], cell[1], cell[2]),
                face: [0, 0, 0],
                entry: origin,
                distance: 0.0,
            });
        }
        None => return PickOutcome::Miss,
    }

    let mut distance = 0.0f32;
    while distance <= max_distance {
        let axis = if boundary[0] <= boundary[1] && boundary[0] <= boundary[2] {
            0
        } else if boundary[1] <= boundary[2] {
            1
        } else {
            2
        };
        distance = boundary[axis];
        if distance > max_distance {
            break;
        }
        cell[axis] += step[axis];
        let mut face = [0i32; 3];
        face[axis] = -(step[axis] as i32);
        boundary[axis] += delta[axis];

        match classify(cell) {
            Some(true) => {
                let mut entry = [0.0f32; 3];
                for item in 0..3 {
                    entry[item] = origin[item] + direction[item] * distance;
                }
                return PickOutcome::Hit(PickHit {
                    block: BlockPos::new(cell[0], cell[1], cell[2]),
                    face,
                    entry,
                    distance,
                });
            }
            Some(false) => continue,
            None => return PickOutcome::Miss,
        }
    }
    PickOutcome::Miss
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn world(solid_cells: &[(i64, i64, i64)]) -> impl Fn(BlockPos) -> Option<bool> + '_ {
        let set: BTreeSet<(i64, i64, i64)> = solid_cells.iter().copied().collect();
        move |pos: BlockPos| Some(set.contains(&(pos.x, pos.y, pos.z)))
    }

    #[test]
    fn ray_hits_a_wall_and_reports_the_entry_face() {
        let cells = [(3i64, 0i64, 0i64)];
        let outcome = cast_ray(world(&cells), [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 16.0);
        let PickOutcome::Hit(hit) = outcome else {
            panic!("expected a hit");
        };
        assert_eq!(hit.block, BlockPos::new(3, 0, 0));
        assert_eq!(hit.face, [-1, 0, 0]);
        assert!((hit.distance - 2.5).abs() < 1e-4);
        assert!((hit.entry[0] - 3.0).abs() < 1e-4);
    }

    #[test]
    fn ray_passes_diagonally_and_picks_the_axis_it_crossed_last() {
        let cells = [(2i64, 2i64, 0i64)];
        let outcome = cast_ray(world(&cells), [0.6, 0.6, 0.5], [1.0, 1.0, 0.0], 16.0);
        let PickOutcome::Hit(hit) = outcome else {
            panic!("expected a hit");
        };
        assert_eq!(hit.block, BlockPos::new(2, 2, 0));
        // One of the two crossed faces is reported; both are valid entries.
        assert!(hit.face == [-1, 0, 0] || hit.face == [0, -1, 0]);
    }

    #[test]
    fn ray_missing_everything_reports_miss() {
        assert_eq!(
            cast_ray(world(&[]), [0.5, 0.5, 0.5], [0.0, 1.0, 0.0], 64.0),
            PickOutcome::Miss
        );
        assert_eq!(
            cast_ray(world(&[(50, 0, 0)]), [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 8.0),
            PickOutcome::Miss
        );
    }

    #[test]
    fn unloaded_cell_terminates_the_traversal() {
        let solid = |pos: BlockPos| {
            if pos.x < 0 { None } else { Some(false) }
        };
        // A wall behind unloaded space is not reachable through it.
        assert_eq!(
            cast_ray(solid, [0.5, 0.5, 0.5], [-1.0, 0.0, 0.0], 64.0),
            PickOutcome::Miss
        );
    }

    #[test]
    fn origin_inside_a_solid_cell_hits_immediately() {
        let cells = [(0i64, 0i64, 0i64)];
        let outcome = cast_ray(world(&cells), [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 16.0);
        let PickOutcome::Hit(hit) = outcome else {
            panic!("expected a hit");
        };
        assert_eq!(hit.block, BlockPos::new(0, 0, 0));
        assert_eq!(hit.face, [0, 0, 0]);
        assert_eq!(hit.distance, 0.0);
    }

    #[test]
    fn zero_direction_or_range_is_a_miss() {
        assert_eq!(
            cast_ray(world(&[(0, 0, 0)]), [0.5, 0.5, 0.5], [0.0, 0.0, 0.0], 8.0),
            PickOutcome::Miss
        );
        assert_eq!(
            cast_ray(world(&[(0, 0, 1)]), [0.5, 0.5, 0.5], [0.0, 0.0, 1.0], 0.0),
            PickOutcome::Miss
        );
    }
}
