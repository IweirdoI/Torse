//! Minimal component-model host for behavior mods (P1 prototype).
//!
//! This module wires a Wasmtime component instance to a fixture world. It
//! proves the boundary end to end and measures per-call cost against a native
//! baseline for the DM01 decision. Connecting the same boundary to the real
//! authority (`torse-sim`) is P2 work and intentionally not done here.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use exports::torse::behavior::invoke::Event;
pub use exports::torse::behavior::invoke::{
    Event as InvokeEvent, Invocation, ObjectRef, OwnedState, Plan, WakeupTarget,
};
use torse::behavior::diagnostics::LogLevel;
use torse::behavior::query::{
    CellSample, ComponentSnapshot, ComponentTarget, EntityPage, PortEstimate, QueryError,
};
use torse::behavior::schedule::WakeupSlot;
pub use torse::behavior::types::EntityId;
use torse::behavior::types::{Atom, Bbox, BlockRef, Cell, Field, Vec3i};
use wasmtime::component::bindgen;
use wasmtime::{Config, Engine, Store};

bindgen!({
    path: "../mod_api/wit/behavior.wit",
    world: "mod-behavior",
});

/// Encode a core wasm module built from the Rust SDK into a component using
/// the same WIT file that produced the host bindings.
pub fn encode_component(core_wasm: &[u8]) -> Result<Vec<u8>, String> {
    wit_component::ComponentEncoder::default()
        .module(core_wasm)
        .map_err(|error| format!("module ingest failed: {error}"))?
        .validate(true)
        .encode()
        .map_err(|error| format!("component encoding failed: {error}"))
}

/// Host-side metering for a single dispatch loop. Wasm fuel and epoch limits
/// bound guest work; these counters bound and record host work done on the
/// guest's behalf, which fuel cannot see (04-mods 4.1).
#[derive(Clone, Debug, Default)]
pub struct Meters {
    pub query_block_calls: u32,
    pub cells_returned: u32,
    pub resolve_calls: u32,
    pub log_entries: u32,
    pub log_dropped: u32,
}

/// Budgets applied per invocation. Initial prototype values; the design log
/// records them as starting points, not acceptance promises.
#[derive(Clone, Copy, Debug)]
pub struct CallBudget {
    pub fuel: u64,
    pub max_cells_per_query: u32,
    pub max_log_entries: u32,
}

impl CallBudget {
    pub fn prototype() -> Self {
        Self {
            fuel: 200_000_000,
            max_cells_per_query: 4_096,
            max_log_entries: 64,
        }
    }
}

/// In-memory stand-in for the authority: a block fixture around the machine
/// position plus the mod's committed component state. P2 replaces this with
/// `WorldAuthority` access.
pub struct FixtureHost {
    symbols: HashMap<String, u32>,
    symbol_names: Vec<String>,
    blocks: HashMap<(i64, i64, i64), Cell>,
    pub committed_count: u32,
    pub wakeups: Vec<(u64, u64, String)>,
    pub meters: Meters,
    budget: CallBudget,
}

impl FixtureHost {
    pub fn new(budget: CallBudget) -> Self {
        let mut symbols = HashMap::new();
        let mut symbol_names = Vec::new();
        for key in ["roundtrip:counter", "roundtrip:counter-state"] {
            let symbol = symbol_names.len() as u32 + 1;
            symbols.insert(key.to_string(), symbol);
            symbol_names.push(key.to_string());
        }
        let mut blocks = HashMap::new();
        let floor = Cell::Occupied(BlockRef {
            definition: *symbols.get("roundtrip:counter").expect("symbol registered"),
            state_values: Vec::new(),
        });
        for x in -1..=1 {
            for z in -1..=1 {
                blocks.insert((x, 63, z), floor.clone());
            }
        }
        blocks.insert((0, 64, 0), floor.clone());
        Self {
            symbols,
            symbol_names,
            blocks,
            committed_count: 0,
            wakeups: Vec::new(),
            meters: Meters::default(),
            budget,
        }
    }

    /// Applies a returned plan to the fixture, standing in for the authority's
    /// prepare/publish cycle at prototype fidelity.
    pub fn apply_plan(&mut self, plan: &Plan) {
        for edit in &plan.edits {
            let pos = (edit.pos.x, edit.pos.y, edit.pos.z);
            match &edit.new {
                Cell::Empty => {
                    self.blocks.remove(&pos);
                }
                cell => {
                    self.blocks.insert(pos, cell.clone());
                }
            }
        }
        for edit in &plan.component_edits {
            if let Some(fields) = &edit.fields {
                for field in fields {
                    if field.key == "count"
                        && let Atom::U32(count) = field.value
                    {
                        self.committed_count = count;
                    }
                }
            }
        }
        for wakeup in &plan.wakeups {
            self.wakeups
                .push((wakeup.tick, wakeup.entity.low, wakeup.key.clone()));
        }
    }

