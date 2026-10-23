//! Action contracts shared by commands, UI, network input, and Mods.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use torse_types::ContentKey;

const MAX_COMMAND_NAME_BYTES: usize = 64;
const MAX_USAGE_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionKind {
    Query,
    Mutation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ActionPhase {
    Input,
    Lifecycle,
    Interaction,
    Simulation,
    Followup,
    Publish,
}

impl ActionPhase {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionRule {
    pub permission: Option<ContentKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandlerBinding {
    pub handler: ContentKey,
    pub owner_module: ContentKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandBinding {
    pub name: ContentKey,
    pub aliases: Vec<ContentKey>,
    pub usage_key: ContentKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionDescriptor {
    pub id: ContentKey,
    pub api_version: u32,
    pub kind: ActionKind,
    pub parameter_schema: ContentKey,
    pub result_schema: ContentKey,
    pub permission: PermissionRule,
    pub target_constraints: Vec<ContentKey>,
    pub phase: ActionPhase,
    pub max_items: u32,
    pub max_bytes: u64,
    pub handler: HandlerBinding,
    pub commands: Vec<CommandBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ActionDiagnostic {
    pub action: ContentKey,
    pub field: String,
    pub message: String,
}

impl fmt::Display for ActionDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.action, self.field, self.message)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ActionRegistry {
    actions: BTreeMap<ContentKey, ActionDescriptor>,
    commands: BTreeMap<String, ContentKey>,
}

impl ActionRegistry {
    pub fn actions(&self) -> &BTreeMap<ContentKey, ActionDescriptor> {
        &self.actions
    }

    pub fn command(&self, name: &str) -> Option<&ActionDescriptor> {
        self.commands
            .get(name)
            .and_then(|action| self.actions.get(action))
    }
}

/// Validate and freeze a set of action descriptors.
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

fn is_command_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
}

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
        let registry = validate_actions(vec![action("base:action/inspect", "inspect")]).unwrap();
        assert!(registry.command("base:command.inspect").is_some());
        assert!(registry.command("missing").is_none());
    }

    fn action(id: &str, command: &str) -> ActionDescriptor {
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
