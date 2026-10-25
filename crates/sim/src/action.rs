//! Frozen native action dispatch with a host-issued authorization context.
//!
//! Commands, UI, network adapters, and Mods supply the same typed request. The
//! origin label grants no authority. A host derives the actor and finite target
//! grants from an authenticated session or trusted task, then dispatches at the
//! declared phase. Mutation output is only a transaction plan, never a success
//! acknowledgment; journal reservation and durable confirmation belong to B6.

use std::collections::{BTreeMap, BTreeSet};

use torse_actions::{ActionDescriptor, ActionKind, ActionRegistry, validate_actions};
use torse_types::{BlockPos, ContentKey, EntityId, RejectCode, SectionPos, TransactionId};
use torse_world::{
    BlockStateId, ComponentFields, ComponentSchemas, ComponentSet, ComponentValue, TerrainQuery,
    component_size,
};

use crate::{OrderingKey, Phase, Transaction, WorldAuthority};

/// Add one optional native module's schema and action declarations to a pack.
///
/// Content compilation remains responsible for duplicate identities and missing
/// references. Adding this metadata does not bind or start native handlers.
pub fn register_action_module(
    pack: &mut torse_content::ContentPack,
    owner: ContentKey,
    state: ContentKey,
    components: Vec<torse_content::ComponentSchema>,
    actions: Vec<ActionDescriptor>,
    capabilities: Vec<torse_content::CapabilityDescriptor>,
) {
    use torse_content::{ModuleDescriptor, ModuleSide, SourceRef, Sourced, StateSchema};
    let source = SourceRef::new("native/module", 0);
    pack.modules.push(Sourced::new(
        source.clone(),
        ModuleDescriptor {
            id: owner,
            module_version: 1,
            framework_api: 1,
            side: ModuleSide::Server,
            required_modules: Vec::new(),
            optional_modules: Vec::new(),
            provided_services: Vec::new(),
            required_services: Vec::new(),
            registered_schemas: vec![state.clone()],
            registered_capabilities: capabilities.iter().map(|c| c.id.clone()).collect(),
            registered_actions: actions.iter().map(|a| a.id.clone()).collect(),
            registered_systems: Vec::new(),
            requested_permissions: Vec::new(),
            configuration_schema: None,
            lifecycle_binding: None,
        },
    ));
    pack.state_schemas.push(Sourced::new(
        source.clone(),
        StateSchema {
            id: state,
            schema_version: 1,
            finite_fields: Vec::new(),
            component_schemas: components,
        },
    ));
    pack.actions
        .extend(actions.into_iter().map(|a| Sourced::new(source.clone(), a)));
    pack.capabilities.extend(
        capabilities
            .into_iter()
            .map(|c| Sourced::new(source.clone(), c)),
    );
}

/// Input adapter metadata, deliberately independent of authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionOrigin {
    Console,
    Ui,
    Network,
    Mod,
}

/// Primary target named by one request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionTarget {
    Block(BlockPos),
    Entity(EntityId),
}

/// Upper bound on section grants one actor may carry at registration.
pub const MAX_ACTOR_SECTIONS: usize = 64;

