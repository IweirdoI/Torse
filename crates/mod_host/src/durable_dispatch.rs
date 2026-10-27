//! Durable dispatch core for behavior packages (Mod durability milestone).
//!
//! The execution host owns the world and calls rule callbacks with a shared
//! reference; a Wasmtime store cannot borrow that world, so every dispatch
//! first copies a bounded neighborhood snapshot into the store data and the
//! guest imports answer from the snapshot. Guests see a limited, read-only
//! view; the returned plan is converted back into transactions by native
//! code holding the real world reference. Every dispatch starts with a fresh
//! instance, so freeze retries and recovery cannot observe uncommitted guest state.

use std::collections::BTreeMap;
use std::sync::Arc;

use torse::behavior::diagnostics::LogLevel;
use torse::behavior::query::{
    CellSample, ComponentSnapshot, ComponentTarget, EntityPage, EntitySample, PortEstimate,
    QueryError,
};
use torse::behavior::schedule::WakeupSlot;
use torse::behavior::types::{Atom, Bbox, BlockRef, Cell, EntityId, Field, Vec3i};
use torse_sim::WorldAuthority;
use torse_sim::{OrderingKey, Phase, Transaction};
use torse_types::{BlockPos, ContentKey, EntityId as EngineEntityId, TransactionId};
use torse_world::{BlockStateId, ComponentEdit, ComponentFields, ComponentValue, TerrainQuery};
use wasmtime::Engine;
use wasmtime::Store;
use wasmtime::component::{Component, bindgen};

use crate::budget::{GuestLimits, HostBudget};
use crate::component::{CallBudget, Meters, encode_component};

bindgen!({
    path: "../mod_api/wit/behavior.wit",
    world: "mod-behavior",
});

use exports::torse::behavior::invoke::{
    Event, Invocation, ObjectRef, OwnedState, Plan, WakeupTarget,
};

crate::budget::boundary_sizes!();

/// Session-wide stable symbol table shared by all dispatches.
#[derive(Clone, Debug, Default)]
pub struct SymbolTable {
    map: BTreeMap<String, u32>,
    keys: Vec<String>,
}

impl SymbolTable {
    pub fn build(content: &torse_content::CompiledContent, extra: &[String]) -> Self {
        let mut table = Self::default();
        let intern = |text: &str, table: &mut SymbolTable| {
            if !table.map.contains_key(text) {
                let symbol = table.keys.len() as u32 + 1;
                table.map.insert(text.to_string(), symbol);
                table.keys.push(text.to_string());
            }
        };
        for block in content.blocks.keys() {
            intern(&block.to_string(), &mut table);
        }
        for state in content.state_schemas.values() {
            for schema in &state.component_schemas {
                intern(&schema.id.to_string(), &mut table);
            }
        }
        for text in extra {
            intern(text, &mut table);
        }
        table
    }

    pub fn symbol_of(&self, key: &str) -> Option<u32> {
        self.map.get(key).copied()
    }

    pub fn key_of(&self, symbol: u32) -> Option<ContentKey> {
        let stored = self.keys.get(symbol.checked_sub(1)? as usize)?;
        ContentKey::parse(stored).ok()
    }
}

/// The bounded read snapshot installed for one dispatch.
struct BoundedView {
    #[expect(dead_code, reason = "kept for diagnostics and future range checks")]
    center: (i64, i64, i64),
    radius: i64,
    cells: BTreeMap<(i64, i64, i64), Cell>,
    entities: Vec<EntitySample>,
    components: BTreeMap<(u128, String), Option<Vec<Field>>>,
}

/// Store data for the durable dispatch: the snapshot plus shared symbols.
pub struct DispatchState {
    view: BoundedView,
    symbols: Arc<SymbolTable>,
    pub meters: Meters,
    budget: HostBudget,
    guest_limits: GuestLimits,
}

/// Outcome of one guest dispatch.
pub enum GuestOutcome {
    Plan(Box<Plan>),
    NoPlan,
    Trapped(String),
}

