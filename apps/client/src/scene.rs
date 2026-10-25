//! Terrain scene for the client: load, generate, mesh, and expose vertices.
//!
//! The scene owns an authoritative world through [`WorldAuthority`]. Sections in
//! a square radius are generated deterministically and meshed once per terrain
//! revision. A mesh is rebuilt only when its section or one of its neighbours
//! changed, which is the rendering half of the revision contract the mesher
//! already enforces.

use std::collections::BTreeMap;

use torse_content::{BlockDefinition, Face, Shape, compile, m0_fixture_pack};
use torse_render::{MeshData, MeshSample, RenderBlock, RenderBox, build_mesh};
use torse_sim::WorldAuthority;
use torse_sim::action::{ActionOrigin, ActionRequest, ActionService, ActionTarget, ActorContext};
use torse_sim::replica::{ReplicaChanges, ReplicaEntity, ReplicaSnapshot};
use torse_sim::{OrderingKey, Phase, block_actions};
use torse_types::{
    BlockPos, Compatibility, ContentKey, LocalPos, SectionPos, TransactionId, WorldEpoch, WorldId,
};
use torse_world::{
    BlockStateId, BlockStateRegistry, ComponentFields, ComponentSchemas, ComponentSet,
    ComponentValue, Position, Section, SnapshotCells, SnapshotSection, StateEntry, TerrainQuery,
    WorldBounds,
};
use torse_world_services::{GenerationParams, HillTerrain};

use crate::pick::{PickHit, PickOutcome, cast_ray};
use crate::texture::{Palette, TexturePack};

mod background;
pub use background::BackgroundStats;
use background::{Background, MeshStamp};

pub fn background_config() -> torse_runtime::ExecutorConfig {
    background::default_config()
}

fn scene_epoch() -> Result<WorldEpoch, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        .map(|n| WorldEpoch(u128::from(n)))
        .map_err(|_| "scene epoch exhausted".to_owned())
}

/// One vertex uploaded to the GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    /// World-space position.
    pub position: [f32; 3],
    /// Face normal.
    pub normal: [f32; 3],
    /// Local texture coordinates within the face.
    pub uv: [f32; 2],
    /// Material slot looked up in the client material table.
    pub material: u32,
}

impl Vertex {
    /// Bytes per interleaved vertex.
    pub const STRIDE: usize = 36;

    fn write(&self, out: &mut Vec<u8>) {
        for value in self
            .position
            .iter()
            .chain(self.normal.iter())
            .chain(self.uv.iter())
        {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&self.material.to_le_bytes());
    }
}

/// Terrain scene bound to one authoritative world.
pub struct Scene {
    authority: WorldAuthority,
    service: ActionService,
    principal: ContentKey,
    edit_counter: u64,
    walker: Option<torse_sim::player::WalkerState>,
    generator: Option<HillTerrain>,
    side: u32,
    radius: i64,
    /// Cached mesh plus the dependency revisions it was built against;
    /// a neighbour edit is detected by validating this stored job.
    meshes: BTreeMap<SectionPos, (MeshStamp, MeshData)>,
    sections: Vec<SectionPos>,
    epoch: WorldEpoch,
    asset_generation: u64,
    next_load_generation: u64,
    load_generations: BTreeMap<SectionPos, u64>,
    background: Option<Background>,
    palette: Palette,
    /// Material slot per block state and face.
    state_materials: BTreeMap<BlockStateId, [u16; 6]>,
}

/// One content pack for every client mode, matching the server demo assembly
/// so the handshake compatibility and the block colours agree everywhere.
fn content_pack() -> torse_content::ContentPack {
    let mut pack = m0_fixture_pack();
    block_actions::register_content(&mut pack);
    torse_inventory::register_content(&mut pack);
    pack
}

/// Build the registry, schemas, and handshake compatibility from the shared pack.
fn compile_content() -> Result<(BlockStateRegistry, ComponentSchemas, Compatibility), String> {
    let content =
        compile(content_pack()).map_err(|errors| format!("content failed: {errors:?}"))?;
    let registry =
        BlockStateRegistry::build(&content).map_err(|error| format!("registry failed: {error}"))?;
    let schemas = ComponentSchemas::build(&content);
    let compatibility = Compatibility {
        authority_schema: 4,
        rules_digest: content.authority_digest,
        content_digest: content.presentation_digest,
    };
    Ok((registry, schemas, compatibility))
}

/// Build the palette and the per-state material slots.
///
/// Every block state resolves six material slots once at startup; meshing then
/// copies the slots into each render block without touching content keys.
fn material_tables(
    registry: &BlockStateRegistry,
    pack: &TexturePack,
) -> Result<(Palette, BTreeMap<BlockStateId, [u16; 6]>), String> {
    let palette = Palette::build(registry, pack)?;
    let mut states = BTreeMap::new();
    for index in 0..registry.state_count() {
        let state = BlockStateId(index as u16);
        let Some(definition) = registry.definition(state) else {
            continue;
        };
        let profile = &definition.render_profile;
        let mut slots = [0u16; 6];
        for face in Face::ALL {
            let material = profile.face_materials.resolve(face, &profile.material);
            slots[face.index()] = palette
                .slot(material)
                .ok_or_else(|| format!("material `{material}` is missing from the palette"))?;
        }
        states.insert(state, slots);
    }
    Ok((palette, states))
}

impl Scene {
    /// Build a scene with a seeded hill terrain and the base game texture pack.
    pub fn new(radius: i64) -> Result<Self, String> {
        Self::with_pack(radius, crate::texture::base_pack()?)
    }

