//! Incremental derived lighting. The scene thread alone validates and publishes.
use super::*;
use torse_types::{Binding, EvictionRank, MemoryClass, MemoryLease, MemoryPool};
use torse_world_services::lighting::{LightCache, LightJob, LightLimits, LightSection, LightStamp};

const WORK_BYTES: u64 = 16 * 1024 * 1024;
const LIMITS: LightLimits = LightLimits {
    cells: 1_000_000,
    dependencies: 1024,
    frontier: 1_000_000,
    bytes: WORK_BYTES,
};

struct Pending {
    target: SectionPos,
    kind: usize,
    job: Option<LightJob>,
    result: Option<LightSection>,
    _memory: Option<MemoryLease>,
}
struct Cached {
    stamp: LightStamp,
    _memory: Option<MemoryLease>,
    generation: u64,
}
pub(super) struct Lighting {
    binding: Binding,
    caches: [LightCache; 2],
    entries: [BTreeMap<SectionPos, Cached>; 2],
    pending: Option<Pending>,
    memory: Option<MemoryPool>,
    pub generation: u64,
}
impl Lighting {
    pub fn new(binding: Binding, memory: Option<MemoryPool>) -> Result<Self, String> {
        Ok(Self {
            binding,
            caches: [
                LightCache::new(binding, 32768, 64 * 1024 * 1024).map_err(|e| e.to_string())?,
                LightCache::new(binding, 32768, 64 * 1024 * 1024).map_err(|e| e.to_string())?,
            ],
            entries: [BTreeMap::new(), BTreeMap::new()],
            pending: None,
            memory,
            generation: 0,
        })
    }
    fn advance_generation(&mut self) -> Result<(), String> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("visual lighting generation exhausted")?;
        Ok(())
    }
    pub fn step(&mut self, scene: &Scene, work: usize) -> Result<(), String> {
        let store = scene.authority.store();
        let registry = scene.authority.registry();
        for kind in 0..2 {
            let invalid: Vec<_> = self.entries[kind]
                .iter()
                .filter_map(|(pos, entry)| {
                    (!scene.sections.contains(pos)
                        || !self.caches[kind].validate_stamp(
                            entry.stamp,
                            self.binding,
                            store,
                            registry,
                        ))
                    .then_some(*pos)
                })
                .collect();
            for pos in invalid {
                self.advance_generation()?;
                self.entries[kind].remove(&pos);
                self.caches[kind].remove(pos);
            }
        }
        if let Some(pending) = self.pending.as_mut() {
            if !scene.sections.contains(&pending.target) {
                self.pending = None;
                return Ok(());
            }
            if let Some(job) = pending.job.as_mut() {
                match job.poll(self.binding, store, registry, work) {
                    Ok(false) => return Ok(()),
                    Err(_) => {
                        self.pending = None;
                        return Ok(());
                    }
                    Ok(true) => {
                        pending.result = Some(
                            pending
                                .job
                                .take()
                                .expect("active light job")
                                .finish(self.binding, store, registry)
                                .map_err(|e| e.to_string())?,
                        );
                    }
                }
            }
            let result = pending.result.as_ref().expect("resolved light result");
            if !result.validate(self.binding, store, registry) {
                self.pending = None;
                return Ok(());
            }
            let lease = match self
                .memory
                .as_ref()
                .map(|pool| {
                    pool.reserve(
                        MemoryClass::Mesh,
                        (result.allocated_bytes() + 256) as u64,
                        EvictionRank::Pinned,
                    )
                })
                .transpose()
            {
                Ok(lease) => lease,
                Err(_) => return Ok(()),
            };
            self.advance_generation()?;
            let pending = self.pending.take().expect("light publication");
            let stamp = self.caches[pending.kind]
                .publish(
                    pending.result.expect("resolved light result"),
                    store,
                    registry,
                )
                .map_err(|e| e.to_string())?;
            self.entries[pending.kind].insert(
                pending.target,
                Cached {
                    stamp,
                    _memory: lease,
                    generation: self.generation,
                },
            );
            return Ok(());
        }
        for target in &scene.sections {
            if store.revision(*target).is_none()
                || !matches!(
                    store.phase(*target),
                    Some(torse_world::SectionPhase::Ready | torse_world::SectionPhase::Active)
                )
            {
                continue;
            }
            for kind in 0..2 {
                if self.entries[kind].contains_key(target) {
                    continue;
                }
                let lease = match self
                    .memory
                    .as_ref()
                    .map(|pool| {
                        pool.reserve(MemoryClass::Executor, WORK_BYTES, EvictionRank::Pinned)
                    })
                    .transpose()
                {
                    Ok(lease) => lease,
                    Err(_) => return Ok(()),
                };
                let job = if kind == 0 {
                    LightJob::prepare(self.binding, store, registry, *target, LIMITS)
                } else {
                    LightJob::prepare_sky(self.binding, store, registry, *target, LIMITS)
                };
                let job = match job {
                    Ok(job) => job,
                    Err(_) => return Ok(()),
                };
                self.pending = Some(Pending {
                    target: *target,
                    kind,
                    job: Some(job),
                    result: None,
                    _memory: lease,
                });
                return Ok(());
            }
        }
        Ok(())
    }
    pub fn complete(&self, scene: &Scene) -> bool {
        self.pending.is_none()
            && scene.sections.iter().all(|pos| {
                scene.authority.store().revision(*pos).is_some()
                    && self.entries.iter().all(|entries| entries.contains_key(pos))
            })
    }
    pub fn view<'a>(&'a self, scene: &Scene) -> View<'a> {
        let mut grids: [BTreeMap<SectionPos, &'a [u8]>; 2] = [BTreeMap::new(), BTreeMap::new()];
        for (kind, grid) in grids.iter_mut().enumerate() {
            for pos in self.entries[kind].keys() {
                if let Some(data) = self.caches[kind].section(
                    self.binding,
                    scene.authority.store(),
                    scene.authority.registry(),
                    *pos,
                ) {
                    grid.insert(*pos, data.levels());
                }
            }
        }
        View {
            grids,
            side: scene.side,
            bounds: scene.authority.store().bounds(),
        }
    }
    pub fn stamp(&self, scene: &Scene, target: SectionPos) -> [u64; 2] {
        std::array::from_fn(|kind| {
            self.entries[kind]
                .get(&target)
                .filter(|entry| {
                    self.caches[kind].validate_stamp(
                        entry.stamp,
                        self.binding,
                        scene.authority.store(),
                        scene.authority.registry(),
                    )
                })
                .map_or(0, |entry| entry.generation)
        })
    }
}
pub(super) struct View<'a> {
    grids: [BTreeMap<SectionPos, &'a [u8]>; 2],
    side: u32,
    bounds: WorldBounds,
}
impl View<'_> {
    fn light(&self, kind: usize, pos: BlockPos) -> u8 {
        if kind == 1 && pos.y >= self.bounds.max_y {
            return 15;
        }
        let Ok((section, local)) = pos.to_section(self.side) else {
            return 0;
        };
        self.grids[kind]
            .get(&section)
            .map_or(0, |grid| grid[torse_world::linear_index(self.side, local)])
    }
    pub fn shade(&self, scene: &Scene, pos: BlockPos, block: &mut RenderBlock) {
        let mut faces = [[255; 4]; 6];
        for face in torse_render::Face::ALL {
            let normal = face.normal();
            let axis = normal.iter().position(|n| *n != 0.0).expect("axis face");
            let (a, b) = match axis {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let (dx, dy, dz) = face.offset();
            let Ok(outside) = pos.checked_add(dx, dy, dz) else {
                continue;
            };
            if block.bounds == Some(RenderBox::FULL_CUBE)
                && !block.cross
                && let TerrainQuery::Ready(state) = scene.authority.store().query(outside)
                && scene.authority.registry().occlusion_mask(state) == 0x3f
            {
                continue;
            }
            let emission = self.light(0, pos).max(self.light(0, outside));
            let sky = self.light(1, outside);
            let directional = match face {
                torse_render::Face::PosY => 1.0,
                torse_render::Face::NegY => 0.5,
                torse_render::Face::NegX | torse_render::Face::PosX => 0.6,
                _ => 0.8,
            };
            // Match face_quad winding so shared corners sample the same cells.
            let corners = match face {
                torse_render::Face::NegX => [[0, 0], [0, 1], [1, 1], [1, 0]],
                torse_render::Face::PosX => [[0, 1], [0, 0], [1, 0], [1, 1]],
                torse_render::Face::NegY => [[0, 0], [1, 0], [1, 1], [0, 1]],
                torse_render::Face::PosY => [[0, 1], [1, 1], [1, 0], [0, 0]],
                torse_render::Face::NegZ => [[1, 0], [0, 0], [0, 1], [1, 1]],
                torse_render::Face::PosZ => [[0, 0], [1, 0], [1, 1], [0, 1]],
            };
            for (corner, [u, v]) in corners.into_iter().enumerate() {
                let mut da = [0i64; 3];
                da[a] = if u == 0 { -1 } else { 1 };
                let mut db = [0i64; 3];
                db[b] = if v == 0 { -1 } else { 1 };
                let covered = |offset: [i64; 3]| {
                    let Ok(p) = outside.checked_add(offset[0], offset[1], offset[2]) else {
                        return true;
                    };
                    match scene.authority.store().query(p) {
                        TerrainQuery::Ready(state) => {
                            scene.authority.registry().occlusion_mask(state) == 0x3f
                        }
                        TerrainQuery::NotLoaded => true,
                        TerrainQuery::OutOfBounds => false,
                    }
                };
                let sa = covered(da);
                let sb = covered(db);
                let occlusion = if sa && sb {
                    3
                } else {
                    u8::from(sa)
                        + u8::from(sb)
                        + u8::from(covered(std::array::from_fn(|i| da[i] + db[i])))
                };
                let ao = 1.0 - f32::from(occlusion) * 0.15;
                let light = (f32::from(emission) / 15.0)
                    .max(f32::from(sky) / 15.0 * directional)
                    .max(0.08)
                    * ao;
                faces[face.index() as usize][corner] = (light * 255.0).round() as u8;
            }
        }
        block.face_shades = Some(faces);
    }
}

