//! Action contracts shared by commands, UI, network input, and Mods.
//!
//! All external access paths eventually describe the same typed action. Keeping
//! the declaration independent of a text parser or UI event lets the authority
//! apply one authorization and execution policy regardless of where a request
//! originated. This module only validates and freezes declarations; it does not
//! execute actions or grant permissions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use torse_types::ContentKey;

/// Maximum command path length accepted by the registration boundary.
const MAX_COMMAND_NAME_BYTES: usize = 64;
/// Maximum usage metadata length accepted by the registration boundary.
const MAX_USAGE_BYTES: usize = 512;

/// Whether an action observes state or requests a mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionKind {
    /// Read-only query evaluated against an authorized view.
    Query,
    /// Mutation that must pass the normal transaction path.
    Mutation,
}

/// Simulation phase in which an action is allowed to execute.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ActionPhase {
    /// Admission and request validation phase.
    Input,
    /// Lifecycle and loading phase.
    Lifecycle,
    /// Gameplay interaction phase.
    Interaction,
    /// Simulation phase for scheduled systems.
    Simulation,
    /// Follow-up rule phase for events produced by earlier work.
    Followup,
    /// Publication phase for committed visible changes.
    Publish,
}

impl ActionPhase {
    /// Numeric phase used by external declarations and diagnostics.
    pub const fn index(self) -> u8 {
        match self {
            Self::Input => 0,
            Self::Lifecycle => 1,
            Self::Interaction => 2,
            Self::Simulation => 3,
            Self::Followup => 4,
            Self::Publish => 5,
        }
    }
}

/// Permission requirement checked before an action handler is invoked.
///
/// `None` means no additional permission is requested by the declaration; it
/// does not bypass target ownership, reach, or other authority checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionRule {
    /// Permission key required by the declaration, if any.
    pub permission: Option<ContentKey>,
}

/// Module-owned handler selected for an action.
///
/// `owner_module` is recorded separately from the handler name so a later
/// loader can verify that a module is allowed to provide the binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandlerBinding {
    /// Stable handler identity implemented by the owner module.
    pub handler: ContentKey,
    /// Module that owns and registers the handler implementation.
    pub owner_module: ContentKey,
}

/// Optional textual command aliases for a typed action.
///
/// Commands are adapters only. Parsing must produce the same typed action and
/// authorization context as UI, network, or direct Mod calls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandBinding {
    /// Fully namespaced primary command identity.
    pub name: ContentKey,
    /// Additional fully namespaced aliases with the same target action.
    pub aliases: Vec<ContentKey>,
    /// Localized usage/help resource key.
    pub usage_key: ContentKey,
}

/// Versioned, typed declaration for one externally invokable action.
///
/// Output limits apply to the action's result, not to arbitrary internal work.
/// The authority may impose stricter limits at execution time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionDescriptor {
    /// Stable action identity and request discriminator.
    pub id: ContentKey,
    /// API version used for compatibility negotiation and encoding.
    pub api_version: u32,
    /// Query or mutation classification.
    pub kind: ActionKind,
    /// Schema identity for the action input payload.
    pub parameter_schema: ContentKey,
    /// Schema identity for the action result payload.
    pub result_schema: ContentKey,
    /// Additional authorization requirement for this action.
    pub permission: PermissionRule,
    /// Object/content schemas this action may target.
    pub target_constraints: Vec<ContentKey>,
    /// Simulation phase in which the action is eligible to execute.
    pub phase: ActionPhase,
    /// Maximum logical query entries, or transaction entries in a composed subtree.
    pub max_items: u32,
    /// Maximum query bytes, or conservative native plan bytes in a composed subtree.
    /// Durable hosts independently enforce their storage and wire budgets.
    pub max_bytes: u64,
    /// Module-owned implementation selected during assembly.
    pub handler: HandlerBinding,
    /// Optional textual adapters that resolve to this action.
    pub commands: Vec<CommandBinding>,
}

/// Registration-time error tied to a specific action and field.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ActionDiagnostic {
    /// Action whose declaration failed validation.
    pub action: ContentKey,
    /// Declaration field that produced the error.
    pub field: String,
    /// Human-readable explanation suitable for startup diagnostics.
    pub message: String,
}

impl fmt::Display for ActionDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.action, self.field, self.message)
    }
}

/// Immutable action and command lookup created after validation succeeds.
///
/// The registry stores complete command keys rather than bare aliases. This
/// avoids collisions where two namespaces independently define an `inspect`
/// command and makes diagnostics unambiguous.
#[derive(Clone, Debug, Default)]
pub struct ActionRegistry {
    actions: BTreeMap<ContentKey, ActionDescriptor>,
    commands: BTreeMap<String, ContentKey>,
}

impl ActionRegistry {
    /// Return all validated actions in deterministic key order.
    pub fn actions(&self) -> &BTreeMap<ContentKey, ActionDescriptor> {
        &self.actions
    }

    /// Resolve a full command key such as `base:command.inspect`.
    pub fn command(&self, name: &str) -> Option<&ActionDescriptor> {
        self.commands
            .get(name)
            .and_then(|action| self.actions.get(action))
    }
}

