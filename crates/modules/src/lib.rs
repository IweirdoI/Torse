//! Module dependency resolution and registration-time validation.

#![forbid(unsafe_code)]

mod resolver;

pub use resolver::{ModuleError, ResolvedModules, resolve_modules};