/// Stable block notification captured with the successful action, including
/// the affected state even when the block has since been removed or replaced.
pub struct BlockNotification {
    pub pos: BlockPos,
    pub definition: ContentKey,
    pub values: Vec<u16>,
    pub placed: bool,
}

enum DispatchTarget<'a> {
    Wakeup(EngineEntityId, &'a str),
    Block(&'a BlockNotification),
}

/// A compiled behavior executor creating fresh instances over bounded snapshots.
pub struct DurableGuest {
    engine: Engine,
    component: Component,
    bindings: ModBehavior,
    store: Store<DispatchState>,
    budget: CallBudget,
    dispatches: u64,
}

/// Snapshot box half-width around the dispatch target.
const VIEW_RADIUS: i64 = 2;

fn wit_entity(engine: EngineEntityId) -> EntityId {
    EntityId {
        low: engine.0 as u64,
        high: (engine.0 >> 64) as u64,
    }
}

fn engine_entity(wit: &EntityId) -> EngineEntityId {
    EngineEntityId((wit.high as u128) << 64 | wit.low as u128)
}

fn snapshot_cell(world: &WorldAuthority, symbols: &SymbolTable, pos: BlockPos) -> Cell {
    match world.store().query(pos) {
        TerrainQuery::Ready(BlockStateId::EMPTY) => Cell::Empty,
        TerrainQuery::Ready(state) => {
            let definition = world
                .registry()
                .definition_key(state)
                .and_then(|key| symbols.symbol_of(&key.to_string()))
                .unwrap_or(0);
            let values = world
                .registry()
                .state_values(state)
                .map(Vec::from)
                .unwrap_or_default();
            Cell::Occupied(BlockRef {
                definition,
                state_values: values,
            })
        }
        TerrainQuery::NotLoaded | TerrainQuery::OutOfBounds => Cell::NotLoaded,
    }
}

fn snapshot_fields(
    world: &WorldAuthority,
    symbols: &SymbolTable,
    entity: EngineEntityId,
    schema: &ContentKey,
    budget: &mut HostBudget,
) -> Option<Vec<Field>> {
    let handle = world.entities().handle_of(entity)?;
    let fields = world.entities().get(handle)?.components.component(schema)?;
    budget.work(fields.len() as u64).ok()?;
    budget
        .output(crate::budget::flat_source_size(fields)?)
        .ok()?;
    let mut flat = Vec::with_capacity(fields.len());
    for (field_key, value) in fields {
        let atom = match value {
            ComponentValue::Bool(value) => Atom::Bool(*value),
            ComponentValue::U32(value) => Atom::U32(*value),
            ComponentValue::I64(value) => Atom::I64(*value),
            ComponentValue::Content(content) => {
                Atom::Content(symbols.symbol_of(&content.to_string())?)
            }
            ComponentValue::Entity(entity) => Atom::Entity(wit_entity(*entity)),
            ComponentValue::Text(text) => Atom::Text(text.clone()),
            // Nested values do not cross the flat-atom boundary.
            ComponentValue::List(_) | ComponentValue::Record(_) => return None,
        };
        flat.push(Field {
            key: field_key.to_string(),
            value: atom,
        });
    }
    Some(flat)
}

impl DurableGuest {
    /// Compiles and instantiates the guest component; `symbols` stay shared.
    pub fn new(
        component_bytes: &[u8],
        symbols: Arc<SymbolTable>,
        budget: CallBudget,
    ) -> Result<Self, String> {
        let engine = crate::component::engine_with(false)?;
        let component = Component::new(&engine, component_bytes)
            .map_err(|error| format!("component compile failed: {error}"))?;
        let (bindings, store) = Self::build(&engine, &component, symbols, budget)?;
        Ok(Self {
            engine,
            component,
            bindings,
            store,
            budget,
            dispatches: 0,
        })
    }

    fn build(
        engine: &Engine,
        component: &Component,
        symbols: Arc<SymbolTable>,
        budget: CallBudget,
    ) -> Result<(ModBehavior, Store<DispatchState>), String> {
        let mut linker: wasmtime::component::Linker<DispatchState> =
            wasmtime::component::Linker::new(engine);
        ModBehavior::add_to_linker::<DispatchState, wasmtime::component::HasSelf<DispatchState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("linker setup failed: {error}"))?;
        let view = BoundedView {
            center: (0, 0, 0),
            radius: -1,
            cells: BTreeMap::new(),
            entities: Vec::new(),
            components: BTreeMap::new(),
        };
        let mut store = Store::new(
            engine,
            DispatchState {
                view,
                symbols,
                meters: Meters::default(),
                budget: HostBudget::new(budget),
                guest_limits: GuestLimits::new(budget),
            },
        );
        store.limiter(|host| &mut host.guest_limits);
        store.set_hostcall_fuel(budget.copy_fuel());
        store
            .set_fuel(budget.fuel)
            .map_err(|error| format!("fuel setup failed: {error}"))?;
        let bindings = ModBehavior::instantiate(&mut store, component, &linker)
            .map_err(|error| format!("instantiation failed: {error}"))?;
        if store.data().budget.exceeded() {
            return Err("component initialization exceeded host budget".to_string());
        }
        Ok((bindings, store))
    }

    /// Number of guest dispatches served; used to prove settled tasks do not
    /// re-run the guest after recovery.
    pub fn dispatches(&self) -> u64 {
        self.dispatches
    }

    /// Discards all guest state before planning, and after a trap.
    fn rebuild(&mut self) -> Result<(), String> {
        let symbols = Arc::clone(&self.store.data().symbols);
        let (bindings, store) = Self::build(&self.engine, &self.component, symbols, self.budget)?;
        self.bindings = bindings;
        self.store = store;
        Ok(())
    }

    /// Snapshots a bounded view and dispatches one wakeup to the guest.
    pub fn dispatch(
        &mut self,
        world: &WorldAuthority,
        tick: u64,
        entity: EngineEntityId,
        key: &str,
        owned_schemas: &[ContentKey],
    ) -> Result<GuestOutcome, String> {
        self.dispatch_target(
            world,
            tick,
            DispatchTarget::Wakeup(entity, key),
            owned_schemas,
        )
    }

    /// Dispatch a block event with entity zero as the reserved block-only
    /// object identity. Such an object has no owned entity components.
    pub fn dispatch_block(
        &mut self,
        world: &WorldAuthority,
        tick: u64,
        notification: &BlockNotification,
        owned_schemas: &[ContentKey],
    ) -> Result<GuestOutcome, String> {
        self.dispatch_target(
            world,
            tick,
            DispatchTarget::Block(notification),
            owned_schemas,
        )
    }

    fn dispatch_target(
        &mut self,
        world: &WorldAuthority,
        tick: u64,
        target: DispatchTarget<'_>,
        owned_schemas: &[ContentKey],
    ) -> Result<GuestOutcome, String> {
        self.rebuild()?;
        self.store.data_mut().budget.reset();
        self.dispatches += 1;
        let symbols = Arc::clone(&self.store.data().symbols);

        let (entity, center, event, block) = match target {
            DispatchTarget::Wakeup(entity, key) => {
                let handle = world
                    .entities()
                    .handle_of(entity)
                    .ok_or_else(|| format!("dispatch target {entity:?} does not exist"))?;
                let position = world.entities().get(handle).expect("entity").position;
                (
                    entity,
                    (
                        position.x.floor() as i64,
                        position.y.floor() as i64,
                        position.z.floor() as i64,
                    ),
                    Event::Wakeup(WakeupTarget {
                        entity: wit_entity(entity),
                        key: key.to_string(),
                    }),
                    0,
                )
            }
            DispatchTarget::Block(notification) => {
                let definition = symbols
                    .symbol_of(&notification.definition.to_string())
                    .ok_or("unknown block notification symbol")?;
                let event = if notification.placed {
                    Event::BlockPlaced
                } else {
                    Event::BlockBroken(Cell::Occupied(BlockRef {
                        definition,
                        state_values: notification.values.clone(),
                    }))
                };
                let pos = notification.pos;
                (EngineEntityId(0), (pos.x, pos.y, pos.z), event, definition)
            }
        };

        for coordinate in [center.0, center.1, center.2] {
            if coordinate.checked_sub(VIEW_RADIUS).is_none()
                || coordinate.checked_add(VIEW_RADIUS).is_none()
            {
                return Err("dispatch neighborhood exceeds coordinate range".into());
            }
        }
        let mut cells = BTreeMap::new();
        for y in center.1 - VIEW_RADIUS..=center.1 + VIEW_RADIUS {
            for z in center.2 - VIEW_RADIUS..=center.2 + VIEW_RADIUS {
                for x in center.0 - VIEW_RADIUS..=center.0 + VIEW_RADIUS {
                    self.store
                        .data_mut()
                        .budget
                        .work(1)
                        .map_err(|_| "snapshot work budget exceeded".to_string())?;
                    let cell = snapshot_cell(world, &symbols, BlockPos::new(x, y, z));
                    self.store
                        .data_mut()
                        .budget
                        .output(64 + cell_size(&cell))
                        .map_err(|_| "snapshot output budget exceeded".to_string())?;
                    cells.insert((x, y, z), cell);
                }
            }
        }
        let in_box = |x: i64, y: i64, z: i64| {
            center.0.abs_diff(x) <= VIEW_RADIUS as u64
                && center.1.abs_diff(y) <= VIEW_RADIUS as u64
                && center.2.abs_diff(z) <= VIEW_RADIUS as u64
        };
        let mut entities = Vec::new();
        let mut components = BTreeMap::new();
        self.store
            .data_mut()
            .budget
            .work(world.entities().allocated_slots() as u64)
            .map_err(|_| "snapshot work budget exceeded".to_string())?;
        for (_, candidate) in world.entities().iter() {
            let position = candidate.position;
            let x = position.x.floor() as i64;
            let y = position.y.floor() as i64;
            let z = position.z.floor() as i64;
            if !in_box(x, y, z) {
                continue;
            }
            self.store
                .data_mut()
                .budget
                .output(64)
                .map_err(|_| "snapshot output budget exceeded".to_string())?;
            entities.push(EntitySample {
                id: wit_entity(candidate.id),
                position: Vec3i { x, y, z },
            });
            for schema in owned_schemas {
                self.store
                    .data_mut()
                    .budget
                    .work(1)
                    .map_err(|_| "snapshot work budget exceeded".to_string())?;
                let fields = snapshot_fields(
                    world,
                    &symbols,
                    candidate.id,
                    schema,
                    &mut self.store.data_mut().budget,
                );
                components.insert((candidate.id.0, schema.to_string()), fields);
            }
        }

        // Build the invocation: self carries the owned flat state.
        let owned = owned_schemas
            .iter()
            .filter_map(|schema| {
                let fields = components.get(&(entity.0, schema.to_string()))?.clone();
                let symbol = symbols.symbol_of(&schema.to_string())?;
                Some(OwnedState {
                    schema: symbol,
                    fields,
                })
            })
            .collect::<Vec<_>>();
        let invocation = Invocation {
            event,
            tick,
            self_: Some(ObjectRef {
                entity: wit_entity(entity),
                pos: Vec3i {
                    x: center.0,
                    y: center.1,
                    z: center.2,
                },
                block,
                owned,
            }),
            rng_seed: tick.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5eed_cafe,
        };

        self.store
            .data_mut()
            .budget
            .output(invocation_size(&invocation))
            .map_err(|_| "snapshot output budget exceeded".to_string())?;

        {
            let state = self.store.data_mut();
            state.view = BoundedView {
                center,
                radius: VIEW_RADIUS,
                cells,
                entities,
                components,
            };
        }
        self.store
            .set_fuel(self.budget.fuel)
            .map_err(|error| format!("fuel reset failed: {error}"))?;
        let result = {
            self.bindings
                .torse_behavior_invoke()
                .call_handle_event(&mut self.store, &invocation)
        };
        if let Ok(plan) = &result
            && self
                .store
                .data_mut()
                .budget
                .input(plan.as_ref().map_or(0, plan_size))
                .is_err()
        {
            self.rebuild()?;
            return Ok(GuestOutcome::Trapped("host budget exceeded".to_string()));
        }
        match result {
            Err(trap) => {
                let reason = trap
                    .chain()
                    .map(|cause| cause.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                self.rebuild()?;
                Ok(GuestOutcome::Trapped(reason))
            }
            Ok(None) => Ok(GuestOutcome::NoPlan),
            Ok(Some(plan)) => Ok(GuestOutcome::Plan(Box::new(plan))),
        }
    }
}

impl torse::behavior::types::Host for DispatchState {}

impl torse::behavior::query::Host for DispatchState {
    fn resolve_symbol(&mut self, key: String) -> Option<u32> {
        self.budget.enter(key.len() as u64).ok()?;
        self.meters.resolve_calls = self.meters.resolve_calls.saturating_add(1);
        self.budget.output(4).ok()?;
        self.symbols.symbol_of(&key)
    }

    fn symbol_key(&mut self, symbol: u32) -> Option<String> {
        self.budget.enter(4).ok()?;
        let key = self.symbols.keys.get(symbol.checked_sub(1)? as usize)?;
        self.budget.output(key.len() as u64).ok()?;
        Some(key.clone())
    }

    fn query_blocks(&mut self, region: Bbox) -> Result<Vec<CellSample>, QueryError> {
        self.meters.query_block_calls = self.meters.query_block_calls.saturating_add(1);
        let count = self
            .budget
            .region(
                [region.min.x, region.min.y, region.min.z],
                [region.max.x, region.max.y, region.max.z],
                self.view.cells_available(),
            )
            .map_err(|_| QueryError::BudgetExceeded)?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut samples = Vec::new();
        for y in region.min.y..region.max.y {
            for z in region.min.z..region.max.z {
                for x in region.min.x..region.max.x {
                    let cell = self.view.cells.get(&(x, y, z)).unwrap_or(&Cell::NotLoaded);
                    self.budget
                        .output(24 + cell_size(cell))
                        .map_err(|_| QueryError::BudgetExceeded)?;
                    samples.push(CellSample {
                        pos: Vec3i { x, y, z },
                        cell: cell.clone(),
                    });
                }
            }
        }
        self.meters.cells_returned = self
            .meters
            .cells_returned
            .saturating_add(samples.len() as u32);
        Ok(samples)
    }

    fn query_entities(
        &mut self,
        region: Bbox,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<EntityPage, QueryError> {
        self.budget
            .enter(52 + cursor.as_ref().map_or(0, |value| value.len() as u64))
            .map_err(|_| QueryError::BudgetExceeded)?;
        if limit > self.budget.limits.max_cells_per_query {
            return self.budget.reject().map_err(|_| QueryError::BudgetExceeded);
        }
        self.budget
            .work(self.view.entities.len() as u64)
            .map_err(|_| QueryError::BudgetExceeded)?;
        let skip = match &cursor {
            None => 0_usize,
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|_| QueryError::InvalidCursor)?,
        };
        let in_region = |sample: &EntitySample| {
            sample.position.x >= region.min.x
                && sample.position.x < region.max.x
                && sample.position.y >= region.min.y
                && sample.position.y < region.max.y
                && sample.position.z >= region.min.z
                && sample.position.z < region.max.z
        };
        let matched = self.view.entities.iter().filter(|e| in_region(e));
        let mut items = Vec::new();
        let mut total = 0_usize;
        for sample in matched {
            if total >= skip && items.len() < limit as usize {
                self.budget
                    .output(40)
                    .map_err(|_| QueryError::BudgetExceeded)?;
                items.push(EntitySample {
                    id: sample.id,
                    position: sample.position,
                });
            }
            total += 1;
        }
        let next = if skip.saturating_add(items.len()) < total {
            Some(skip.saturating_add(items.len()).to_string())
        } else {
            None
        };
        Ok(EntityPage { items, next })
    }

    fn read_components(
        &mut self,
        targets: Vec<ComponentTarget>,
    ) -> Result<Vec<ComponentSnapshot>, QueryError> {
        self.budget
            .enter((targets.len() as u64).saturating_mul(24))
            .and_then(|_| self.budget.work(targets.len() as u64))
            .map_err(|_| QueryError::BudgetExceeded)?;
        let mut snapshots = Vec::with_capacity(targets.len());
        for target in targets {
            let schema = self
                .symbols
                .key_of(target.schema)
                .ok_or(QueryError::InvalidCursor)?;
            let fields = self
                .view
                .components
                .get(&(engine_entity(&target.entity).0, schema.to_string()))
                .and_then(|fields| fields.as_deref());
            self.budget
                .work(fields.map_or(0, |fields| fields.len() as u64))
                .and_then(|_| self.budget.output(40 + fields.map_or(0, fields_size)))
                .map_err(|_| QueryError::BudgetExceeded)?;
            snapshots.push(ComponentSnapshot {
                entity: target.entity,
                fields: fields.map(Vec::from),
            });
        }
        Ok(snapshots)
    }

    fn probe_item_port(&mut self, _entity: EntityId, port: String) -> Option<PortEstimate> {
        self.budget.enter(16 + port.len() as u64).ok()?;
        // Port estimates need live inventory access; not part of the durable
        // snapshot.
        None
    }
}

impl torse::behavior::diagnostics::Host for DispatchState {
    fn log(&mut self, level: LogLevel, message: String) {
        if self.budget.enter(4 + message.len() as u64).is_err()
            || self.budget.logs >= self.budget.limits.max_log_entries
        {
            self.meters.log_dropped = self.meters.log_dropped.saturating_add(1);
            return;
        }
        self.budget.logs += 1;
        self.meters.log_entries = self.meters.log_entries.saturating_add(1);
        let _ = (level, message);
    }
}

impl BoundedView {
    fn cells_available(&self) -> u32 {
        if self.radius < 0 {
            0
        } else {
            let side = 2 * self.radius as u64 + 1;
            (side * side * side) as u32
        }
    }
}

impl torse::behavior::schedule::Host for DispatchState {
    fn pending_wakeups(&mut self, key_prefix: Option<String>) -> Vec<WakeupSlot> {
        let _ = self
            .budget
            .enter(key_prefix.as_ref().map_or(0, |key| key.len() as u64));
        // The durable queue is not guest-visible; the host owns scheduling.
        Vec::new()
    }
}

/// One transfer request converted by the native inventory planner before the
/// caller freezes the machine and inventory members as one atomic group.
#[derive(Clone, Debug)]
pub struct TransferSpec {
    pub source: EngineEntityId,
    pub source_slot: u32,
    pub destination: EngineEntityId,
    pub destination_slot: u32,
    pub count: u32,
}

/// Decomposed guest plan.
pub struct PlanParts {
    pub machine: Option<Transaction>,
    pub transfer: Option<TransferSpec>,
    pub wakeups: Vec<(u64, EngineEntityId, String)>,
}

/// Why a plan could not be decomposed.
#[derive(Debug)]
pub enum ConvertError {
    ForeignSchema(ContentKey),
    UnknownSymbol(u32),
    UnknownSchema(u32),
    UnknownEntity(EngineEntityId),
    TooManyTransfers,
    MissingExpected(BlockPos),
    BadField(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForeignSchema(schema) => {
                write!(f, "schema {schema} is owned by another module")
            }
            Self::UnknownSymbol(symbol) => write!(f, "unknown content symbol {symbol}"),
            Self::UnknownSchema(symbol) => write!(f, "unknown schema symbol {symbol}"),
            Self::UnknownEntity(entity) => write!(f, "entity {entity:?} does not exist"),
            Self::TooManyTransfers => write!(f, "at most one transfer per plan"),
            Self::MissingExpected(pos) => write!(f, "no readable state at {pos:?}"),
            Self::BadField(detail) => write!(f, "bad field: {detail}"),
        }
    }
}