/// Validate and freeze a set of action descriptors.
///
/// Validation is deliberately aggregate: all independently detectable issues
/// are returned together so a content author can fix a registry in one pass.
/// No partially valid registry is returned. Command ownership is tracked across
/// actions, while duplicate command names inside one action are also rejected.
pub fn validate_actions(
    actions: Vec<ActionDescriptor>,
) -> Result<ActionRegistry, Vec<ActionDiagnostic>> {
    let mut diagnostics = Vec::new();
    let mut registry = ActionRegistry::default();

    for action in actions {
        let id = action.id.clone();
        if action.api_version == 0 {
            push(
                &mut diagnostics,
                &id,
                "api_version",
                "API version must be greater than zero",
            );
        }
        if action.max_items == 0 || action.max_bytes == 0 {
            push(
                &mut diagnostics,
                &id,
                "limits",
                "action output limits must be greater than zero",
            );
        }
        // Collect both primary command names and aliases into one namespace so
        // aliases cannot silently shadow another action.
        let mut commands = Vec::new();
        for command in &action.commands {
            let name = command.name.path();
            if name.len() > MAX_COMMAND_NAME_BYTES || !name.bytes().all(is_command_byte) {
                push(
                    &mut diagnostics,
                    &id,
                    "commands.name",
                    format!("invalid command name {name}"),
                );
            }
            if command.usage_key.path().len() > MAX_USAGE_BYTES {
                push(
                    &mut diagnostics,
                    &id,
                    "commands.usage_key",
                    "usage key exceeds command metadata limit",
                );
            }
            commands.push(command.name.to_string());
            for alias in &command.aliases {
                if alias.path().len() > MAX_COMMAND_NAME_BYTES
                    || !alias.path().bytes().all(is_command_byte)
                {
                    push(
                        &mut diagnostics,
                        &id,
                        "commands.aliases",
                        format!("invalid alias {}", alias.path()),
                    );
                }
                commands.push(alias.to_string());
            }
        }
        // Track local names first. This reports a duplicate even when both
        // entries belong to the same action and therefore have the same owner.
        let mut local_commands = BTreeSet::new();
        for command in commands {
            if !local_commands.insert(command.clone()) {
                push(
                    &mut diagnostics,
                    &id,
                    "commands",
                    format!("duplicate command name {command}"),
                );
            }
            if let Some(existing) = registry.commands.get(&command) {
                if existing != &id {
                    push(
                        &mut diagnostics,
                        &id,
                        "commands",
                        format!("command {command} is already owned by {existing}"),
                    );
                }
            } else {
                registry.commands.insert(command, id.clone());
            }
        }

        if registry.actions.insert(id.clone(), action).is_some() {
            push(&mut diagnostics, &id, "id", "duplicate action definition");
        }
    }

    diagnostics.sort();
    if diagnostics.is_empty() {
        Ok(registry)
    } else {
        Err(diagnostics)
    }
}

/// Restrict command path segments to portable lowercase ASCII tokens.
fn is_command_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
}

/// Append a diagnostic without interrupting validation of later actions.
fn push(
    diagnostics: &mut Vec<ActionDiagnostic>,
    action: &ContentKey,
    field: impl Into<String>,
    message: impl Into<String>,
) {
    diagnostics.push(ActionDiagnostic {
        action: action.clone(),
        field: field.into(),
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_registry_rejects_duplicate_command_names() {
        // A command adapter must have exactly one typed target. Global ownership
        // prevents load order from deciding which behavior a command invokes.
        let actions = vec![
            action("base:action/inspect", "inspect"),
            action("base:action/probe", "inspect"),
        ];
        let errors = validate_actions(actions).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("already owned"))
        );
    }

    #[test]
    fn action_registry_freezes_commands() {
        // Successful validation returns a lookup table that resolves the full
        // namespaced command key and rejects unregistered names.
        let registry = validate_actions(vec![action("base:action/inspect", "inspect")]).unwrap();
        assert!(registry.command("base:command.inspect").is_some());
        assert!(registry.command("missing").is_none());
    }

    fn action(id: &str, command: &str) -> ActionDescriptor {
        // Keep the fixture semantically valid so failures in these tests point
        // at command ownership rather than unrelated declaration fields.
        ActionDescriptor {
            id: key(id),
            api_version: 1,
            kind: ActionKind::Query,
            parameter_schema: key("base:schema/empty"),
            result_schema: key("base:schema/inspect_result"),
            permission: PermissionRule { permission: None },
            target_constraints: Vec::new(),
            phase: ActionPhase::Interaction,
            max_items: 32,
            max_bytes: 4096,
            handler: HandlerBinding {
                handler: key("base:handler/inspect"),
                owner_module: key("base:core"),
            },
            commands: vec![CommandBinding {
                name: key(&format!("base:command.{command}")),
                aliases: Vec::new(),
                usage_key: key("base:text/usage/inspect"),
            }],
        }
    }

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).unwrap()
    }
}
