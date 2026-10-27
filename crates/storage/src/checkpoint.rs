//! Publish immutable snapshots before making their manifest references durable.

use std::marker::PhantomData;

use torse_codec::{CodecLimits, StorageCodec, record};
use torse_types::{
    AuthorityData, Binding, CommitCursor, Durability, Fault, FaultCode, ModelSnapshot, OutputBudget,
};

use crate::checkpoint_backend::{
    CheckpointBackend, FileCheckpointBackend, MemoryCheckpointBackend,
};
use crate::manifest::{self, CheckpointManifest, corrupt};

/// Local resource policy. Explicit pruning compacts the manifest before deleting snapshots.
#[derive(Clone, Copy, Debug)]
pub struct CheckpointConfig {
    pub codec_limits: CodecLimits,
    pub codec_budget: OutputBudget,
    pub max_manifest_bytes: u64,
    pub max_snapshot_files: u32,
}

impl CheckpointConfig {
    /// Validate local retention policy before a host opens or repairs world data.
    pub fn validate(self) -> Result<(), Fault> {
        if self.max_manifest_bytes < manifest::ENTRY_BYTES as u64 || self.max_snapshot_files < 2 {
            return Err(Fault::invalid_data(
                "checkpoint policy must fit a manifest and two snapshots",
            ));
        }
        usize::try_from(self.max_manifest_bytes)
            .map_err(|_| Fault::resource_limit("manifest limit is not addressable"))?;
        usize::try_from(self.snapshot_limit())
            .map_err(|_| Fault::resource_limit("snapshot limit is not addressable"))?;
        Ok(())
    }

    fn snapshot_limit(self) -> u64 {
        u64::from(self.codec_limits.max_frame_bytes).min(self.codec_budget.max_bytes)
            + record::OVERHEAD_BYTES as u64
    }
}

/// The caller captures the complete finite world at a paused tick boundary, after flushing
/// the journal through the snapshot cursor. The store verifies the supplied durability
/// binding and cursor, but cannot prove the authority's capture or synchronization semantics.
pub trait CheckpointStore<M: AuthorityData> {
    fn publish_checkpoint(
        &mut self,
        snapshot: &ModelSnapshot<M>,
        durable: Durability,
    ) -> Result<(), Fault>;
}

/// Conservative recovery floor after explicit pruning. Journal records strictly after this
/// position must remain available for every retained checkpoint.
#[derive(Debug, Eq, PartialEq)]
pub struct CheckpointRetention {
    pub log_floor: Option<CommitCursor>,
    pub removed_generations: Vec<u64>,
}

pub type MemoryCheckpoints<M, C> = Checkpoints<M, C, MemoryCheckpointBackend>;
pub type FileCheckpoints<M, C> = Checkpoints<M, C, FileCheckpointBackend>;

pub struct Checkpoints<M: AuthorityData, C, B> {
    binding: Binding,
    codec: C,
    backend: B,
    config: CheckpointConfig,
    history: Vec<CheckpointManifest>,
    manifest_bytes: u64,
    next_generation: u64,
    stopped: bool,
    active: bool,
    incomplete_tail: bool,
    model: PhantomData<fn() -> M>,
}

impl<M: AuthorityData, C: StorageCodec<M>, B: CheckpointBackend> Checkpoints<M, C, B> {
    /// Read-only validation precedes any torn-tail repair. Only the latest complete manifest
    /// is authoritative; a missing or corrupt referenced snapshot never triggers fallback.
    /// Orphans are retained until explicit pruning, and their generation numbers are skipped.
    pub fn open(
        binding: Binding,
        codec: C,
        config: CheckpointConfig,
        backend: B,
    ) -> Result<Self, Fault> {
        let mut store = Self::inspect(binding, codec, config, backend)?;
        store.activate()?;
        Ok(store)
    }

    /// Validate without repairing or synchronizing storage. Startup uses this phase while
    /// an isolated authority builder checks domain invariants. Publication and pruning stay
    /// disabled until activation succeeds, so a rejected world leaves both data tails intact.
    pub fn inspect(
        binding: Binding,
        codec: C,
        config: CheckpointConfig,
        mut backend: B,
    ) -> Result<Self, Fault> {
        config.validate()?;
        let bytes = backend.read_manifest(config.max_manifest_bytes)?;
        let (history, valid_bytes) = manifest::scan(&bytes, binding, config.snapshot_limit())?;
        let files = backend.snapshot_generations(config.max_snapshot_files)?;
        let maximum = files
            .iter()
            .copied()
            .chain(history.iter().map(|entry| entry.generation))
            .max()
            .unwrap_or(0);
        let next_generation = maximum
            .checked_add(1)
            .ok_or_else(|| Fault::resource_limit("checkpoint generation exhausted"))?;
        let mut store = Self {
            binding,
            codec,
            backend,
            config,
            history,
            manifest_bytes: valid_bytes as u64,
            next_generation,
            stopped: false,
            active: false,
            incomplete_tail: valid_bytes != bytes.len(),
            model: PhantomData,
        };
        store.load_current()?;
        Ok(store)
    }