fn atom_to_value(symbols: &SymbolTable, atom: &Atom) -> Result<ComponentValue, ConvertError> {
    match atom {
        Atom::Bool(value) => Ok(ComponentValue::Bool(*value)),
        Atom::U32(value) => Ok(ComponentValue::U32(*value)),
        Atom::I64(value) => Ok(ComponentValue::I64(*value)),
        Atom::Text(text) => Ok(ComponentValue::Text(text.clone())),
        Atom::Content(symbol) => symbols
            .key_of(*symbol)
            .map(ComponentValue::Content)
            .ok_or(ConvertError::UnknownSymbol(*symbol)),
        Atom::Entity(entity) => Ok(ComponentValue::Entity(engine_entity(entity))),
    }
}

fn flat_to_fields(symbols: &SymbolTable, flat: &[Field]) -> Result<ComponentFields, ConvertError> {
    let mut fields = ComponentFields::new();
    for field in flat {
        let key = ContentKey::parse(&field.key)
            .map_err(|error| ConvertError::BadField(format!("{}: {error}", field.key)))?;
        let value = atom_to_value(symbols, &field.value)?;
        fields.insert(key, value);
    }
    Ok(fields)
}

fn cell_to_state(
    world: &WorldAuthority,
    symbols: &SymbolTable,
    cell: &Cell,
) -> Result<BlockStateId, ConvertError> {
    match cell {
        Cell::Empty => Ok(BlockStateId::EMPTY),
        Cell::NotLoaded => Err(ConvertError::MissingExpected(BlockPos::new(0, 0, 0))),
        Cell::Occupied(block) => {
            let definition = symbols
                .key_of(block.definition)
                .ok_or(ConvertError::UnknownSymbol(block.definition))?;
            world
                .registry()
                .state_of(&definition, &block.state_values)
                .ok_or(ConvertError::UnknownSchema(block.definition))
        }
    }
}

