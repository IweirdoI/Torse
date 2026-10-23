//! Public content contracts shared by content compilation and runtime registration.

use torse_types::{ContentKey, ReplicationDeclaration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    FullCube,
    LowerSlab,
    Panel,
    Corner,
    Stair,
    Box(BoxBounds),
}

impl Shape {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Box(bounds) => validate_box(bounds),
            Self::FullCube | Self::LowerSlab | Self::Panel | Self::Corner | Self::Stair => Ok(()),
        }
    }
}

fn validate_box(bounds: &BoxBounds) -> Result<(), &'static str> {
    for (minimum, maximum) in bounds.min.iter().zip(bounds.max.iter()) {
        if !minimum.is_finite() || !maximum.is_finite() {
            return Err("box bounds must be finite");
        }
        if *minimum < 0.0 || *maximum > 1.0 {
            return Err("box bounds must stay within the cell");
        }
        if minimum >= maximum {
            return Err("box minimum must be smaller than maximum");
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderProfile {
    pub shape: Shape,
    pub material: ContentKey,
    pub alpha_mode: AlphaMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollisionProfile {
    pub shape: Option<Shape>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionProfile {
    pub shape: Option<Shape>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcclusionProfile {
    /// Bit mask for the six cell faces, in `-x, +x, -y, +y, -z, +z` order.
    pub face_mask: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightTransport {
    pub absorption: f32,
    pub emission: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionProfile {
    pub action: Option<ContentKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiniteField {
    pub id: ContentKey,
    pub default_value: u16,
    pub values: Vec<u16>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComponentFieldType {
    Bool,
    Unsigned32,
    Signed64,
    ContentReference,
    EntityReference,
    String { max_bytes: u32 },
    List { max_items: u32 },
    Record,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentField {
    pub id: ContentKey,
    pub field_type: ComponentFieldType,
    pub required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistencePolicy {
    Never,
    Always,
    OwnerDefined,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentSchema {
    pub id: ContentKey,
    pub schema_version: u32,
    pub owner_module: ContentKey,
    pub fields: Vec<ComponentField>,
    pub persistence: PersistencePolicy,
    pub replication: ReplicationDeclaration,
    pub migration_binding: Option<ContentKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateSchema {
    pub id: ContentKey,
    pub schema_version: u32,
    pub finite_fields: Vec<FiniteField>,
    pub component_schemas: Vec<ComponentSchema>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityBinding {
    pub capability: ContentKey,
    /// Bit mask matching [`OcclusionProfile::face_mask`] ordering.
    pub face_mask: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockDefinition {
    pub id: ContentKey,
    pub definition_version: u32,
    pub state_schema: ContentKey,
    pub render_profile: RenderProfile,
    pub collision_profile: CollisionProfile,
    pub selection_profile: SelectionProfile,
    pub occlusion_profile: OcclusionProfile,
    pub light_transport: LightTransport,
    pub interaction_profile: InteractionProfile,
    pub component_defaults: Vec<ContentKey>,
    pub capability_bindings: Vec<CapabilityBinding>,
    pub behavior_bindings: Vec<ContentKey>,
    pub tags: Vec<ContentKey>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionContract {
    NoRuntimeCode,
    BoundedNative,
    Sandboxed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodDescriptor {
    pub id: ContentKey,
    pub parameter_schema: ContentKey,
    pub result_schema: ContentKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityDescriptor {
    pub id: ContentKey,
    pub api_version: u32,
    pub query_methods: Vec<MethodDescriptor>,
    pub mutation_methods: Vec<MethodDescriptor>,
    pub target_schema_constraints: Vec<ContentKey>,
    pub access_contract: ContentKey,
    pub execution_contract: ExecutionContract,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleSide {
    Server,
    Client,
    Shared,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDescriptor {
    pub id: ContentKey,
    pub api_version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemDescriptor {
    pub id: ContentKey,
    pub phase: u8,
    pub max_items: u32,
    pub max_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestedPermission {
    pub id: ContentKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleDescriptor {
    pub id: ContentKey,
    pub module_version: u32,
    pub framework_api: u32,
    pub side: ModuleSide,
    pub required_modules: Vec<ContentKey>,
    pub optional_modules: Vec<ContentKey>,
    pub provided_services: Vec<ServiceDescriptor>,
    pub required_services: Vec<ServiceDescriptor>,
    pub registered_schemas: Vec<ContentKey>,
    pub registered_capabilities: Vec<ContentKey>,
    pub registered_actions: Vec<ContentKey>,
    pub registered_systems: Vec<SystemDescriptor>,
    pub requested_permissions: Vec<RequestedPermission>,
    pub configuration_schema: Option<ContentKey>,
    pub lifecycle_binding: Option<ContentKey>,
}
