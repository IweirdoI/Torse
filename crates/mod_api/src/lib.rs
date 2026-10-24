//! Public Mod SDK interface contracts.
//!
//! The behavior boundary is defined by `wit/behavior.wit`: the query,
//! diagnostics, and schedule imports plus the `invoke` export that behavior
//! mods implement. Guests generate bindings from this file; the host binds the
//! same file through `wasmtime::component::bindgen!`, so there is exactly one
//! description of the boundary.
