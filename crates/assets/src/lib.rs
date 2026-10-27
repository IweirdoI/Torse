//! Asset location, decoding, and adaptation.
//!
//! The crate currently provides the texture-atlas primitives: PNG decoding and
//! material-pack validation. Callers own the mapping from their content
//! materials to pack entries; the base game's pack is declared by the client.

#![forbid(unsafe_code)]

pub mod catalog;
pub mod texture;

pub use catalog::{AssetAlias, AssetCatalog, AssetError, AssetGuid, AssetHandle};