/// Converts a guest plan into single-source parts against the live world.
///
/// Block edits and this package's component edits become one transaction
/// sourced from `package`; a single transfer is returned separately so the
/// caller can obtain an inventory member for the same group. This mirrors the authority
/// bridge's ownership rules without committing anything.
pub fn convert_plan(
    world: &WorldAuthority,
    symbols: &SymbolTable,
    package: &ContentKey,
    plan: &Plan,
    stable_task_key: u64,
    id: TransactionId,
    local: u32,
) -> Result<PlanParts, ConvertError> {
    let ordering = OrderingKey {
        phase: Phase::Simulation,
        rule_priority: 0,
        stable_task_key,
        local_sequence: local,
    };
    let mut transaction = Transaction::new(id, package.clone(), ordering);
    let mut has_work = false;

    for edit in &plan.edits {
        has_work = true;
        let pos = BlockPos::new(edit.pos.x, edit.pos.y, edit.pos.z);
        let expected = match &edit.expected {
            Some(cell) => cell_to_state(world, symbols, cell)?,
            None => match world.store().query(pos) {
                TerrainQuery::Ready(state) => state,
                _ => return Err(ConvertError::MissingExpected(pos)),
            },
        };
        let new = cell_to_state(world, symbols, &edit.new)?;
        transaction.read_block(pos, expected);
        transaction.write_block(pos, new);
    }

    for edit in &plan.component_edits {
        has_work = true;
        let schema = symbols
            .key_of(edit.schema)
            .ok_or(ConvertError::UnknownSchema(edit.schema))?;
        match world.schemas().get(&schema) {
            Some(declared) if declared.owner_module == *package => {}
            Some(_) => return Err(ConvertError::ForeignSchema(schema)),
            None => return Err(ConvertError::UnknownSchema(edit.schema)),
        }
        let entity = engine_entity(&edit.entity);
        if world.entities().handle_of(entity).is_none() {
            return Err(ConvertError::UnknownEntity(entity));
        }
        let previous = match &edit.previous {
            Some(flat) => Some(flat_to_fields(symbols, flat)?),
            None => None,
        };
        let fields = match &edit.fields {
            Some(flat) => Some(flat_to_fields(symbols, flat)?),
            None => None,
        };
        transaction.components.push(ComponentEdit {
            id: entity,
            schema,
            previous,
            fields,
        });
    }

    let transfer = match plan.transfers.as_slice() {
        [] => None,
        [only] => Some(TransferSpec {
            source: engine_entity(&only.source),
            source_slot: only.source_slot,
            destination: engine_entity(&only.destination),
            destination_slot: only.destination_slot,
            count: only.count,
        }),
        _ => return Err(ConvertError::TooManyTransfers),
    };

    let wakeups = plan
        .wakeups
        .iter()
        .map(|wakeup| {
            (
                wakeup.tick,
                engine_entity(&wakeup.entity),
                wakeup.key.clone(),
            )
        })
        .collect();

    Ok(PlanParts {
        machine: has_work.then_some(transaction),
        transfer,
        wakeups,
    })
}

