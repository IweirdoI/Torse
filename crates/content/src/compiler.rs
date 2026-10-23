//! Deterministic validation and compilation for the M0 content fixture.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use sha2::{Digest as ShaDigest, Sha256};
use torse_actions::{ActionDescriptor, ActionKind, validate_actions};
use torse_types::{ContentKey, Digest};

use crate::descriptor::{
    AlphaMode, BlockDefinition, CapabilityDescriptor, ComponentFieldType, ComponentSchema,
    ExecutionContract, ModuleDescriptor, ModuleSide, PersistencePolicy, Shape, StateSchema,
};

pub const CONTENT_FORMAT_VERSION: u32 = 1;
pub const MAX_FINITE_STATES: u64 = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SourceRef {
    pub source: String,
    pub line: u32,
}

impl SourceRef {
    pub fn new(source: impl Into<String>, line: u32) -> Self {
        Self {
            source: source.into(),
            line,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sourced<T> {
    pub source: SourceRef,
    pub value: T,
}

impl<T> Sourced<T> {
    pub fn new(source: SourceRef, value: T) -> Self {
        Self { source, value }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ContentPack {
    pub state_schemas: Vec<Sourced<StateSchema>>,
    pub capabilities: Vec<Sourced<CapabilityDescriptor>>,
    pub actions: Vec<Sourced<ActionDescriptor>>,
    pub modules: Vec<Sourced<ModuleDescriptor>>,
    pub blocks: Vec<Sourced<BlockDefinition>>,
}

#[derive(Clone, Debug, Default)]
pub struct CompiledContent {
    pub state_schemas: BTreeMap<ContentKey, StateSchema>,
    pub capabilities: BTreeMap<ContentKey, CapabilityDescriptor>,
    pub actions: BTreeMap<ContentKey, ActionDescriptor>,
    pub modules: BTreeMap<ContentKey, ModuleDescriptor>,
    pub blocks: BTreeMap<ContentKey, BlockDefinition>,
    pub authority_digest: Digest,
    pub presentation_digest: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentDiagnostic {
    pub source: SourceRef,
    pub content_id: Option<ContentKey>,
    pub field: String,
    pub message: String,
}

impl fmt::Display for ContentDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source.source, self.source.line)?;
        if let Some(content_id) = &self.content_id {
            write!(f, " [{content_id}]")?;
        }
        write!(f, " {}: {}", self.field, self.message)
    }
}

impl Ord for ContentDiagnostic {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.source, &self.content_id, &self.field, &self.message).cmp(&(
            &other.source,
            &other.content_id,
            &other.field,
            &other.message,
        ))
    }
}

impl PartialOrd for ContentDiagnostic {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Validate and compile a content pack.
///
/// The returned registry and digests are independent of source enumeration order.
pub fn compile(pack: ContentPack) -> Result<CompiledContent, Vec<ContentDiagnostic>> {
    let mut diagnostics = Vec::new();
    let mut state_schemas = collect_unique(pack.state_schemas, |value| &value.id, &mut diagnostics);
    let mut capabilities = collect_unique(pack.capabilities, |value| &value.id, &mut diagnostics);
    let mut actions = collect_unique(pack.actions, |value| &value.id, &mut diagnostics);
    let mut modules = collect_unique(pack.modules, |value| &value.id, &mut diagnostics);

    for sourced in state_schemas.values() {
        validate_state_schema(&sourced.source, &sourced.value, &mut diagnostics);
    }
    for sourced in capabilities.values() {
        validate_capability(&sourced.source, &sourced.value, &mut diagnostics);
    }
    for sourced in modules.values() {
        validate_module(&sourced.source, &sourced.value, &mut diagnostics);
    }
    match validate_actions(
        actions
            .values()
            .map(|sourced| sourced.value.clone())
            .collect(),
    ) {
        Ok(_) => {}
        Err(action_errors) => {
            for error in action_errors {
                let source = actions
                    .get(&error.action)
                    .map(|sourced| sourced.source.clone())
                    .unwrap_or_else(|| SourceRef::new("compiled-registry", 0));
                diagnostics.push(diagnostic(source, error.action, error.field, error.message));
            }
        }
    }

    let mut component_ids = BTreeMap::<ContentKey, ContentKey>::new();
    for sourced in state_schemas.values() {
        for component in &sourced.value.component_schemas {
            if let Some(previous) =
                component_ids.insert(component.id.clone(), sourced.value.id.clone())
            {
                diagnostics.push(diagnostic(
                    sourced.source.clone(),
                    sourced.value.id.clone(),
                    "component_schemas.id",
                    format!("component {} is also declared by {previous}", component.id),
                ));
            }
        }
    }

    validate_module_references(
        &modules,
        &state_schemas,
        &capabilities,
        &actions,
        &mut diagnostics,
    );

    let mut blocks = BTreeMap::new();
    for sourced in pack.blocks {
        let mut block = sourced.value;
        normalize_block(&mut block);
        let id = block.id.clone();
        validate_block(
            &block,
            &sourced.source,
            &state_schemas,
            &capabilities,
            &component_ids,
            &mut diagnostics,
        );
        if blocks.insert(id.clone(), block).is_some() {
            diagnostics.push(diagnostic(
                sourced.source,
                id,
                "id",
                "duplicate block definition",
            ));
        }
    }

    diagnostics.sort();
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    for sourced in state_schemas.values_mut() {
        normalize_state_schema(&mut sourced.value);
    }
    for sourced in capabilities.values_mut() {
        normalize_capability(&mut sourced.value);
    }
    for sourced in actions.values_mut() {
        normalize_action(&mut sourced.value);
    }
    for sourced in modules.values_mut() {
        normalize_module(&mut sourced.value);
    }

    let plain_schemas = into_plain(state_schemas);
    let plain_capabilities = into_plain(capabilities);
    let plain_actions = into_plain(actions);
    let plain_modules = into_plain(modules);
    let authority_digest = authority_digest(
        &plain_schemas,
        &plain_capabilities,
        &plain_actions,
        &plain_modules,
        &blocks,
    );
    let presentation_digest = presentation_digest(&blocks);
    Ok(CompiledContent {
        state_schemas: plain_schemas,
        capabilities: plain_capabilities,
        actions: plain_actions,
        modules: plain_modules,
        blocks,
        authority_digest,
        presentation_digest,
    })
}

fn collect_unique<T>(
    values: Vec<Sourced<T>>,
    key: impl Fn(&T) -> &ContentKey,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> BTreeMap<ContentKey, Sourced<T>> {
    let mut output = BTreeMap::new();
    for sourced in values {
        let id = key(&sourced.value).clone();
        if let Some(existing) = output.insert(id.clone(), sourced) {
            diagnostics.push(diagnostic(
                existing.source,
                id,
                "id",
                "duplicate definition; retained one deterministic entry",
            ));
        }
    }
    output
}

fn into_plain<T>(values: BTreeMap<ContentKey, Sourced<T>>) -> BTreeMap<ContentKey, T> {
    values
        .into_iter()
        .map(|(id, sourced)| (id, sourced.value))
        .collect()
}

fn validate_state_schema(
    source: &SourceRef,
    schema: &StateSchema,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    if schema.schema_version == 0 {
        diagnostics.push(diagnostic(
            source.clone(),
            schema.id.clone(),
            "schema_version",
            "schema version must be greater than zero",
        ));
    }

    let mut states = 1_u64;
    let mut field_ids = BTreeSet::new();
    for field in &schema.finite_fields {
        if !field_ids.insert(field.id.clone()) {
            diagnostics.push(diagnostic(
                source.clone(),
                schema.id.clone(),
                "finite_fields.id",
                format!("duplicate finite field {}", field.id),
            ));
        }
        if field.values.is_empty() {
            diagnostics.push(diagnostic(
                source.clone(),
                schema.id.clone(),
                "finite_fields.values",
                format!("{} has no legal values", field.id),
            ));
            continue;
        }
        if !field.values.contains(&field.default_value) {
            diagnostics.push(diagnostic(
                source.clone(),
                schema.id.clone(),
                "finite_fields.default_value",
                format!("{} default is not in its legal values", field.id),
            ));
        }
        let unique_values: BTreeSet<_> = field.values.iter().copied().collect();
        if unique_values.len() != field.values.len() {
            diagnostics.push(diagnostic(
                source.clone(),
                schema.id.clone(),
                "finite_fields.values",
                format!("{} contains duplicate values", field.id),
            ));
        }
        states = states.saturating_mul(unique_values.len() as u64);
    }
    if states > MAX_FINITE_STATES {
        diagnostics.push(diagnostic(
            source.clone(),
            schema.id.clone(),
            "finite_fields",
            format!("state combinations {states} exceed limit {MAX_FINITE_STATES}"),
        ));
    }

    for component in &schema.component_schemas {
        validate_component(source, schema, component, diagnostics);
    }
}

fn validate_component(
    source: &SourceRef,
    schema: &StateSchema,
    component: &ComponentSchema,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    if component.schema_version == 0 {
        diagnostics.push(diagnostic(
            source.clone(),
            schema.id.clone(),
            "component.schema_version",
            format!("{} version must be greater than zero", component.id),
        ));
    }
    let mut field_ids = BTreeSet::new();
    for field in &component.fields {
        if !field_ids.insert(field.id.clone()) {
            diagnostics.push(diagnostic(
                source.clone(),
                schema.id.clone(),
                "component.fields.id",
                format!("duplicate field {} in {}", field.id, component.id),
            ));
        }
        if let ComponentFieldType::String { max_bytes: 0 }
        | ComponentFieldType::List { max_items: 0 } = field.field_type
        {
            diagnostics.push(diagnostic(
                source.clone(),
                schema.id.clone(),
                "component.fields.field_type",
                format!("{} must have a positive container limit", field.id),
            ));
        }
    }
}

fn validate_capability(
    source: &SourceRef,
    capability: &CapabilityDescriptor,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    if capability.api_version == 0 {
        diagnostics.push(diagnostic(
            source.clone(),
            capability.id.clone(),
            "api_version",
            "capability API version must be greater than zero",
        ));
    }
    let mut methods = BTreeSet::new();
    for method in capability
        .query_methods
        .iter()
        .chain(&capability.mutation_methods)
    {
        if !methods.insert(method.id.clone()) {
            diagnostics.push(diagnostic(
                source.clone(),
                capability.id.clone(),
                "methods.id",
                format!("duplicate method {}", method.id),
            ));
        }
    }
}

fn validate_module(
    source: &SourceRef,
    module: &ModuleDescriptor,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    if module.module_version == 0 || module.framework_api == 0 {
        diagnostics.push(diagnostic(
            source.clone(),
            module.id.clone(),
            "version",
            "module and framework versions must be greater than zero",
        ));
    }
    let mut services = BTreeSet::new();
    for service in &module.provided_services {
        if !services.insert(service.id.clone()) {
            diagnostics.push(diagnostic(
                source.clone(),
                module.id.clone(),
                "provided_services.id",
                format!("duplicate provided service {}", service.id),
            ));
        }
        if service.api_version == 0 {
            diagnostics.push(diagnostic(
                source.clone(),
                module.id.clone(),
                "provided_services.api_version",
                format!("{} API version must be positive", service.id),
            ));
        }
    }
}

fn validate_module_references(
    modules: &BTreeMap<ContentKey, Sourced<ModuleDescriptor>>,
    schemas: &BTreeMap<ContentKey, Sourced<StateSchema>>,
    capabilities: &BTreeMap<ContentKey, Sourced<CapabilityDescriptor>>,
    actions: &BTreeMap<ContentKey, Sourced<ActionDescriptor>>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    let mut known_schemas: BTreeSet<_> = schemas.keys().cloned().collect();
    for schema in schemas.values() {
        known_schemas.extend(
            schema
                .value
                .component_schemas
                .iter()
                .map(|component| component.id.clone()),
        );
    }
    for module in modules.values() {
        for schema in &module.value.registered_schemas {
            if !known_schemas.contains(schema) {
                diagnostics.push(diagnostic(
                    module.source.clone(),
                    module.value.id.clone(),
                    "registered_schemas",
                    format!("unknown schema {schema}"),
                ));
            }
        }
        for capability in &module.value.registered_capabilities {
            if !capabilities.contains_key(capability) {
                diagnostics.push(diagnostic(
                    module.source.clone(),
                    module.value.id.clone(),
                    "registered_capabilities",
                    format!("unknown capability {capability}"),
                ));
            }
        }
        for action in &module.value.registered_actions {
            if !actions.contains_key(action) {
                diagnostics.push(diagnostic(
                    module.source.clone(),
                    module.value.id.clone(),
                    "registered_actions",
                    format!("unknown action {action}"),
                ));
            }
        }
    }
}

fn validate_block(
    block: &BlockDefinition,
    source: &SourceRef,
    schemas: &BTreeMap<ContentKey, Sourced<StateSchema>>,
    capabilities: &BTreeMap<ContentKey, Sourced<CapabilityDescriptor>>,
    components: &BTreeMap<ContentKey, ContentKey>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    if block.definition_version == 0 {
        diagnostics.push(diagnostic(
            source.clone(),
            block.id.clone(),
            "definition_version",
            "definition version must be greater than zero",
        ));
    }
    if !schemas.contains_key(&block.state_schema) {
        diagnostics.push(diagnostic(
            source.clone(),
            block.id.clone(),
            "state_schema",
            format!("unknown state schema {}", block.state_schema),
        ));
    }
    validate_shape(
        &block.render_profile.shape,
        source,
        &block.id,
        "render_profile.shape",
        diagnostics,
    );
    if let Some(shape) = &block.collision_profile.shape {
        validate_shape(
            shape,
            source,
            &block.id,
            "collision_profile.shape",
            diagnostics,
        );
    }
    if let Some(shape) = &block.selection_profile.shape {
        validate_shape(
            shape,
            source,
            &block.id,
            "selection_profile.shape",
            diagnostics,
        );
    }
    if block.occlusion_profile.face_mask > 0b11_1111 {
        diagnostics.push(diagnostic(
            source.clone(),
            block.id.clone(),
            "occlusion_profile.face_mask",
            "face mask may only contain six face bits",
        ));
    }
    for (name, value) in [
        (
            "light_transport.absorption",
            block.light_transport.absorption,
        ),
        ("light_transport.emission", block.light_transport.emission),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            diagnostics.push(diagnostic(
                source.clone(),
                block.id.clone(),
                name,
                "value must be finite and between 0 and 1",
            ));
        }
    }
    for component in &block.component_defaults {
        if !components.contains_key(component) {
            diagnostics.push(diagnostic(
                source.clone(),
                block.id.clone(),
                "component_defaults",
                format!("unknown component schema {component}"),
            ));
        }
    }
    for binding in &block.capability_bindings {
        if !capabilities.contains_key(&binding.capability) {
            diagnostics.push(diagnostic(
                source.clone(),
                block.id.clone(),
                "capability_bindings",
                format!("unknown capability {}", binding.capability),
            ));
        }
        if binding.face_mask > 0b11_1111 {
            diagnostics.push(diagnostic(
                source.clone(),
                block.id.clone(),
                "capability_bindings.face_mask",
                "capability face mask may only contain six face bits",
            ));
        }
    }
}

fn validate_shape(
    shape: &Shape,
    source: &SourceRef,
    content_id: &ContentKey,
    field: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    if let Err(message) = shape.validate() {
        diagnostics.push(diagnostic(
            source.clone(),
            content_id.clone(),
            field,
            message,
        ));
    }
}

fn normalize_block(block: &mut BlockDefinition) {
    block.component_defaults.sort();
    block
        .capability_bindings
        .sort_by(|left, right| left.capability.cmp(&right.capability));
    block.behavior_bindings.sort();
    block.tags.sort();
}

fn normalize_state_schema(schema: &mut StateSchema) {
    schema
        .finite_fields
        .sort_by(|left, right| left.id.cmp(&right.id));
    for field in &mut schema.finite_fields {
        field.values.sort_unstable();
    }
    schema
        .component_schemas
        .sort_by(|left, right| left.id.cmp(&right.id));
    for component in &mut schema.component_schemas {
        component
            .fields
            .sort_by(|left, right| left.id.cmp(&right.id));
    }
}

fn normalize_capability(capability: &mut CapabilityDescriptor) {
    capability
        .query_methods
        .sort_by(|left, right| left.id.cmp(&right.id));
    capability
        .mutation_methods
        .sort_by(|left, right| left.id.cmp(&right.id));
    capability.target_schema_constraints.sort();
}

fn normalize_action(action: &mut ActionDescriptor) {
    action.target_constraints.sort();
    action
        .commands
        .sort_by(|left, right| left.name.cmp(&right.name));
    for command in &mut action.commands {
        command.aliases.sort();
    }
}

fn normalize_module(module: &mut ModuleDescriptor) {
    module.required_modules.sort();
    module.optional_modules.sort();
    module
        .provided_services
        .sort_by(|left, right| left.id.cmp(&right.id));
    module
        .required_services
        .sort_by(|left, right| left.id.cmp(&right.id));
    module.registered_schemas.sort();
    module.registered_capabilities.sort();
    module.registered_actions.sort();
    module
        .registered_systems
        .sort_by(|left, right| left.id.cmp(&right.id));
    module
        .requested_permissions
        .sort_by(|left, right| left.id.cmp(&right.id));
}

fn authority_digest(
    schemas: &BTreeMap<ContentKey, StateSchema>,
    capabilities: &BTreeMap<ContentKey, CapabilityDescriptor>,
    actions: &BTreeMap<ContentKey, ActionDescriptor>,
    modules: &BTreeMap<ContentKey, ModuleDescriptor>,
    blocks: &BTreeMap<ContentKey, BlockDefinition>,
) -> Digest {
    let mut writer = CanonicalWriter::new();
    writer.u32(CONTENT_FORMAT_VERSION);
    writer.u32(schemas.len() as u32);
    for schema in schemas.values() {
        writer.key(&schema.id);
        writer.u32(schema.schema_version);
        writer.u32(schema.finite_fields.len() as u32);
        for field in &schema.finite_fields {
            writer.key(&field.id);
            writer.u16(field.default_value);
            writer.u32(field.values.len() as u32);
            for value in &field.values {
                writer.u16(*value);
            }
        }
        writer.u32(schema.component_schemas.len() as u32);
        for component in &schema.component_schemas {
            writer.key(&component.id);
            writer.key(&component.owner_module);
            writer.u32(component.schema_version);
            writer.u32(component.fields.len() as u32);
            for field in &component.fields {
                writer.key(&field.id);
                writer.component_field_type(field.field_type);
                writer.bool(field.required);
            }
            writer.u8(match component.persistence {
                PersistencePolicy::Never => 0,
                PersistencePolicy::Always => 1,
                PersistencePolicy::OwnerDefined => 2,
            });
            writer.replication(component.replication);
            writer.optional_key(component.migration_binding.as_ref());
        }
    }
    writer.u32(capabilities.len() as u32);
    for capability in capabilities.values() {
        writer.key(&capability.id);
        writer.u32(capability.api_version);
        writer.key(&capability.access_contract);
        writer.u8(execution_tag(capability.execution_contract));
        writer.u32((capability.query_methods.len() + capability.mutation_methods.len()) as u32);
        for method in capability
            .query_methods
            .iter()
            .chain(&capability.mutation_methods)
        {
            writer.key(&method.id);
            writer.key(&method.parameter_schema);
            writer.key(&method.result_schema);
        }
        writer.u32(capability.target_schema_constraints.len() as u32);
        for constraint in &capability.target_schema_constraints {
            writer.key(constraint);
        }
    }
    writer.u32(actions.len() as u32);
    for action in actions.values() {
        writer.key(&action.id);
        writer.u32(action.api_version);
        writer.u8(match action.kind {
            ActionKind::Query => 0,
            ActionKind::Mutation => 1,
        });
        writer.u8(action.phase.index());
        writer.u32(action.max_items);
        writer.u64(action.max_bytes);
        writer.key(&action.parameter_schema);
        writer.key(&action.result_schema);
        writer.optional_key(action.permission.permission.as_ref());
        writer.key(&action.handler.handler);
        writer.key(&action.handler.owner_module);
        writer.u32(action.target_constraints.len() as u32);
        for target in &action.target_constraints {
            writer.key(target);
        }
        writer.u32(action.commands.len() as u32);
        for command in &action.commands {
            writer.key(&command.name);
            writer.key(&command.usage_key);
            writer.u32(command.aliases.len() as u32);
            for alias in &command.aliases {
                writer.key(alias);
            }
        }
    }
    writer.u32(modules.len() as u32);
    for module in modules.values() {
        writer.key(&module.id);
        writer.u32(module.module_version);
        writer.u32(module.framework_api);
        writer.u8(module_side_tag(module.side));
        writer.u32(module.required_modules.len() as u32);
        for dependency in &module.required_modules {
            writer.key(dependency);
        }
        writer.u32(module.optional_modules.len() as u32);
        for dependency in &module.optional_modules {
            writer.key(dependency);
        }
        writer.u32(module.provided_services.len() as u32);
        for service in &module.provided_services {
            writer.key(&service.id);
            writer.u32(service.api_version);
        }
        writer.u32(module.required_services.len() as u32);
        for service in &module.required_services {
            writer.key(&service.id);
            writer.u32(service.api_version);
        }
        writer.keys(&module.registered_schemas);
        writer.keys(&module.registered_capabilities);
        writer.keys(&module.registered_actions);
        writer.u32(module.registered_systems.len() as u32);
        for system in &module.registered_systems {
            writer.key(&system.id);
            writer.u8(system.phase);
            writer.u32(system.max_items);
            writer.u64(system.max_bytes);
        }
        writer.u32(module.requested_permissions.len() as u32);
        for permission in &module.requested_permissions {
            writer.key(&permission.id);
        }
        writer.optional_key(module.configuration_schema.as_ref());
        writer.optional_key(module.lifecycle_binding.as_ref());
    }
    writer.u32(blocks.len() as u32);
    for block in blocks.values() {
        writer.key(&block.id);
        writer.u32(block.definition_version);
        writer.key(&block.state_schema);
        writer.optional_shape(block.collision_profile.shape.as_ref());
        writer.optional_shape(block.selection_profile.shape.as_ref());
        writer.u8(block.occlusion_profile.face_mask);
        writer.u32(block.light_transport.absorption.to_bits());
        writer.u32(block.light_transport.emission.to_bits());
        writer.optional_key(block.interaction_profile.action.as_ref());
        writer.u32(block.component_defaults.len() as u32);
        for component in &block.component_defaults {
            writer.key(component);
        }
        writer.u32(block.behavior_bindings.len() as u32);
        for behavior in &block.behavior_bindings {
            writer.key(behavior);
        }
        writer.u32(block.capability_bindings.len() as u32);
        for binding in &block.capability_bindings {
            writer.key(&binding.capability);
            writer.u8(binding.face_mask);
        }
    }
    writer.finish()
}

fn presentation_digest(blocks: &BTreeMap<ContentKey, BlockDefinition>) -> Digest {
    let mut writer = CanonicalWriter::new();
    writer.u32(CONTENT_FORMAT_VERSION);
    writer.u32(blocks.len() as u32);
    for block in blocks.values() {
        writer.key(&block.id);
        writer.key(&block.render_profile.material);
        writer.u8(match block.render_profile.alpha_mode {
            AlphaMode::Opaque => 0,
            AlphaMode::Mask => 1,
            AlphaMode::Blend => 2,
        });
        writer.shape(&block.render_profile.shape);
        writer.u32(block.tags.len() as u32);
        for tag in &block.tags {
            writer.key(tag);
        }
    }
    writer.finish()
}

struct CanonicalWriter {
    bytes: Vec<u8>,
}

impl CanonicalWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn key(&mut self, value: &ContentKey) {
        self.u32(value.namespace().len() as u32);
        self.bytes.extend_from_slice(value.namespace().as_bytes());
        self.u32(value.path().len() as u32);
        self.bytes.extend_from_slice(value.path().as_bytes());
    }

