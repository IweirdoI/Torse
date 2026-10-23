//! Content descriptors, validation, and deterministic compilation.

#![forbid(unsafe_code)]

mod builtin;
mod compiler;
mod descriptor;

#[cfg(test)]
mod compiler_tests;

pub use builtin::m0_fixture_pack;
pub use compiler::{
    CONTENT_FORMAT_VERSION, CompiledContent, ContentDiagnostic, ContentPack, MAX_FINITE_STATES,
    SourceRef, Sourced, compile,
};
pub use descriptor::{
    AlphaMode, BlockDefinition, BoxBounds, CapabilityBinding, CapabilityDescriptor,
    CollisionProfile, ComponentField, ComponentFieldType, ComponentSchema, ExecutionContract,
    FiniteField, InteractionProfile, LightTransport, MethodDescriptor, ModuleDescriptor,
    ModuleSide, OcclusionProfile, PersistencePolicy, RenderProfile, RequestedPermission,
    SelectionProfile, ServiceDescriptor, Shape, StateSchema, SystemDescriptor,
};