/// Encodes a behavior-package component from core wasm bytes; shared with the
/// package loader for a single build path.
pub fn encode_guest(core: &[u8]) -> Result<Vec<u8>, String> {
    encode_component(core)
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use torse::behavior::query::Host;

    fn state(budget: CallBudget) -> DispatchState {
        DispatchState {
            view: BoundedView {
                center: (0, 0, 0),
                radius: 2,
                cells: [((0, 0, 0), Cell::Empty)].into(),
                entities: Vec::new(),
                components: BTreeMap::new(),
            },
            symbols: Arc::new(SymbolTable::default()),
            meters: Meters::default(),
            budget: HostBudget::new(budget),
            guest_limits: GuestLimits::new(budget),
        }
    }

    fn region(min: [i64; 3], max: [i64; 3]) -> Bbox {
        Bbox {
            min: Vec3i {
                x: min[0],
                y: min[1],
                z: min[2],
            },
            max: Vec3i {
                x: max[0],
                y: max[1],
                z: max[2],
            },
        }
    }

    #[test]
    fn durable_queries_reject_extremes_before_cloning() {
        let mut host = state(CallBudget::prototype());
        for (min, max) in [
            ([0; 3], [1_i64 << 32, 1_i64 << 32, 1]),
            ([i64::MIN, 0, 0], [i64::MAX, 1, 1]),
        ] {
            host.budget.reset();
            assert!(matches!(
                host.query_blocks(region(min, max)),
                Err(QueryError::BudgetExceeded)
            ));
        }
        host.budget.reset();
        assert!(
            host.query_blocks(region([0; 3], [0, i64::MAX, i64::MAX]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(host.meters.cells_returned, 0);
        assert_eq!(host.view.cells.len(), 1);
    }

    #[test]
    fn durable_totals_outputs_and_huge_cursor_are_bounded() {
        let mut host = state(CallBudget {
            max_host_work: 1,
            ..CallBudget::prototype()
        });
        assert_eq!(host.query_blocks(region([0; 3], [1; 3])).unwrap().len(), 1);
        assert!(matches!(
            host.query_blocks(region([0; 3], [1; 3])),
            Err(QueryError::BudgetExceeded)
        ));
        let mut host = state(CallBudget {
            max_output_bytes: 1,
            ..CallBudget::prototype()
        });
        assert!(matches!(
            host.query_blocks(region([0; 3], [1; 3])),
            Err(QueryError::BudgetExceeded)
        ));
        let mut host = state(CallBudget::prototype());
        let page = host
            .query_entities(
                region([i64::MIN; 3], [i64::MAX; 3]),
                Some(usize::MAX.to_string()),
                1,
            )
            .unwrap();
        assert!(page.items.is_empty());
        assert!(page.next.is_none());
        assert!(matches!(
            host.query_entities(region([0; 3], [1; 3]), None, u32::MAX),
            Err(QueryError::BudgetExceeded)
        ));
        let mut host = state(CallBudget {
            max_output_bytes: 1,
            ..CallBudget::prototype()
        });
        host.symbols = Arc::new(SymbolTable {
            map: [("probe:schema/state".to_string(), 1)].into(),
            keys: vec!["probe:schema/state".to_string()],
        });
        assert!(matches!(
            host.read_components(vec![ComponentTarget {
                entity: EntityId { low: 1, high: 0 },
                schema: 1,
            }]),
            Err(QueryError::BudgetExceeded)
        ));
    }
}
