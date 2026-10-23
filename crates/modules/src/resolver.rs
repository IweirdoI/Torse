//! Deterministic module graph resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use torse_content::ModuleDescriptor;
use torse_types::ContentKey;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ModuleError {
    pub module: Option<ContentKey>,
    pub field: String,
    pub message: String,
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(module) = &self.module {
            write!(f, "[{module}] ")?;
        }
        write!(f, "{}: {}", self.field, self.message)
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedModules {
    modules: BTreeMap<ContentKey, ModuleDescriptor>,
    order: Vec<ContentKey>,
}

impl ResolvedModules {
    pub fn modules(&self) -> &BTreeMap<ContentKey, ModuleDescriptor> {
        &self.modules
    }

    pub fn order(&self) -> &[ContentKey] {
        &self.order
    }
}

pub fn resolve_modules(
    modules: Vec<ModuleDescriptor>,
) -> Result<ResolvedModules, Vec<ModuleError>> {
    let mut errors = Vec::new();
    let mut by_id = BTreeMap::new();
    for module in modules {
        let id = module.id.clone();
        if by_id.insert(id.clone(), module).is_some() {
            error(&mut errors, Some(id), "id", "duplicate module definition");
        }
    }

    validate_service_providers(&by_id, &mut errors);
    validate_registered_content(&by_id, &mut errors);
    validate_dependencies(&by_id, &mut errors);
    let order = topological_order(&by_id, &mut errors);

    errors.sort();
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ResolvedModules {
        modules: by_id,
        order,
    })
}

fn validate_service_providers(
    modules: &BTreeMap<ContentKey, ModuleDescriptor>,
    errors: &mut Vec<ModuleError>,
) {
    let mut providers = BTreeMap::<ContentKey, (ContentKey, u32)>::new();
    for module in modules.values() {
        for service in &module.provided_services {
            if let Some((existing, api_version)) =
                providers.insert(service.id.clone(), (module.id.clone(), service.api_version))
            {
                error(
                    errors,
                    Some(module.id.clone()),
                    "provided_services",
                    format!(
                        "{} is also provided by {existing} with API version {api_version}; \
                         a single-provider service cannot have multiple candidates",
                        service.id
                    ),
                );
            }
        }
    }

    for module in modules.values() {
        for requirement in &module.required_services {
            match providers.get(&requirement.id) {
                None => error(
                    errors,
                    Some(module.id.clone()),
                    "required_services",
                    format!("missing service provider {}", requirement.id),
                ),
                Some((_, api_version)) if *api_version != requirement.api_version => error(
                    errors,
                    Some(module.id.clone()),
                    "required_services.api_version",
                    format!(
                        "{} requires API {}, provider exposes {}",
                        requirement.id, requirement.api_version, api_version
                    ),
                ),
                Some(_) => {}
            }
        }
    }
}

fn validate_registered_content(
    modules: &BTreeMap<ContentKey, ModuleDescriptor>,
    errors: &mut Vec<ModuleError>,
) {
    let mut schemas = BTreeMap::<ContentKey, ContentKey>::new();
    let mut capabilities = BTreeMap::<ContentKey, ContentKey>::new();
    let mut actions = BTreeMap::<ContentKey, ContentKey>::new();
    for module in modules.values() {
        for key in &module.registered_schemas {
            register(errors, &mut schemas, module, key, "registered_schemas");
        }
        for key in &module.registered_capabilities {
            register(
                errors,
                &mut capabilities,
                module,
                key,
                "registered_capabilities",
            );
        }
        for key in &module.registered_actions {
            register(errors, &mut actions, module, key, "registered_actions");
        }
    }
}

fn register(
    errors: &mut Vec<ModuleError>,
    registry: &mut BTreeMap<ContentKey, ContentKey>,
    module: &ModuleDescriptor,
    key: &ContentKey,
    field: &str,
) {
    if let Some(existing) = registry.insert(key.clone(), module.id.clone()) {
        error(
            errors,
            Some(module.id.clone()),
            field,
            format!("{key} is already registered by {existing}"),
        );
    }
}