    fn optional_key(&mut self, value: Option<&ContentKey>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.key(value);
        }
    }

    fn keys(&mut self, values: &[ContentKey]) {
        self.u32(values.len() as u32);
        for value in values {
            self.key(value);
        }
    }

    fn component_field_type(&mut self, field_type: ComponentFieldType) {
        let tag = component_field_tag(field_type);
        self.u8(tag);
        match field_type {
            ComponentFieldType::String { max_bytes } => self.u32(max_bytes),
            ComponentFieldType::List { max_items } => self.u32(max_items),
            _ => {}
        }
    }

    fn replication(&mut self, declaration: torse_types::ReplicationDeclaration) {
        self.u8(match declaration.authority {
            torse_types::AuthorityMode::Server => 0,
            torse_types::AuthorityMode::ClientPresentation => 1,
        });
        self.u8(match declaration.visibility {
            torse_types::ReplicationVisibility::Private => 0,
            torse_types::ReplicationVisibility::Authorized => 1,
            torse_types::ReplicationVisibility::Public => 2,
        });
        self.u8(match declaration.update_mode {
            torse_types::UpdateMode::CompleteTransaction => 0,
            torse_types::UpdateMode::LatestValue => 1,
        });
        self.u8(match declaration.presentation_hint {
            torse_types::PresentationHint::None => 0,
            torse_types::PresentationHint::Discrete => 1,
            torse_types::PresentationHint::Interpolate => 2,
        });
    }

