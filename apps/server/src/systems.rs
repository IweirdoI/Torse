//! Bind declared native module systems to bounded, durable rules at startup.

use std::collections::{BTreeMap, BTreeSet};

use torse_codec::wire;
use torse_content::{CompiledContent, ModuleSide, SystemDescriptor};
use torse_modules::resolve_modules;
use torse_sim::action::ActionService;
use torse_sim::durable::{EventDraft, FrozenTask, TimedEvent};
use torse_sim::{Phase, SystemSpec, Transaction, WorldAuthority, order_phase};
use torse_types::{Binding, ContentKey, Fault, OutputBudget, Tick};
use torse_world::{BlockStateRegistry, ComponentSchemas};

use crate::execution::{DurableRule, ExecutionHost, ExecutionLimits, ExecutionRecovery, WorkPlan};

/// A trusted startup binding, matched exactly to a module's system declaration.
pub struct SystemBinding {
    pub module: ContentKey,
    pub rule: Box<dyn DurableRule>,
}

struct DeclaredRule {
    owner: ContentKey,
    spec: SystemSpec,
    budget: OutputBudget,
    events: BTreeSet<ContentKey>,
    rule: Box<dyn DurableRule>,
    limits: ExecutionLimits,
}

/// Resolve the module graph and bind every authoritative system exactly once.
/// Client-only declarations are excluded and cannot satisfy server dependencies.
/// The returned rules are serial: native output validation is not a sandbox.
pub fn bind_module_systems(
    content: &CompiledContent,
    bindings: Vec<SystemBinding>,
    limits: ExecutionLimits,
) -> Result<Vec<Box<dyn DurableRule>>, Fault> {
    limits.validate()?;
    let graph = resolve_modules(
        content
            .modules
            .values()
            .filter(|m| m.side != ModuleSide::Client)
            .cloned()
            .collect(),
    )
    .map_err(|errors| {
        Fault::invalid_data(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    let server = |id: &ContentKey| {
        graph
            .modules()
            .get(id)
            .is_some_and(|m| m.side != ModuleSide::Client)
    };
    let mut declarations = BTreeMap::new();
    let mut dependencies = BTreeMap::<ContentKey, BTreeSet<ContentKey>>::new();
    for module in graph.modules().values().filter(|m| server(&m.id)) {
        let mut required: BTreeSet<_> = module.required_modules.iter().cloned().collect();
        for service in &module.required_services {
            let provider = graph
                .modules()
                .values()
                .find(|m| m.provided_services.iter().any(|s| s.id == service.id))
                .ok_or_else(|| Fault::invalid_data("missing system service provider"))?;
            if provider.id != module.id {
                required.insert(provider.id.clone());
            }
        }
        if required.iter().any(|id| !server(id)) {
            return Err(Fault::invalid_data(
                "authoritative module depends on a client-only module",
            ));
        }
        for declaration in &module.registered_systems {
            declarations.insert(
                declaration.id.clone(),
                (module.id.clone(), declaration.clone()),
            );
        }
        dependencies.insert(module.id.clone(), required);
    }
    let mut transitive = BTreeMap::new();
    for owner in dependencies.keys() {
        let mut reached = BTreeSet::new();
        let mut pending: Vec<_> = dependencies[owner].iter().cloned().collect();
        while let Some(dependency) = pending.pop() {
            if dependency == *owner {
                return Err(Fault::invalid_data("module system dependency cycle"));
            }
            if reached.insert(dependency.clone()) {
                pending.extend(dependencies[&dependency].iter().cloned());
            }
        }
        transitive.insert(owner.clone(), reached);
    }
    let dependencies = transitive;
    let mut provided = BTreeMap::new();
    for binding in bindings {
        let spec = binding.rule.spec();
        let (owner, declaration) = declarations.get(&spec.id).ok_or_else(|| {
            Fault::invalid_data(format!("undeclared authoritative system {}", spec.id))
        })?;
        if *owner != binding.module
            || u16::from(declaration.phase) != spec.phase.index_u16()
            || provided.contains_key(&spec.id)
        {
            return Err(Fault::invalid_data(format!(
                "system binding mismatch for {}",
                spec.id
            )));
        }
        provided.insert(spec.id.clone(), (binding, spec));
    }
    if provided.len() != declarations.len() {
        return Err(Fault::invalid_data(
            "missing declared system implementation",
        ));
    }
    let mut specs = Vec::with_capacity(provided.len());
    for (id, (_, spec)) in &provided {
        let mut spec = spec.clone();
        let owner = &declarations[id].0;
        for (dependency_id, (dependency_owner, declaration)) in &declarations {
            if dependencies[owner].contains(dependency_owner)
                && u16::from(declaration.phase) == spec.phase.index_u16()
            {
                spec.after.push(dependency_id.clone());
            }
        }
        spec.before.sort();
        spec.before.dedup();
        spec.after.sort();
        spec.after.dedup();
        specs.push(spec);
    }
    // Freeze topological order into transaction priority, so arbitration cannot
    // undo a dependency by sorting the callback's original task keys afterward.
    let mut sealed = BTreeMap::new();
    for phase in Phase::ALL {
        let phase_specs: Vec<_> = specs.iter().filter(|s| s.phase == phase).cloned().collect();
        for (rank, id) in order_phase(&phase_specs)
            .map_err(|e| Fault::invalid_data(e.to_string()))?
            .into_iter()
            .enumerate()
        {
            let mut spec = phase_specs
                .iter()
                .find(|s| s.id == id)
                .expect("ordered declaration")
                .clone();
            spec.rule_priority = i32::try_from(rank)
                .map_err(|_| Fault::resource_limit("too many declared systems"))?;
            sealed.insert(id, spec);
        }
    }
    let events: BTreeSet<_> = sealed
        .values()
        .filter(|s| s.phase == Phase::Simulation)
        .map(|s| s.id.clone())
        .collect();
    Ok(provided
        .into_iter()
        .map(|(id, (binding, _))| {
            let declaration = &declarations[&id].1;
            Box::new(DeclaredRule {
                owner: binding.module,
                spec: sealed.remove(&id).expect("sealed system"),
                budget: system_budget(declaration, limits),
                events: events.clone(),
                rule: binding.rule,
                limits,
            }) as Box<dyn DurableRule>
        })
        .collect())
}

fn system_budget(declaration: &SystemDescriptor, limits: ExecutionLimits) -> OutputBudget {
    OutputBudget {
        max_items: declaration.max_items.min(limits.runtime.max_items),
        max_bytes: declaration.max_bytes.min(limits.runtime.max_bytes),
    }
}

impl DeclaredRule {
    fn check_events(&self, events: &[EventDraft]) -> Result<(), Fault> {
        if events.iter().any(|e| !self.events.contains(&e.rule)) {
            return Err(Fault::invalid_data(
                "declared system scheduled a non-simulation event",
            ));
        }
        Ok(())
    }
    fn check(&self, world: &WorldAuthority, plans: &mut [WorkPlan]) -> Result<(), Fault> {
        if plans.len() > self.budget.max_items as usize {
            return Err(Fault::resource_limit("declared system plan count"));
        }
        for plan in plans.iter_mut() {
            self.check_events(&plan.schedule)?;
            if let Some(group) = &mut plan.transaction {
                if group.ordering.phase != self.spec.phase
                    || group
                        .members
                        .iter()
                        .any(|m| m.source != self.owner || m.ordering != group.ordering)
                {
                    return Err(Fault::invalid_data(
                        "declared system transaction source or phase mismatch",
                    ));
                }
                group.ordering.rule_priority = self.spec.rule_priority;
                for member in &mut group.members {
                    member.ordering = group.ordering;
                }
            }
        }
        let frozen: Vec<FrozenTask> = plans
            .iter()
            .map(|plan| {
                // Capture by reference without calling a planner or publishing state.
                Ok(FrozenTask {
                    transaction: plan
                        .transaction
                        .as_ref()
                        .map(|g| torse_sim::durable::StoredTransactionGroup::capture(world, g))
                        .transpose()?,
                    request: None,
                    consume: None,
                    schedule: plan.schedule.clone(),
                    cancel: plan.cancel.clone(),
                    status: torse_sim::durable::ReplyStatus::Committed,
                    output: Default::default(),
                })
            })
            .collect::<Result<_, Fault>>()?;
        wire::measure_inner(&frozen, self.budget, self.limits.codec)?;
        Ok(())
    }
}

impl DurableRule for DeclaredRule {
    fn spec(&self) -> SystemSpec {
        self.spec.clone()
    }
    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        let mut plans = self.rule.plan(world, tick)?;
        self.check(world, &mut plans)?;
        Ok(plans)
    }
    fn event(
        &self,
        world: &WorldAuthority,
        tick: Tick,
        event: &TimedEvent,
    ) -> Result<WorkPlan, Fault> {
        if self.spec.phase != Phase::Simulation || event.event.rule != self.spec.id {
            return Err(Fault::invalid_data("declared event target mismatch"));
        }
        let mut plan = self.rule.event(world, tick, event)?;
        self.check(world, std::slice::from_mut(&mut plan))?;
        Ok(plan)
    }
    fn action_events(
        &self,
        world: &WorldAuthority,
        tick: Tick,
        transaction: &Transaction,
    ) -> Result<Vec<EventDraft>, Fault> {
        let events = self.rule.action_events(world, tick, transaction)?;
        self.check_events(&events)?;
        wire::measure_inner(&events, self.budget, self.limits.codec)?;
        Ok(events)
    }
}

impl ExecutionHost {
    /// Assemble authoritative module declarations before exposing a usable host.
    pub fn with_modules(
        binding: Binding,
        world: WorldAuthority,
        actions: ActionService,
        content: &CompiledContent,
        systems: Vec<SystemBinding>,
        limits: ExecutionLimits,
    ) -> Result<Self, Fault> {
        if binding.compatibility.content_digest != content.authority_digest {
            return Err(Fault::incompatible(
                "declared system content digest mismatch",
            ));
        }
        Self::new(
            binding,
            world,
            actions,
            bind_module_systems(content, systems, limits)?,
            limits,
        )
    }
}

impl<'a> ExecutionRecovery<'a> {
    /// Restore with the same declared binding and budget checks as fresh startup.
    #[allow(clippy::too_many_arguments)]
    pub fn with_modules(
        binding: Binding,
        registry: &'a BlockStateRegistry,
        schemas: &'a ComponentSchemas,
        actions: ActionService,
        content: &CompiledContent,
        systems: Vec<SystemBinding>,
        limits: ExecutionLimits,
    ) -> Result<Self, Fault> {
        if binding.compatibility.content_digest != content.authority_digest {
            return Err(Fault::incompatible(
                "declared system content digest mismatch",
            ));
        }
        Ok(Self::new(
            binding,
            registry,
            schemas,
            actions,
            bind_module_systems(content, systems, limits)?,
            limits,
        ))
    }
}