    /// Build a scene from one authorized replica baseline.
    ///
    /// The network client has no generator: sections and entities come from
    /// the server projection only, and `load_around` keeps the subscribed
    /// section list as-is.
    pub fn from_snapshot(snapshot: &ReplicaSnapshot) -> Result<Self, String> {
        Self::from_snapshot_with_pack(snapshot, crate::texture::base_pack()?)
    }

    /// Build a scene with an explicit texture pack.
    pub fn with_pack(radius: i64, pack: TexturePack) -> Result<Self, String> {
        if !(0..=16).contains(&radius) {
            return Err("scene radius must be 0..=16".to_owned());
        }
        let (registry, schemas, compatibility) = compile_content()?;
        let (palette, state_materials) = material_tables(&registry, &pack)?;
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
        let authority =
            WorldAuthority::new(WorldId(1), registry, schemas, side, bounds, compatibility)
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
            generator: Some(generator),
            side,
            radius,
            meshes: BTreeMap::new(),
            sections: Vec::new(),
            epoch: scene_epoch()?,
            asset_generation: 1,
            next_load_generation: 1,
            load_generations: BTreeMap::new(),
            background: None,
            palette,
            state_materials,
        };
        scene.load_around(SectionPos::ZERO)?;
        Ok(scene)
    }

    /// Build a network scene with an explicit texture pack.
    pub fn from_snapshot_with_pack(
        snapshot: &ReplicaSnapshot,
        pack: TexturePack,
    ) -> Result<Self, String> {
        let (registry, schemas, compatibility) = compile_content()?;
        let (palette, state_materials) = material_tables(&registry, &pack)?;
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
            snapshot.side,
            snapshot.bounds,
            compatibility,
        )
        .map_err(|error| error.to_string())?;
        let mut scene = Self {
            authority,
            service,
            principal: key("client:player"),
            edit_counter: 0,
            walker: None,
            generator: None,
            side: snapshot.side,
            radius: 0,
            meshes: BTreeMap::new(),
            sections: Vec::new(),
            epoch: scene_epoch()?,
            asset_generation: 1,
            next_load_generation: 1,
            load_generations: BTreeMap::new(),
            background: None,
            palette,
            state_materials,
        };
        scene.install_snapshot(snapshot)?;
        Ok(scene)
    }

    /// Material palette used by the terrain shader.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Whether this scene mirrors a server projection instead of generating.
    pub const fn is_network(&self) -> bool {
        self.generator.is_none()
    }

    /// Handshake compatibility of the shared content pack.
    pub fn compatibility() -> Result<Compatibility, String> {
        Ok(compile_content()?.2)
    }

    /// Translate one replica baseline into local sections and entities.
    fn install_snapshot(&mut self, snapshot: &ReplicaSnapshot) -> Result<(), String> {
        let symbols = self.resolve_states(&snapshot.states)?;
        for section in &snapshot.sections {
            self.install_replica_section(section, &symbols)?;
        }
        self.sections = snapshot.sections.iter().map(|s| s.pos).collect();
        for entity in &snapshot.entities {
            self.install_replica_entity(entity)?;
        }
        Ok(())
    }

    /// Map the snapshot's runtime state table into local registry IDs.
    fn resolve_states(
        &self,
        states: &[StateEntry],
    ) -> Result<BTreeMap<BlockStateId, BlockStateId>, String> {
        let mut symbols = BTreeMap::new();
        symbols.insert(BlockStateId::EMPTY, BlockStateId::EMPTY);
        for entry in states {
            let local = registry_state(self.authority.registry(), &entry.block, &entry.values)?;
            symbols.insert(entry.id, local);
        }
        Ok(symbols)
    }

    /// Install or replace one section from replica cells.
    fn install_replica_section(
        &mut self,
        section: &SnapshotSection,
        symbols: &BTreeMap<BlockStateId, BlockStateId>,
    ) -> Result<(), String> {
        let side = self.side;
        let mut built = Section::new(side, BlockStateId::EMPTY)
            .map_err(|error| format!("section failed: {error}"))?;
        let write = |built: &mut Section, index: usize, state: BlockStateId| {
            let side = side as usize;
            let x = (index % side) as u32;
            let z = ((index / side) % side) as u32;
            let y = (index / (side * side)) as u32;
            built
                .set(LocalPos { x, y, z }, state)
                .map_err(|error| format!("cell failed: {error}"))
        };
        match &section.cells {
            SnapshotCells::Uniform(state) => {
                let local = *symbols.get(state).ok_or("unknown replica state")?;
                if !local.is_empty() {
                    // `Section::new` fills every cell; only real blocks need
                    // per-cell writes.
                    for index in 0..(side as usize).pow(3) {
                        write(&mut built, index, local)?;
                    }
                }
            }
            SnapshotCells::Dense(cells) => {
                if cells.len() != (side as usize).pow(3) {
                    return Err("dense section has the wrong length".to_owned());
                }
                for (index, state) in cells.iter().enumerate() {
                    let local = *symbols.get(state).ok_or("unknown replica state")?;
                    if !local.is_empty() {
                        write(&mut built, index, local)?;
                    }
                }
            }
        }
        let store = self.authority.store_mut();
        let _ = store.unload(section.pos);
        store
            .request(section.pos)
            .map_err(|error| error.to_string())?;
        store
            .install(section.pos, built)
            .map_err(|error| error.to_string())?;
        store
            .finish_load(section.pos)
            .map_err(|error| error.to_string())?;
        // A reinstalled section can reuse the previous revision number, so the
        // cached mesh must go or the change would render as a no-op.
        self.meshes.remove(&section.pos);
        self.assign_load_generation(section.pos)?;
        Ok(())
    }

    /// Insert or replace one entity from a replica record, keeping its
    /// persistent identity.
    fn install_replica_entity(&mut self, entity: &ReplicaEntity) -> Result<(), String> {
        if let Some(handle) = self.authority.entities().handle_of(entity.id) {
            let _ = self.authority.entities_mut().despawn(handle);
        }
        let position = Position::from_bits(entity.position_bits);
        let mut components = ComponentSet::new();
        let schemas = self.authority.schemas().clone();
        for (schema, fields) in &entity.components {
            components
                .insert(&schemas, schema, fields.clone())
                .map_err(|error| format!("component failed: {error}"))?;
        }
        self.authority
            .entities_mut()
            .insert_restored(entity.id, position, components)
            .map_err(|error| format!("entity failed: {error}"))?;
        Ok(())
    }

    /// Apply one replica increment: replaced sections and entities, removals.
    pub fn apply_changes(&mut self, changes: &ReplicaChanges) -> Result<(), String> {
        let symbols = self.resolve_states(&changes.states)?;
        for pos in &changes.removed_sections {
            let _ = self.authority.store_mut().unload(*pos);
            self.meshes.remove(pos);
            self.load_generations.remove(pos);
            self.sections.retain(|item| item != pos);
        }
        for section in &changes.sections {
            self.install_replica_section(section, &symbols)?;
            if !self.sections.contains(&section.pos) {
                self.sections.push(section.pos);
            }
        }
        for id in &changes.removed_entities {
            if let Some(handle) = self.authority.entities().handle_of(*id) {
                let _ = self.authority.entities_mut().despawn(handle);
            }
        }
        for entity in &changes.entities {
            self.install_replica_entity(entity)?;
        }
        Ok(())
    }

    /// Authoritative world.
    pub const fn authority(&self) -> &WorldAuthority {
        &self.authority
    }

    /// Shape of one cell for picking: empty, or the block's render bounds.
    /// `None` marks cells the ray must not traverse (unloaded or outside).
    fn shape_at(&self, pos: BlockPos) -> Option<Option<RenderBox>> {
        match self.authority.store().query(pos) {
            TerrainQuery::Ready(state) => {
                if state.is_empty() {
                    return Some(None);
                }
                let Some(definition) = self.authority.registry().definition(state) else {
                    return Some(None);
                };
                Some(render_box(definition))
            }
            TerrainQuery::NotLoaded | TerrainQuery::OutOfBounds => None,
        }
    }

    /// Build the selection outline edges for one hit, following the block's
    /// render bounds: a slab outlines its lower half, a plant its panel.
    pub fn outline_bytes(&self, hit: &PickHit) -> Vec<u8> {
        let state = match self.authority.store().query(hit.block) {
            TerrainQuery::Ready(state) if !state.is_empty() => state,
            _ => return Vec::new(),
        };
        let Some(definition) = self.authority.registry().definition(state) else {
            return Vec::new();
        };
        let bounds = render_box(definition).unwrap_or(RenderBox::FULL_CUBE);
        const INFLATE: f32 = 0.004;
        let min = [
            hit.block.x as f32 + bounds.min[0] - INFLATE,
            hit.block.y as f32 + bounds.min[1] - INFLATE,
            hit.block.z as f32 + bounds.min[2] - INFLATE,
        ];
        let max = [
            hit.block.x as f32 + bounds.max[0] + INFLATE,
            hit.block.y as f32 + bounds.max[1] + INFLATE,
            hit.block.z as f32 + bounds.max[2] + INFLATE,
        ];
        let corner = |bits: u32| {
            [
                if bits & 1 != 0 { max[0] } else { min[0] },
                if bits & 2 != 0 { max[1] } else { min[1] },
                if bits & 4 != 0 { max[2] } else { min[2] },
            ]
        };
        const EDGES: [(u32, u32); 12] = [
            (0, 1),
            (2, 3),
            (4, 5),
            (6, 7),
            (0, 2),
            (1, 3),
            (4, 6),
            (5, 7),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        let mut vertices = Vec::with_capacity(EDGES.len() * 2);
        for (from, to) in EDGES {
            for position in [corner(from), corner(to)] {
                vertices.push(Vertex {
                    position,
                    normal: [0.0, 1.0, 0.0],
                    uv: [0.0, 0.0],
                    material: 0,
                });
            }
        }
        Scene::vertex_bytes(&vertices)
    }

    /// Cast a ray through loaded terrain and report the first solid cell.
    pub fn pick(&self, origin: [f32; 3], direction: [f32; 3], max_distance: f32) -> PickOutcome {
        cast_ray(
            &mut |pos| self.shape_at(pos),
            origin,
            direction,
            max_distance,
        )
    }

    /// Whether placing an identical block into `pos` completes its contents.
    ///
    /// A lower slab with a completion target fills its empty upper half; the
    /// action service makes the final call, this only steers the aim.
    pub fn completes_at(&self, pos: BlockPos) -> bool {
        match self.authority.store().query(pos) {
            TerrainQuery::Ready(state) => {
                self.authority
                    .registry()
                    .definition(state)
                    .is_some_and(|definition| {
                        definition.render_profile.shape == Shape::LowerSlab
                            && definition.interaction_profile.completes_to.is_some()
                    })
            }
            TerrainQuery::NotLoaded | TerrainQuery::OutOfBounds => false,
        }
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
        self.walker = Some(torse_sim::player::WalkerState::new(handle));
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

    /// Integrate one walk tick through the shared player domain; see the
    /// movement contract. Rejections drop the tick and keep the entity at
    /// its committed position.
    pub fn step_walker(
        &mut self,
        wish: [f64; 3],
        jump: bool,
        dt: f32,
    ) -> Result<(Position, bool), String> {
        let Some(mut walker) = self.walker else {
            return Err("walk mode has no entity".to_owned());
        };
        self.edit_counter += 1;
        let counter = self.edit_counter;
        let outcome = torse_sim::player::walker_step(
            &mut self.authority,
            &mut walker,
            wish,
            jump,
            dt,
            &self.principal,
            TransactionId(u128::from(counter)),
        );
        self.walker = Some(walker);
        match outcome {
            Ok(result) => Ok(result),
            Err(torse_sim::player::StepError::EntityGone) => {
                Err("walker entity is gone".to_owned())
            }
            Err(torse_sim::player::StepError::Fault(fault)) => {
                Err(format!("commit fault: {fault:?}"))
            }
        }
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
    ///
    /// Network scenes keep the server's subscribed sections; streaming would
    /// fabricate terrain the server does not have.
    pub fn load_around(&mut self, center: SectionPos) -> Result<(), String> {
        self.stream_around(center, usize::MAX)
    }

    /// Stream the radius window around `center`, generating at most `budget`
    /// missing sections this call.
    ///
    /// Splitting generation across frames keeps a crossing from stalling the
    /// frame; callers re-invoke until the window is complete.
    pub fn stream_around(&mut self, center: SectionPos, budget: usize) -> Result<(), String> {
        if self.generator.is_none() {
            return Ok(());
        }
        let wanted = self.wanted_sections(center)?;
        if let Some(mut background) = self.background.take() {
            let result = background.stream(self, wanted, budget);
            self.background = Some(background);
            return result;
        }
        let mut generated = 0usize;
        for pos in &wanted {
            if self.authority.store().phase(*pos).is_none() {
                if generated >= budget {
                    break;
                }
                generated += 1;
                let section = self
                    .generator
                    .as_ref()
                    .expect("generator checked")
                    .generate(*pos)
                    .map_err(|error| error.to_string())?;
                self.install_generated(*pos, section)?;
            }
        }
        for pos in self.sections.clone() {
            if !wanted.contains(&pos) {
                if self.authority.store().phase(pos).is_some() {
                    self.authority
                        .store_mut()
                        .unload(pos)
                        .map_err(|e| e.to_string())?;
                }
                self.meshes.remove(&pos);
                self.load_generations.remove(&pos);
            }
        }
        self.sections = wanted;
        Ok(())
    }

    fn assign_load_generation(&mut self, pos: SectionPos) -> Result<u64, String> {
        let generation = self.next_load_generation;
        self.next_load_generation = generation
            .checked_add(1)
            .ok_or("load generation exhausted")?;
        self.load_generations.insert(pos, generation);
        Ok(generation)
    }

    fn install_generated(&mut self, pos: SectionPos, section: Section) -> Result<(), String> {
        let store = self.authority.store_mut();
        store.request(pos).map_err(|e| e.to_string())?;
        store.install(pos, section).map_err(|e| e.to_string())?;
        store.finish_load(pos).map_err(|e| e.to_string())?;
        self.assign_load_generation(pos)?;
        Ok(())
    }

    fn wanted_sections(&self, center: SectionPos) -> Result<Vec<SectionPos>, String> {
        let mut wanted = Vec::new();
        for x in -self.radius..=self.radius {
            for z in -self.radius..=self.radius {
                let sx = center
                    .x
                    .checked_add(x)
                    .ok_or("streaming coordinate overflow")?;
                let sz = center
                    .z
                    .checked_add(z)
                    .ok_or("streaming coordinate overflow")?;
                let pos = SectionPos::new(sx, 0, sz);
                pos.to_block(LocalPos { x: 0, y: 0, z: 0 }, self.side)
                    .map_err(|error| error.to_string())?;
                wanted.push(pos);
            }
        }

        // Near columns first so the terrain under the camera arrives early.
        wanted.sort_by_key(|pos| {
            let dx = pos.x - center.x;
            let dz = pos.z - center.z;
            dx * dx + dz * dz
        });
        Ok(wanted)
    }

    /// Rebuild meshes whose section or neighbour revision changed.
    ///
    /// The cache holds the dependency job captured at build time; validating
    /// that stored job against the live store detects neighbour edits too.
    /// Capturing a fresh job here instead would compare the world with itself
    /// and never notice a neighbour change.
    ///
    /// Returns the rebuilt sections, so callers re-upload only what changed.
    pub fn refresh_meshes(&mut self) -> Vec<SectionPos> {
        let store = self.authority.store();
        let registry = self.authority.registry();
        let mut rebuilt = Vec::new();
        for pos in &self.sections {
            let unchanged = self
                .meshes
                .get(pos)
                .is_some_and(|(job, _)| job == &MeshStamp::capture(self, *pos));
            if unchanged {
                continue;
            }
            let job = MeshStamp::capture(self, *pos);
            let Some(_revision) = job.terrain.section_revision else {
                continue;
            };
            let mesh = build_mesh(self.side, *pos, |query| {
                sample(store, registry, &self.state_materials, query)
            });
            self.meshes.insert(*pos, (job, mesh));
            rebuilt.push(*pos);
        }
        rebuilt
    }

    /// Bootstrap only the starting section synchronously. Subsequent generation
    /// and all meshing use one shared bounded CPU pool in the windowed client.
    pub fn new_background(radius: i64) -> Result<Self, String> {
        if !(0..=16).contains(&radius) {
            return Err("scene radius must be 0..=16".to_owned());
        }
        let mut scene = Self::new(0)?;
        scene.radius = radius;
        scene.enable_background(background::default_config())?;
        Ok(scene)
    }

    pub fn enable_background(
        &mut self,
        config: torse_runtime::ExecutorConfig,
    ) -> Result<(), String> {
        let next = Background::new(config)?;
        self.background = Some(next);
        Ok(())
    }

    pub fn refresh_visuals(&mut self, budget: usize) -> Result<Vec<SectionPos>, String> {
        let Some(mut background) = self.background.take() else {
            return Ok(self.refresh_meshes());
        };
        let result = background.refresh(self, budget);
        self.background = Some(background);
        result
    }

    pub fn background_stats(&self) -> BackgroundStats {
        self.background
            .as_ref()
            .map(|b| b.stats())
            .unwrap_or_default()
    }

    /// Interleaved vertices and indices of the entity boxes, world space.
    pub fn entity_geometry(&self) -> (Vec<Vertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (handle, entity) in self.authority.entities().iter() {
            // The walker is the camera; drawing its body would fill the view.
            if Some(handle) == self.walker.map(|walker| walker.handle) {
                continue;
            }
            push_entity_box(
                &mut vertices,
                &mut indices,
                [
                    entity.position.x as f32,
                    entity.position.y as f32,
                    entity.position.z as f32,
                ],
                u32::from(self.palette.entity_slot()),
            );
        }
        (vertices, indices)
    }

    /// Interleaved vertices and indices of one cached section, world space.
    pub fn section_geometry(&self, pos: SectionPos) -> Option<(Vec<Vertex>, Vec<u32>)> {
        let (_, mesh) = self.meshes.get(&pos)?;
        let offset = [
            pos.x as f32 * self.side as f32,
            0.0,
            pos.z as f32 * self.side as f32,
        ];
        let mut vertices = Vec::with_capacity(mesh.positions.len());
        let mut indices = Vec::with_capacity(mesh.indices.len());
        for (index, position) in mesh.positions.iter().enumerate() {
            vertices.push(Vertex {
                position: [
                    position[0] + offset[0],
                    position[1] + offset[1],
                    position[2] + offset[2],
                ],
                normal: mesh.normals[index],
                uv: mesh.uvs[index],
                material: u32::from(mesh.materials[index / 4]),
            });
        }
        indices.extend(mesh.indices.iter().copied());
        Some((vertices, indices))
    }

    /// Face count of one cached mesh, for tests.
    #[cfg(test)]
    pub fn mesh_face_count(&self, pos: SectionPos) -> Option<usize> {
        self.meshes.get(&pos).map(|(_, mesh)| mesh.face_count())
    }

    /// Section containing a world-space position on the section layer.
    pub fn center_section(&self, position: [f32; 3]) -> SectionPos {
        // Floor before the cast: `as i64` truncates toward zero, which would
        // put a camera at -16.5 into section -1 instead of -2.
        let x = (position[0].floor() as i64).div_euclid(i64::from(self.side));
        let z = (position[2].floor() as i64).div_euclid(i64::from(self.side));
        SectionPos::new(x, 0, z)
    }

    /// Flatten every cached mesh into one interleaved vertex and index buffer,
    /// then append one coloured box per stored entity so mirrored bodies are
    /// visible even before real entity art exists.
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
                vertices.push(Vertex {
                    position: [
                        position[0] + offset[0],
                        position[1] + offset[1],
                        position[2] + offset[2],
                    ],
                    normal: mesh.normals[index],
                    uv: mesh.uvs[index],
                    material: u32::from(mesh.materials[index / 4]),
                });
            }
            indices.extend(mesh.indices.iter().map(|value| base + value));
            faces += mesh.face_count();
        }
        for (handle, entity) in self.authority.entities().iter() {
            // The walker is the camera; drawing its body would fill the view.
            if Some(handle) == self.walker.map(|walker| walker.handle) {
                continue;
            }
            faces += push_entity_box(
                &mut vertices,
                &mut indices,
                [
                    entity.position.x as f32,
                    entity.position.y as f32,
                    entity.position.z as f32,
                ],
                u32::from(self.palette.entity_slot()),
            );
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

/// Resolve stable content into a local runtime state ID.
fn registry_state(
    registry: &BlockStateRegistry,
    block: &ContentKey,
    values: &[u16],
) -> Result<BlockStateId, String> {
    registry
        .state_of(block, values)
        .ok_or_else(|| format!("unresolved replica content {block}"))
}

/// Append one boxy body for an entity; returns the face count added.
fn push_entity_box(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    center: [f32; 3],
    material: u32,
) -> usize {
    const HALF: f32 = 0.35;
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [HALF, -HALF, -HALF], [HALF, HALF, HALF]),
        ([-1.0, 0.0, 0.0], [-HALF, HALF, -HALF], [-HALF, -HALF, HALF]),
        ([0.0, 1.0, 0.0], [-HALF, HALF, -HALF], [HALF, HALF, HALF]),
        ([0.0, -1.0, 0.0], [-HALF, -HALF, HALF], [HALF, -HALF, -HALF]),
        ([0.0, 0.0, 1.0], [-HALF, -HALF, HALF], [HALF, HALF, HALF]),
        ([0.0, 0.0, -1.0], [-HALF, HALF, -HALF], [HALF, -HALF, -HALF]),
    ];
    let mut added = 0usize;
    for (normal, min_corner, max_corner) in faces {
        let (nx, ny, nz) = (normal[0], normal[1], normal[2]);
        let (lo, hi) = (min_corner, max_corner);
        let corners = [
            [lo[0], lo[1], lo[2]],
            [hi[0], lo[1], lo[2]],
            [hi[0], hi[1], lo[2]],
            [lo[0], hi[1], lo[2]],
        ];
        let base = vertices.len() as u32;
        for corner in corners {
            vertices.push(Vertex {
                position: [
                    center[0] + corner[0],
                    center[1] + corner[1],
                    center[2] + corner[2],
                ],
                normal: [nx, ny, nz],
                uv: [0.0, 0.0],
                material,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        added += 2;
    }
    added
}

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("fixture key is valid")
}

fn sample(
    store: &torse_world::SectionStore,
    registry: &BlockStateRegistry,
    state_materials: &BTreeMap<BlockStateId, [u16; 6]>,
    pos: BlockPos,
) -> MeshSample {
    match store.query(pos) {
        TerrainQuery::Ready(state) => {
            if state.is_empty() {
                return MeshSample::Ready(RenderBlock::EMPTY);
            }
            let Some(definition) = registry.definition(state) else {
                return MeshSample::Ready(RenderBlock::EMPTY);
            };
            MeshSample::Ready(RenderBlock {
                bounds: render_box(definition),
                occlusion: definition.occlusion_profile.face_mask,
                material: 0,
                face_materials: Some(state_materials.get(&state).copied().unwrap_or([0; 6])),
            })
        }
        TerrainQuery::NotLoaded => MeshSample::NotLoaded,
        TerrainQuery::OutOfBounds => MeshSample::OutOfBounds,
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

#[cfg(test)]
mod tests {
    use super::*;
    use torse_types::EntityId;

    #[test]
    fn scene_generates_and_meshes_its_radius() {
        let mut scene = Scene::new(1).expect("scene builds");
        assert_eq!(scene.sections().len(), 9);
        assert_eq!(scene.refresh_meshes().len(), 9);
        assert_eq!(scene.mesh_count(), 9);
        // Nothing changed, so the second pass rebuilds nothing.
        assert_eq!(scene.refresh_meshes().len(), 0);
        let (vertices, indices, faces) = scene.geometry();
        assert!(!vertices.is_empty());
        assert!(!indices.is_empty());
        assert!(faces > 0);
        assert_eq!(indices.len() % 3, 0);
    }

    #[test]
    fn neighbour_edit_invalidates_the_border_mesh() {
        let mut scene = Scene::new(1).expect("scene builds");
        scene.refresh_meshes();
        let stone = key("base:stone");
        // X sits in section (0,0,0) against the border, exposing a +x face
        // into neighbour (1,0,0).
        scene
            .apply_edit(true, &stone, BlockPos::new(15, 15, 1))
            .expect("place X");
        scene.refresh_meshes();
        let faces_before = scene.mesh_face_count(SectionPos::ZERO).expect("cached");

        // Y lands in the neighbour and occludes that face; the border
        // section's own revision does not change.
        scene
            .apply_edit(true, &stone, BlockPos::new(16, 15, 1))
            .expect("place Y");
        let rebuilt = scene.refresh_meshes();
        assert!(
            rebuilt.len() >= 2,
            "neighbour and border section must rebuild"
        );

        let faces_after = scene.mesh_face_count(SectionPos::ZERO).expect("cached");
        assert_eq!(
            faces_before - faces_after,
            1,
            "the occluded border face is gone"
        );
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
        assert!(!scene.refresh_meshes().is_empty());
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
    fn scrolling_once_then_flying_keeps_streaming_bounded() {
        let mut scene = Scene::new(0).unwrap();
        let mut camera = crate::camera::FlyCamera::new(scene.eye());
        let mut input = crate::input::InputState::new();
        input.key(
            winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyW),
            winit::event::ElementState::Pressed,
        );
        input.wheel(winit::event::MouseScrollDelta::LineDelta(0.0, 1.0));
        let mut center = scene.center_section(camera.eye);
        let mut streamed = 0;
        for _ in 0..3600 {
            let (motion, speed) = input.take_input(camera.speed);
            camera.speed = speed;
            camera.advance(motion, 1.0 / 60.0);
            assert_eq!(camera.speed, 15.0);
            assert!(camera.eye.iter().all(|v| v.is_finite()));
            let next = scene.center_section(camera.eye);
            if next != center {
                scene.load_around(next).unwrap();
                center = next;
                streamed += 1;
                assert_eq!(scene.sections().len(), 1);
            }
        }
        assert!(
            streamed > 40,
            "exercise repeated section loading after the wheel event"
        );
    }

    #[test]
    fn streaming_rejects_overflow_before_changing_the_scene() {
        let mut scene = Scene::new(1).unwrap();
        let before = scene.sections().to_vec();
        for center in [
            SectionPos::new(i64::MAX, 0, 0),
            SectionPos::new(0, 0, i64::MIN),
        ] {
            assert!(scene.load_around(center).is_err());
            assert_eq!(scene.sections(), before);
        }
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
        // Non-integer negatives must floor, not truncate toward zero.
        assert_eq!(
            scene.center_section([-16.5, 5.0, -0.5]),
            SectionPos::new(-2, 0, -1)
        );
        assert_eq!(
            scene.center_section([-0.5, 5.0, 15.5]),
            SectionPos::new(-1, 0, 0)
        );
    }

    #[test]
    fn vertex_bytes_match_the_documented_stride() {
        let vertices = [Vertex {
            position: [1.0, 2.0, 3.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.25, 0.75],
            material: 3,
        }];
        let bytes = Scene::vertex_bytes(&vertices);
        assert_eq!(bytes.len(), Vertex::STRIDE);
        // Material is the trailing little-endian u32.
        assert_eq!(&bytes[32..36], &3u32.to_le_bytes());
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
    fn two_slabs_merge_into_one_full_block() {
        let mut scene = Scene::new(1).expect("scene builds");
        let target = air_above_surface(&scene);

        let slab = key("base:glass_slab");
        scene.apply_edit(true, &slab, target).expect("first slab");
        assert!(scene.completes_at(target));

        // Placing the same slab into the occupied cell merges the pair into
        // one full glass block, exactly what the top-face aim sends.
        scene
            .apply_edit(true, &slab, target)
            .expect("second slab merges");
        assert!(!scene.completes_at(target));
        let state = scene
            .authority()
            .store()
            .query(target)
            .ready()
            .expect("merged cell holds a block");
        assert_eq!(
            scene.authority().registry().definition_key(state),
            Some(&key("base:glass"))
        );

        // The full block completes no further; a third slab is a conflict.
        let third = scene.apply_edit(true, &slab, target);
        assert!(third.is_err());
    }

    /// Highest and lowest world-space vertex Y inside one cell's footprint.
    fn cell_vertex_range(scene: &Scene, cell: BlockPos) -> (f32, f32) {
        let (vertices, _, _) = scene.geometry();
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        for vertex in &vertices {
            if vertex.position[0] >= cell.x as f32
                && vertex.position[0] <= cell.x as f32 + 1.0
                && vertex.position[2] >= cell.z as f32
                && vertex.position[2] <= cell.z as f32 + 1.0
                && vertex.position[1] >= cell.y as f32
                && vertex.position[1] <= cell.y as f32 + 1.0
            {
                min_y = min_y.min(vertex.position[1]);
                max_y = max_y.max(vertex.position[1]);
            }
        }
        (min_y, max_y)
    }

    #[test]
    fn slab_and_plant_meshes_stay_within_their_shapes() {
        let mut scene = Scene::new(1).expect("scene builds");
        scene.refresh_meshes();
        let target = air_above_surface(&scene);

        let glass = key("base:glass_slab");
        scene.apply_edit(true, &glass, target).expect("place slab");
        scene.refresh_meshes();
        let (min_y, max_y) = cell_vertex_range(&scene, target);
        // A lower slab fills only the bottom half of the cell.
        assert!((min_y - target.y as f32).abs() < 1e-4);
        assert!(
            (max_y - (target.y as f32 + 0.5)).abs() < 1e-4,
            "slab top {max_y} must sit at the cell midline"
        );

        let plant = key("base:plant");
        scene.apply_edit(false, &glass, target).expect("clear");
        scene.apply_edit(true, &plant, target).expect("place plant");
        scene.refresh_meshes();
        // A panel is a thin vertical slab along x: vertices hug the cell's
        // x midline while y spans the full cell.
        let (min_x, max_x) = {
            let (vertices, _, _) = scene.geometry();
            let mut min_x = f32::MAX;
            let mut max_x = f32::MIN;
            for vertex in &vertices {
                // Strictly inside the cell: the terrain block below shares
                // the y boundary plane and its top face spans the cell.
                if vertex.position[0] >= target.x as f32
                    && vertex.position[0] <= target.x as f32 + 1.0
                    && vertex.position[2] >= target.z as f32
                    && vertex.position[2] <= target.z as f32 + 1.0
                    && vertex.position[1] > target.y as f32
                    && vertex.position[1] <= target.y as f32 + 1.0
                {
                    min_x = min_x.min(vertex.position[0]);
                    max_x = max_x.max(vertex.position[0]);
                }
            }
            (min_x, max_x)
        };
        assert!(
            min_x >= target.x as f32 + 0.44 && max_x <= target.x as f32 + 0.56,
            "plant x range [{min_x}, {max_x}] must hug the panel"
        );
        let (_, plant_max_y) = cell_vertex_range(&scene, target);
        assert!((plant_max_y - (target.y as f32 + 1.0)).abs() < 1e-4);
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

    fn stone_layer_section(pos: SectionPos) -> SnapshotSection {
        let side = 16u32;
        let mut cells = vec![BlockStateId::EMPTY; side.pow(3) as usize];
        for x in 0..side {
            for z in 0..side {
                let index = x as usize + side as usize * z as usize;
                cells[index] = BlockStateId(7);
            }
        }
        SnapshotSection {
            pos,
            cells: SnapshotCells::Dense(cells),
        }
    }

    fn replica_snapshot() -> ReplicaSnapshot {
        ReplicaSnapshot {
            side: 16,
            bounds: WorldBounds::new(0, 32).expect("bounds"),
            states: vec![StateEntry {
                id: BlockStateId(7),
                block: key("base:stone"),
                values: Vec::new(),
            }],
            sections: vec![
                stone_layer_section(SectionPos::ZERO),
                stone_layer_section(SectionPos::new(1, 0, 0)),
            ],
            entities: vec![ReplicaEntity {
                id: EntityId(9),
                position_bits: Position::new(2.5, 2.0, 2.0).to_bits(),
                components: BTreeMap::new(),
            }],
        }
    }

    #[test]
    fn network_scene_installs_a_replica_baseline() {
        let mut scene = Scene::from_snapshot(&replica_snapshot()).expect("scene");
        assert!(scene.is_network());
        // The symbol table resolved remote runtime ID 7 into local stone.
        assert!(query(&scene, BlockPos::new(3, 0, 4)));
        assert!(!query(&scene, BlockPos::new(3, 1, 4)));
        // The mirrored entity landed in the client entity store.
        assert_eq!(scene.authority().entities().len(), 1);
        let stored = scene.authority().entities().get(
            scene
                .authority()
                .entities()
                .handle_of(EntityId(9))
                .expect("entity"),
        );
        assert_eq!(stored.expect("entity").position.y, 2.0);
        // Meshes build from the mirrored sections alone.
        assert_eq!(scene.refresh_meshes().len(), 2);
        let (vertices, indices, _) = scene.geometry();
        assert!(!vertices.is_empty() && !indices.is_empty());
        // Streaming must not fabricate terrain around the mirror.
        scene.load_around(SectionPos::new(5, 0, 5)).expect("noop");
        assert_eq!(scene.sections().len(), 2);
    }

    #[test]
    fn network_scene_applies_a_replica_increment() {
        let mut scene = Scene::from_snapshot(&replica_snapshot()).expect("scene");
        scene.refresh_meshes();
        // Carve one hole by replacing the section without its stone at x=3,z=4.
        let mut cells = vec![BlockStateId(7); 16usize.pow(3)];
        for y in 0..16usize {
            for x in 0..16usize {
                for z in 0..16usize {
                    let index = x + 16 * (z + 16 * y);
                    cells[index] = if y == 0 && x != 3 && z != 4 {
                        BlockStateId(7)
                    } else {
                        BlockStateId::EMPTY
                    };
                }
            }
        }
        let changes = ReplicaChanges {
            states: vec![StateEntry {
                id: BlockStateId(7),
                block: key("base:stone"),
                values: Vec::new(),
            }],
            sections: vec![SnapshotSection {
                pos: SectionPos::ZERO,
                cells: SnapshotCells::Dense(cells),
            }],
            removed_sections: Vec::new(),
            entities: Vec::new(),
            removed_entities: Vec::new(),
        };
        scene.apply_changes(&changes).expect("apply");
        assert!(!query(&scene, BlockPos::new(3, 0, 4)));
        assert!(query(&scene, BlockPos::new(5, 0, 5)));
        // The replaced section rebuilds despite matching revision numbers,
        // and the neighbour rebuilds because it observed the old revision.
        assert_eq!(scene.refresh_meshes().len(), 2);
    }

    #[test]
    fn stacked_slabs_keep_the_gap_plane() {
        let mut scene = Scene::new(1).expect("scene builds");
        scene.refresh_meshes();
        let base = air_above_surface(&scene);
        let glass = key("base:glass_slab");
        scene.apply_edit(true, &glass, base).expect("place lower");
        let upper = BlockPos::new(base.x, base.y + 1, base.z);
        scene.apply_edit(true, &glass, upper).expect("place upper");
        scene.refresh_meshes();

        // Same-material neighbours merge: no face lies flat on the shared
        // plane (side quads touch it with their bottom edge only).
        let boundary = base.y as f32 + 1.0;
        let (vertices, _, _) = scene.geometry();
        let flat_on_plane = vertices
            .iter()
            .filter(|v| {
                (v.position[1] - boundary).abs() < 1e-4
                    && v.normal[1].abs() > 0.5
                    && v.position[0] >= base.x as f32
                    && v.position[0] <= base.x as f32 + 1.0
                    && v.position[2] >= base.z as f32
                    && v.position[2] <= base.z as f32 + 1.0
            })
            .count();
        // The upper slab's bottom faces the half-block gap between the two
        // slabs, so it must render; the lower slab never reaches the plane.
        assert_eq!(
            flat_on_plane, 4,
            "the upper slab's bottom quad must face the gap"
        );
    }

    #[test]
    fn glass_keeps_its_face_against_a_different_transparent_block() {
        let mut scene = Scene::new(1).expect("scene builds");
        scene.refresh_meshes();
        let base = air_above_surface(&scene);
        let glass = key("base:glass_slab");
        let plant = key("base:plant");
        // Glass with a plant in the x-adjacent cell: different states keep
        // the shared face (the plant occludes nothing).
        scene.apply_edit(true, &glass, base).expect("place glass");
        let beside = BlockPos::new(base.x + 1, base.y, base.z);
        scene.apply_edit(true, &plant, beside).expect("place plant");
        scene.refresh_meshes();

        let plane = base.x as f32 + 1.0;
        let (vertices, _, _) = scene.geometry();
        let on_plane = vertices
            .iter()
            .filter(|v| {
                (v.position[0] - plane).abs() < 1e-4
                    && v.normal[0].abs() > 0.5
                    && v.position[2] >= base.z as f32
                    && v.position[2] <= base.z as f32 + 1.0
                    && v.position[1] >= base.y as f32
                    && v.position[1] <= base.y as f32 + 0.5
            })
            .count();
        assert!(on_plane > 0, "differing neighbours must keep the face");
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