/// Identity and finite access grants established by the trusted host.
///
/// Never deserialize this object from client assertions. A grant can represent
/// a reach/visibility check, container ownership, or a scheduled machine's ports.
/// Handlers receive read-only world access; all output targets are checked again.
#[derive(Clone, Debug)]
pub struct ActorContext {
    principal: ContentKey,
    permissions: BTreeSet<ContentKey>,
    targets: BTreeSet<TargetKey>,
    sections: BTreeSet<SectionPos>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum TargetKey {
    Block(BlockPos),
    Entity(EntityId),
}

impl ActorContext {
    /// Create a host identity with no permissions or object access.
    pub fn new(principal: ContentKey) -> Self {
        Self {
            principal,
            permissions: BTreeSet::new(),
            targets: BTreeSet::new(),
            sections: BTreeSet::new(),
        }
    }
    /// Add an independently verified permission.
    pub fn permission(mut self, key: ContentKey) -> Self {
        self.permissions.insert(key);
        self
    }
    /// Add one independently verified target grant.
    pub fn target(mut self, target: ActionTarget) -> Self {
        self.targets.insert(match target {
            ActionTarget::Block(pos) => TargetKey::Block(pos),
            ActionTarget::Entity(id) => TargetKey::Entity(id),
        });
        self
    }
    /// Grant every cell of one section. Hosts use this for scope-wide access
    /// such as editing a subscribed area; it never widens entity access.
    /// Registration rejects actors carrying more than [`MAX_ACTOR_SECTIONS`]
    /// grants; the builder itself stays infallible.
    pub fn section(mut self, pos: SectionPos) -> Self {
        self.sections.insert(pos);
        self
    }
    /// How many section grants this actor carries.
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
    /// Authenticated caller identity, available for module-specific policies.
    pub fn principal(&self) -> &ContentKey {
        &self.principal
    }
    /// Test a host-issued permission for a native module operation.
    pub fn permits(&self, permission: &ContentKey) -> bool {
        self.permissions.contains(permission)
    }
    /// Open the same restricted view for a scheduled native operation.
    pub fn view<'a>(&'a self, world: &'a WorldAuthority) -> ActionView<'a> {
        ActionView { world, actor: self }
    }
    /// Whether the host granted access to this exact target. Section grants
    /// are not consulted; use [`ActorContext::allows_block`] for cells.
    pub fn allows(&self, target: ActionTarget) -> bool {
        self.targets.contains(&match target {
            ActionTarget::Block(pos) => TargetKey::Block(pos),
            ActionTarget::Entity(id) => TargetKey::Entity(id),
        })
    }
    /// Whether a block target is granted by exact cell or a covering
    /// section grant. `side` is the world's section side.
    pub fn allows_block(&self, pos: BlockPos, side: u32) -> bool {
        if self.targets.contains(&TargetKey::Block(pos)) {
            return true;
        }
        match pos.to_section(side) {
            Ok((section, _)) => self.sections.contains(&section),
            Err(_) => false,
        }
    }
}

/// Typed request shared by all adapters; actor authority is supplied separately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionRequest {
    pub action: ContentKey,
    pub api_version: u32,
    pub origin: ActionOrigin,
    pub target: ActionTarget,
    pub parameters: ComponentFields,
    pub transaction: TransactionId,
    pub ordering: OrderingKey,
}

/// A bounded query result or an unpublished mutation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionOutput {
    Query(ComponentFields),
    Mutation(Box<Transaction>),
}

/// Read-only authority access restricted to host-granted objects.
///
/// Content definitions are public configuration; terrain and component reads
/// pass through target grants. No raw store or mutable world reference escapes.
pub struct ActionView<'a> {
    world: &'a WorldAuthority,
    actor: &'a ActorContext,
}

impl ActionView<'_> {
    pub fn actor(&self) -> &ActorContext {
        self.actor
    }
    pub fn terrain(&self, pos: BlockPos) -> Result<TerrainQuery, RejectCode> {
        let side = self.world.store().side();
        if !self.actor.allows_block(pos, side) {
            return Err(RejectCode::PermissionDenied);
        }
        Ok(self.world.store().query(pos))
    }
    pub fn component(
        &self,
        id: EntityId,
        schema: &ContentKey,
    ) -> Result<&ComponentFields, RejectCode> {
        if !self.actor.allows(ActionTarget::Entity(id)) {
            return Err(RejectCode::PermissionDenied);
        }
        let handle = self
            .world
            .entities()
            .handle_of(id)
            .ok_or(RejectCode::MissingTarget)?;
        self.world
            .entities()
            .get(handle)
            .and_then(|entity| entity.components.component(schema))
            .ok_or(RejectCode::MissingTarget)
    }
    pub fn default_state(&self, block: &ContentKey) -> Option<BlockStateId> {
        self.world.registry().default_state(block)
    }
    /// State written when an identical `block` is placed into a cell that
    /// already holds it, or `None` when the occupied cell must stay rejected.
    ///
    /// Completion is content-defined: the held block names the block its
    /// remaining volume turns into, which is how two half slabs become one
    /// full block.
    pub fn completion_state(
        &self,
        block: &ContentKey,
        current: BlockStateId,
    ) -> Option<BlockStateId> {
        let definition = self.world.registry().definition(current)?;
        if &definition.id != block {
            return None;
        }
        let target = definition.interaction_profile.completes_to.as_ref()?;
        self.world.registry().default_state(target)
    }
}

