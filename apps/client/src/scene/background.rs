//! Owned generation/mesh inputs. Only the scene thread installs their results.
use std::collections::BTreeMap;

use torse_render::{Face, MeshBuildError, MeshJob, build_mesh_bounded};
use torse_runtime::{
    Executor, ExecutorConfig, Job, JobBudget, JobError, JobOutput, Priority, ReserveError,
};
use torse_types::Revision;
use torse_world::{SectionPhase, SectionStore};
use torse_world_services::SectionGenerator;

use super::*;

pub(super) fn default_config() -> ExecutorConfig {
    ExecutorConfig {
        workers: std::thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(2).clamp(1, 4)),
        max_jobs: 8,
        max_input_bytes: 8 * 1024 * 1024,
        max_output_bytes: 32 * 1024 * 1024,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MeshStamp {
    pub terrain: MeshJob,
    epoch: WorldEpoch,
    assets: u64,
    tint_provider_generation: u64,
    tint_provider_revision: u64,
    loads: [Option<u64>; 7],
}

fn ready_revision(store: &SectionStore, pos: SectionPos) -> Option<Revision> {
    matches!(
        store.phase(pos),
        Some(SectionPhase::Ready | SectionPhase::Active)
    )
    .then(|| store.revision(pos))
    .flatten()
}

impl MeshStamp {
    pub fn capture(scene: &Scene, pos: SectionPos) -> Self {
        let mut loads = [None; 7];
        let positions =
            std::iter::once(Some(pos)).chain(Face::ALL.map(|face| face.neighbor_section(pos)));
        for (slot, pos) in loads.iter_mut().zip(positions) {
            let Some(pos) = pos else {
                continue;
            };
            if ready_revision(scene.authority.store(), pos).is_some() {
                *slot = scene.load_generations.get(&pos).copied();
            }
        }
        Self {
            terrain: MeshJob::capture(scene.side, pos, |p| {
                ready_revision(scene.authority.store(), p)
            }),
            epoch: scene.epoch,
            assets: scene.asset_generation,
            tint_provider_generation: scene.tint_provider_generation,
            tint_provider_revision: scene
                .tint_provider
                .as_ref()
                .map_or(0, |provider| provider.revision(pos)),
            loads,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BackgroundStats {
    pub admitted: u64,
    pub cancelled: u64,
    pub stale: u64,
    pub backpressure: u64,
    pub peak_jobs: usize,
    pub peak_input_bytes: usize,
    pub peak_output_bytes: usize,
}

enum Output {
    Generated(Section),
    Meshed(MeshData),
}

struct Generation {
    epoch: WorldEpoch,
    load: u64,
    version: u32,
    job: Job<Output>,
}

struct Meshing {
    stamp: MeshStamp,
    job: Job<Output>,
}

pub(super) struct Background {
    pool: Executor<Output>,
    generating: BTreeMap<SectionPos, Generation>,
    meshing: BTreeMap<SectionPos, Meshing>,
    stats: BackgroundStats,
}

impl Background {
    pub fn new(config: ExecutorConfig) -> Result<Self, String> {
        Ok(Self {
            pool: Executor::new(config).map_err(|e| e.to_string())?,
            generating: BTreeMap::new(),
            meshing: BTreeMap::new(),
            stats: BackgroundStats::default(),
        })
    }

    pub fn stats(&self) -> BackgroundStats {
        self.stats
    }

    fn sample_usage(&mut self) {
        let usage = self.pool.usage();
        self.stats.peak_jobs = self.stats.peak_jobs.max(usage.jobs);
        self.stats.peak_input_bytes = self.stats.peak_input_bytes.max(usage.input_bytes);
        self.stats.peak_output_bytes = self.stats.peak_output_bytes.max(usage.output_bytes);
    }

    fn reserve(
        &mut self,
        budget: JobBudget,
    ) -> Result<Option<torse_runtime::Reservation<Output>>, String> {
        match self.pool.try_reserve(budget) {
            Ok(lease) => {
                self.stats.admitted += 1;
                self.sample_usage();
                Ok(Some(lease))
            }
            Err(ReserveError::Busy) => {
                self.stats.backpressure += 1;
                Ok(None)
            }
            Err(error) => Err(format!("background admission failed: {error:?}")),
        }
    }

    pub fn stream(
        &mut self,
        scene: &mut Scene,
        wanted: Vec<SectionPos>,
        budget: usize,
    ) -> Result<(), String> {
        self.generating.retain(|pos, _| {
            let keep = wanted.contains(pos);
            if !keep {
                scene.load_generations.remove(pos);
                self.stats.cancelled += 1;
            }
            keep
        });
        for pos in scene.sections.clone() {
            if !wanted.contains(&pos) {
                if scene.authority.store().phase(pos).is_some() {
                    scene
                        .authority
                        .store_mut()
                        .unload(pos)
                        .map_err(|e| e.to_string())?;
                }
                scene.meshes.remove(&pos);
                scene.load_generations.remove(&pos);
            }
        }
        scene.sections = wanted;
        self.cancel_old_meshes(scene);
        let generator = *scene.generator.as_ref().expect("local generator");
        let version = generator.generation_version();
        let mut completed = 0;
        for pos in scene.sections.clone() {
            if completed >= budget {
                break;
            }
            let Some(task) = self.generating.get_mut(&pos) else {
                continue;
            };
            let Some(result) = task.job.try_take() else {
                continue;
            };
            completed += 1;
            let task = self.generating.remove(&pos).expect("pending generation");
            if task.epoch != scene.epoch
                || task.version != version
                || scene.load_generations.get(&pos) != Some(&task.load)
                || scene.authority.store().phase(pos).is_some()
            {
                self.stats.stale += 1;
                continue;
            }
            match result {
                Ok(Output::Generated(section)) => scene.install_generated(pos, section)?,
                Err(JobError::Cancelled) => {}
                other => return Err(format!("generation failed: {}", output_error(other))),
            }
        }
        let mut admitted = 0;
        for pos in scene.sections.clone() {
            if admitted >= budget {
                break;
            }
            if scene.authority.store().phase(pos).is_some() || self.generating.contains_key(&pos) {
                continue;
            }
            let side = scene.side as usize;
            let task_budget = JobBudget {
                input_bytes: size_of::<GenerationParams>() + size_of::<HillTerrain>(),
                output_bytes: size_of::<Section>()
                    + side.pow(3) * size_of::<BlockStateId>()
                    + side.pow(2) * size_of::<i64>(),
            };
            let Some(lease) = self.reserve(task_budget)? else {
                break;
            };
            let load = scene.assign_load_generation(pos)?;
            let job = lease.submit(Priority::Background, move |cancel| {
                let section = generator
                    .generate_cancellable(pos, || cancel.is_cancelled())
                    .map_err(|e| JobError::Failed(e.to_string()))?
                    .ok_or(JobError::Cancelled)?;
                let cells = match section.storage() {
                    torse_world::SectionStorage::Uniform(_) => 0,
                    torse_world::SectionStorage::Dense(cells) => cells.capacity(),
                };
                let bytes = size_of::<Section>() + cells * size_of::<BlockStateId>();
                Ok(JobOutput {
                    value: Output::Generated(section),
                    bytes,
                })
            });
            self.generating.insert(
                pos,
                Generation {
                    epoch: scene.epoch,
                    load,
                    version,
                    job,
                },
            );
            admitted += 1;
        }
        Ok(())
    }

    fn cancel_old_meshes(&mut self, scene: &Scene) {
        self.meshing.retain(|pos, task| {
            let keep =
                scene.sections.contains(pos) && task.stamp == MeshStamp::capture(scene, *pos);
            if !keep {
                self.stats.cancelled += 1;
            }
            keep
        });
    }

    pub fn refresh(&mut self, scene: &mut Scene, budget: usize) -> Result<Vec<SectionPos>, String> {
        let mut rebuilt = Vec::new();
        // Check late results before cancelling tasks whose dependencies changed.
        for pos in self.meshing.keys().copied().collect::<Vec<_>>() {
            if rebuilt.len() >= budget {
                break;
            }
            let task = self.meshing.get_mut(&pos).expect("pending mesh");
            let Some(result) = task.job.try_take() else {
                continue;
            };
            let task = self.meshing.remove(&pos).expect("pending mesh");
            if !scene.sections.contains(&pos) || task.stamp != MeshStamp::capture(scene, pos) {
                self.stats.stale += 1;
                continue;
            }
            match result {
                Ok(Output::Meshed(mesh)) => {
                    scene.meshes.insert(pos, (task.stamp, mesh));
                    rebuilt.push(pos);
                }
                Err(JobError::Cancelled) => {}
                other => return Err(format!("meshing failed: {}", output_error(other))),
            }
        }
        self.cancel_old_meshes(scene);
        let mut admitted = 0;
        for pos in scene.sections.clone() {
            if admitted >= budget {
                break;
            }
            let stamp = MeshStamp::capture(scene, pos);
            if stamp.terrain.section_revision.is_none()
                || self.meshing.contains_key(&pos)
                || scene
                    .meshes
                    .get(&pos)
                    .is_some_and(|(previous, _)| previous == &stamp)
            {
                continue;
            }
            let side = scene.side as usize;
            let max_faces = side.pow(3) * 6;
            // Geometry, two per-vertex tint channels, indices, and two slots.
            let face_bytes = 8 * size_of::<[f32; 3]>()
                + 4 * size_of::<[f32; 2]>()
                + 8 * size_of::<[f32; 3]>()
                + 6 * size_of::<u32>()
                + 2 * size_of::<u16>();
            let task_budget = JobBudget {
                input_bytes: (side + 2).pow(3) * size_of::<MeshSample>() + size_of::<MeshInput>(),
                output_bytes: size_of::<MeshData>() + max_faces * face_bytes * 2,
            };
            let Some(lease) = self.reserve(task_budget)? else {
                break;
            };
            // Reservation precedes the only owned input copy.
            let input = MeshInput::capture(scene, pos)?;
            let job = lease.submit(Priority::Visible, move |cancel| {
                let mesh = build_mesh_bounded(
                    input.side,
                    pos,
                    |p| input.sample(p),
                    || cancel.is_cancelled(),
                    max_faces,
                )
                .map_err(|e| {
                    if e == MeshBuildError::Cancelled {
                        JobError::Cancelled
                    } else {
                        JobError::OutputLimit
                    }
                })?;
                let bytes = size_of::<MeshData>() + mesh.allocated_bytes();
                Ok(JobOutput {
                    value: Output::Meshed(mesh),
                    bytes,
                })
            });
            self.meshing.insert(pos, Meshing { stamp, job });
            admitted += 1;
        }
        Ok(rebuilt)
    }
}

fn output_error(result: Result<Output, JobError>) -> String {
    match result {
        Err(e) => format!("{e:?}"),
        Ok(_) => "unexpected CPU result type".to_owned(),
    }
}

struct MeshInput {
    side: u32,
    origin: BlockPos,
    cells: Vec<MeshSample>,
}

impl MeshInput {
    fn capture(scene: &Scene, pos: SectionPos) -> Result<Self, String> {
        let origin = pos
            .to_block(LocalPos { x: 0, y: 0, z: 0 }, scene.side)
            .map_err(|e| e.to_string())?;
        let extent = scene.side as usize + 2;
        let mut cells = Vec::with_capacity(extent.pow(3));
        for y in -1..=i64::from(scene.side) {
            for z in -1..=i64::from(scene.side) {
                for x in -1..=i64::from(scene.side) {
                    cells.push(match origin.checked_add(x, y, z) {
                        Ok(p) => sample(
                            scene.authority.store(),
                            scene.authority.registry(),
                            &scene.state_materials,
                            &scene.palette,
                            scene.tint_provider.as_deref(),
                            p,
                        ),
                        Err(_) => MeshSample::OutOfBounds,
                    });
                }
            }
        }
        Ok(Self {
            side: scene.side,
            origin,
            cells,
        })
    }

    fn sample(&self, pos: BlockPos) -> MeshSample {
        let extent = self.side as usize + 2;
        let x = (pos.x - self.origin.x + 1) as usize;
        let y = (pos.y - self.origin.y + 1) as usize;
        let z = (pos.z - self.origin.z + 1) as usize;
        self.cells[x + extent * (z + extent * y)]
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
