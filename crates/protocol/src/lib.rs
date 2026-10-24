//! Versioned application protocol; identities are assigned by the host.
#![forbid(unsafe_code)]
mod messages;
pub use messages::{ClientMessage, PROTOCOL_VERSION, ServerMessage};
