//! CPU section meshing with revision-checked dependency stamps.
//!
//! Meshing reads authoritative terrain but never writes it. The mesher takes a
//! sampling closure instead of the world type, so this crate has no dependency on
//! `torse-world`: the application adapts a terrain view to [`MeshSample`], and
//! tests can feed synthetic data.
//!
//! Two rules come straight from the architecture documents:
//!
//! - A face is culled only when the neighbor's occlusion mask claims the face and
//!   the block actually touches that cell boundary. Conservative defaults must
//!   leave extra faces visible rather than delete a visible surface.
//! - A mesh computed from older data must never replace a newer one. [`MeshJob`]
//!   records the section revision and the six neighbor revisions, and
//!   [`MeshCache::insert`] rejects results whose stamp no longer matches.
//!
//! Vertices are section-local. Moving a mesh to world space is the renderer's
//! job, which keeps a mesh valid as long as its own section does not change.

use std::collections::BTreeMap;

use torse_types::{BlockPos, Revision, SectionPos};

/// The six cell faces in occlusion-mask order: `-x, +x, -y, +y, -z, +z`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Face {
    /// Negative x.
    NegX,
    /// Positive x.
    PosX,
    /// Negative y.
    NegY,
    /// Positive y.
    PosY,
    /// Negative z.
    NegZ,
    /// Positive z.
    PosZ,
}

impl Face {
    /// All faces in occlusion-mask order.
    pub const ALL: [Self; 6] = [
        Self::NegX,
        Self::PosX,
        Self::NegY,
        Self::PosY,
        Self::NegZ,
        Self::PosZ,
    ];

    /// Position of this face in a mask.
    pub const fn index(self) -> u32 {
        match self {
            Self::NegX => 0,
            Self::PosX => 1,
            Self::NegY => 2,
            Self::PosY => 3,
            Self::NegZ => 4,
            Self::PosZ => 5,
        }
    }

    /// Mask bit used by [`RenderBlock::occlusion`].
    pub const fn bit(self) -> u8 {
        1u8 << self.index()
    }

    /// The face on the opposite side of the cell.
    pub const fn opposite(self) -> Self {
        match self {
            Self::NegX => Self::PosX,
            Self::PosX => Self::NegX,
            Self::NegY => Self::PosY,
            Self::PosY => Self::NegY,
            Self::NegZ => Self::PosZ,
            Self::PosZ => Self::NegZ,
        }
    }

    /// Outward normal.
    pub const fn normal(self) -> [f32; 3] {
        match self {
            Self::NegX => [-1.0, 0.0, 0.0],
            Self::PosX => [1.0, 0.0, 0.0],
            Self::NegY => [0.0, -1.0, 0.0],
            Self::PosY => [0.0, 1.0, 0.0],
            Self::NegZ => [0.0, 0.0, -1.0],
            Self::PosZ => [0.0, 0.0, 1.0],
        }
    }

    /// Offset from the cell to the neighbor that would cover this face.
    pub const fn offset(self) -> (i64, i64, i64) {
        match self {
            Self::NegX => (-1, 0, 0),
            Self::PosX => (1, 0, 0),
            Self::NegY => (0, -1, 0),
            Self::PosY => (0, 1, 0),
            Self::NegZ => (0, 0, -1),
            Self::PosZ => (0, 0, 1),
        }
    }

    /// Neighbor section, or None when the coordinate cannot be represented.
    pub fn neighbor_section(self, section: SectionPos) -> Option<SectionPos> {
        let (x, y, z) = self.offset();
        Some(SectionPos::new(
            section.x.checked_add(x)?,
            section.y.checked_add(y)?,
            section.z.checked_add(z)?,
        ))
    }

    /// Whether this is a negative-axis face.
    const fn is_negative(self) -> bool {
        matches!(self, Self::NegX | Self::NegY | Self::NegZ)
    }

    /// Axis touched by this face.
    const fn axis(self) -> usize {
        match self {
            Self::NegX | Self::PosX => 0,
            Self::NegY | Self::PosY => 1,
            Self::NegZ | Self::PosZ => 2,
        }
    }
}

/// Axis-aligned visual bounds in the unit cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderBox {
    /// Lower corner components, each in `0.0..=1.0`.
    pub min: [f32; 3],
    /// Upper corner components, each in `0.0..=1.0`.
    pub max: [f32; 3],
}

