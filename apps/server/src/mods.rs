//! Mod assembly for the durable execution host.
//!
//! Loads behavior packages from directories, spawns the demo machine world,
//! and adapts each package into one `DurableRule`. Guest plans are decomposed
//! into single-source settlements: this package's edits become one
//! transaction, a transfer becomes its own future event, and guest wakeups
//! become persistent event drafts. Traps settle as empty work with a bounded
//! retry; rejections settle terminally without touching the world.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::execution::WorkPlan;
use torse_inventory::{ItemCatalog, ItemStack, Transfer, plan_transfer};
use torse_mod_host::component::CallBudget;
use torse_mod_host::durable_dispatch::{
    DurableGuest, GuestOutcome, SymbolTable, TransferSpec, convert_plan,
};
use torse_mod_host::package::{self, resolve};
use torse_sim::action::{ActionTarget, ActorContext};
use torse_sim::durable::{EventDraft, TimedEvent};
use torse_sim::{OrderingKey, Phase, SystemSpec, WorldAuthority};
use torse_types::{Compatibility, ContentKey, EntityId, Fault, Tick, TransactionId};
use torse_world::{ComponentFields, ComponentValue, Position, Section, WorldBounds};

use crate::bootstrap;
use crate::execution::DurableRule;

/// Package ids whose guest is built with the `spin` feature (trap tests);
/// every other package builds normally.
pub type SpinPackages = [String];

/// Shared counters exposed to tests; one set for all packages.
#[derive(Clone, Debug, Default)]
pub struct ModStats {
    pub guest_dispatches: BTreeMap<String, u64>,
    pub traps: BTreeMap<String, u64>,
    pub rejections: BTreeMap<String, u64>,
    pub transfer_rejections: BTreeMap<String, u64>,
    pub parks: BTreeMap<String, u64>,
}

fn bump(map: &mut BTreeMap<String, u64>, key: &str) {
    *map.entry(key.to_string()).or_insert(0) += 1;
}

/// Retry backoff in ticks and the bounded attempt budget for trapped guests.
const RETRY_TICKS: u64 = 8;
const MAX_ATTEMPTS: u32 = 8;

/// Mutable rule state: the reusable guest and id allocators. The rule trait
/// takes `&self`, so mutation lives behind a `RefCell`; the server is
/// single-threaded and dispatches never recurse.
struct RuleInner {
    guest: DurableGuest,
    transaction_counter: u128,
    local: u32,
}

/// One behavior package adapted to the durable execution host.
pub struct ModRule {
    package_id: String,
    package: ContentKey,
    rule: ContentKey,
    owned_schemas: Vec<ContentKey>,
    symbols: Arc<SymbolTable>,
    catalog: ItemCatalog,
    actor: ActorContext,
    stats: Rc<RefCell<ModStats>>,
    inner: RefCell<RuleInner>,
}

fn payload_key(package: &str, field: &str) -> ContentKey {
    ContentKey::parse(&format!("{package}:event/{field}")).expect("payload key")
}

fn wakeup_draft(
    rule: &ContentKey,
    package_id: &str,
    tick: u64,
    entity: EntityId,
    key: &str,
    attempt: u32,
) -> EventDraft {
    EventDraft {
        tick: Tick(tick),
        rule: rule.clone(),
        payload: ComponentFields::from([
            (
                payload_key(package_id, "kind"),
                ComponentValue::Text("wakeup".into()),
            ),
            (
                payload_key(package_id, "entity"),
                ComponentValue::Entity(entity),
            ),
            (
                payload_key(package_id, "key"),
                ComponentValue::Text(key.into()),
            ),
            (
                payload_key(package_id, "attempt"),
                ComponentValue::U32(attempt),
            ),
        ]),
    }
}

