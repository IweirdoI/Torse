//! Bounded, optional collision work; journal and serial publication remain authoritative.
use super::*;
use crate::{HostCheckpointStore, Server};
use std::collections::BTreeSet;
use torse_runtime::{ExecutorConfig, JobBudget, JobOutput, run_scoped};
use torse_sim::MovementPrevalidation;
use torse_storage::CommitJournal;

/// Limits a joined window of trusted, read-only movement preparation.
#[derive(Clone, Copy, Debug)]
pub struct PrevalidationConfig {
    pub workers: usize,
    pub max_in_flight: usize,
    pub max_job_bytes: usize,
    pub min_cells: usize,
}
impl Default for PrevalidationConfig {
    fn default() -> Self {
        Self {
            workers: 4,
            max_in_flight: 16,
            max_job_bytes: 8 * 1024 * 1024,
            min_cells: 512,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct PrevalidationStats {
    pub batches: u64,
    pub prepared: u64,
    pub hits: u64,
    pub stale: u64,
    pub skipped: u64,
    pub peak_jobs: usize,
    pub peak_reserved_bytes: usize,
    /// Sum of token-owned bytes, excluding the cache container and borrowed table.
    pub peak_cached_bytes: usize,
}
#[derive(Default)]
pub(super) struct PrevalidationCache {
    table: Option<Arc<Vec<FrozenTask>>>,
    point: Option<ResumePoint>,
    start: usize,
    end: usize,
    entries: Vec<Option<MovementPrevalidation>>,
    stats: PrevalidationStats,
}

fn candidate(task: &FrozenTask, config: PrevalidationConfig) -> Option<EntityId> {
    let group = task.transaction.as_ref()?;
    if task.status != ReplyStatus::Committed || group.members.len() != 1 {
        return None;
    }
    let m = &group.members[0];
    if m.moves.len() != 1
        || !m.reads.is_empty()
        || !m.writes.is_empty()
        || !m.components.is_empty()
        || !m.spawns.is_empty()
        || !m.despawns.is_empty()
    {
        return None;
    }
    let movement = &m.moves[0];
    let half = movement.half.map(f64::from_bits);
    let delta = movement.delta.map(f64::from_bits);
    let mut cells = 1usize;
    for (half, delta) in half.into_iter().zip(delta) {
        if !half.is_finite()
            || half <= 0.0
            || half > torse_sim::movement::MAX_COLLIDER_HALF
            || !delta.is_finite()
            || delta.abs() > torse_sim::movement::MAX_MOVEMENT_DELTA
        {
            return None;
        }
        cells = cells.checked_mul((2.0 * half + delta.abs() + 4.0).ceil() as usize)?;
    }
    // Covers tracked terrain maps, witness vectors, output, and fixed scratch.
    let estimate = cells.checked_mul(256)?.checked_add(65536)?;
    (cells >= config.min_cells && estimate <= config.max_job_bytes).then_some(movement.id)
}

impl ExecutionHost {
    pub fn set_prevalidation(&mut self, config: Option<PrevalidationConfig>) -> Result<(), Fault> {
        if let Some(c) = config
            && (!(1..=64).contains(&c.workers)
                || !(1..=64).contains(&c.max_in_flight)
                || c.max_job_bytes == 0
                || c.max_job_bytes.checked_mul(c.max_in_flight).is_none())
        {
            return Err(Fault::invalid_data("invalid movement prevalidation limits"));
        }
        self.prevalidation = config;
        *self.prevalidated.borrow_mut() = PrevalidationCache::default();
        Ok(())
    }
    pub fn prevalidation_stats(&self) -> PrevalidationStats {
        self.prevalidated.borrow().stats
    }

    pub(super) fn fill_prevalidation(&self) {
        let Some(config) = self.prevalidation else {
            return;
        };
        let Some(work) = &self.runtime.work else {
            return;
        };
        let start = work.next as usize;
        let mut cache = self.prevalidated.borrow_mut();
        let same = cache.point == Some(self.resume_point())
            && cache
                .table
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &work.tasks));
        if same && (cache.start..cache.end).contains(&start) {
            return;
        }
        cache.entries.clear();
        cache.table = Some(Arc::clone(&work.tasks));
        cache.point = Some(self.resume_point());
        cache.start = start;
        cache.end = start + 1;
        let mut ids = BTreeSet::new();
        let mut count = 0;
        for task in work.tasks.iter().skip(start).take(config.max_in_flight) {
            let Some(id) = candidate(task, config) else {
                break;
            };
            if !ids.insert(id) {
                break;
            }
            count += 1;
        }
        if count < 2 {
            cache.stats.skipped += 1;
            return;
        }
        let world = self.world();
        let tasks = &work.tasks;
        let result = run_scoped(
            ExecutorConfig {
                workers: config.workers,
                max_jobs: count,
                max_input_bytes: count * size_of::<usize>(),
                max_output_bytes: count * config.max_job_bytes,
            },
            JobBudget {
                input_bytes: size_of::<usize>(),
                output_bytes: config.max_job_bytes,
            },
            count,
            |index| {
                let value = tasks[start + index]
                    .transaction
                    .as_ref()
                    .unwrap()
                    .restore(world)
                    .ok()
                    .and_then(|tx| world.prevalidate_movement(&tx).ok());
                let bytes = value
                    .as_ref()
                    .map_or(0, MovementPrevalidation::allocated_bytes);
                Ok(JobOutput { value, bytes })
            },
        );
        cache.end = start + count;
        cache.stats.batches += 1;
        cache.stats.peak_jobs = cache.stats.peak_jobs.max(count);
        cache.stats.peak_reserved_bytes = cache
            .stats
            .peak_reserved_bytes
            .max(count * config.max_job_bytes);
        if let Ok(results) = result {
            cache.entries = results.into_iter().map(|r| r.ok().flatten()).collect();
        }
        let bytes = cache
            .entries
            .iter()
            .flatten()
            .map(MovementPrevalidation::allocated_bytes)
            .sum();
        cache.stats.peak_cached_bytes = cache.stats.peak_cached_bytes.max(bytes);
        cache.stats.prepared += cache.entries.iter().flatten().count() as u64;
    }

    pub(super) fn cached_movement(&self) -> Option<torse_sim::PreparedChange> {
        let work = self.runtime.work.as_ref()?;
        let index = work.next as usize;
        let mut cache = self.prevalidated.borrow_mut();
        if cache.point != Some(self.resume_point())
            || !cache
                .table
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &work.tasks))
        {
            return None;
        }
        let offset = index.checked_sub(cache.start)?;
        let token = cache.entries.get(offset)?.as_ref()?;
        match self.world().refresh_movement(token) {
            Ok(prepared) => {
                cache.stats.hits += 1;
                Some(prepared)
            }
            Err(_) => {
                cache.stats.stale += 1;
                cache.entries[offset] = None;
                None
            }
        }
    }
    pub(super) fn release_prevalidation(&self) {
        let mut cache = self.prevalidated.borrow_mut();
        let same = self.runtime.work.as_ref().filter(|w| {
            cache
                .table
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &w.tasks))
        });
        if let Some(work) = same {
            let completed = (work.next as usize).saturating_sub(cache.start);
            for entry in cache.entries.iter_mut().take(completed) {
                *entry = None;
            }
        } else {
            cache.entries.clear();
            cache.table = None;
            cache.point = None;
        }
    }
}
impl<J: CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>
    Server<ExecutionHost, J, S>
{
    pub fn set_prevalidation(&mut self, config: Option<PrevalidationConfig>) -> Result<(), Fault> {
        self.authority()?;
        self.authority.set_prevalidation(config)
    }
}
