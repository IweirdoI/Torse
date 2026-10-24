//! Terrain scene for the client: load, generate, mesh, and expose vertices.
//!
//! The scene owns an authoritative world through [`WorldAuthority`]. Sections in
//! a square radius are generated deterministically and meshed once per terrain
//! revision. A mesh is rebuilt only when its section or one of its neighbours
//! changed, which is the rendering half of the revision contract the mesher
//! already enforces.

use std::collections::BTreeMap;

use torse_content::{BlockDefinition, Shape, compile, m0_fixture_pack};
use torse_render::{MeshData, MeshJob, MeshSample, RenderBlock, RenderBox, build_mesh};
use torse_sim::WorldAuthority;
use torse_sim::action::{ActionOrigin, ActionRequest, ActionService, ActionTarget, ActorContext};
use torse_sim::{OrderingKey, Phase, block_actions};
use torse_types::{
    BlockPos, Compatibility, ContentKey, Digest, Revision, SectionPos, TransactionId, WorldId,
};
use torse_world::{
    BlockStateId, BlockStateRegistry, ComponentFields, ComponentSchemas, ComponentValue,
    TerrainQuery, WorldBounds,
};
use torse_world_services::{GenerationParams, HillTerrain};

use crate::pick::{PickOutcome, cast_ray};

/// One vertex uploaded to the GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    /// World-space position.
    pub position: [f32; 3],
    /// Face normal.
    pub normal: [f32; 3],
    /// Flat colour for the material.
    pub color: [f32; 3],
}

impl Vertex {
    /// Bytes per interleaved vertex.
    pub const STRIDE: usize = 36;