fn validate_dependencies(
    modules: &BTreeMap<ContentKey, ModuleDescriptor>,
    errors: &mut Vec<ModuleError>,
) {
    for module in modules.values() {
        for dependency in &module.required_modules {
            if !modules.contains_key(dependency) {
                error(
                    errors,
                    Some(module.id.clone()),
                    "required_modules",
                    format!("missing required module {dependency}"),
                );
            }
        }
    }
}

fn topological_order(
    modules: &BTreeMap<ContentKey, ModuleDescriptor>,
    errors: &mut Vec<ModuleError>,
) -> Vec<ContentKey> {
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for id in modules.keys() {
        visit(id, modules, &mut visiting, &mut visited, &mut order, errors);
    }
    order
}

fn visit(
    id: &ContentKey,
    modules: &BTreeMap<ContentKey, ModuleDescriptor>,
    visiting: &mut BTreeSet<ContentKey>,
    visited: &mut BTreeSet<ContentKey>,
    order: &mut Vec<ContentKey>,
    errors: &mut Vec<ModuleError>,
) {
    if visited.contains(id) {
        return;
    }
    if !visiting.insert(id.clone()) {
        error(
            errors,
            Some(id.clone()),
            "required_modules",
            "dependency cycle detected",
        );
        return;
    }
    if let Some(module) = modules.get(id) {
        for dependency in &module.required_modules {
            if modules.contains_key(dependency) {
                visit(dependency, modules, visiting, visited, order, errors);
            }
        }
    }
    visiting.remove(id);
    if visited.insert(id.clone()) {
        order.push(id.clone());
    }
}

fn error(
    errors: &mut Vec<ModuleError>,
    module: Option<ContentKey>,
    field: impl Into<String>,
    message: impl Into<String>,
) {
    errors.push(ModuleError {
        module,
        field: field.into(),
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use torse_content::{ModuleSide, ServiceDescriptor};

    #[test]
    fn dependencies_are_ordered_before_dependants() {
        let modules = resolve_modules(vec![
            module("base:dependent", &["base:core"]),
            module("base:core", &[]),
        ])
        .unwrap();
        assert_eq!(modules.order(), &[key("base:core"), key("base:dependent")]);
    }

    #[test]
    fn dependency_cycle_is_rejected() {
        let errors = resolve_modules(vec![
            module("base:left", &["base:right"]),
            module("base:right", &["base:left"]),
        ])
        .unwrap_err();
        assert!(errors.iter().any(|error| error.message.contains("cycle")));
    }

    #[test]
    fn missing_required_module_is_rejected() {
        let errors =
            resolve_modules(vec![module("base:dependent", &["base:missing"])]).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("missing required module"))
        );
    }

    #[test]
    fn duplicate_service_provider_is_rejected() {
        let mut left = module("base:left", &[]);
        let mut right = module("base:right", &[]);
        let service = ServiceDescriptor {
            id: key("base:service/world"),
            api_version: 1,
        };
        left.provided_services.push(service.clone());
        right.provided_services.push(service);
        let errors = resolve_modules(vec![left, right]).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("single-provider"))
        );
    }

    fn module(id: &str, required: &[&str]) -> ModuleDescriptor {
        ModuleDescriptor {
            id: key(id),
            module_version: 1,
            framework_api: 1,
            side: ModuleSide::Shared,
            required_modules: required.iter().map(|id| key(id)).collect(),
            optional_modules: Vec::new(),
            provided_services: Vec::new(),
            required_services: Vec::new(),
            registered_schemas: Vec::new(),
            registered_capabilities: Vec::new(),
            registered_actions: Vec::new(),
            registered_systems: Vec::new(),
            requested_permissions: Vec::new(),
            configuration_schema: None,
            lifecycle_binding: None,
        }
    }

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).unwrap()
    }
}
