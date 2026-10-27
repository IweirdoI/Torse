//! Nonblocking residency scheduling with one bounded, world-owned page I/O worker.
use std::collections::BTreeSet;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use torse_storage::CommitJournal;
use torse_types::*;
use torse_world::{Snapshot, WorldBounds};

use crate::execution::{ExecutionHost, ExecutionModel};
use crate::section_pages::SectionPages;
use crate::{CommitStep, GenerationIdentity, HostCheckpointStore, InvalidationSink, Server};

#[derive(Clone, Copy, Debug)]
pub struct ResidencyConfig {
    pub max_resident: usize,
    /// Reserve page input, codec scratch, decoded output, and handoff before capture.
    pub max_io_bytes: u64,
}
impl ResidencyConfig {
    fn validate(self) -> Result<(), Fault> {
        if self.max_resident == 0 || self.max_resident > 4096 || self.max_io_bytes == 0 {
            return Err(Fault::invalid_data("invalid residency limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GenerationContext {
    pub meta: SnapshotMeta,
    pub side: u32,
    pub bounds: WorldBounds,
    pub next_entity_id: u128,
}

/// Trusted bounded generator, isolated from the mutable authority and its runtime handles.
/// Output depends only on identity, context shape, and position, never execution order.
pub trait GenerationSource: Send + Sync + 'static {
    fn identity(&self) -> GenerationIdentity;
    fn generate(&self, context: GenerationContext, pos: SectionPos) -> Result<Snapshot, Fault>;
}

#[derive(Debug)]
pub enum ResidencyProgress {
    Idle,
    Pending,
    BudgetBlocked,
    CapacityBlocked,
    Unavailable(SectionPos),
    Published(CommitStep),
}

enum Request {
    Save(Snapshot),
    Load(SectionPos, Digest),
    Generate(SectionPos, GenerationContext),
}
struct Completed {
    page: Snapshot,
    digest: Digest,
    load: bool,
    fresh: bool,
}
struct Worker {
    sender: Option<mpsc::SyncSender<Request>>,
    receiver: mpsc::Receiver<Result<Completed, Fault>>,
    handle: Option<JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.sender.take();
        // At most one job exists. Finish it before the owner's world lease is dropped.
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
impl Worker {
    fn new(
        pages: SectionPages,
        generator: Option<Arc<dyn GenerationSource>>,
    ) -> Result<Self, Fault> {
        let (sender, requests) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("torse-page-io".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                        || -> Result<Completed, Fault> {
                            match request {
                                Request::Save(page) => {
                                    let digest = pages.put(&page)?;
                                    Ok(Completed {
                                        page,
                                        digest,
                                        load: false,
                                        fresh: false,
                                    })
                                }
                                Request::Load(pos, digest) => {
                                    let page = pages.get(digest)?;
                                    if page.sections[0].pos != pos {
                                        return Err(Fault::invalid_data(
                                            "loaded page position mismatch",
                                        ));
                                    }
                                    Ok(Completed {
                                        page,
                                        digest,
                                        load: true,
                                        fresh: false,
                                    })
                                }
                                Request::Generate(pos, context) => {
                                    let source = generator.as_ref().ok_or_else(|| {
                                        Fault::invalid_data("generation source missing")
                                    })?;
                                    let page = source.generate(context, pos)?;
                                    if page.meta != context.meta
                                        || page.side != context.side
                                        || page.bounds != context.bounds
                                        || page.next_entity_id != context.next_entity_id
                                        || !page.entities.is_empty()
                                        || page.sections.len() != 1
                                        || page.sections[0].pos != pos
                                    {
                                        return Err(Fault::invalid_data(
                                            "generated page does not match requested context",
                                        ));
                                    }
                                    let digest = pages.digest(&page)?;
                                    Ok(Completed {
                                        page,
                                        digest,
                                        load: true,
                                        fresh: true,
                                    })
                                }
                            }
                        },
                    ))
                    .unwrap_or_else(|_| {
                        Err(Fault::new(
                            FaultCode::InvalidState,
                            "page I/O worker panicked",
                        ))
                    });
                    if results.send(result).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Fault::resource_limit(format!("start page I/O worker: {e}")))?;
        Ok(Self {
            sender: Some(sender),
            receiver,
            handle: Some(handle),
        })
    }
}

pub(crate) struct ResidencyScheduler {
    binding: Binding,
    config: ResidencyConfig,
    pages: SectionPages,
    generator: Option<Arc<dyn GenerationSource>>,
    identity: Option<GenerationIdentity>,
    desired: BTreeSet<SectionPos>,
    worker: Worker,
    busy: bool,
    ready: Option<Completed>,
    memory: Option<MemoryLease>,
}

impl<J: CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>
    Server<ExecutionHost, J, S>
{
    /// Configure between drive calls. The worker is owned by the server and cannot outlive
    /// its world lease. Existing generators are verified before a worker starts.
    pub fn enable_residency(
        &mut self,
        config: ResidencyConfig,
        pages: SectionPages,
        generator: Option<Arc<dyn GenerationSource>>,
        desired: &[SectionPos],
    ) -> Result<(), Fault> {
        config.validate()?;
        if !self.authority()?.residency_ready(false) {
            return Err(Fault::invalid_data(
                "configure residency at an idle tick boundary",
            ));
        }
        let desired = interests(
            desired,
            config.max_resident,
            self.authority.world().store().side(),
        )?;
        let identity = generator.as_ref().map(|source| source.identity());
        if let Some(identity) = &identity {
            identity.bind(self.storage_root())?;
        }
        // Drop any previous worker under the same lease, before starting another one.
        self.residency.take();
        let worker = Worker::new(pages.clone(), generator.clone())?;
        self.residency = Some(ResidencyScheduler {
            binding: self.binding(),
            config,
            pages,
            generator,
            identity,
            desired,
            worker,
            busy: false,
            ready: None,
            memory: None,
        });
        Ok(())
    }

    pub fn set_residency_interest(&mut self, desired: &[SectionPos]) -> Result<(), Fault> {
        self.authority()?;
        let scheduler = self
            .residency
            .as_mut()
            .ok_or_else(|| Fault::invalid_data("residency is not configured"))?;
        scheduler.desired = interests(
            desired,
            scheduler.config.max_resident,
            self.authority.world().store().side(),
        )?;
        Ok(())
    }

    /// At most one publication or one dispatch per call; never waits for a page operation.
    /// An I/O failure leaves authority unchanged and permits the next call to retry.
    pub fn poll_residency(
        &mut self,
        sink: &mut impl InvalidationSink,
    ) -> Result<ResidencyProgress, Fault> {
        self.authority()?;
        let Some(mut scheduler) = self.residency.take() else {
            return Ok(ResidencyProgress::Idle);
        };
        let result = scheduler.poll(self, sink);
        self.residency = Some(scheduler);
        result
    }
}

fn interests(
    desired: &[SectionPos],
    limit: usize,
    side: u32,
) -> Result<BTreeSet<SectionPos>, Fault> {
    if desired.len() > limit {
        return Err(Fault::resource_limit(
            "residency interest capacity exceeded",
        ));
    }
    for pos in desired {
        pos.to_block(LocalPos { x: 0, y: 0, z: 0 }, side)
            .map_err(|_| Fault::invalid_data("unrepresentable residency position"))?;
    }
    Ok(desired.iter().copied().collect())
}

impl ResidencyScheduler {
    pub(crate) fn is_idle(&self) -> bool {
        !self.busy && self.ready.is_none()
    }
    fn poll<J: CommitJournal<ExecutionModel>, S: HostCheckpointStore<ExecutionModel>>(
        &mut self,
        server: &mut Server<ExecutionHost, J, S>,
        sink: &mut impl InvalidationSink,
    ) -> Result<ResidencyProgress, Fault> {
        if server.binding() != self.binding
            || self.generator.as_ref().map(|source| source.identity()) != self.identity
        {
            return Err(Fault::incompatible(
                "residency world or generation identity changed",
            ));
        }
        let tickets = server
            .authority()?
            .event_tickets(false, self.config.max_resident)?;
        let mut required = self.desired.clone();
        for pos in tickets.sections {
            if required.len() >= self.config.max_resident && !required.contains(&pos) {
                return Ok(ResidencyProgress::CapacityBlocked);
            }
            required.insert(pos);
        }
        if self.busy {
            match self.worker.receiver.try_recv() {
                Ok(result) => {
                    self.busy = false;
                    match result {
                        Ok(completed) => self.ready = Some(completed),
                        Err(error) => {
                            self.memory = None;
                            return Err(error);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(ResidencyProgress::Pending),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    self.memory = None;
                    return Err(Fault::new(
                        FaultCode::StorageUnavailable,
                        "page I/O worker disconnected",
                    ));
                }
            }
        }
        if let Some(completed) = self.ready.take() {
            let pos = completed.page.sections[0].pos;
            let wanted = if completed.load {
                required.contains(&pos)
            } else {
                !required.contains(&pos) && !tickets.global
            };
            if !wanted {
                drop(completed);
                self.memory = None;
                return Ok(ResidencyProgress::Pending);
            }
            if !server.authority.residency_ready(!completed.load) {
                self.ready = Some(completed);
                return Ok(ResidencyProgress::Pending);
            }
            // Pure preparation revalidates revisions, parked references, and entity identity.
            let applied = server.apply_residency_page(
                completed.page,
                completed.digest,
                completed.load,
                completed.fresh,
                sink,
            );
            self.memory = None;
            match applied {
                Ok(step) => return Ok(ResidencyProgress::Published(step)),
                Err(error)
                    if error.code == FaultCode::ResourceLimit
                        && server.state() == crate::ServerState::Running =>
                {
                    return Ok(ResidencyProgress::BudgetBlocked);
                }
                Err(error)
                    if error.code == FaultCode::InvalidData
                        && server.state() == crate::ServerState::Running =>
                {
                    return Ok(ResidencyProgress::Pending);
                }
                Err(error) => return Err(error),
            }
        }
        if !server.authority.residency_ready(false) {
            return Ok(ResidencyProgress::Pending);
        }
        let charge = self
            .pages
            .budget()
            .max_bytes
            .checked_mul(4)
            .ok_or_else(|| Fault::resource_limit("page I/O reservation overflow"))?;
        if charge > self.config.max_io_bytes {
            return Ok(ResidencyProgress::BudgetBlocked);
        }
        let memory = if let Some(pool) = server.authority.memory_pool() {
            match pool.reserve(MemoryClass::Codec, charge, EvictionRank::Pinned) {
                Ok(lease) => Some(lease),
                Err(error) if error.code == FaultCode::ResourceLimit => {
                    return Ok(ResidencyProgress::BudgetBlocked);
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let world = server.authority.world();
        let missing = required
            .iter()
            .find(|pos| world.store().section(**pos).is_none())
            .copied();
        let victim = if tickets.global || !server.authority.residency_ready(true) {
            None
        } else {
            world.store().positions().find(|pos| {
                !required.contains(pos)
                    && world.store().phase(*pos) == Some(torse_world::SectionPhase::Ready)
            })
        };
        let request =
            if let Some(pos) = missing.filter(|_| world.store().len() < self.config.max_resident) {
                if let Some(page) = server.authority.parked_sections().get(&pos) {
                    Request::Load(pos, page.digest)
                } else if self.generator.is_some() {
                    Request::Generate(
                        pos,
                        GenerationContext {
                            meta: torse_sim::streaming::page_meta(world),
                            side: world.store().side(),
                            bounds: world.store().bounds(),
                            next_entity_id: world.entities().next_entity_id(),
                        },
                    )
                } else {
                    return Ok(ResidencyProgress::Unavailable(pos));
                }
            } else if let Some(pos) = victim {
                Request::Save(world.capture_section(pos, self.pages.budget())?)
            } else if missing.is_some() || world.store().len() > self.config.max_resident {
                return Ok(ResidencyProgress::CapacityBlocked);
            } else {
                return Ok(ResidencyProgress::Idle);
            };
        self.worker
            .sender
            .as_ref()
            .expect("live worker")
            .try_send(request)
            .map_err(|_| Fault::new(FaultCode::StorageUnavailable, "page I/O queue unavailable"))?;
        self.busy = true;
        self.memory = memory;
        Ok(ResidencyProgress::Pending)
    }
}