    /// Repair only a previously validated incomplete suffix and establish the barrier
    /// before this instance may publish or delete files. Repeated activation is harmless.
    pub fn activate(&mut self) -> Result<(), Fault> {
        self.ensure_running()?;
        if self.active {
            return Ok(());
        }
        // Even an intact prefix must pass a synchronization barrier before this instance
        // publishes or prunes. A complete last entry may have survived a failed prior sync.
        let result = (|| {
            if self.incomplete_tail {
                self.backend.truncate_manifest(self.manifest_bytes)?;
            }
            self.backend.sync_manifest()
        })();
        if let Err(error) = result {
            self.stopped = true;
            return Err(error);
        }
        self.active = true;
        self.incomplete_tail = false;
        Ok(())
    }

    pub fn current(&self) -> Option<CheckpointManifest> {
        self.history.last().copied()
    }

    /// Read every retained snapshot before collecting references owned by its payload.
    /// The callback runs without mutation and must discard its result if any load fails.
    pub fn visit_retained(
        &mut self,
        mut visit: impl FnMut(ModelSnapshot<M>) -> Result<(), Fault>,
    ) -> Result<(), Fault> {
        self.ensure_running()?;
        let files = self
            .backend
            .snapshot_generations(self.config.max_snapshot_files)?;
        for index in 0..self.history.len() {
            let entry = self.history[index];
            // Legacy append-only histories can refer to already-pruned snapshots.
            if files.contains(&entry.generation) {
                visit(self.load(entry)?)?;
            }
        }
        Ok(())
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Retaining the backend permits a caller to reopen the same in-memory namespace.
    pub fn into_backend(self) -> B {
        self.backend
    }

    pub fn load_current(&mut self) -> Result<Option<ModelSnapshot<M>>, Fault> {
        self.ensure_running()?;
        let Some(entry) = self.current() else {
            return Ok(None);
        };
        let result = self.load(entry).map(Some);
        if result.is_err() {
            self.stopped = true;
        }
        result
    }

    /// Remove obsolete and unpublished snapshots only after revalidating the active one.
    /// Retained predecessors are maintenance data, not an automatic recovery fallback.
    /// A cleanup failure stops the instance; the active snapshot is never a deletion target.
    pub fn prune(&mut self, retain: usize) -> Result<CheckpointRetention, Fault> {
        self.ensure_active()?;
        if retain == 0 {
            return Err(Fault::invalid_data(
                "retain at least one published checkpoint",
            ));
        }
        self.load_current()?;
        let files = self
            .backend
            .snapshot_generations(self.config.max_snapshot_files)?;
        // Previously pruned generations remain in the append-only history. Count only
        // snapshots that still exist, so increasing retention never promises lost files.
        let retained: Vec<_> = self
            .history
            .iter()
            .rev()
            .filter(|entry| files.contains(&entry.generation))
            .take(retain)
            .copied()
            .collect();
        let log_floor = retained.last().map(|entry| entry.snapshot.cursor);
        // Validate every retained reference before publishing the new recovery root.
        for entry in &retained {
            self.load(*entry)?;
        }
        let mut retained = retained;
        retained.reverse();
        if retained != self.history {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(retained.len() * manifest::ENTRY_BYTES)
                .map_err(|_| Fault::resource_limit("compact manifest allocation"))?;
            for entry in &retained {
                bytes.extend_from_slice(&manifest::encode_compacted(*entry)?);
            }
            if let Err(error) = self.backend.replace_manifest(&bytes) {
                self.stopped = true;
                return Err(error);
            }
            self.manifest_bytes = bytes.len() as u64;
            self.history = retained.clone();
        }
        let mut removed_generations = Vec::new();
        removed_generations
            .try_reserve_exact(files.len())
            .map_err(|_| Fault::resource_limit("checkpoint cleanup report allocation"))?;
        for generation in files {
            if retained.iter().any(|entry| entry.generation == generation) {
                continue;
            }
            if let Err(error) = self.backend.remove_snapshot(generation) {
                self.stopped = true;
                return Err(error);
            }
            removed_generations.push(generation);
        }
        Ok(CheckpointRetention {
            log_floor,
            removed_generations,
        })
    }

    fn ensure_running(&self) -> Result<(), Fault> {
        if self.stopped {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "checkpoint instance is stopped",
            ));
        }
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), Fault> {
        self.ensure_running()?;
        if !self.active {
            return Err(Fault::new(
                FaultCode::InvalidState,
                "checkpoint instance is not activated",
            ));
        }
        Ok(())
    }

    fn load(&mut self, entry: CheckpointManifest) -> Result<ModelSnapshot<M>, Fault> {
        let bytes = self
            .backend
            .read_snapshot(entry.generation, self.config.snapshot_limit())?;
        if bytes.len() as u64 != entry.snapshot_bytes
            || crc32c::crc32c(&bytes) != entry.snapshot_crc
        {
            return Err(corrupt(
                "checkpoint snapshot length or manifest checksum mismatch",
            ));
        }
        let record::RecordRead::Complete {
            kind,
            payload,
            encoded_bytes,
        } = record::read_record(&bytes, self.config.codec_limits)?
        else {
            return Err(corrupt("incomplete published checkpoint snapshot"));
        };
        if kind != record::RecordKind::Snapshot || encoded_bytes != bytes.len() {
            return Err(corrupt("invalid checkpoint snapshot kind or trailing data"));
        }
        let snapshot = self
            .codec
            .decode_snapshot(payload, self.config.codec_budget)?;
        if snapshot.meta != entry.snapshot {
            return Err(corrupt("checkpoint metadata differs from manifest"));
        }
        Ok(snapshot)
    }
}