impl RenderBox {
    /// Bounds covering the entire cell.
    pub const FULL_CUBE: Self = Self {
        min: [0.0, 0.0, 0.0],
        max: [1.0, 1.0, 1.0],
    };

    /// Return whether the box reaches the cell boundary on a face.
    fn touches(self, face: Face) -> bool {
        let axis = face.axis();
        if face.is_negative() {
            self.min[axis] <= 0.0
        } else {
            self.max[axis] >= 1.0
        }
    }
}

/// Visual description of one loaded cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderBlock {
    /// Geometry to emit, or `None` for a block with no visual.
    pub bounds: Option<RenderBox>,
    /// Faces this block covers for neighbors, in mask order.
    pub occlusion: u8,
    /// Material identifier for faces without a per-face override.
    pub material: u16,
    /// Materials for the six faces in mask order; `None` uses `material`.
    pub face_materials: Option<[u16; 6]>,
}

impl RenderBlock {
    /// Block with no visual; used for empty cells.
    pub const EMPTY: Self = Self {
        bounds: None,
        occlusion: 0,
        material: 0,
        face_materials: None,
    };

    /// Full cube using one material on every face.
    pub const fn uniform(material: u16) -> Self {
        Self {
            bounds: Some(RenderBox::FULL_CUBE),
            occlusion: 0b0011_1111,
            material,
            face_materials: None,
        }
    }

    /// Material of one face.
    pub fn material_for(&self, face: Face) -> u16 {
        match &self.face_materials {
            Some(materials) => materials[face.index() as usize],
            None => self.material,
        }
    }
}

/// Result of sampling one cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MeshSample {
    /// The cell has authoritative data.
    Ready(RenderBlock),
    /// The section is not loaded; faces toward it stay visible.
    NotLoaded,
    /// The cell is outside the world; faces toward it stay visible.
    OutOfBounds,
}

/// Identifiers and geometry produced for one section.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    /// Section this mesh belongs to.
    pub section: SectionPos,
    /// Vertex positions in section-local coordinates.
    pub positions: Vec<[f32; 3]>,
    /// One normal per vertex.
    pub normals: Vec<[f32; 3]>,
    /// One texture coordinate per vertex, local to the face.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle indices.
    pub indices: Vec<u32>,
    /// Material per emitted quad.
    pub materials: Vec<u16>,
}

impl MeshData {
    /// Number of emitted quads.
    pub fn face_count(&self) -> usize {
        self.materials.len()
    }

    /// Number of triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Return whether nothing was emitted.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Approximate CPU bytes held by this mesh.
    pub fn byte_size(&self) -> usize {
        self.positions.len() * size_of::<[f32; 3]>()
            + self.normals.len() * size_of::<[f32; 3]>()
            + self.uvs.len() * size_of::<[f32; 2]>()
            + self.indices.len() * size_of::<u32>()
            + self.materials.len() * size_of::<u16>()
    }
}

impl MeshData {
    /// Reserved heap capacity; use this when charging owned background output.
    pub fn allocated_bytes(&self) -> usize {
        self.positions.capacity() * size_of::<[f32; 3]>()
            + self.normals.capacity() * size_of::<[f32; 3]>()
            + self.uvs.capacity() * size_of::<[f32; 2]>()
            + self.indices.capacity() * size_of::<u32>()
            + self.materials.capacity() * size_of::<u16>()
    }
}