/// Native handlers may observe authority but cannot mutate it through this API.
///
/// Implementations must enforce module rules and check every secondary read
/// against actor grants. The dispatcher also checks every emitted write target.
pub trait ActionHandler {
    fn plan(
        &self,
        view: &ActionView<'_>,
        request: &ActionRequest,
    ) -> Result<ActionOutput, RejectCode>;
}

/// Explicit implementation binding checked before a service becomes usable.
pub struct NativeHandler {
    pub handler: ContentKey,
    pub owner: ContentKey,
    pub implementation: Box<dyn ActionHandler>,
}

/// Startup failure; no partially bound service is returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionBindError(pub String);

impl std::fmt::Display for ActionBindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ActionBindError {}

/// Immutable action catalog and native implementations selected by the host.
pub struct ActionService {
    registry: ActionRegistry,
    handlers: BTreeMap<ContentKey, NativeHandler>,
}

impl ActionService {
    /// Declared execution phase; lookup does not authorize a caller.
    pub fn phase(&self, action: &ContentKey) -> Option<Phase> {
        self.registry
            .actions()
            .get(action)
            .and_then(|a| Phase::from_index(a.phase.index() as u16))
    }
    /// Validate aliases, schemas, ownership, and exact handler coverage at startup.
    pub fn bind(
        actions: Vec<ActionDescriptor>,
        handlers: Vec<NativeHandler>,
        schemas: &ComponentSchemas,
    ) -> Result<Self, ActionBindError> {
        let registry =
            validate_actions(actions).map_err(|errors| ActionBindError(format!("{errors:?}")))?;
        let mut bound = BTreeMap::new();
        for handler in handlers {
            if bound.insert(handler.handler.clone(), handler).is_some() {
                return Err(ActionBindError("duplicate native handler".to_owned()));
            }
        }
        let mut used = BTreeSet::new();
        for action in registry.actions().values() {
            let handler = bound
                .get(&action.handler.handler)
                .ok_or_else(|| ActionBindError(format!("missing handler for {}", action.id)))?;
            if handler.owner != action.handler.owner_module
                || schemas.get(&action.parameter_schema).is_none()
                || schemas.get(&action.result_schema).is_none()
            {
                return Err(ActionBindError(format!(
                    "invalid binding for {}",
                    action.id
                )));
            }
            used.insert(action.handler.handler.clone());
        }
        if used.len() != bound.len() {
            return Err(ActionBindError("unreferenced native handler".to_owned()));
        }
        Ok(Self {
            registry,
            handlers: bound,
        })
    }

    /// Resolve a namespaced command into the same request consumed by other adapters.
    ///
    /// This method accepts already typed parameters. Shell execution, free-form
    /// text parsing, remote transport, and admission queues are outside A6.
    pub fn command(
        &self,
        command: &str,
        mut request: ActionRequest,
    ) -> Result<ActionRequest, RejectCode> {
        let descriptor = self
            .registry
            .command(command)
            .ok_or(RejectCode::MissingTarget)?;
        request.action = descriptor.id.clone();
        request.api_version = descriptor.api_version;
        request.origin = ActionOrigin::Console;
        Ok(request)
    }

