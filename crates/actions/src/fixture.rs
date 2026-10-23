//! M0 action fixtures used by compiler and tooling integration tests.
//!
//! These descriptors cover read-only and mutation actions. They are not gameplay
//! implementations; their purpose is to exercise registry validation, command
//! ownership, versioning, phases, and output limits without a running world.

use torse_types::ContentKey;

use crate::{
    ActionDescriptor, ActionKind, ActionPhase, CommandBinding, HandlerBinding, PermissionRule,
};

/// Build the deterministic action set used by the M0 content fixture.
///
/// The fixture is intentionally dependency-free so tooling can compile and
/// validate the action boundary before the authority simulation exists.
pub fn m0_fixture_actions() -> Vec<ActionDescriptor> {
    vec![
        action(
            "base:action/inspect",
            ActionKind::Query,
            "inspect",
            ActionPhase::Input,
        ),
        action(
            "base:action/place",
            ActionKind::Mutation,
            "place",
            ActionPhase::Interaction,
        ),
        action(
            "base:action/break",
            ActionKind::Mutation,
            "break",
            ActionPhase::Interaction,
        ),
    ]
}

fn action(id: &str, kind: ActionKind, command: &str, phase: ActionPhase) -> ActionDescriptor {
    // The handler identity follows the action path. It remains a namespaced
    // key so the later module loader can verify the declaring owner.
    let id = key(id);
    ActionDescriptor {
        id: id.clone(),
        api_version: 1,
        kind,
        parameter_schema: key("base:schema/empty"),
        result_schema: key("base:schema/action_result"),
        permission: PermissionRule { permission: None },
        target_constraints: Vec::new(),
        phase,
        max_items: 64,
        max_bytes: 8192,
        handler: HandlerBinding {
            handler: key(&format!("base:handler.{}", id.path())),
            owner_module: key("base:core"),
        },
        commands: vec![CommandBinding {
            name: key(&format!("base:command.{command}")),
            aliases: Vec::new(),
            usage_key: key(&format!("base:text.usage.{command}")),
        }],
    }
}

fn key(value: &str) -> ContentKey {
    ContentKey::parse(value).expect("built-in action key is valid")
}