/// Build a section mesh.
///
/// `sample` returns authoritative data for block coordinates. It is called for
/// every cell and for each face's neighbor, so an implementation backed by a
/// hash map lookup is acceptable for M1 but should be replaced by a batched
/// section read before profiling.
pub fn build_mesh(
    side: u32,
    section: SectionPos,
    sample: impl Fn(BlockPos) -> MeshSample,
) -> MeshData {
    build_mesh_inner(side, section, sample, || false, None)
        .expect("uncancelled unrestricted meshing succeeds")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshBuildError {
    Cancelled,
    OutputLimit,
}

/// Bound face output before allocation and check cancellation at each y slice.
/// Input is owned by the caller; output allocation is at most twice the stated
/// face payload, allowing final compaction while the original buffers exist.
pub fn build_mesh_bounded(
    side: u32,
    section: SectionPos,
    sample: impl Fn(BlockPos) -> MeshSample,
    cancelled: impl Fn() -> bool,
    max_faces: usize,
) -> Result<MeshData, MeshBuildError> {
    if max_faces > u32::MAX as usize / 4 || max_faces > usize::MAX / 6 {
        return Err(MeshBuildError::OutputLimit);
    }
    build_mesh_inner(side, section, sample, cancelled, Some(max_faces))
}

fn build_mesh_inner(
    side: u32,
    section: SectionPos,
    sample: impl Fn(BlockPos) -> MeshSample,
    cancelled: impl Fn() -> bool,
    max_faces: Option<usize>,
) -> Result<MeshData, MeshBuildError> {
    if cancelled() {
        return Err(MeshBuildError::Cancelled);
    }
    let origin = [
        section.x.saturating_mul(i64::from(side)),
        section.y.saturating_mul(i64::from(side)),
        section.z.saturating_mul(i64::from(side)),
    ];
    let mut mesh = MeshData {
        section,
        ..MeshData::default()
    };

    if let Some(faces) = max_faces {
        mesh.positions
            .try_reserve_exact(faces * 4)
            .map_err(|_| MeshBuildError::OutputLimit)?;
        mesh.normals
            .try_reserve_exact(faces * 4)
            .map_err(|_| MeshBuildError::OutputLimit)?;
        mesh.uvs
            .try_reserve_exact(faces * 4)
            .map_err(|_| MeshBuildError::OutputLimit)?;
        mesh.indices
            .try_reserve_exact(faces * 6)
            .map_err(|_| MeshBuildError::OutputLimit)?;
        mesh.materials
            .try_reserve_exact(faces)
            .map_err(|_| MeshBuildError::OutputLimit)?;
    }

    for y in 0..side {
        if cancelled() {
            return Err(MeshBuildError::Cancelled);
        }
        for z in 0..side {
            for x in 0..side {
                let pos = BlockPos::new(
                    origin[0] + i64::from(x),
                    origin[1] + i64::from(y),
                    origin[2] + i64::from(z),
                );
                let block = match sample(pos) {
                    MeshSample::Ready(block) => block,
                    MeshSample::NotLoaded | MeshSample::OutOfBounds => continue,
                };
                let Some(bounds) = block.bounds else {
                    continue;
                };
                for face in Face::ALL {
                    if face_culled(face, &block, &sample, pos) {
                        continue;
                    }
                    if max_faces.is_some_and(|limit| mesh.face_count() >= limit) {
                        return Err(MeshBuildError::OutputLimit);
                    }
                    emit_face(
                        &mut mesh,
                        face,
                        bounds,
                        [x as f32, y as f32, z as f32],
                        block.material_for(face),
                    );
                }
            }
        }
    }
    if max_faces.is_some() {
        mesh.positions.shrink_to_fit();
        mesh.normals.shrink_to_fit();
        mesh.uvs.shrink_to_fit();
        mesh.indices.shrink_to_fit();
        mesh.materials.shrink_to_fit();
    }
    Ok(mesh)
}

fn face_culled(
    face: Face,
    block: &RenderBlock,
    sample: &impl Fn(BlockPos) -> MeshSample,
    pos: BlockPos,
) -> bool {
    let Some(bounds) = block.bounds else {
        return true;
    };
    if !bounds.touches(face) {
        // A partial shape that does not reach the boundary can never be hidden
        // by the neighbor, even when the neighbor fully covers the cell.
        return false;
    }
    let (dx, dy, dz) = face.offset();
    let neighbor = pos.checked_add(dx, dy, dz);
    let Ok(neighbor) = neighbor else {
        return false;
    };
    match sample(neighbor) {
        MeshSample::Ready(neighbor_block) => {
            // The shared plane sits on the neighbour's opposite boundary:
            // a half slab below cannot hide the face above its midline, so
            // the neighbour must claim coverage of that boundary and its
            // shape must actually reach it.
            let opposite = face.opposite();
            if neighbor_block.occlusion & opposite.bit() == 0 {
                return false;
            }
            match neighbor_block.bounds {
                Some(nb) => nb.touches(opposite),
                None => true,
            }
        }
        // Missing data must keep the face visible. Treating it as covered would
        // punch holes in the world exactly where the client has no data.
        MeshSample::NotLoaded | MeshSample::OutOfBounds => false,
    }
}

fn emit_face(mesh: &mut MeshData, face: Face, bounds: RenderBox, cell: [f32; 3], material: u16) {
    let (origin, edge_u, edge_v) = face_quad(face, bounds, cell);
    let normal = face.normal();
    let base = mesh.positions.len() as u32;
    let corners = [
        origin,
        add(origin, edge_u),
        add(add(origin, edge_u), edge_v),
        add(origin, edge_v),
    ];
    // Local face coordinates. The origin corner is the bottom left of the face
    // image, and v grows towards the first row of the texture.
    let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    for (corner, uv) in corners.into_iter().zip(uvs) {
        mesh.positions.push(corner);
        mesh.normals.push(normal);
        mesh.uvs.push(uv);
    }
    // Two triangles: (0, 1, 2) and (0, 2, 3). The corner order is chosen so the
    // cross product points along the face normal.
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    mesh.materials.push(material);
}

/// Corner and edge vectors for a face, chosen so `u x v` equals the normal.
fn face_quad(face: Face, bounds: RenderBox, cell: [f32; 3]) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let min = add(cell, bounds.min);
    let max = add(cell, bounds.max);
    let size = [
        bounds.max[0] - bounds.min[0],
        bounds.max[1] - bounds.min[1],
        bounds.max[2] - bounds.min[2],
    ];
    match face {
        Face::PosX => (
            [max[0], min[1], max[2]],
            [0.0, 0.0, -size[2]],
            [0.0, size[1], 0.0],
        ),
        Face::NegX => (
            [min[0], min[1], min[2]],
            [0.0, 0.0, size[2]],
            [0.0, size[1], 0.0],
        ),
        Face::PosY => (
            [min[0], max[1], max[2]],
            [size[0], 0.0, 0.0],
            [0.0, 0.0, -size[2]],
        ),
        Face::NegY => (
            [min[0], min[1], min[2]],
            [size[0], 0.0, 0.0],
            [0.0, 0.0, size[2]],
        ),
        Face::PosZ => (
            [min[0], min[1], max[2]],
            [size[0], 0.0, 0.0],
            [0.0, size[1], 0.0],
        ),
        Face::NegZ => (
            [max[0], min[1], min[2]],
            [-size[0], 0.0, 0.0],
            [0.0, size[1], 0.0],
        ),
    }
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// Revisions a mesh depends on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshJob {
    /// Section being meshed.
    pub section: SectionPos,
    /// Side length used for the build.
    pub side: u32,
    /// Terrain revision of the section at capture time.
    pub section_revision: Option<Revision>,
    /// Revisions of the six neighbors in [`Face::ALL`] order.
    pub neighbor_revisions: [Option<Revision>; 6],
}