    fn cell_at(&self, pos: (i64, i64, i64)) -> Cell {
        self.blocks.get(&pos).cloned().unwrap_or(Cell::Empty)
    }
}

impl torse::behavior::types::Host for FixtureHost {}

impl torse::behavior::query::Host for FixtureHost {
    fn resolve_symbol(&mut self, key: String) -> Option<u32> {
        self.meters.resolve_calls += 1;
        self.symbols.get(&key).copied()
    }

    fn symbol_key(&mut self, symbol: u32) -> Option<String> {
        self.symbol_names
            .get(symbol.checked_sub(1)? as usize)
            .cloned()
    }

    fn query_blocks(&mut self, region: Bbox) -> Result<Vec<CellSample>, QueryError> {
        self.meters.query_block_calls += 1;
        let width = (region.max.x - region.min.x).max(0) as u64;
        let height = (region.max.y - region.min.y).max(0) as u64;
        let depth = (region.max.z - region.min.z).max(0) as u64;
        if width * height * depth > u64::from(self.budget.max_cells_per_query) {
            return Err(QueryError::BudgetExceeded);
        }
        let mut samples = Vec::new();
        for y in region.min.y..region.max.y {
            for z in region.min.z..region.max.z {
                for x in region.min.x..region.max.x {
                    let cell = self.cell_at((x, y, z));
                    samples.push(CellSample {
                        pos: Vec3i { x, y, z },
                        cell,
                    });
                }
            }
        }
        self.meters.cells_returned += samples.len() as u32;
        Ok(samples)
    }

    fn query_entities(
        &mut self,
        _region: Bbox,
        _cursor: Option<String>,
        _limit: u32,
    ) -> Result<EntityPage, QueryError> {
        // The fixture world has no entities; P1 guests never query them.
        Ok(EntityPage {
            items: Vec::new(),
            next: None,
        })
    }

    fn read_components(
        &mut self,
        _targets: Vec<ComponentTarget>,
    ) -> Result<Vec<ComponentSnapshot>, QueryError> {
        Ok(Vec::new())
    }

    fn probe_item_port(&mut self, _entity: EntityId, _port: String) -> Option<PortEstimate> {
        None
    }
}

impl torse::behavior::diagnostics::Host for FixtureHost {
    fn log(&mut self, level: LogLevel, message: String) {
        if self.meters.log_entries >= self.budget.max_log_entries {
            self.meters.log_dropped += 1;
            return;
        }
        self.meters.log_entries += 1;
        let _ = (level, message);
    }
}

impl torse::behavior::schedule::Host for FixtureHost {
    fn pending_wakeups(&mut self, key_prefix: Option<String>) -> Vec<WakeupSlot> {
        self.wakeups
            .iter()
            .filter(|(_, _, key)| {
                key_prefix
                    .as_ref()
                    .is_none_or(|prefix| key.starts_with(prefix.as_str()))
            })
            .map(|(tick, entity, key)| WakeupSlot {
                entity: EntityId {
                    low: *entity,
                    high: 0,
                },
                tick: *tick,
                key: key.clone(),
            })
            .collect()
    }
}

/// One dispatch loop against one component instance.
pub struct RoundtripReport {
    pub instantiate: Duration,
    pub calls: u32,
    pub plans_returned: u32,
    pub committed_count: u32,
    pub wakeups: u32,
    pub total: Duration,
    pub per_call: Duration,
    pub meters: Meters,
}

/// Builds the engine used by both correctness runs and measurements.
pub fn engine_with(epoch_interruption: bool) -> Result<Engine, String> {
    let mut config = Config::new();
    config.consume_fuel(true);
    if epoch_interruption {
        config.epoch_interruption(true);
    }
    Engine::new(&config).map_err(|error| format!("engine creation failed: {error}"))
}

/// Public accessor for the exported `invoke` interface handle; the generated
/// world struct keeps its per-interface fields private.
pub fn invoke(bindings: &ModBehavior) -> &exports::torse::behavior::invoke::Guest {
    bindings.torse_behavior_invoke()
}

/// Instantiates the component and returns the world bindings with the store.
pub fn instantiate(
    engine: &Engine,
    component: &wasmtime::component::Component,
    budget: CallBudget,
) -> Result<(ModBehavior, Store<FixtureHost>), String> {
    let mut linker: wasmtime::component::Linker<FixtureHost> =
        wasmtime::component::Linker::new(engine);
    ModBehavior::add_to_linker::<FixtureHost, wasmtime::component::HasSelf<FixtureHost>>(
        &mut linker,
        |state| state,
    )
    .map_err(|error| format!("linker setup failed: {error}"))?;
    let mut store = Store::new(engine, FixtureHost::new(budget));
    store
        .set_fuel(budget.fuel)
        .map_err(|error| format!("fuel setup failed: {error}"))?;
    let bindings = ModBehavior::instantiate(&mut store, component, &linker)
        .map_err(|error| format!("instantiation failed: {error}"))?;
    Ok((bindings, store))
}