    fn write(&self, out: &mut Vec<u8>) {
        for value in self
            .position
            .iter()
            .chain(self.normal.iter())
            .chain(self.color.iter())
        {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
}

/// Terrain scene bound to one authoritative world.
pub struct Scene {
    authority: WorldAuthority,
    service: ActionService,
    principal: ContentKey,
    edit_counter: u64,
    generator: HillTerrain,
    side: u32,
    radius: i64,
    meshes: BTreeMap<SectionPos, (Revision, MeshData)>,
    sections: Vec<SectionPos>,
}

impl Scene {
    /// Build a scene with a seeded hill terrain.
    pub fn new(radius: i64) -> Result<Self, String> {
        let mut pack = m0_fixture_pack();
        block_actions::register_content(&mut pack);
        let content = compile(pack).map_err(|errors| format!("content failed: {errors:?}"))?;
        let registry = BlockStateRegistry::build(&content)
            .map_err(|error| format!("registry failed: {error}"))?;
        let schemas = ComponentSchemas::build(&content);
        let stone = registry
            .default_state(&key("base:stone"))
            .ok_or_else(|| "missing base:stone".to_owned())?;
        let side = 16;
        let bounds = WorldBounds::new(0, 64).map_err(|error| error.to_string())?;
        let service = ActionService::bind(
            block_actions::descriptors(),
            block_actions::handlers(),
            &schemas,
        )
        .map_err(|error| format!("actions failed: {error}"))?;
        let authority = WorldAuthority::new(
            WorldId(1),
            registry,
            schemas,
            side,
            bounds,
            Compatibility {
                authority_schema: 1,
                rules_digest: Digest([1; 32]),
                content_digest: content.presentation_digest,
            },
        )
        .map_err(|error| error.to_string())?;
        let params = GenerationParams::new(side, bounds, stone, BlockStateId::EMPTY)
            .map_err(|error| error.to_string())?;
        let generator = HillTerrain::new(params, 7, 7, 9);
        let mut scene = Self {
            authority,
            service,
            principal: key("client:player"),
            edit_counter: 0,
            generator,
            side,
            radius,
            meshes: BTreeMap::new(),
            sections: Vec::new(),
        };
        scene.load_around(SectionPos::ZERO)?;
        Ok(scene)
    }

    /// Authoritative world.
    pub const fn authority(&self) -> &WorldAuthority {
        &self.authority
    }

    /// Classify one cell for picking: solid, empty, or outside the loaded world.
    fn solid_at(&self, pos: BlockPos) -> Option<bool> {
        match self.authority.store().query(pos) {
            TerrainQuery::Ready(state) => Some(!state.is_empty()),
            TerrainQuery::NotLoaded | TerrainQuery::OutOfBounds => None,
        }
    }

    /// Cast a ray through loaded terrain and report the first solid cell.
    pub fn pick(&self, origin: [f32; 3], direction: [f32; 3], max_distance: f32) -> PickOutcome {
        cast_ray(|pos| self.solid_at(pos), origin, direction, max_distance)
    }

    /// Plan and commit one place or break action through the action service.
    ///
    /// The windowed client is a trusted local host: it grants the player the
    /// edit permission and the single target cell for this request, then lets
    /// the service authorize, validate, and commit like any other adapter.
    /// The error string names the rejection, it is diagnostics only.
    pub fn apply_edit(
        &mut self,
        place: bool,
        block: &ContentKey,
        target: BlockPos,
    ) -> Result<(), String> {
        self.edit_counter += 1;
        let counter = self.edit_counter;
        let action = if place {
            key("world:action/place")
        } else {
            key("world:action/break")
        };
        let mut parameters = ComponentFields::new();
        if place {
            parameters.insert(
                key("world:field/block"),
                ComponentValue::Content(block.clone()),
            );
        }
        let request = ActionRequest {
            action,
            api_version: 1,
            origin: ActionOrigin::Ui,
            target: ActionTarget::Block(target),
            parameters,
            transaction: TransactionId(u128::from(counter)),
            ordering: OrderingKey {
                phase: Phase::Interaction,
                rule_priority: 0,
                stable_task_key: counter,
                local_sequence: 0,
            },
        };
        let actor = ActorContext::new(self.principal.clone())
            .permission(key("world:permission/edit"))
            .target(ActionTarget::Block(target));
        let output = self
            .service
            .plan(&self.authority, &actor, Phase::Interaction, &request)
            .map_err(|code| format!("{code:?}"))?;
        let torse_sim::action::ActionOutput::Mutation(transaction) = output else {
            return Err("place and break must be mutations".to_owned());
        };
        self.authority
            .commit(&transaction)
            .map_err(|fault| format!("{fault:?}"))?;
        Ok(())
    }

    /// Loaded section coordinates.
    pub fn sections(&self) -> &[SectionPos] {
        &self.sections
    }

    /// Number of cached meshes.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Load sections inside the radius around a centre and unload the rest.
    pub fn load_around(&mut self, center: SectionPos) -> Result<(), String> {
        let mut wanted = Vec::new();
        for x in -self.radius..=self.radius {
            for z in -self.radius..=self.radius {
                wanted.push(SectionPos::new(center.x + x, 0, center.z + z));
            }
        }

        for pos in &wanted {
            if self.authority.store().phase(*pos).is_none() {
                let store = self.authority.store_mut();
                store.request(*pos).map_err(|error| error.to_string())?;
                let section = self
                    .generator
                    .generate(*pos)
                    .map_err(|error| error.to_string())?;
                store
                    .install(*pos, section)
                    .map_err(|error| error.to_string())?;
                store.finish_load(*pos).map_err(|error| error.to_string())?;
            }
        }

        let wanted_set = wanted.clone();
        for pos in self.sections.clone() {
            if !wanted_set.contains(&pos) {
                let store = self.authority.store_mut();
                let _ = store.unload(pos);
                self.meshes.remove(&pos);
            }
        }
        self.sections = wanted;
        Ok(())
    }

    /// Rebuild meshes whose section or neighbour revision changed.
    ///
    /// Returns how many meshes were rebuilt, so callers can skip a GPU upload
    /// when nothing changed.
    pub fn refresh_meshes(&mut self) -> usize {
        let store = self.authority.store();
        let registry = self.authority.registry();
        let mut rebuilt = 0usize;
        for pos in &self.sections {
            let job = MeshJob::capture(self.side, *pos, |query| store.revision(query));
            let Some(revision) = job.section_revision else {
                continue;
            };
            let unchanged = self
                .meshes
                .get(pos)
                .is_some_and(|(cached, _)| *cached == revision)
                && job.validate(|query| store.revision(query));
            if unchanged {
                continue;
            }
            let mesh = build_mesh(self.side, *pos, |query| sample(store, registry, query));
            self.meshes.insert(*pos, (revision, mesh));
            rebuilt += 1;
        }
        rebuilt
    }

    /// Section containing a world-space position on the section layer.
    pub fn center_section(&self, position: [f32; 3]) -> SectionPos {
        SectionPos::new(
            (position[0] as i64).div_euclid(self.side as i64),
            0,
            (position[2] as i64).div_euclid(self.side as i64),
        )
    }

    /// Flatten every cached mesh into one interleaved vertex and index buffer.
    pub fn geometry(&self) -> (Vec<Vertex>, Vec<u32>, usize) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut faces = 0usize;
        for (pos, (_, mesh)) in &self.meshes {
            let offset = [
                pos.x as f32 * self.side as f32,
                0.0,
                pos.z as f32 * self.side as f32,
            ];
            let base = vertices.len() as u32;
            for (index, position) in mesh.positions.iter().enumerate() {
                let material = mesh.materials[index / 4];
                vertices.push(Vertex {
                    position: [
                        position[0] + offset[0],
                        position[1] + offset[1],
                        position[2] + offset[2],
                    ],
                    normal: mesh.normals[index],
                    color: material_color(self.authority.registry(), material),
                });
            }
            indices.extend(mesh.indices.iter().map(|value| base + value));
            faces += mesh.face_count();
        }
        (vertices, indices, faces)
    }

    /// Encode vertices into the interleaved GPU layout.
    pub fn vertex_bytes(vertices: &[Vertex]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(vertices.len() * Vertex::STRIDE);
        for vertex in vertices {
            vertex.write(&mut bytes);
        }
        bytes
    }

    /// Camera position standing on the terrain at the scene centre.
    pub fn eye(&self) -> [f32; 3] {
        let x = 8;
        let z = 8;
        for y in (0..64).rev() {
            if let TerrainQuery::Ready(state) = self.authority.store().query(BlockPos::new(x, y, z))
                && !state.is_empty()
            {
                return [x as f32 + 0.5, y as f32 + 2.4, z as f32 + 0.5];
            }
        }
        [8.5, 12.0, 8.5]
    }
}

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("fixture key is valid")
}

