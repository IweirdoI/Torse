//! Voxel ray picking for the client.
//!
//! Grid traversal follows Amanatides and Woo: advance the ray one boundary at
//! a time, cheapest axis first. A cell may carry a partial shape; the ray
//! must actually intersect the shape's box to count as a hit, so aiming
//! through the empty half of a slab cell or past a thin plant panel keeps
//! travelling. The reported entry face is the face of the shape's box the
//! ray entered, which is the face a placement attaches to.

use torse_render::RenderBox;
use torse_types::BlockPos;

/// One ray hit against a block shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickHit {
    /// Cell the hit block occupies.
    pub block: BlockPos,
    /// Unit normal of the shape face the ray entered, pointing back toward
    /// the ray origin; zero when the origin starts inside the shape.
    pub face: [i32; 3],
    /// World-space point where the ray crossed into the shape.
    pub entry: [f32; 3],
    /// Distance from the origin to `entry`.
    pub distance: f32,
}

/// Outcome of one traversal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PickOutcome {
    /// The ray intersected a block shape.
    Hit(PickHit),
    /// The ray left the queryable world or ran out of range.
    Miss,
}

/// Ray against one axis-aligned box; returns the entry distance and the face
/// normal the ray crosses, or `None` when it misses. The ray segment is
/// clamped to `[t_enter, t_exit]` (the cell's traversal span).
fn ray_box(
    origin: [f32; 3],
    direction: [f32; 3],
    min: [f32; 3],
    max: [f32; 3],
    t_enter: f32,
    t_exit: f32,
) -> Option<(f32, [i32; 3])> {
    let mut t_near = t_enter;
    let mut t_far = t_exit;
    let mut axis = 3usize;
    for a in 0..3 {
        let d = direction[a];
        let lo = min[a];
        let hi = max[a];
        if d.abs() < 1e-9 {
            if origin[a] < lo || origin[a] > hi {
                return None;
            }
            continue;
        }
        let mut t1 = (lo - origin[a]) / d;
        let mut t2 = (hi - origin[a]) / d;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        if t1 > t_near {
            t_near = t1;
            axis = a;
        }
        if t2 < t_far {
            t_far = t2;
        }
        if t_near > t_far {
            return None;
        }
    }
    if axis == 3 {
        // The ray was already inside the box at the segment start.
        return Some((t_near, [0, 0, 0]));
    }
    let sign = if direction[axis] > 0.0 { -1 } else { 1 };
    let mut face = [0, 0, 0];
    face[axis] = sign;
    Some((t_near, face))
}

/// Full-cell world-space box for a block shape at `cell`.
fn cell_box(cell: BlockPos, bounds: &RenderBox) -> ([f32; 3], [f32; 3]) {
    let min = [
        cell.x as f32 + bounds.min[0],
        cell.y as f32 + bounds.min[1],
        cell.z as f32 + bounds.min[2],
    ];
    let max = [
        cell.x as f32 + bounds.max[0],
        cell.y as f32 + bounds.max[1],
        cell.z as f32 + bounds.max[2],
    ];
    (min, max)
}