fn transfer_draft(
    rule: &ContentKey,
    package_id: &str,
    tick: u64,
    transfer: &TransferSpec,
) -> EventDraft {
    EventDraft {
        tick: Tick(tick),
        rule: rule.clone(),
        payload: ComponentFields::from([
            (
                payload_key(package_id, "kind"),
                ComponentValue::Text("transfer".into()),
            ),
            (
                payload_key(package_id, "source"),
                ComponentValue::Entity(transfer.source),
            ),
            (
                payload_key(package_id, "source-slot"),
                ComponentValue::U32(transfer.source_slot),
            ),
            (
                payload_key(package_id, "destination"),
                ComponentValue::Entity(transfer.destination),
            ),
            (
                payload_key(package_id, "destination-slot"),
                ComponentValue::U32(transfer.destination_slot),
            ),
            (
                payload_key(package_id, "count"),
                ComponentValue::U32(transfer.count),
            ),
        ]),
    }
}

fn empty_plan() -> WorkPlan {
    WorkPlan {
        transaction: None,
        schedule: Vec::new(),
        cancel: Vec::new(),
    }
}

impl ModRule {
    fn settle_wakeup(
        &self,
        world: &WorldAuthority,
        tick: Tick,
        entity: EntityId,
        key: &str,
        attempt: u32,
    ) -> WorkPlan {
        let outcome = {
            let mut inner = self.inner.borrow_mut();
            let outcome = inner
                .guest
                .dispatch(world, tick.0, entity, key, &self.owned_schemas);
            if outcome.is_ok() {
                bump(
                    &mut self.stats.borrow_mut().guest_dispatches,
                    &self.package_id,
                );
            }
            outcome
        };
        match outcome {
            Err(_setup) => {
                // Structural dispatch failures are terminal for this event;
                // they never fault the phase or touch the world.
                bump(&mut self.stats.borrow_mut().rejections, &self.package_id);
                empty_plan()
            }
            Ok(GuestOutcome::Trapped(_reason)) => {
                bump(&mut self.stats.borrow_mut().traps, &self.package_id);
                if attempt + 1 >= MAX_ATTEMPTS {
                    bump(&mut self.stats.borrow_mut().parks, &self.package_id);
                    return empty_plan();
                }
                WorkPlan {
                    transaction: None,
                    schedule: vec![wakeup_draft(
                        &self.rule,
                        &self.package_id,
                        tick.0 + RETRY_TICKS,
                        entity,
                        key,
                        attempt + 1,
                    )],
                    cancel: Vec::new(),
                }
            }
            Ok(GuestOutcome::NoPlan) => empty_plan(),
            Ok(GuestOutcome::Plan(plan)) => {
                let stable = entity.0 as u64;
                let (transaction_id, local) = {
                    let mut inner = self.inner.borrow_mut();
                    inner.transaction_counter += 1;
                    let local = inner.local;
                    inner.local = inner.local.wrapping_add(1);
                    (TransactionId(inner.transaction_counter), local)
                };
                match convert_plan(
                    world,
                    &self.symbols,
                    &self.package,
                    &plan,
                    stable,
                    transaction_id,
                    local,
                ) {
                    Ok(parts) => {
                        let mut schedule = Vec::new();
                        if let Some(transfer) = &parts.transfer {
                            schedule.push(transfer_draft(
                                &self.rule,
                                &self.package_id,
                                tick.0 + 1,
                                transfer,
                            ));
                        }
                        for (wakeup_tick, wakeup_entity, wakeup_key) in parts.wakeups {
                            schedule.push(wakeup_draft(
                                &self.rule,
                                &self.package_id,
                                wakeup_tick.max(tick.0 + 1),
                                wakeup_entity,
                                &wakeup_key,
                                0,
                            ));
                        }
                        WorkPlan {
                            transaction: parts.machine,
                            schedule,
                            cancel: Vec::new(),
                        }
                    }
                    Err(_error) => {
                        // Plan-level rejection settles terminally without any
                        // world change; the event stays consumed.
                        bump(&mut self.stats.borrow_mut().rejections, &self.package_id);
                        empty_plan()
                    }
                }
            }
        }
    }

