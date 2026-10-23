//! Typed action declarations and registration-time validation.

#![forbid(unsafe_code)]

mod descriptor;
mod fixture;

pub use descriptor::{
    ActionDescriptor, ActionDiagnostic, ActionKind, ActionPhase, ActionRegistry, CommandBinding,
    HandlerBinding, PermissionRule, validate_actions,
};
pub use fixture::m0_fixture_actions;