fn sample(
    store: &torse_world::SectionStore,
    registry: &BlockStateRegistry,
    pos: BlockPos,
) -> MeshSample {
    match store.query(pos) {
        TerrainQuery::Ready(state) => {
            if state.is_empty() {
                return MeshSample::Ready(empty_block());
            }
            let Some(definition) = registry.definition(state) else {
                return MeshSample::Ready(empty_block());
            };
            MeshSample::Ready(RenderBlock {
                bounds: render_box(definition),
                occlusion: definition.occlusion_profile.face_mask,
                material: state.0,
            })
        }
        TerrainQuery::NotLoaded => MeshSample::NotLoaded,
        TerrainQuery::OutOfBounds => MeshSample::OutOfBounds,
    }
}

const fn empty_block() -> RenderBlock {
    RenderBlock {
        bounds: None,
        occlusion: 0,
        material: 0,
    }
}

fn render_box(definition: &BlockDefinition) -> Option<RenderBox> {
    match &definition.render_profile.shape {
        Shape::FullCube => Some(RenderBox::FULL_CUBE),
        Shape::LowerSlab => Some(RenderBox {
            min: [0.0, 0.0, 0.0],
            max: [1.0, 0.5, 1.0],
        }),
        Shape::Panel => Some(RenderBox {
            min: [0.45, 0.0, 0.0],
            max: [0.55, 1.0, 1.0],
        }),
        Shape::Corner | Shape::Stair => Some(RenderBox::FULL_CUBE),
        Shape::Box(bounds) => Some(RenderBox {
            min: bounds.min,
            max: bounds.max,
        }),
    }
}