    fn settle_transfer(&self, world: &WorldAuthority, transfer: &TransferSpec) -> WorkPlan {
        let source = transfer.source;
        let request = Transfer {
            source,
            source_slot: transfer.source_slot as usize,
            destination: transfer.destination,
            destination_slot: transfer.destination_slot as usize,
            count: transfer.count,
        };
        let (transaction_id, local) = {
            let mut inner = self.inner.borrow_mut();
            inner.transaction_counter += 1;
            let local = inner.local;
            inner.local = inner.local.wrapping_add(1);
            (TransactionId(inner.transaction_counter), local)
        };
        let ordering = OrderingKey {
            phase: Phase::Simulation,
            rule_priority: 0,
            stable_task_key: source.0 as u64,
            local_sequence: local,
        };
        let view = self.actor.view(world);
        match plan_transfer(&view, &self.catalog, request, transaction_id, ordering) {
            Ok(transaction) => WorkPlan::transaction(transaction),
            Err(_code) => {
                // Business rejection: empty barrier, no retry scheduling; the
                // machine's next cycle retries on its own.
                bump(
                    &mut self.stats.borrow_mut().transfer_rejections,
                    &self.package_id,
                );
                empty_plan()
            }
        }
    }

    fn decode_payload(
        &self,
        event: &TimedEvent,
    ) -> Option<(String, EntityId, String, u32, Option<TransferSpec>)> {
        let payload = &event.event.payload;
        let kind = match payload.get(&payload_key(&self.package_id, "kind")) {
            Some(ComponentValue::Text(kind)) => kind.clone(),
            _ => return None,
        };
        let entity = match payload.get(&payload_key(&self.package_id, "entity")) {
            Some(ComponentValue::Entity(entity)) => *entity,
            _ => EntityId(0),
        };
        let key = match payload.get(&payload_key(&self.package_id, "key")) {
            Some(ComponentValue::Text(key)) => key.clone(),
            _ => String::new(),
        };
        let attempt = match payload.get(&payload_key(&self.package_id, "attempt")) {
            Some(ComponentValue::U32(attempt)) => *attempt,
            _ => 0,
        };
        let transfer = (kind == "transfer")
            .then(|| {
                let field = |name: &str| payload.get(&payload_key(&self.package_id, name));
                Some(TransferSpec {
                    source: match field("source") {
                        Some(ComponentValue::Entity(entity)) => *entity,
                        _ => return None,
                    },
                    source_slot: match field("source-slot") {
                        Some(ComponentValue::U32(value)) => *value,
                        _ => return None,
                    },
                    destination: match field("destination") {
                        Some(ComponentValue::Entity(entity)) => *entity,
                        _ => return None,
                    },
                    destination_slot: match field("destination-slot") {
                        Some(ComponentValue::U32(value)) => *value,
                        _ => return None,
                    },
                    count: match field("count") {
                        Some(ComponentValue::U32(value)) => *value,
                        _ => return None,
                    },
                })
            })
            .flatten();
        Some((kind, entity, key, attempt, transfer))
    }
}

impl DurableRule for ModRule {
    fn spec(&self) -> SystemSpec {
        SystemSpec {
            id: self.rule.clone(),
            phase: Phase::Simulation,
            rule_priority: 0,
            before: Vec::new(),
            after: Vec::new(),
        }
    }

    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        if tick != Tick(0) {
            return Ok(Vec::new());
        }
        // Arm every entity carrying this package's schemas.
        let mut schedule = Vec::new();
        for (_, entity) in world.entities().iter() {
            if self
                .owned_schemas
                .iter()
                .any(|schema| entity.components.component(schema).is_some())
            {
                schedule.push(wakeup_draft(
                    &self.rule,
                    &self.package_id,
                    1,
                    entity.id,
                    "machine:tick",
                    0,
                ));
            }
        }
        if schedule.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![WorkPlan {
            transaction: None,
            schedule,
            cancel: Vec::new(),
        }])
    }

    fn event(
        &self,
        world: &WorldAuthority,
        tick: Tick,
        event: &TimedEvent,
    ) -> Result<WorkPlan, Fault> {
        // The rule stays a pure planner over the world; mutation is confined
        // to guest-local state behind the RefCell.
        let Some((kind, entity, key, attempt, transfer)) = self.decode_payload(event) else {
            return Ok(empty_plan());
        };
        let plan = match kind.as_str() {
            "transfer" => match transfer {
                Some(transfer) => self.settle_transfer(world, &transfer),
                None => empty_plan(),
            },
            _ => self.settle_wakeup(world, tick, entity, &key, attempt),
        };
        Ok(plan)
    }
}