impl<M: AuthorityData, C: StorageCodec<M>, B: CheckpointBackend> CheckpointStore<M>
    for Checkpoints<M, C, B>
{
    fn publish_checkpoint(
        &mut self,
        snapshot: &ModelSnapshot<M>,
        durable: Durability,
    ) -> Result<(), Fault> {
        self.ensure_active()?;
        manifest::validate_meta(snapshot.meta, self.binding)?;
        if durable.binding != self.binding || !durable.covers(snapshot.meta.cursor) {
            return Err(Fault::invalid_data(
                "checkpoint requires matching durable binding and cursor",
            ));
        }
        if let Some(previous) = self.current() {
            if snapshot.meta.cursor.seq < previous.snapshot.cursor.seq
                || durable.through < previous.durable_through
            {
                return Err(Fault::invalid_data(
                    "checkpoint publication regresses persisted positions",
                ));
            }
        } else if snapshot.meta.cursor.seq.0 != 0 {
            return Err(Fault::invalid_data(
                "initial checkpoint must describe position zero",
            ));
        }
        let manifest_bytes = self
            .manifest_bytes
            .checked_add(manifest::ENTRY_BYTES as u64)
            .filter(|total| *total <= self.config.max_manifest_bytes)
            .ok_or_else(|| Fault::resource_limit("checkpoint manifest capacity exhausted"))?;
        let next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or_else(|| Fault::resource_limit("checkpoint generation exhausted"))?;
        if self
            .backend
            .snapshot_generations(self.config.max_snapshot_files)?
            .len() as u64
            >= u64::from(self.config.max_snapshot_files)
        {
            return Err(Fault::resource_limit(
                "prune old checkpoints before publishing",
            ));
        }
        let mut payload = Vec::new();
        self.codec
            .encode_snapshot(snapshot, &mut payload, self.config.codec_budget)?;
        if payload.len() as u64 > self.config.codec_budget.max_bytes {
            return Err(Fault::resource_limit(
                "snapshot codec exceeded its output budget",
            ));
        }
        // Check the codec's result before performing I/O. A second decode below validates
        // bytes read back from storage, so an adapter cannot publish an unreadable snapshot.
        if self
            .codec
            .decode_snapshot(&payload, self.config.codec_budget)?
            .meta
            != snapshot.meta
        {
            return Err(corrupt("snapshot codec changed metadata"));
        }
        let bytes = manifest::wrap(
            record::RecordKind::Snapshot,
            &payload,
            self.config.codec_limits,
        )?;
        let entry = CheckpointManifest {
            generation: self.next_generation,
            snapshot: snapshot.meta,
            durable_through: durable.through,
            snapshot_bytes: bytes.len() as u64,
            snapshot_crc: crc32c::crc32c(&bytes),
        };
        let manifest = manifest::encode(entry)?;
        self.history
            .try_reserve(1)
            .map_err(|_| Fault::resource_limit("checkpoint history allocation"))?;
        let result = (|| {
            self.backend.create_snapshot(entry.generation, &bytes)?;
            self.backend.sync_snapshot(entry.generation)?;
            self.load(entry)?;
            self.backend.append_manifest(&manifest)?;
            self.backend.sync_manifest()
        })();
        if result.is_err() {
            // Some or all manifest bytes may now exist. No retry or cleanup is safe until
            // reopening determines which complete reference actually survived the failure.
            self.stopped = true;
            return result;
        }
        self.history.push(entry);
        self.manifest_bytes = manifest_bytes;
        self.next_generation = next_generation;
        Ok(())
    }
}