    fn shape(&mut self, shape: &Shape) {
        match shape {
            Shape::FullCube => self.u8(0),
            Shape::LowerSlab => self.u8(1),
            Shape::Panel => self.u8(2),
            Shape::Corner => self.u8(3),
            Shape::Stair => self.u8(4),
            Shape::Box(bounds) => {
                self.u8(5);
                for value in bounds.min.iter().chain(bounds.max.iter()) {
                    self.u32(value.to_bits());
                }
            }
        }
    }

    fn optional_shape(&mut self, shape: Option<&Shape>) {
        self.bool(shape.is_some());
        if let Some(shape) = shape {
            self.shape(shape);
        }
    }

    fn finish(self) -> Digest {
        Digest(Sha256::digest(self.bytes).into())
    }
}

fn component_field_tag(field_type: ComponentFieldType) -> u8 {
    match field_type {
        ComponentFieldType::Bool => 0,
        ComponentFieldType::Unsigned32 => 1,
        ComponentFieldType::Signed64 => 2,
        ComponentFieldType::ContentReference => 3,
        ComponentFieldType::EntityReference => 4,
        ComponentFieldType::String { .. } => 5,
        ComponentFieldType::List { .. } => 6,
        ComponentFieldType::Record => 7,
    }
}

fn execution_tag(contract: ExecutionContract) -> u8 {
    match contract {
        ExecutionContract::NoRuntimeCode => 0,
        ExecutionContract::BoundedNative => 1,
        ExecutionContract::Sandboxed => 2,
    }
}

fn module_side_tag(side: ModuleSide) -> u8 {
    match side {
        ModuleSide::Server => 0,
        ModuleSide::Client => 1,
        ModuleSide::Shared => 2,
    }
}

fn diagnostic(
    source: SourceRef,
    content_id: ContentKey,
    field: impl Into<String>,
    message: impl Into<String>,
) -> ContentDiagnostic {
    ContentDiagnostic {
        source,
        content_id: Some(content_id),
        field: field.into(),
        message: message.into(),
    }
}
