//! Asset location, decoding, and adaptation.
//!
//! The crate provides resident CPU asset handles with explicit load/unload and
//! resource-class budgets, plus PNG decoding and material-pack validation.
//! Callers own the mapping from their content
//! materials to pack entries; the base game's pack is declared by the client.

#![forbid(unsafe_code)]

pub mod catalog;
pub mod texture;

pub use catalog::{AssetAlias, AssetCatalog, AssetError, AssetGuid, AssetHandle};
