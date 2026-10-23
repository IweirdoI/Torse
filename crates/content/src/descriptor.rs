//! Public content contracts shared by content compilation and runtime registration.
//!
//! These types describe content without embedding world storage details. A block
//! keeps visual, collision, selection, occlusion, light, interaction, component,
//! capability, and behavior data separate so one concern cannot silently imply
//! another. In particular, visual transparency does not imply pass-through
//! collision, and providing a gameplay capability does not grant a Mod or player
//! permission to use it.

use torse_types::{ContentKey, ReplicationDeclaration};

/// Material blending mode consumed by the presentation pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlphaMode {
    /// The material is fully opaque.
    Opaque,
    /// Alpha is interpreted as a cutoff mask.
    Mask,
    /// Alpha is blended with the already-rendered surface.
    Blend,
}

/// Axis-aligned bounds expressed in the block's local unit cell.
///
/// Components are expected to remain in `0.0..=1.0`; validation is performed by
/// the surrounding [`Shape`] so malformed geometry never reaches a renderer or
/// collision index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxBounds {
    /// Inclusive lower corner components in cell-local space.
    pub min: [f32; 3],
    /// Exclusive upper corner components in cell-local space.
    pub max: [f32; 3],
}

/// Static geometry preset available to block definitions.
///
/// Presets compile to deterministic shape data. Arbitrary meshes and moving
/// multi-cell structures are intentionally outside the M0 contract.
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
    /// Validate the local geometry encoded by this shape.
    ///
    /// The error is a stable English description suitable for content
    /// diagnostics. It does not identify the containing file; the compiler adds
    /// source and field context.
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

/// Geometry and material selected for rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderProfile {
    /// Geometry used for visual meshing.
    pub shape: Shape,
    /// Material resource selected for the block.
    pub material: ContentKey,
    /// Material blending mode.
    pub alpha_mode: AlphaMode,
}

/// Geometry used by authoritative collision and client prediction.
///
/// `None` means the block does not contribute a collision surface, which is a
/// valid choice for plants and similar decorative content.
#[derive(Clone, Debug, PartialEq)]
pub struct CollisionProfile {
    /// Collision geometry, or `None` for non-solid content.
    pub shape: Option<Shape>,
}

/// Geometry used for ray selection and interaction targeting.
///
/// Selection may intentionally differ from collision, for example a plant can
/// have no collision while remaining clickable.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionProfile {
    /// Ray-selection geometry, or `None` for non-selectable content.
    pub shape: Option<Shape>,
}

/// Conservative face coverage used by mesh occlusion calculations.
///
/// The mask uses six bits in `-x, +x, -y, +y, -z, +z` order. A missing bit must
/// produce extra visible faces rather than incorrectly removing a neighbor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcclusionProfile {
    /// Bit mask for the six cell faces, in `-x, +x, -y, +y, -z, +z` order.
    pub face_mask: u8,
}

/// Light absorption and emission values for the content definition.
///
/// Both values are normalized to `0.0..=1.0` and are independent of collision
/// and visual alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightTransport {
    /// Fraction of incident light absorbed; validated to `0.0..=1.0`.
    pub absorption: f32,
    /// Normalized emission contribution; validated to `0.0..=1.0`.
    pub emission: f32,
}

/// Action binding used when a player or system interacts with the block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionProfile {
    /// Registered action invoked by a normal interaction.
    pub action: Option<ContentKey>,
}

/// One finite-valued state field such as orientation or growth stage.
///
/// The legal values are explicit so the compiler can calculate the total state
/// combination count and reject definitions that expand without bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiniteField {
    /// Stable field identity within the content schema.
    pub id: ContentKey,
    /// Default finite value and the value used for newly placed blocks.
    pub default_value: u16,
    /// Complete legal value set for this field.
    pub values: Vec<u16>,
}

/// Primitive field kinds supported by the initial component schema model.
///
/// Container limits are part of the type rather than an unconstrained runtime
/// convention, which lets validation and migration reason about worst-case size.
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

/// Named, typed field inside a component schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentField {
    /// Stable field identity within the component schema.
    pub id: ContentKey,
    /// Primitive storage and validation type.
    pub field_type: ComponentFieldType,
    /// Whether the field must be present when the component is instantiated.
    pub required: bool,
}

/// Whether a component is expected to survive a save/reload cycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistencePolicy {
    /// The component is runtime-only.
    Never,
    /// The component must be saved.
    Always,
    /// The owning module decides according to a narrower policy.
    OwnerDefined,
}

/// Versioned component schema owned by one module.
///
/// Ownership makes mutation responsibility explicit. Other modules are expected
/// to use capabilities or actions instead of directly rewriting protected
/// fields, even though the descriptor itself is only data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentSchema {
    /// Stable component schema identity.
    pub id: ContentKey,
    /// Schema version used by migration and compatibility checks.
    pub schema_version: u32,
    /// Module allowed to define and mutate protected fields.
    pub owner_module: ContentKey,
    /// Declared fields and their bounds.
    pub fields: Vec<ComponentField>,
    /// Save policy for this component.
    pub persistence: PersistencePolicy,
    /// Replication contract for this component.
    pub replication: ReplicationDeclaration,
    /// Optional migration handler binding.
    pub migration_binding: Option<ContentKey>,
}

/// Complete state schema for a content definition.
///
/// The finite fields determine the compact block-state space. Component schemas
/// describe larger or sparse instance data that should not be expanded into the
/// block registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateSchema {
    /// Stable state schema identity.
    pub id: ContentKey,
    /// Schema version used by content and persistence compatibility.
    pub schema_version: u32,
    /// Compact finite fields expanded into the block-state registry.
    pub finite_fields: Vec<FiniteField>,
    /// Sparse instance components associated with the state.
    pub component_schemas: Vec<ComponentSchema>,
}