/// Builds an invocation for the fixture machine at (0, 64, 0).
pub fn machine_invocation(store: &Store<FixtureHost>, tick: u64, event: Event) -> Invocation {
    let host = store.data();
    let schema = host
        .symbols
        .get("roundtrip:counter-state")
        .copied()
        .expect("schema symbol registered");
    Invocation {
        event,
        tick,
        self_: Some(ObjectRef {
            entity: EntityId { low: 1, high: 0 },
            pos: Vec3i { x: 0, y: 64, z: 0 },
            block: 1,
            owned: vec![OwnedState {
                schema,
                fields: Some(vec![Field {
                    key: "count".to_string(),
                    value: Atom::U32(host.committed_count),
                }]),
            }],
        }),
        rng_seed: tick.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x1234_5678,
    }
}

/// Runs `calls` wakeup dispatches, applying each returned plan before the next
/// call, and returns the timing report.
pub fn run_roundtrip(
    engine: &Engine,
    component: &wasmtime::component::Component,
    budget: CallBudget,
    calls: u32,
) -> Result<RoundtripReport, String> {
    let start = Instant::now();
    let (bindings, mut store) = instantiate(engine, component, budget)?;
    let instantiate_elapsed = start.elapsed();

    let invoke = bindings.torse_behavior_invoke();

    let start = Instant::now();
    let mut plans_returned = 0_u32;
    for tick in 1..=u64::from(calls) {
        store
            .set_fuel(budget.fuel)
            .map_err(|error| format!("fuel reset failed: {error}"))?;
        let invocation = machine_invocation(
            &store,
            tick,
            Event::Wakeup(WakeupTarget {
                entity: EntityId { low: 1, high: 0 },
                key: "roundtrip:tick".to_string(),
            }),
        );
        let plan = invoke
            .call_handle_event(&mut store, &invocation)
            .map_err(|error| format!("call at tick {tick} failed: {error}"))?;
        if let Some(plan) = plan {
            plans_returned += 1;
            store.data_mut().apply_plan(&plan);
        }
    }
    let total = start.elapsed();
    let wakeups = store.data().wakeups.len() as u32;
    let committed_count = store.data().committed_count;
    Ok(RoundtripReport {
        instantiate: instantiate_elapsed,
        calls,
        plans_returned,
        committed_count,
        wakeups,
        per_call: total / calls,
        total,
        meters: store.data().meters.clone(),
    })
}

/// Native baseline: the same machine logic as the roundtrip guest, running as
/// plain Rust with no boundary. Used to price the component boundary for DM01.
pub fn run_native(calls: u32) -> Duration {
    let mut symbols: HashMap<&str, u32> = HashMap::new();
    symbols.insert("roundtrip:counter", 1);
    symbols.insert("roundtrip:counter-state", 2);
    let mut blocks: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let floor = *symbols.get("roundtrip:counter").expect("symbol");
    for x in -1..=1 {
        for z in -1..=1 {
            blocks.insert((x, 63, z), floor);
        }
    }
    blocks.insert((0, 64, 0), floor);
    let mut committed_count = 0_u32;
    let mut wakeups: Vec<(u64, u64)> = Vec::new();
    let mut rng_state;
    let start = Instant::now();
    for tick in 1..=u64::from(calls) {
        let schema = symbols
            .get("roundtrip:counter-state")
            .copied()
            .expect("symbol");
        let mut occupied = 0_u32;
        let mut above = false;
        for y in 63..66 {
            for z in -1..2 {
                for x in -1..2 {
                    if blocks.contains_key(&(x, y, z)) {
                        occupied += 1;
                    }
                    if x == 0 && y == 65 && z == 0 {
                        above = blocks.contains_key(&(0, 65, 0));
                    }
                }
            }
        }
        rng_state = tick.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x1234_5678;
        let mut work = 0_u64;
        for index in 0..occupied {
            rng_state = rng_state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = rng_state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            work ^= z ^ u64::from(index);
        }
        let _ = work;
        committed_count += 1;
        wakeups.push((tick + 1, u64::from(committed_count)));
        if tick.is_multiple_of(8) {
            if above {
                blocks.remove(&(0, 65, 0));
            } else {
                blocks.insert((0, 65, 0), schema);
            }
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(wakeups.len(), calls as usize);
    elapsed
}
