//! World services assembled by the application, not by the world core.
//!
//! Services read authoritative data through explicit inputs and return results;
//! they do not own terrain. M1 provides two of them:
//!
//! - [`generation`] produces deterministic sections from a seed and a rule set.
//! - [`collision`] answers and moves against block collision shapes.
//!
//! Navigation provides a bounded lattice search with caller-supplied traversal policy.
//! Lighting and streaming remain application responsibilities.

#![forbid(unsafe_code)]

pub mod collision;
pub mod generation;
pub mod navigation;

pub use collision::{
    Aabb, CONTACT_EPSILON, CollisionCell, CollisionSource, LocalBox, MAX_SUBSTEP, SweepResult,
    collides, sweep, sweep_bounded,
};
pub use generation::{FlatTerrain, GenerationParams, HillTerrain, SectionGenerator};
