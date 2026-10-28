//! Bounded occupancy rows and opaque, identically shaded coplanar runs.
use super::*;

pub(super) fn eligible(block: RenderBlock) -> bool {
    !block.cross
        && block.bounds == Some(RenderBox::FULL_CUBE)
        && block.occlusion == 0x3f
        && block
            .face_overlays
            .is_none_or(|faces| faces.iter().all(Option::is_none))
        && block
            .face_shades
            .is_none_or(|faces| faces.iter().all(|face| face.iter().all(|n| *n == face[0])))
}

pub(super) fn emit(
    mesh: &mut MeshData,
    occupancy: &RowOccupancy,
    side: u32,
    origin: [i64; 3],
    sample: &impl Fn(BlockPos) -> MeshSample,
    cancelled: &impl Fn() -> bool,
    limit: Option<usize>,
) -> Result<(), MeshBuildError> {
    let side = side as usize;
    let position = |cell: [usize; 3]| {
        BlockPos::new(
            origin[0] + cell[0] as i64,
            origin[1] + cell[1] as i64,
            origin[2] + cell[2] as i64,
        )
    };
    let occupied =
        |cell: [usize; 3]| occupancy.row(cell[1] as i64, cell[2] as i64) & (1u64 << cell[0]) != 0;
    for face in Face::ALL {
        let normal = face.axis();
        let (a, b) = match normal {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        for plane in 0..side {
            if cancelled() {
                return Err(MeshBuildError::Cancelled);
            }
            for row in 0..side {
                let visible = |u| {
                    let mut cell = [0; 3];
                    cell[normal] = plane;
                    cell[a] = u;
                    cell[b] = row;
                    if !occupied(cell) {
                        return None;
                    }
                    let mut neighbor = cell;
                    let inside = if face.is_negative() {
                        plane.checked_sub(1)
                    } else {
                        plane.checked_add(1).filter(|p| *p < side)
                    };
                    if let Some(n) = inside {
                        neighbor[normal] = n;
                        if occupied(neighbor) {
                            return None;
                        }
                    }
                    let MeshSample::Ready(block) = sample(position(cell)) else {
                        return None;
                    };
                    if !eligible(block) || face_culled(face, &block, sample, position(cell)) {
                        return None;
                    }
                    Some((cell, FaceAppearance::new(&block, face)))
                };
                let mut u = 0;
                while u < side {
                    let Some((cell, appearance)) = visible(u) else {
                        u += 1;
                        continue;
                    };
                    let start = u;
                    u += 1;
                    while u < side && visible(u).is_some_and(|(_, next)| next == appearance) {
                        u += 1;
                    }
                    if limit.is_some_and(|n| mesh.face_count() >= n) {
                        return Err(MeshBuildError::OutputLimit);
                    }
                    let mut bounds = RenderBox::FULL_CUBE;
                    bounds.max[a] = (u - start) as f32;
                    emit_face(mesh, face, bounds, cell.map(|v| v as f32), appearance);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::BTreeMap;

    #[test]
    fn varying_corner_brightness_prevents_merging_and_reaches_vertices() {
        let mut block = RenderBlock::uniform(1);
        block.face_shades = Some([[32, 64, 128, 255]; 6]);
        let sample = |p: BlockPos| {
            if p.y == 0 && p.z == 0 && (0..2).contains(&p.x) {
                MeshSample::Ready(block)
            } else {
                MeshSample::NotLoaded
            }
        };
        let reference = build_mesh(2, SectionPos::ZERO, sample);
        let optimized =
            build_mesh_optimized_bounded(2, SectionPos::ZERO, sample, || false, 48).unwrap();
        assert_eq!(reference, optimized);
        assert_eq!(
            &optimized.shades[..4],
            &[32.0 / 255.0, 64.0 / 255.0, 128.0 / 255.0, 1.0]
        );
    }

    // Rasterize axis-aligned quads at half-cell resolution. This compares
    // coverage, winding, materials and tints independently of quad splitting.
    fn surfaces(mesh: &MeshData) -> BTreeMap<[i32; 6], (u16, u16, [u32; 3])> {
        let mut result = BTreeMap::new();
        for face in 0..mesh.face_count() {
            let vertices = &mesh.positions[face * 4..face * 4 + 4];
            let normal = mesh.normals[face * 4];
            let axis = normal.iter().position(|n| n.abs() == 1.0).unwrap();
            let (a, b) = match axis {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let min = |axis| {
                vertices
                    .iter()
                    .map(|v| (v[axis] * 2.0) as i32)
                    .min()
                    .unwrap()
            };
            let max = |axis| {
                vertices
                    .iter()
                    .map(|v| (v[axis] * 2.0) as i32)
                    .max()
                    .unwrap()
            };
            for u in min(a)..max(a) {
                for v in min(b)..max(b) {
                    let key = [axis as i32, normal[axis] as i32, min(axis), u, v, 0];
                    assert!(
                        result
                            .insert(
                                key,
                                (
                                    mesh.materials[face],
                                    mesh.overlays[face],
                                    mesh.tints[face * 4].map(f32::to_bits)
                                )
                            )
                            .is_none()
                    );
                }
            }
        }
        result
    }
    #[test]
    fn opaque_runs_keep_surface_coverage_and_reduce_sampling_and_geometry() {
        let calls = Cell::new(0);
        let sample = |p: BlockPos| {
            calls.set(calls.get() + 1);
            if [p.x, p.y, p.z].iter().all(|n| (0..8).contains(n)) {
                MeshSample::Ready(RenderBlock::uniform(7))
            } else {
                MeshSample::NotLoaded
            }
        };
        let ordinary = build_mesh_reference(8, SectionPos::ZERO, sample);
        let ordinary_calls = calls.replace(0);
        let merged =
            build_mesh_optimized_bounded(8, SectionPos::ZERO, sample, || false, 8usize.pow(3) * 6)
                .unwrap();
        assert_eq!(surfaces(&ordinary), surfaces(&merged));
        assert_eq!(ordinary.face_count(), 6 * 8 * 8);
        assert_eq!(merged.face_count(), 6 * 8);
        assert!(calls.get() < ordinary_calls);
        assert!(merged.byte_size() < ordinary.byte_size());
        assert!(merged.uvs.iter().flatten().any(|v| v.abs() > 1.0));
        assert_eq!(
            build_mesh_optimized_bounded(8, SectionPos::ZERO, sample, || false, 47),
            Err(MeshBuildError::OutputLimit)
        );
        assert_eq!(
            build_mesh_optimized_bounded(8, SectionPos::ZERO, sample, || true, 48),
            Err(MeshBuildError::Cancelled)
        );
    }
    #[test]
    fn partial_transparent_tinted_and_overlay_surfaces_match_the_reference() {
        let sample = |p: BlockPos| {
            if ![p.x, p.y, p.z].iter().all(|n| (0..4).contains(n)) {
                return MeshSample::NotLoaded;
            }
            let mut block = RenderBlock::uniform((p.z % 2) as u16);
            match (p.x + 3 * p.y + p.z) % 5 {
                0 => {
                    block.bounds.as_mut().unwrap().max[1] = 0.5;
                    block.occlusion = 4;
                }
                1 => block.occlusion = 0,
                2 => block.face_overlays = Some([Some(9); 6]),
                3 => block.face_tints = Some([[0.5, 1.0, 0.5]; 6]),
                _ => {}
            }
            MeshSample::Ready(block)
        };
        let ordinary = build_mesh(4, SectionPos::ZERO, sample);
        let merged =
            build_mesh_optimized_bounded(4, SectionPos::ZERO, sample, || false, 384).unwrap();
        assert_eq!(surfaces(&ordinary), surfaces(&merged));
    }
}