/// Binding from a content object to a gameplay capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityBinding {
    /// Capability interface attached to the object.
    pub capability: ContentKey,
    /// Bit mask matching [`OcclusionProfile::face_mask`] ordering.
    pub face_mask: u8,
}

/// Top-level static block definition.
///
/// Profiles are separate fields on purpose. A content author can use a blended
/// material, full collision, and partial light transport without any one of
/// those choices being inferred from another.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockDefinition {
    /// Stable block identity across content and save versions.
    pub id: ContentKey,
    /// Version of this block definition.
    pub definition_version: u32,
    /// State schema used to interpret finite block state.
    pub state_schema: ContentKey,
    /// Visual geometry and material.
    pub render_profile: RenderProfile,
    /// Authoritative collision geometry.
    pub collision_profile: CollisionProfile,
    /// Interaction targeting geometry.
    pub selection_profile: SelectionProfile,
    /// Face coverage used by conservative mesh culling.
    pub occlusion_profile: OcclusionProfile,
    /// Light behavior independent of rendering alpha.
    pub light_transport: LightTransport,
    /// Interaction action binding.
    pub interaction_profile: InteractionProfile,
    /// Component schemas instantiated by default.
    pub component_defaults: Vec<ContentKey>,
    /// Gameplay capabilities exposed by this block.
    pub capability_bindings: Vec<CapabilityBinding>,
    /// Behavior module hooks associated with this block.
    pub behavior_bindings: Vec<ContentKey>,
    /// Filtering tags; tags do not grant capabilities or permissions.
    pub tags: Vec<ContentKey>,
}

/// Execution class of a capability implementation.
///
/// This is descriptive M0 metadata; permission checking and sandbox execution
/// are enforced by later host layers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionContract {
    /// The capability is declarative and executes no arbitrary code.
    NoRuntimeCode,
    /// A trusted native implementation performs bounded work.
    BoundedNative,
    /// A sandboxed implementation must run under host resource controls.
    Sandboxed,
}

/// Typed method exposed by a gameplay capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodDescriptor {
    /// Stable method identity within the capability API.
    pub id: ContentKey,
    /// Parameter schema used to validate and encode the call.
    pub parameter_schema: ContentKey,
    /// Result schema used to validate and encode the response.
    pub result_schema: ContentKey,
}

/// Versioned gameplay capability such as an item port or energy receiver.
///
/// A capability describes what an object can expose. It does not grant a caller
/// permission to use that capability; permissions are a separate module/host
/// concern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityDescriptor {
    /// Stable gameplay capability identity.
    pub id: ContentKey,
    /// Capability API version used for compatibility negotiation.
    pub api_version: u32,
    /// Read-only methods exposed by the capability.
    pub query_methods: Vec<MethodDescriptor>,
    /// Mutating methods exposed through the authority transaction path.
    pub mutation_methods: Vec<MethodDescriptor>,
    /// Schemas an object must satisfy to bind this capability.
    pub target_schema_constraints: Vec<ContentKey>,
    /// Contract key describing who may call the capability.
    pub access_contract: ContentKey,
    /// Execution class of the implementation.
    pub execution_contract: ExecutionContract,
}

/// Side on which a module or its implementation is available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleSide {
    Server,
    Client,
    Shared,
}

/// Stable service identity and API version provided or required by a module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDescriptor {
    /// Stable service identity.
    pub id: ContentKey,
    /// API version required for provider/consumer compatibility.
    pub api_version: u32,
}

/// Registered simulation system with an explicit phase and output budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemDescriptor {
    /// Stable system identity used for scheduling and diagnostics.
    pub id: ContentKey,
    /// Simulation phase in which the system is eligible to run.
    pub phase: u8,
    /// Maximum logical output entries allowed per execution.
    pub max_items: u32,
    /// Maximum owned or emitted bytes allowed per execution.
    pub max_bytes: u64,
}

/// Permission requested by a module, distinct from object capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestedPermission {
    /// Host permission requested by the module declaration.
    pub id: ContentKey,
}

/// Complete module registration and dependency declaration.
///
/// M0 stores the descriptor and validates its references. Lifecycle execution,
/// permission approval, and runtime service binding are implemented by later
/// milestones.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleDescriptor {
    /// Stable module identity.
    pub id: ContentKey,
    /// Version of this module package/declaration.
    pub module_version: u32,
    /// Framework API version expected by the module.
    pub framework_api: u32,
    /// Server, client, or shared availability.
    pub side: ModuleSide,
    /// Modules that must be present and started first.
    pub required_modules: Vec<ContentKey>,
    /// Modules that alter capabilities but are not required for startup.
    pub optional_modules: Vec<ContentKey>,
    /// Services supplied by this module.
    pub provided_services: Vec<ServiceDescriptor>,
    /// Services this module expects another module to supply.
    pub required_services: Vec<ServiceDescriptor>,
    /// State schema IDs registered by this module.
    pub registered_schemas: Vec<ContentKey>,
    /// Capability IDs registered by this module.
    pub registered_capabilities: Vec<ContentKey>,
    /// Action IDs registered by this module.
    pub registered_actions: Vec<ContentKey>,
    /// Simulation systems registered by this module.
    pub registered_systems: Vec<SystemDescriptor>,
    /// Host permissions requested by this module.
    pub requested_permissions: Vec<RequestedPermission>,
    /// Optional schema used to validate module configuration.
    pub configuration_schema: Option<ContentKey>,
    /// Optional lifecycle implementation binding.
    pub lifecycle_binding: Option<ContentKey>,
}