/// The assembled mod world: authority, actions, and behavior rules.
pub struct ModWorld {
    pub world: WorldAuthority,
    pub actions: torse_sim::action::ActionService,
    pub rules: Vec<Box<dyn DurableRule>>,
    pub stats: Rc<RefCell<ModStats>>,
    /// Machine entity per package id, in package order.
    pub machines: Vec<(String, EntityId)>,
    pub input: EntityId,
    pub output: EntityId,
}

fn build_guest_bytes(package: &package::LoadedPackage, spin: bool) -> Result<Vec<u8>, Fault> {
    let Some(build_crate) = &package.manifest.build_crate else {
        return Err(Fault::invalid_data("behavior package without build crate"));
    };
    let workspace = package
        .manifest
        .build_workspace
        .as_ref()
        .map(|relative| package.dir.join(relative))
        .unwrap_or_else(|| package.dir.clone());
    let mut command = std::process::Command::new("cargo");
    command
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("-p")
        .arg(build_crate)
        .current_dir(&workspace);
    if spin {
        command.arg("--features").arg("spin");
    }
    let status = command
        .status()
        .map_err(|error| Fault::invalid_data(format!("cargo spawn: {error}")))?;
    if !status.success() {
        return Err(Fault::invalid_data("guest build failed"));
    }
    let artifact = workspace
        .join("target")
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(build_crate.replace('-', "_"))
        .with_extension("wasm");
    let core = std::fs::read(&artifact)
        .map_err(|error| Fault::invalid_data(format!("guest artifact: {error}")))?;
    torse_mod_host::durable_dispatch::encode_guest(&core).map_err(Fault::invalid_data)
}