impl Scene {
    pub fn enable_lighting(&mut self) -> Result<(), String> {
        if self.lighting.is_none() {
            // Direct skylight needs the columns above the visible terrain.
            // The built-in generator owns this finite vertical extent.
            if self.generator.is_some() {
                let bounds = self.authority.store().bounds();
                self.layers =
                    ((bounds.max_y - bounds.min_y) as u64).div_ceil(u64::from(self.side)) as i64;
                self.resize_visual_control().map_err(|e| e.to_string())?;
                if self.background.is_none() {
                    self.load_around(SectionPos::ZERO)?;
                }
            }
            let binding = Binding {
                world: self.authority.world(),
                epoch: self.epoch,
                compatibility: self.authority.coordinator().compatibility(),
            };
            self.lighting = Some(Lighting::new(
                binding,
                self.memory.as_ref().map(|m| m.pool.clone()),
            )?);
            let positions: Vec<_> = self.meshes.keys().copied().collect();
            for pos in positions {
                self.forget_mesh(&pos);
            }
        }
        Ok(())
    }
    pub fn refresh_lighting(&mut self, work: usize) -> Result<bool, String> {
        let Some(mut lighting) = self.lighting.take() else {
            return Ok(true);
        };
        if lighting.binding.epoch != self.epoch {
            let binding = Binding {
                world: self.authority.world(),
                epoch: self.epoch,
                compatibility: self.authority.coordinator().compatibility(),
            };
            lighting = Lighting::new(binding, self.memory.as_ref().map(|m| m.pool.clone()))?;
        }
        let result = lighting.step(self, work);
        let complete = lighting.complete(self);
        self.lighting = Some(lighting);
        result.map(|()| complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scene() -> Scene {
        let mut scene = Scene::new(0).unwrap();
        let mut pack = content_pack();
        for block in &mut pack.blocks {
            if block.value.id == key("base:stone") {
                block.value.light_transport.absorption = 1.0;
            }
            if block.value.id == key("base:glass") {
                block.value.light_transport.emission = 1.0;
                block.value.light_transport.absorption = 0.0;
            }
        }
        let content = compile(pack).unwrap();
        let registry = BlockStateRegistry::build(&content).unwrap();
        let (palette, materials) =
            material_tables(&registry, &crate::texture::base_pack().unwrap()).unwrap();
        scene.authority = WorldAuthority::new(
            WorldId(3),
            registry,
            ComponentSchemas::build(&content),
            16,
            WorldBounds::new(0, 16).unwrap(),
            Compatibility {
                authority_schema: 4,
                rules_digest: content.authority_digest,
                content_digest: content.presentation_digest,
            },
        )
        .unwrap();
        scene.palette = palette;
        scene.state_materials = materials;
        scene.generator = None;
        scene.load_generations.clear();
        scene.sections = vec![SectionPos::ZERO, SectionPos::new(1, 0, 0)];
        for x in -1..=2 {
            for z in -1..=1 {
                assert!(
                    scene
                        .install_generated(SectionPos::new(x, 0, z), Section::empty(16).unwrap())
                        .unwrap()
                );
            }
        }
        scene
    }
    fn resolve(scene: &mut Scene) {
        for _ in 0..200 {
            if scene.refresh_lighting(16_384).unwrap() {
                return;
            }
        }
        panic!("derived light did not finish");
    }
    fn block(scene: &Scene, pos: BlockPos) -> RenderBlock {
        let view = scene.lighting.as_ref().unwrap().view(scene);
        let MeshSample::Ready(block) = sample_visual(scene, Some(&view), pos) else {
            panic!("loaded render sample");
        };
        block
    }
    #[test]
    fn emission_and_sky_reach_vertex_brightness_and_source_removal_rebuilds() {
        let mut scene = scene();
        let stone = scene
            .authority
            .registry()
            .default_state(&key("base:stone"))
            .unwrap();
        let glass = scene
            .authority
            .registry()
            .default_state(&key("base:glass"))
            .unwrap();
        for x in -16..48 {
            for z in -16..32 {
                scene
                    .authority
                    .store_mut()
                    .edit(BlockPos::new(x, 12, z), stone)
                    .unwrap();
            }
        }
        let source = BlockPos::new(8, 8, 8);
        let wall = BlockPos::new(9, 8, 8);
        scene.authority.store_mut().edit(source, glass).unwrap();
        scene.authority.store_mut().edit(wall, stone).unwrap();
        scene.enable_lighting().unwrap();
        resolve(&mut scene);
        let before = block(&scene, wall).face_shades.unwrap()[0];
        assert!(before.iter().all(|v| *v > 200));
        scene.refresh_meshes();
        let stamp = MeshStamp::capture(&scene, SectionPos::ZERO);
        scene
            .authority
            .store_mut()
            .edit(source, BlockStateId::EMPTY)
            .unwrap();
        resolve(&mut scene);
        assert_ne!(stamp, MeshStamp::capture(&scene, SectionPos::ZERO));
        let after = block(&scene, wall).face_shades.unwrap()[0];
        assert!(after.iter().all(|v| *v < 40));
        scene.refresh_meshes();
        assert!(
            scene
                .geometry()
                .0
                .iter()
                .any(|v| v.light >= 0.0 && v.light < 0.2)
        );
    }
    #[test]
    fn ambient_occlusion_shared_corners_agree_across_section_edges() {
        let mut scene = scene();
        let stone = scene
            .authority
            .registry()
            .default_state(&key("base:stone"))
            .unwrap();
        for p in [
            BlockPos::new(15, 7, 8),
            BlockPos::new(16, 7, 8),
            BlockPos::new(15, 8, 9),
        ] {
            scene.authority.store_mut().edit(p, stone).unwrap();
        }
        scene.enable_lighting().unwrap();
        resolve(&mut scene);
        let left = block(&scene, BlockPos::new(15, 7, 8)).face_shades.unwrap()[3];
        let right = block(&scene, BlockPos::new(16, 7, 8)).face_shades.unwrap()[3];
        assert_eq!(left[1], right[0]);
        assert_eq!(left[2], right[3]);
        assert!(left[1] < left[2]);
        assert!(left.iter().any(|value| *value != left[0]));
    }
    #[test]
    fn lighting_pressure_retries_stale_work_is_dropped_and_lifetimes_release() {
        let mut scene = scene();
        let pool = MemoryPool::new(torse_types::MemoryBudget::new(128 * 1024 * 1024));
        scene.attach_memory(pool.clone()).unwrap();
        scene.enable_lighting().unwrap();
        let held = pool
            .reserve(
                MemoryClass::Resource,
                pool.budget().total_bytes() - pool.usage().total - 1,
                EvictionRank::Pinned,
            )
            .unwrap();
        assert!(!scene.refresh_lighting(1).unwrap());
        assert!(scene.lighting.as_ref().unwrap().pending.is_none());
        drop(held);
        scene.refresh_lighting(1).unwrap();
        assert!(scene.lighting.as_ref().unwrap().pending.is_some());
        let stone = scene
            .authority
            .registry()
            .default_state(&key("base:stone"))
            .unwrap();
        scene
            .authority
            .store_mut()
            .edit(BlockPos::new(8, 8, 8), stone)
            .unwrap();
        scene.refresh_lighting(1).unwrap();
        assert!(scene.lighting.as_ref().unwrap().pending.is_none());
        resolve(&mut scene);
        scene.epoch = WorldEpoch(999);
        resolve(&mut scene);
        drop(scene);
        assert_eq!(pool.usage().total, 0);
    }

    #[test]
    fn publishing_distant_light_does_not_invalidate_an_unrelated_mesh() {
        let mut scene = scene();
        scene.enable_lighting().unwrap();
        resolve(&mut scene);
        let before = MeshStamp::capture(&scene, SectionPos::ZERO);
        for x in 7..=9 {
            for z in -1..=1 {
                assert!(
                    scene
                        .install_generated(SectionPos::new(x, 0, z), Section::empty(16).unwrap())
                        .unwrap()
                );
            }
        }
        scene.sections.push(SectionPos::new(8, 0, 0));
        resolve(&mut scene);
        assert_eq!(before, MeshStamp::capture(&scene, SectionPos::ZERO));
    }
}
