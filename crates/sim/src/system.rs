//! Systems, their read-only context, and the transactions they produce.
//!
//! A system never mutates the authority directly. It reads through a scoped
//! context and appends transactions; the simulation commits them after the phase
//! finishes. This is what makes the read set and the write set explicit, and it
//! keeps the commit order under one coordinator instead of one lock per system.

use torse_types::{ContentKey, Tick};
use torse_world::{EntityStore, SectionStore, TerrainQuery};

use crate::phase::{Phase, SystemSpec};
use crate::transaction::Transaction;

/// Read-only view handed to a system for one phase of one tick.
///
/// The transactions a system appends are collected after `run` returns. A system
/// that pushes nothing is a pure observer, which is valid for extraction and
/// diagnostics.
pub struct SystemContext<'a> {
    tick: Tick,
    phase: Phase,
    system: ContentKey,
    store: &'a SectionStore,
    entities: &'a EntityStore,
    transactions: Vec<Transaction>,
}

impl<'a> SystemContext<'a> {
    /// Create a context for one system invocation.
    pub fn new(
        tick: Tick,
        phase: Phase,
        system: ContentKey,
        store: &'a SectionStore,
        entities: &'a EntityStore,
    ) -> Self {
        Self {
            tick,
            phase,
            system,
            store,
            entities,
            transactions: Vec::new(),
        }
    }

    /// Tick being simulated.
    pub const fn tick(&self) -> Tick {
        self.tick
    }

    /// Phase being executed.
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Identity of the running system, for diagnostics and transaction source.
    pub fn system(&self) -> &ContentKey {
        &self.system
    }

    /// Read one authoritative cell.
    pub fn query(&self, pos: torse_types::BlockPos) -> TerrainQuery {
        self.store.query(pos)
    }

    /// Read entities in a section.
    pub fn entities_in(
        &self,
        section: torse_types::SectionPos,
    ) -> impl Iterator<Item = torse_types::EntityHandle> + '_ {
        self.entities.entities_in_section(section)
    }

    /// Borrow the entity store.
    pub const fn entities(&self) -> &EntityStore {
        self.entities
    }

    /// Borrow the section store.
    pub const fn store(&self) -> &SectionStore {
        self.store
    }

    /// Append a transaction to this phase's collection.
    pub fn push(&mut self, transaction: Transaction) {
        self.transactions.push(transaction);
    }

    /// Consume the context and return the collected transactions.
    pub fn into_transactions(self) -> Vec<Transaction> {
        self.transactions
    }
}

/// A registered unit of simulation work.
pub trait System {
    /// Declaration used for ordering and validation.
    fn spec(&self) -> &SystemSpec;

    /// Produce transactions for this tick and phase.
    fn run(&mut self, context: &mut SystemContext<'_>);
}