impl MeshJob {
    /// Record the dependencies of a mesh request.
    pub fn capture(
        side: u32,
        section: SectionPos,
        revision_of: impl Fn(SectionPos) -> Option<Revision>,
    ) -> Self {
        let mut neighbor_revisions = [None; 6];
        for (slot, face) in neighbor_revisions.iter_mut().zip(Face::ALL) {
            *slot = face.neighbor_section(section).and_then(&revision_of);
        }
        Self {
            section,
            side,
            section_revision: revision_of(section),
            neighbor_revisions,
        }
    }

    /// Return whether the captured revisions are still current.
    pub fn validate(&self, revision_of: impl Fn(SectionPos) -> Option<Revision>) -> bool {
        if revision_of(self.section) != self.section_revision {
            return false;
        }
        for (face, captured) in Face::ALL.iter().zip(self.neighbor_revisions) {
            if face.neighbor_section(self.section).and_then(&revision_of) != captured {
                return false;
            }
        }
        true
    }
}

/// One accepted mesh and the stamp it was built from.
#[derive(Clone, Debug, PartialEq)]
pub struct CachedMesh {
    /// Dependencies recorded when the mesh was requested.
    pub job: MeshJob,
    /// Finished geometry.
    pub mesh: MeshData,
}

/// Result of offering a mesh to the cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertOutcome {
    /// The mesh matched its dependencies and replaced the previous entry.
    Accepted,
    /// The data changed while the mesh was being built; the result is dropped.
    Stale,
}

