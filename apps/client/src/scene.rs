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
use torse_sim::{OrderingKey, Phase, Transaction, block_actions};
use torse_types::{
    BlockPos, Compatibility, ContentKey, Digest, EntityHandle, Revision, SectionPos, TransactionId,
    TransactionOutcome, WorldId,
};
use torse_world::{
    BlockStateId, BlockStateRegistry, ComponentFields, ComponentSchemas, ComponentValue, Position,
    TerrainQuery, WorldBounds,
};
use torse_world_services::{GenerationParams, HillTerrain};

use crate::pick::{PickOutcome, cast_ray};

/// Walk speed in blocks per second.
const WALK_SPEED: f64 = 4.3;
/// Gravity in blocks per second squared.
const GRAVITY: f64 = 28.0;
/// Jump launch speed in blocks per second.
const JUMP_SPEED: f64 = 8.4;
/// Player collider half extents: 0.6 wide, 1.8 tall, centre at body middle.
const WALKER_HALF: [f64; 3] = [0.3, 0.9, 0.3];

/// Client-side walk state for the player entity.
#[derive(Clone, Copy)]
struct Walker {
    handle: EntityHandle,
    velocity: [f64; 3],
    grounded: bool,
}

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
    walker: Option<Walker>,
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
            walker: None,
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

    /// Spawn the walk-mode player entity at a world position.
    ///
    /// The windowed client is a trusted local host, so it installs the player
    /// directly through the entity store the same way the server bootstrap
    /// installs its fixtures; movement still goes through transactions.
    pub fn spawn_walker(&mut self, at: Position) -> Result<(), String> {
        let handle = self
            .authority
            .entities_mut()
            .spawn(at)
            .map_err(|error| format!("spawn failed: {error}"))?;
        self.walker = Some(Walker {
            handle,
            velocity: [0.0; 3],
            grounded: false,
        });
        Ok(())
    }

    /// Walker position, if walk mode has a live entity.
    pub fn walker_position(&self) -> Option<Position> {
        let walker = self.walker?;
        self.authority
            .entities()
            .get(walker.handle)
            .map(|entity| entity.position)
    }

    /// Integrate one walk tick and commit the displacement as an A4 movement
    /// transaction; the authority resolves terrain collision in `prepare`.
    ///
    /// `wish` is a normalized horizontal direction in world units, `jump`
    /// requests a jump while grounded, and `dt` is the fixed tick length.
    /// Rejections (a stale observation, for instance) drop the tick and keep
    /// the entity where it is; the caller sees the unchanged position.
    pub fn step_walker(
        &mut self,
        wish: [f64; 3],
        jump: bool,
        dt: f32,
    ) -> Result<(Position, bool), String> {
        let Some(mut walker) = self.walker else {
            return Err("walk mode has no entity".to_owned());
        };
        let Some(entity) = self.authority.entities().get(walker.handle) else {
            return Err("walker entity is gone".to_owned());
        };
        let (id, previous) = (entity.id, entity.position);

        // Client-side integration in blocks per second: horizontal velocity
        // follows the input directly, gravity integrates downward, and jumps
        // are only possible while standing on something. The committed delta
        // scales by the tick length.
        walker.velocity[0] = wish[0] * WALK_SPEED;
        walker.velocity[2] = wish[2] * WALK_SPEED;
        walker.velocity[1] -= GRAVITY * f64::from(dt);
        if walker.grounded && jump {
            walker.velocity[1] = JUMP_SPEED;
        }
        let dt = f64::from(dt);
        let requested = previous.translated([
            walker.velocity[0] * dt,
            walker.velocity[1] * dt,
            walker.velocity[2] * dt,
        ]);
        self.walker = Some(walker);

        self.edit_counter += 1;
        let counter = self.edit_counter;
        let mut tx = Transaction::new(
            TransactionId(u128::from(counter)),
            key("client:motion"),
            OrderingKey {
                phase: Phase::Simulation,
                rule_priority: 0,
                stable_task_key: counter,
                local_sequence: 0,
            },
        );
        tx.move_entity(
            self.walker.as_ref().expect("walker stored").handle,
            id,
            previous,
            [
                requested.x - previous.x,
                requested.y - previous.y,
                requested.z - previous.z,
            ],
            WALKER_HALF,
        );
        let result = self.authority.commit(&tx);

        let Some(walker) = self.walker.as_mut() else {
            return Err("walker vanished mid step".to_owned());
        };
        let Some(entity) = self.authority.entities().get(walker.handle) else {
            return Err("walker entity is gone".to_owned());
        };
        if let Ok(outcome) = &result
            && let TransactionOutcome::Rejected(code) = outcome.outcome
        {
            // Drop this tick: keep the entity at its committed position with
            // the horizontal velocity already reset for the next tick.
            walker.velocity[1] = 0.0;
            let _ = code;
            return Ok((entity.position, walker.grounded));
        }
        result.map_err(|fault| format!("commit fault: {fault:?}"))?;

        // Compare the committed position against the request to recover the
        // ground contact the collision sweep discovered. Resting above the
        // request means the descent was blocked: that is the floor. Ending
        // below the request means a ceiling stopped the rise.
        let settled = entity.position;
        if settled.y - requested.y > 1e-9 {
            walker.grounded = true;
            walker.velocity[1] = 0.0;
        } else if requested.y - settled.y > 1e-9 {
            walker.velocity[1] = 0.0;
            walker.grounded = false;
        } else {
            walker.grounded = false;
        }
        Ok((settled, walker.grounded))
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

    #[test]
    fn walker_spawns_mid_air_walks_and_ends_standing() {
        let mut scene = Scene::new(3).expect("scene");
        scene
            .spawn_walker(Position::new(8.5, 13.3, 8.5))
            .expect("spawn");
        let mut final_position = Position::new(8.5, 13.3, 8.5);
        let mut grounded = false;
        for _ in 0..31 {
            let (position, landed) = scene
                .step_walker([0.0, 0.0, -1.0], false, 0.05)
                .expect("step");
            final_position = position;
            grounded = landed;
        }
        // Walked into negative z and ended standing on the terrain.
        assert!(final_position.z < 8.0, "moved to {final_position:?}");
        assert!(grounded);
        assert!(final_position.y > 0.0 && final_position.y < 16.0);
    }

    #[test]
    fn walker_falls_until_it_lands_and_stays() {
        let mut scene = Scene::new(1).expect("scene builds");
        scene
            .spawn_walker(Position::new(8.5, 12.0, 8.5))
            .expect("spawn");
        let mut settled = Position::new(8.5, 12.0, 8.5);
        for _ in 0..100 {
            let (position, grounded) = scene
                .step_walker([0.0, 0.0, 0.0], false, 0.05)
                .expect("step");
            settled = position;
            if grounded {
                break;
            }
        }
        let (_, grounded) = scene
            .step_walker([0.0, 0.0, 0.0], false, 0.05)
            .expect("settle");
        assert!(grounded, "must land within five seconds");
        // Standing on the surface: inside the loaded layer, x and z unchanged.
        assert!((settled.x - 8.5).abs() < 1e-9);
        assert!((settled.z - 8.5).abs() < 1e-9);
        assert!(settled.y > 0.0 && settled.y < 16.0);
    }

    #[test]
    fn walker_walks_forward_over_rolling_terrain() {
        let mut scene = Scene::new(1).expect("scene builds");
        scene
            .spawn_walker(Position::new(8.5, 12.0, 8.5))
            .expect("spawn");
        // Warm up until grounded.
        for _ in 0..100 {
            let (_, grounded) = scene
                .step_walker([0.0, 0.0, 0.0], false, 0.05)
                .expect("step");
            if grounded {
                break;
            }
        }
        let start = scene.walker_position().expect("walker");
        let mut final_position = start;
        for _ in 0..60 {
            final_position = scene
                .step_walker([0.0, 0.0, -1.0], false, 0.05)
                .expect("step")
                .0;
        }
        // Three seconds of walking moves along the surface until a terrain
        // step blocks the collider; hills stop it early, so only require real
        // progress plus a plausible standing height.
        assert!(
            start.z - final_position.z > 2.0,
            "moved {:?} to {:?}",
            start,
            final_position
        );
        assert!(final_position.y > 0.0 && final_position.y < 16.0);
        assert!((final_position.x - start.x).abs() < 1e-9);
    }

    #[test]
    fn walker_jumps_and_lands_back() {
        let mut scene = Scene::new(1).expect("scene builds");
        scene
            .spawn_walker(Position::new(8.5, 12.0, 8.5))
            .expect("spawn");
        for _ in 0..100 {
            let (_, grounded) = scene
                .step_walker([0.0, 0.0, 0.0], false, 0.05)
                .expect("step");
            if grounded {
                break;
            }
        }
        let ground = scene.walker_position().expect("walker").y;

        let (risen, grounded) = scene
            .step_walker([0.0, 0.0, 0.0], true, 0.05)
            .expect("jump tick");
        assert!(!grounded);
        assert!(risen.y > ground, "first jump tick must rise");

        let mut landed = risen;
        let mut on_ground = false;
        for _ in 0..100 {
            let (position, grounded) = scene
                .step_walker([0.0, 0.0, 0.0], false, 0.05)
                .expect("fall");
            landed = position;
            on_ground = grounded;
            if grounded {
                break;
            }
        }
        assert!(on_ground, "must land again");
        assert!((landed.y - ground).abs() < 0.5, "back at {landed:?}");
    }
}