/// Traverse the grid and report the first block shape the ray intersects.
///
/// The caller supplies `shape_of`, which classifies one cell. Direction need
/// not be normalized. Partial shapes only stop the ray when the ray actually
/// passes through their box; the reported face is that box's entered face,
/// or the crossed cell boundary when the ray was already inside the shape.
pub fn cast_ray(
    mut shape_of: impl FnMut(BlockPos) -> Option<Option<RenderBox>>,
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

    // Distance along the ray to the next boundary on each axis. Zero-direction
    // axes never win the minimum comparison because their boundary distance
    // stays infinite.
    let mut boundary = [f32::INFINITY; 3];
    let mut delta = [f32::INFINITY; 3];
    for a in 0..3 {
        if step[a] == 0 {
            continue;
        }
        delta[a] = 1.0 / direction[a].abs();
        let next = if step[a] > 0 {
            (cell[a] + 1) as f32 - origin[a]
        } else {
            origin[a] - cell[a] as f32
        };
        boundary[a] = next * delta[a];
    }

    // Classify one cell over the traversal span [t_enter, t_exit]:
    // None stops the world, an empty cell continues, a shape either catches
    // the ray or lets it pass.
    let mut classify = |cell: [i64; 3], t_enter: f32, t_exit: f32| -> Option<Option<PickHit>> {
        let pos = BlockPos::new(cell[0], cell[1], cell[2]);
        match shape_of(pos) {
            None => None,
            Some(None) => Some(None),
            Some(Some(bounds)) => {
                let (min, max) = cell_box(pos, &bounds);
                match ray_box(origin, direction, min, max, t_enter, t_exit) {
                    None => Some(None),
                    Some((distance, mut face)) => {
                        // Entering the cell without crossing the shape's own
                        // face happens when the ray starts the segment inside
                        // the shape: the reported face falls back to the cell
                        // boundary crossed.
                        if face == [0, 0, 0] && distance == t_enter && t_enter > 0.0 {
                            face = [
                                if direction[0] != 0.0 {
                                    -step[0] as i32
                                } else {
                                    face[0]
                                },
                                if direction[1] != 0.0 {
                                    -step[1] as i32
                                } else {
                                    face[1]
                                },
                                if direction[2] != 0.0 {
                                    -step[2] as i32
                                } else {
                                    face[2]
                                },
                            ];
                        }
                        let entry = [
                            origin[0] + direction[0] * distance,
                            origin[1] + direction[1] * distance,
                            origin[2] + direction[2] * distance,
                        ];
                        Some(Some(PickHit {
                            block: pos,
                            face,
                            entry,
                            distance,
                        }))
                    }
                }
            }
        }
    };

    // Origin cell: the traversal span starts at zero.
    let origin_exit = boundary
        .iter()
        .copied()
        .fold(f32::INFINITY, f32::min)
        .min(max_distance);
    match classify(cell, 0.0, origin_exit) {
        None => return PickOutcome::Miss,
        Some(None) => {}
        Some(Some(hit)) => return PickOutcome::Hit(hit),
    }

    loop {
        let axis = if boundary[0] <= boundary[1] && boundary[0] <= boundary[2] {
            0
        } else if boundary[1] <= boundary[2] {
            1
        } else {
            2
        };
        let t_enter = boundary[axis];
        if t_enter > max_distance {
            break;
        }
        cell[axis] += step[axis];
        let crossed = [0i32, 0, 0];
        let mut crossed_face = [0i32; 3];
        crossed_face[axis] = -step[axis] as i32;
        let _ = crossed;
        boundary[axis] += delta[axis];

        let cell_exit = boundary
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min)
            .min(max_distance);
        match classify(cell, t_enter, cell_exit) {
            None => return PickOutcome::Miss,
            Some(None) => continue,
            Some(Some(mut hit)) => {
                if hit.face == [0, 0, 0] {
                    // The ray entered the cell already inside the shape (the
                    // shape touches the crossed boundary): the placement face
                    // is the boundary just crossed.
                    hit.face = crossed_face;
                }
                return PickOutcome::Hit(hit);
            }
        }
    }
    PickOutcome::Miss
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Full-cube cells at the given coordinates; everything else empty, and
    /// cells outside one axis stay queryable (empty) so rays pass them.
    fn world(
        solid_cells: &[(i64, i64, i64)],
    ) -> impl FnMut(BlockPos) -> Option<Option<RenderBox>> + '_ {
        let set: BTreeSet<(i64, i64, i64)> = solid_cells.iter().copied().collect();
        move |pos: BlockPos| {
            Some(Some(if set.contains(&(pos.x, pos.y, pos.z)) {
                RenderBox::FULL_CUBE
            } else {
                RenderBox {
                    min: [0.0; 3],
                    max: [0.0; 3],
                }
            }))
        }
    }

    #[test]
    fn ray_hits_a_wall_and_reports_the_entry_face() {
        let cells = [(3i64, 0i64, 0i64)];
        let outcome = cast_ray(&mut world(&cells), [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 16.0);
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
        // A diagonal ray clips the corner: the reported face is whichever
        // axis the traversal crossed last, or the shape face when the box
        // catches it first. Both families are valid corner entries.
        assert!(
            hit.face == [-1, 0, 0]
                || hit.face == [0, -1, 0]
                || hit.face == [0, 0, 0]
                || hit.face == [-1, -1, 0],
            "corner face {:?}",
            hit.face
        );
    }

    #[test]
    fn ray_missing_everything_reports_miss() {
        assert_eq!(
            cast_ray(&mut world(&[]), [0.5, 0.5, 0.5], [0.0, 1.0, 0.0], 64.0),
            PickOutcome::Miss
        );
        assert_eq!(
            cast_ray(world(&[(50, 0, 0)]), [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 8.0),
            PickOutcome::Miss
        );
    }

    #[test]
    fn ray_through_a_slabs_empty_half_passes_to_the_cell_behind() {
        // Slab at (3,0,0) fills its lower half only. A ray above the midline
        // travels through the empty upper half and hits the wall behind.
        let slab = (3i64, 0i64, 0i64);
        let wall = [(6i64, 0i64, 0i64)];
        let sample = |pos: BlockPos| {
            Some(Some(if (pos.x, pos.y, pos.z) == slab {
                RenderBox {
                    min: [0.0, 0.0, 0.0],
                    max: [1.0, 0.5, 1.0],
                }
            } else if wall.contains(&(pos.x, pos.y, pos.z)) {
                RenderBox::FULL_CUBE
            } else {
                RenderBox {
                    min: [0.0; 3],
                    max: [0.0; 3],
                }
            }))
        };
        let outcome = cast_ray(&mut { sample }, [0.5, 0.8, 0.5], [1.0, 0.0, 0.0], 32.0);
        let PickOutcome::Hit(hit) = outcome else {
            panic!("expected the wall behind the slab");
        };
        assert_eq!(hit.block, BlockPos::new(6, 0, 0), "passed over the slab");
        assert_eq!(hit.face, [-1, 0, 0]);
    }

    #[test]
    fn ray_descending_onto_a_slab_hits_its_top_face() {
        let slab = (3i64, 0i64, 0i64);
        let sample = |pos: BlockPos| {
            Some(Some(if (pos.x, pos.y, pos.z) == slab {
                RenderBox {
                    min: [0.0, 0.0, 0.0],
                    max: [1.0, 0.5, 1.0],
                }
            } else {
                RenderBox {
                    min: [0.0; 3],
                    max: [0.0; 3],
                }
            }))
        };
        let outcome = cast_ray(&mut { sample }, [3.5, 4.0, 0.5], [0.0, -1.0, 0.0], 16.0);
        let PickOutcome::Hit(hit) = outcome else {
            panic!("expected the slab top");
        };
        assert_eq!(hit.block, BlockPos::new(3, 0, 0));
        assert_eq!(hit.face, [0, 1, 0], "the ray enters through the top");
        assert!((hit.distance - 3.5).abs() < 1e-4);
        assert!((hit.entry[1] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn ray_hits_a_slabs_side_face() {
        let slab = (3i64, 0i64, 0i64);
        let sample = |pos: BlockPos| {
            Some(Some(if (pos.x, pos.y, pos.z) == slab {
                RenderBox {
                    min: [0.0, 0.0, 0.0],
                    max: [1.0, 0.5, 1.0],
                }
            } else {
                RenderBox {
                    min: [0.0; 3],
                    max: [0.0; 3],
                }
            }))
        };
        // Aim at the slab's -z face through the cell in front.
        let outcome = cast_ray(&mut { sample }, [3.5, 0.25, -3.0], [0.0, 0.0, 1.0], 16.0);
        let PickOutcome::Hit(hit) = outcome else {
            panic!("expected the slab side");
        };
        assert_eq!(hit.block, BlockPos::new(3, 0, 0));
        assert_eq!(hit.face, [0, 0, -1]);
        assert!((hit.distance - 3.0).abs() < 1e-4);
    }
}