fn material_color(registry: &BlockStateRegistry, material: u16) -> [f32; 3] {
    let Some(path) = registry
        .definition_key(BlockStateId(material))
        .map(|key| key.path())
    else {
        return [0.70, 0.70, 0.70];
    };
    match path {
        "stone" => [0.62, 0.61, 0.58],
        "glass_slab" => [0.62, 0.78, 0.82],
        "plant" => [0.36, 0.55, 0.32],
        _ => [0.70, 0.70, 0.70],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_generates_and_meshes_its_radius() {
        let mut scene = Scene::new(1).expect("scene builds");
        assert_eq!(scene.sections().len(), 9);
        assert_eq!(scene.refresh_meshes(), 9);
        assert_eq!(scene.mesh_count(), 9);
        // Nothing changed, so the second pass rebuilds nothing.
        assert_eq!(scene.refresh_meshes(), 0);
        let (vertices, indices, faces) = scene.geometry();
        assert!(!vertices.is_empty());
        assert!(!indices.is_empty());
        assert!(faces > 0);
        assert_eq!(indices.len() % 3, 0);
    }

    #[test]
    fn streaming_recenters_without_leaking_sections() {
        let mut scene = Scene::new(1).expect("scene builds");
        scene.refresh_meshes();
        let before = scene.geometry();
        assert!(!before.0.is_empty());

        scene.load_around(SectionPos::new(3, 0, 0)).expect("load");
        assert_eq!(scene.sections().len(), 9);
        // The unloaded column's meshes are gone; the new ones are built.
        assert!(scene.refresh_meshes() > 0);
        assert_eq!(scene.mesh_count(), 9);
        let after = scene.geometry();
        assert!(!after.0.is_empty());

        // Returning to the origin reloads the same deterministic terrain.
        scene.load_around(SectionPos::ZERO).expect("load back");
        scene.refresh_meshes();
        assert_eq!(scene.sections().len(), 9);
        assert_eq!(scene.mesh_count(), 9);
        assert_eq!(scene.geometry().0.len(), before.0.len());
    }

    #[test]
    fn center_section_maps_positions_including_negatives() {
        let scene = Scene::new(0).expect("scene builds");
        assert_eq!(scene.center_section([0.0, 5.0, 0.0]), SectionPos::ZERO);
        assert_eq!(
            scene.center_section([17.5, 5.0, -1.0]),
            SectionPos::new(1, 0, -1)
        );
        assert_eq!(
            scene.center_section([-16.0, 5.0, -17.0]),
            SectionPos::new(-1, 0, -2)
        );
    }

    #[test]
    fn vertex_bytes_match_the_documented_stride() {
        let vertices = [Vertex {
            position: [1.0, 2.0, 3.0],
            normal: [0.0, 1.0, 0.0],
            color: [0.5, 0.5, 0.5],
        }];
        let bytes = Scene::vertex_bytes(&vertices);
        assert_eq!(bytes.len(), Vertex::STRIDE);
    }

    fn query(scene: &Scene, pos: BlockPos) -> bool {
        matches!(
            scene.authority().store().query(pos),
            TerrainQuery::Ready(state) if !state.is_empty()
        )
    }

    /// Find a column whose surface leaves a loaded, empty cell above it.
    /// Only the `y = 0` section layer is loaded, so test cells must stay below
    /// 16.
    fn air_above_surface(scene: &Scene) -> BlockPos {
        for x in 0..12i64 {
            for z in 0..12i64 {
                for y in (0..14i64).rev() {
                    let pos = BlockPos::new(x, y, z);
                    if query(scene, pos) {
                        return BlockPos::new(x, y + 1, z);
                    }
                }
            }
        }
        panic!("no air cell found above the surface");
    }

    #[test]
    fn apply_edit_places_and_breaks_through_the_action_service() {
        let mut scene = Scene::new(1).expect("scene builds");
        let target = air_above_surface(&scene);
        let surface = BlockPos::new(target.x, target.y - 1, target.z);
        assert!(!query(&scene, target));
        assert!(query(&scene, surface));

        let stone = key("base:stone");
        scene
            .apply_edit(true, &stone, target)
            .expect("place commits");
        assert!(query(&scene, target));

        // Placing into the now occupied cell is rejected without a commit.
        let again = scene.apply_edit(true, &stone, target);
        assert!(again.is_err());

        scene
            .apply_edit(false, &stone, target)
            .expect("break commits");
        assert!(!query(&scene, target));

        // Breaking air is a conflict, not a silent no-op.
        let empty = scene.apply_edit(false, &stone, target);
        assert!(empty.is_err());
    }

    #[test]
    fn apply_edit_rejects_out_of_bounds_targets() {
        let mut scene = Scene::new(1).expect("scene builds");
        let stone = key("base:stone");
        assert!(
            scene
                .apply_edit(true, &stone, BlockPos::new(0, 64, 0))
                .is_err()
        );
        assert!(
            scene
                .apply_edit(true, &stone, BlockPos::new(0, -1, 0))
                .is_err()
        );
        // Above the loaded layer the target is not loaded, which is also a
        // rejection rather than a silent drop.
        assert!(
            scene
                .apply_edit(true, &stone, BlockPos::new(0, 40, 0))
                .is_err()
        );
    }

    #[test]
    fn pick_finds_the_surface_below_the_camera() {
        let scene = Scene::new(1).expect("scene builds");
        // Start inside the loaded layer so the ray can walk down to the hill.
        let outcome = scene.pick([8.5, 15.5, 8.5], [0.0, -1.0, 0.0], 128.0);
        let PickOutcome::Hit(hit) = outcome else {
            panic!("expected the terrain surface");
        };
        assert_eq!((hit.block.x, hit.block.z), (8, 8));
        assert!(hit.block.y < 16);
        assert!(hit.distance < 16.0);
    }

    #[test]
    fn pick_stops_at_unloaded_sections() {
        let scene = Scene::new(1).expect("scene builds");
        // Radius 1 leaves everything past 31 blocks unloaded on the x axis.
        let outcome = scene.pick([30.5, 40.0, 8.5], [1.0, 0.0, 0.0], 256.0);
        assert_eq!(outcome, PickOutcome::Miss);
    }
}