    /// Authorize, validate, and plan an action without changing authoritative state.
    pub fn plan(
        &self,
        world: &WorldAuthority,
        actor: &ActorContext,
        phase: Phase,
        request: &ActionRequest,
    ) -> Result<ActionOutput, RejectCode> {
        let action = self
            .registry
            .actions()
            .get(&request.action)
            .ok_or(RejectCode::MissingTarget)?;
        if action.api_version != request.api_version
            || action.phase.index() as usize != phase.index()
            || request.ordering.phase != phase
        {
            return Err(RejectCode::InvalidData);
        }
        let side = world.store().side();
        let target_allowed = match request.target {
            ActionTarget::Block(pos) => actor.allows_block(pos, side),
            ActionTarget::Entity(id) => actor.allows(ActionTarget::Entity(id)),
        };
        if !target_allowed
            || action
                .permission
                .permission
                .as_ref()
                .is_some_and(|permission| !actor.permissions.contains(permission))
        {
            return Err(RejectCode::PermissionDenied);
        }
        if !action.target_constraints.is_empty() {
            let matches = match request.target {
                ActionTarget::Block(pos) => world
                    .store()
                    .query(pos)
                    .ready()
                    .and_then(|state| world.registry().definition(state))
                    .is_some_and(|definition| {
                        action.target_constraints.contains(&definition.state_schema)
                    }),
                ActionTarget::Entity(id) => world
                    .entities()
                    .handle_of(id)
                    .and_then(|handle| world.entities().get(handle))
                    .is_some_and(|entity| {
                        action
                            .target_constraints
                            .iter()
                            .any(|schema| entity.components.component(schema).is_some())
                    }),
            };
            if !matches {
                return Err(RejectCode::MissingTarget);
            }
        }
        ComponentSet::validate(
            world.schemas(),
            &action.parameter_schema,
            &request.parameters,
        )
        .map_err(|_| RejectCode::InvalidData)?;
        let handler = self
            .handlers
            .get(&action.handler.handler)
            .expect("frozen handler exists");
        let output = handler
            .implementation
            .plan(&ActionView { world, actor }, request)?;
        match &output {
            ActionOutput::Query(fields) if action.kind == ActionKind::Query => {
                ComponentSet::validate(world.schemas(), &action.result_schema, fields)
                    .map_err(|_| RejectCode::InvalidData)?;
                fn items(value: &ComponentValue) -> usize {
                    1 + match value {
                        ComponentValue::List(values) => values.iter().map(items).sum(),
                        ComponentValue::Record(values) => values.values().map(items).sum(),
                        _ => 0,
                    }
                }
                if fields.values().map(items).sum::<usize>() > action.max_items as usize
                    || component_size(fields).map_err(|_| RejectCode::QuotaExceeded)?
                        > action.max_bytes
                {
                    return Err(RejectCode::QuotaExceeded);
                }
            }
            ActionOutput::Mutation(tx) if action.kind == ActionKind::Mutation => {
                // Lifecycle actions need their own target and creation grants. Until those
                // are declared, native handlers cannot emit trusted-host lifecycle work.
                if !tx.spawns.is_empty() || !tx.despawns.is_empty() {
                    return Err(RejectCode::PermissionDenied);
                }
                if tx.source != handler.owner
                    || tx.id != request.transaction
                    || tx.ordering != request.ordering
                {
                    return Err(RejectCode::InvalidData);
                }
                let blocks = tx
                    .preconditions
                    .iter()
                    .map(|read| read.pos)
                    .chain(tx.mutations.iter().map(|write| write.pos));
                if blocks.into_iter().any(|pos| !actor.allows_block(pos, side))
                    || tx
                        .movements
                        .iter()
                        .any(|m| !actor.allows(ActionTarget::Entity(m.id)))
                    || tx
                        .components
                        .iter()
                        .any(|c| !actor.allows(ActionTarget::Entity(c.id)))
                {
                    return Err(RejectCode::PermissionDenied);
                }
                // Reuse preparation to reject malformed native output. The
                // returned plan must still be prepared again at commit time,
                // after earlier sorted transactions have changed the world.
                world.prepare(tx).map_err(|error| match error {
                    crate::PrepareError::Rejected(code) => code,
                    crate::PrepareError::SequenceExhausted => RejectCode::QuotaExceeded,
                })?;
            }
            _ => return Err(RejectCode::InvalidData),
        }
        Ok(output)
    }
}