/// Loads the packages, assembles the machine demo world, and adapts every
/// behavior package into a durable rule. `stock_input` controls whether the
/// input chest starts with one stone; `build` selects the guest variant.
pub fn mod_world(
    dirs: &[PathBuf],
    stock_input: bool,
    spin: &SpinPackages,
) -> Result<ModWorld, Fault> {
    let mut base = torse_content::m0_fixture_pack();
    torse_sim::block_actions::register_content(&mut base);
    torse_inventory::register_content(&mut base);
    let set = resolve(dirs, &base).map_err(|errors| Fault::invalid_data(format!("{errors:?}")))?;

    let catalog = ItemCatalog::new([(bootstrap::key("base:stone"), 64)])
        .map_err(|e| Fault::invalid_data(format!("{e:?}")))?;
    let schemas = torse_world::ComponentSchemas::build(&set.content);
    let compatibility = Compatibility {
        authority_schema: 4,
        rules_digest: set.content.authority_digest,
        content_digest: set.content.presentation_digest,
    };
    let mut handlers = torse_sim::block_actions::handlers();
    handlers.extend(torse_inventory::handlers(catalog.clone()));
    let mut descriptors = torse_sim::block_actions::descriptors();
    descriptors.extend(torse_inventory::descriptors());
    let actions = torse_sim::action::ActionService::bind(descriptors, handlers, &schemas)
        .map_err(|e| Fault::invalid_data(e.to_string()))?;

    let mut world = WorldAuthority::new(
        bootstrap::WORLD,
        torse_world::BlockStateRegistry::build(&set.content)
            .map_err(|e| Fault::invalid_data(format!("{e:?}")))?,
        schemas.clone(),
        16,
        WorldBounds::new(0, 32).expect("bounds"),
        compatibility,
    )
    .map_err(|e| Fault::invalid_data(format!("{e:?}")))?;
    for pos in bootstrap::scope().sections {
        world
            .store_mut()
            .request(pos)
            .map_err(|e| Fault::invalid_data(e.to_string()))?;
        world
            .store_mut()
            .install(pos, Section::empty(16).expect("side"))
            .map_err(|e| Fault::invalid_data(e.to_string()))?;
        world
            .store_mut()
            .finish_load(pos)
            .map_err(|e| Fault::invalid_data(e.to_string()))?;
    }

    // Chests first so machine components can reference their identities.
    let mut ids = [EntityId(0); 2];
    for (index, id) in ids.iter_mut().enumerate() {
        let position = Position::new(2.0 + index as f64, 2.0, 2.0);
        let handle = world
            .entities_mut()
            .spawn(position)
            .map_err(|e| Fault::invalid_data(format!("{e:?}")))?;
        *id = world.entities().get(handle).expect("entity").id;
    }
    let [input, output] = ids;
    for (entity, stock) in [(input, stock_input), (output, false)] {
        let handle = world.entities().handle_of(entity).expect("entity");
        let mut inventory = torse_inventory::Inventory::new(2)
            .map_err(|e| Fault::invalid_data(format!("{e:?}")))?;
        if stock {
            inventory
                .set(
                    0,
                    Some(ItemStack {
                        item: bootstrap::key("base:stone"),
                        count: 1,
                    }),
                    &catalog,
                )
                .map_err(|e| Fault::invalid_data(format!("{e:?}")))?;
        }
        world
            .entities_mut()
            .components_mut(handle)
            .expect("components")
            .insert(&schemas, &torse_inventory::schema(), inventory.fields())
            .map_err(|e| Fault::invalid_data(e.to_string()))?;
    }

    let stats = Rc::new(RefCell::new(ModStats::default()));
    let mut rules: Vec<Box<dyn DurableRule>> = Vec::new();
    let mut machines: Vec<(String, EntityId)> = Vec::new();
    for package in &set.packages {
        let principal = package.manifest.principal();
        let owned = set
            .owned_schemas
            .get(&principal)
            .cloned()
            .unwrap_or_default();
        if package.manifest.server_entry.is_none() {
            continue;
        }
        let mut extra_targets = Vec::new();
        if !owned.is_empty() {
            // One machine entity per package, referencing the shared chests.
            let position = Position::new(4.0 + machines.len() as f64, 2.0, 2.0);
            let handle = world
                .entities_mut()
                .spawn(position)
                .map_err(|e| Fault::invalid_data(format!("{e:?}")))?;
            let machine = world.entities().get(handle).expect("entity").id;
            let fields = ComponentFields::from([
                (
                    ContentKey::parse(&format!("{}:field/progress", package.manifest.id))
                        .expect("field"),
                    ComponentValue::U32(0),
                ),
                (
                    ContentKey::parse(&format!("{}:field/input", package.manifest.id))
                        .expect("field"),
                    ComponentValue::Entity(input),
                ),
                (
                    ContentKey::parse(&format!("{}:field/output", package.manifest.id))
                        .expect("field"),
                    ComponentValue::Entity(output),
                ),
            ]);
            world
                .entities_mut()
                .components_mut(handle)
                .expect("components")
                .insert(&schemas, &owned[0], fields)
                .map_err(|e| Fault::invalid_data(e.to_string()))?;
            machines.push((package.manifest.id.clone(), machine));
            extra_targets.push(machine);
        }
        let bytes = build_guest_bytes(package, spin.contains(&package.manifest.id))?;
        let extras: Vec<String> = package
            .manifest
            .permissions
            .iter()
            .map(|permission| permission.to_string())
            .chain([format!("{}:module", package.manifest.id)])
            .chain([bootstrap::key("base:stone").to_string()])
            .collect();
        let symbols = Arc::new(SymbolTable::build(&set.content, &extras));
        let guest = DurableGuest::new(&bytes, Arc::clone(&symbols), CallBudget::prototype())
            .map_err(Fault::invalid_data)?;
        let mut actor = ActorContext::new(principal.clone());
        for permission in &package.manifest.permissions {
            actor = actor.permission(permission.clone());
        }
        for entity in ids.iter().chain(&extra_targets) {
            actor = actor.target(ActionTarget::Entity(*entity));
        }
        rules.push(Box::new(ModRule {
            package_id: package.manifest.id.clone(),
            package: principal,
            rule: ContentKey::parse(&format!("{}:rule/behavior", package.manifest.id))
                .expect("rule key"),
            owned_schemas: owned,
            symbols,
            catalog: catalog.clone(),
            actor,
            stats: Rc::clone(&stats),
            inner: RefCell::new(RuleInner {
                guest,
                transaction_counter: 0,
                local: 0,
            }),
        }));
    }

    Ok(ModWorld {
        world,
        actions,
        rules,
        stats,
        machines,
        input,
        output,
    })
}
