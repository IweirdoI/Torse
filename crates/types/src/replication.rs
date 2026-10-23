//! Declarative replication metadata. These types do not implement prediction.
//!
//! The declaration records the intended authority and visibility contract.
//! It is deliberately separate from protocol encoding and from gameplay
//! prediction algorithms: a declaration can describe what may be replicated,
//! but the network layer still enforces permissions and the gameplay module
//! still owns any prediction and correction logic.

/// Who is allowed to own the authoritative value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityMode {
    /// The server simulation owns and validates the value.
    Server,
    /// The value is local presentation state and must not overwrite authority.
    ClientPresentation,
}

/// Which viewers may receive a field if other authorization checks also pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicationVisibility {
    /// The value is never sent to clients.
    Private,
    /// The value is sent only after viewer and object authorization.
    Authorized,
    /// The value is visible to every viewer in the relevant subscription.
    Public,
}

/// How updates are grouped when they reach a replica.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateMode {
    /// Preserve the complete transaction boundary.
    CompleteTransaction,
    /// A later value may replace an older value with no ordering guarantee.
    LatestValue,
}

/// Optional hint for the presentation layer only.
///
/// Hints never change authoritative values and do not generate prediction logic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationHint {
    /// No special presentation behavior is requested.
    None,
    /// Apply values as discrete visual state changes.
    Discrete,
    /// Smooth between observed values where the presentation layer supports it.
    Interpolate,
}

/// Complete replication policy attached to a field or component schema.
///
/// Keeping the four dimensions explicit avoids the common mistake of treating
/// "public" as equivalent to "predictable" or "authoritative".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplicationDeclaration {
    /// Owner of the authoritative value.
    pub authority: AuthorityMode,
    /// Viewer-class visibility requested by the schema.
    pub visibility: ReplicationVisibility,
    /// Whether updates preserve transaction grouping or may be collapsed.
    pub update_mode: UpdateMode,
    /// Optional presentation behavior; never changes authority semantics.
    pub presentation_hint: PresentationHint,
}

impl ReplicationDeclaration {
    /// Conservative default for server-owned state that is saved but not replicated.
    pub const SERVER_PRIVATE: Self = Self {
        authority: AuthorityMode::Server,
        visibility: ReplicationVisibility::Private,
        update_mode: UpdateMode::CompleteTransaction,
        presentation_hint: PresentationHint::None,
    };
}