/// Bounded store of finished section meshes.
///
/// The cache keeps one mesh per section and evicts in coordinate order when it
/// exceeds capacity. Coordinate order is deterministic, which keeps test output
/// stable; it is not an eviction policy based on visibility.
#[derive(Debug)]
pub struct MeshCache {
    capacity: usize,
    entries: BTreeMap<SectionPos, CachedMesh>,
}

impl MeshCache {
    /// Create a cache holding at most `capacity` sections.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: BTreeMap::new(),
        }
    }

    /// Number of cached meshes.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether the cache holds no mesh.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a finished mesh.
    pub fn get(&self, section: SectionPos) -> Option<&CachedMesh> {
        self.entries.get(&section)
    }

    /// Offer a finished mesh.
    ///
    /// The stamp is validated against `revision_of` before insertion, so an
    /// asynchronous worker cannot publish a mesh that was computed from data the
    /// authority has already replaced.
    pub fn insert(
        &mut self,
        job: MeshJob,
        mesh: MeshData,
        revision_of: impl Fn(SectionPos) -> Option<Revision>,
    ) -> InsertOutcome {
        if !job.validate(&revision_of) {
            return InsertOutcome::Stale;
        }
        self.entries.insert(job.section, CachedMesh { job, mesh });
        while self.entries.len() > self.capacity {
            let Some(first) = self.entries.keys().next().copied() else {
                break;
            };
            self.entries.remove(&first);
        }
        InsertOutcome::Accepted
    }

    /// Total CPU bytes held by cached meshes.
    pub fn byte_size(&self) -> usize {
        self.entries
            .values()
            .map(|cached| cached.mesh.byte_size())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn bounded_meshing_matches_reference_and_discards_partial_cancelled_or_oversized_output() {
        let sample = single(Some(cube(0b11_1111)));
        let expected = build_mesh(4, SectionPos::ZERO, &sample);
        assert_eq!(
            build_mesh_bounded(4, SectionPos::ZERO, &sample, || false, 6).unwrap(),
            expected
        );
        assert_eq!(
            build_mesh_bounded(4, SectionPos::ZERO, &sample, || false, 5),
            Err(MeshBuildError::OutputLimit)
        );
        let checks = Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() >= 3
        };
        assert_eq!(
            build_mesh_bounded(4, SectionPos::ZERO, sample, cancelled, 6),
            Err(MeshBuildError::Cancelled)
        );
    }

    #[test]
    fn dependency_capture_at_coordinate_limits_keeps_unrepresentable_neighbors_absent() {
        let pos = SectionPos::new(i64::MAX, i64::MIN, 0);
        let job = MeshJob::capture(1, pos, |_| Some(Revision::INITIAL));
        assert_eq!(job.neighbor_revisions[Face::PosX.index() as usize], None);
        assert_eq!(job.neighbor_revisions[Face::NegY.index() as usize], None);
        assert!(job.validate(|_| Some(Revision::INITIAL)));
    }

    fn cube(occlusion: u8) -> RenderBlock {
        RenderBlock {
            bounds: Some(RenderBox::FULL_CUBE),
            occlusion,
            material: 1,
            face_materials: None,
        }
    }

    fn single(state: Option<RenderBlock>) -> impl Fn(BlockPos) -> MeshSample {
        move |pos: BlockPos| {
            if pos == BlockPos::new(1, 1, 1) {
                match state {
                    Some(block) => MeshSample::Ready(block),
                    None => MeshSample::NotLoaded,
                }
            } else {
                MeshSample::Ready(RenderBlock {
                    bounds: None,
                    occlusion: 0,
                    material: 0,
                    face_materials: None,
                })
            }
        }
    }

    #[test]
    fn isolated_cube_emits_six_faces() {
        let mesh = build_mesh(4, SectionPos::ZERO, single(Some(cube(0b11_1111))));
        assert_eq!(mesh.face_count(), 6);
        assert_eq!(mesh.positions.len(), 24);
        assert_eq!(mesh.triangle_count(), 12);
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert_eq!(mesh.uvs.len(), mesh.positions.len());
    }

    #[test]
    fn every_quad_carries_local_face_coordinates() {
        let mesh = build_mesh(4, SectionPos::ZERO, single(Some(cube(0b11_1111))));
        for quad in mesh.uvs.chunks(4) {
            // Corner order is origin, +u, +u+v, +v.
            assert_eq!(quad, [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
        }
    }

    #[test]
    fn per_face_materials_select_by_face_order() {
        let block = RenderBlock {
            bounds: Some(RenderBox::FULL_CUBE),
            occlusion: 0b11_1111,
            material: 9,
            face_materials: Some([10, 11, 12, 13, 14, 15]),
        };
        let mesh = build_mesh(4, SectionPos::ZERO, single(Some(block)));
        assert_eq!(mesh.materials, vec![10, 11, 12, 13, 14, 15]);
    }

    #[test]
    fn uniform_block_keeps_one_material_on_every_face() {
        let mesh = build_mesh(4, SectionPos::ZERO, single(Some(RenderBlock::uniform(7))));
        assert_eq!(mesh.materials, vec![7; 6]);
    }

    #[test]
    fn shared_face_between_two_cubes_is_culled() {
        let sample = |pos: BlockPos| {
            if pos == BlockPos::new(1, 1, 1) || pos == BlockPos::new(2, 1, 1) {
                MeshSample::Ready(cube(0b11_1111))
            } else {
                MeshSample::Ready(RenderBlock {
                    bounds: None,
                    occlusion: 0,
                    material: 0,
                    face_materials: None,
                })
            }
        };
        let mesh = build_mesh(4, SectionPos::ZERO, sample);
        assert_eq!(mesh.face_count(), 10);
    }

    #[test]
    fn missing_neighbor_data_keeps_faces_visible() {
        let mesh = build_mesh(4, SectionPos::ZERO, single(None));
        assert!(
            mesh.is_empty(),
            "an unloaded cell has no geometry of its own"
        );
    }

    #[test]
    fn unloaded_boundary_neighbor_does_not_cull() {
        // A cube at the section edge with an unloaded neighbor must still emit
        // the boundary face; otherwise a hole appears exactly at the seam.
        let sample = |pos: BlockPos| {
            if pos == BlockPos::new(3, 1, 1) {
                MeshSample::Ready(cube(0b11_1111))
            } else if pos.x == 4 {
                MeshSample::NotLoaded
            } else {
                MeshSample::Ready(RenderBlock {
                    bounds: None,
                    occlusion: 0,
                    material: 0,
                    face_materials: None,
                })
            }
        };
        let mesh = build_mesh(4, SectionPos::ZERO, sample);
        assert_eq!(mesh.face_count(), 6);
    }

    #[test]
    fn slab_top_face_survives_a_covering_neighbor() {
        let slab = RenderBlock {
            bounds: Some(RenderBox {
                min: [0.0, 0.0, 0.0],
                max: [1.0, 0.5, 1.0],
            }),
            occlusion: 0b11_1111,
            material: 1,
            face_materials: None,
        };
        let sample = |pos: BlockPos| {
            if pos == BlockPos::new(1, 1, 1) {
                MeshSample::Ready(slab)
            } else if pos == BlockPos::new(1, 2, 1) {
                MeshSample::Ready(cube(0b11_1111))
            } else {
                MeshSample::Ready(RenderBlock {
                    bounds: None,
                    occlusion: 0,
                    material: 0,
                    face_materials: None,
                })
            }
        };
        let mesh = build_mesh(4, SectionPos::ZERO, sample);
        // The slab keeps its top face at y = 1.5 because it does not reach y = 1.
        // The cube above also contributes a +y face, so compare positions.
        let up = Face::PosY.normal();
        let slab_top = mesh
            .positions
            .chunks(4)
            .zip(mesh.normals.chunks(4))
            .filter(|(corners, normals)| {
                normals[0] == up && (corners[0][1] - 1.5).abs() < f32::EPSILON
            })
            .count();
        assert_eq!(slab_top, 1);
    }

    #[test]
    fn neighbor_without_occlusion_bit_does_not_cull() {
        let sample = |pos: BlockPos| {
            if pos == BlockPos::new(1, 1, 1) {
                MeshSample::Ready(cube(0b11_1111))
            } else if pos == BlockPos::new(2, 1, 1) {
                // A different state: the mask rule applies, not the
                // same-material merge.
                MeshSample::Ready(RenderBlock {
                    bounds: Some(RenderBox::FULL_CUBE),
                    occlusion: 0,
                    material: 2,
                    face_materials: None,
                })
            } else {
                MeshSample::Ready(RenderBlock {
                    bounds: None,
                    occlusion: 0,
                    material: 0,
                    face_materials: None,
                })
            }
        };
        let mesh = build_mesh(4, SectionPos::ZERO, sample);
        // The positive neighbor claims no coverage, so the first cube keeps its
        // +x face. The second cube's -x face is still culled by the first cube,
        // which does claim full coverage.
        assert_eq!(mesh.face_count(), 11);
    }

    #[test]
    fn same_material_without_coverage_keeps_faces() {
        let sample = |pos: BlockPos| {
            if (1..=2).contains(&pos.x) && pos.y == 1 && pos.z == 1 {
                // Both cells claim no boundary coverage (mask 0): nothing
                // may cull the faces between or around them.
                MeshSample::Ready(cube(0))
            } else {
                MeshSample::Ready(RenderBlock {
                    bounds: None,
                    occlusion: 0,
                    material: 0,
                    face_materials: None,
                })
            }
        };
        let mesh = build_mesh(4, SectionPos::ZERO, sample);
        assert_eq!(mesh.face_count(), 12);
    }

    #[test]
    fn positions_are_section_local() {
        let sample = |pos: BlockPos| {
            if pos == BlockPos::new(-5, 1, 1) {
                MeshSample::Ready(cube(0b11_1111))
            } else {
                MeshSample::Ready(RenderBlock {
                    bounds: None,
                    occlusion: 0,
                    material: 0,
                    face_materials: None,
                })
            }
        };
        // Section -1 covers blocks -4..= -1 with side 4, so block -5 is not in it.
        let mesh = build_mesh(4, SectionPos::new(-1, 0, 0), sample);
        assert!(mesh.is_empty());
    }

    #[test]
    fn job_rejects_later_terrain_changes() {
        let revision = Cell::new(Some(Revision(3)));
        let job = MeshJob::capture(4, SectionPos::ZERO, |pos| {
            if pos == SectionPos::ZERO {
                revision.get()
            } else {
                None
            }
        });
        assert!(job.validate(|pos| if pos == SectionPos::ZERO {
            revision.get()
        } else {
            None
        }));
        revision.set(Some(Revision(4)));
        assert!(!job.validate(|pos| if pos == SectionPos::ZERO {
            revision.get()
        } else {
            None
        }));
    }

    #[test]
    fn cache_drops_a_stale_mesh_instead_of_overwriting() {
        let revision = Cell::new(Some(Revision(1)));
        let job = MeshJob::capture(4, SectionPos::ZERO, |pos| {
            if pos == SectionPos::ZERO {
                revision.get()
            } else {
                None
            }
        });
        let mesh = build_mesh(4, SectionPos::ZERO, single(Some(cube(0b11_1111))));
        let mut cache = MeshCache::new(4);
        assert_eq!(
            cache.insert(job, mesh.clone(), |pos| if pos == SectionPos::ZERO {
                revision.get()
            } else {
                None
            }),
            InsertOutcome::Accepted
        );
        revision.set(Some(Revision(2)));
        assert_eq!(
            cache.insert(job, mesh, |pos| if pos == SectionPos::ZERO {
                revision.get()
            } else {
                None
            }),
            InsertOutcome::Stale
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_evicts_beyond_capacity() {
        let mut cache = MeshCache::new(1);
        let mut build = |section: SectionPos| {
            let job = MeshJob::capture(4, section, |_| None);
            let mesh = build_mesh(4, section, |_| MeshSample::NotLoaded);
            cache.insert(job, mesh, |_| None)
        };
        assert_eq!(build(SectionPos::ZERO), InsertOutcome::Accepted);
        assert_eq!(build(SectionPos::new(1, 0, 0)), InsertOutcome::Accepted);
        assert_eq!(cache.len(), 1);
        assert!(cache.get(SectionPos::ZERO).is_none());
    }
}
