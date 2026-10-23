//! Declarative replication metadata. These types do not implement prediction.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityMode {
    Server,
    ClientPresentation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicationVisibility {
    Private,
    Authorized,
    Public,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateMode {
    CompleteTransaction,
    LatestValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationHint {
    None,
    Discrete,
    Interpolate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplicationDeclaration {
    pub authority: AuthorityMode,
    pub visibility: ReplicationVisibility,
    pub update_mode: UpdateMode,
    pub presentation_hint: PresentationHint,
}

impl ReplicationDeclaration {
    pub const SERVER_PRIVATE: Self = Self {
        authority: AuthorityMode::Server,
        visibility: ReplicationVisibility::Private,
        update_mode: UpdateMode::CompleteTransaction,
        presentation_hint: PresentationHint::None,
    };
}
