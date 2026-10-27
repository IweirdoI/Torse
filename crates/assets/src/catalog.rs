//! Resident CPU copies of assets.
//!
//! A caller assigns a 128-bit [`AssetGuid`]. Aliases, either a [`ContentKey`]
//! or a relative path, look that guid up. A handle pairs the guid with a
//! generation so an unloaded asset cannot be read through an old handle.
//! Loading is synchronous. Unloading drops the CPU bytes and releases the
//! resource-class charge. This crate does not upload GPU memory, stream
//! assets in the background, or change world identities.

use std::collections::{BTreeMap, BTreeSet};

use torse_types::{
    ContentKey, ContentKeyError, EvictionRank, Fault, MemoryBudget, MemoryClass, MemoryLease,
    MemoryLedger, MemoryPool, MemoryResident,
};

/// Assigned 128-bit asset identity.
///
/// This is not a [`ContentKey`] and not an entity id. Callers choose the
/// value. It is not derived from the file bytes, so a later cook step can
/// keep the same guid when the source image changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AssetGuid(pub u128);

/// Handle for one resident CPU copy.
///
/// Handles belong to the issuing catalog. Hosts keep the catalog/world epoch
/// when moving work across scenes; handles are not persistent asset identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct AssetHandle {
    guid: AssetGuid,
    generation: u64,
}

impl AssetHandle {
    /// Guid named by this handle.
    pub const fn guid(self) -> AssetGuid {
        self.guid
    }

    /// Generation assigned when this copy was loaded.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Name that resolves to a guid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetAlias {
    /// Namespaced content key. It does not become the asset guid.
    Key(ContentKey),
    /// Relative path using `/` separators.
    Path(String),
}

impl AssetAlias {
    /// Parse a content-key alias.
    pub fn key(value: &str) -> Result<Self, AssetError> {
        ContentKey::parse(value)
            .map(Self::Key)
            .map_err(AssetError::Alias)
    }

    /// Parse a relative path alias.
    ///
    /// Empty paths, absolute paths, backslashes, and `..` segments are
    /// rejected so an alias cannot name a location outside the pack.
    pub fn path(value: &str) -> Result<Self, AssetError> {
        if value.is_empty()
            || value.len() > 128
            || value.starts_with('/')
            || value.ends_with('/')
            || value.contains('\\')
            || value.contains("//")
            || value
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !value.bytes().all(is_path_byte)
        {
            return Err(AssetError::BadPath);
        }
        Ok(Self::Path(value.to_owned()))
    }
}

fn is_path_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.' | b'/')
}

/// Why a load or unload did not change the catalog.
#[derive(Debug)]
pub enum AssetError {
    /// The CPU copy contained no bytes.
    Empty,
    /// The path alias was not a relative pack path.
    BadPath,
    /// The content-key alias was not a valid key.
    Alias(ContentKeyError),
    /// An alias already names a different guid.
    AliasUsed,
    /// A dependency guid is not resident.
    MissingDependency(AssetGuid),
    /// This guid is already resident with different bytes.
    DifferentBytes,
    /// Resident dependencies are immutable until the asset is unloaded.
    DifferentDependencies,
    /// The handle does not name the resident generation.
    Stale,
    /// A resident asset still depends on this one.
    StillUsed,
    /// The resource-class budget refused the CPU copy.
    Budget(Fault),
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "asset copy contains no bytes"),
            Self::BadPath => write!(f, "asset path alias is not a relative pack path"),
            Self::Alias(error) => write!(f, "{error}"),
            Self::AliasUsed => write!(f, "asset alias already names another guid"),
            Self::MissingDependency(guid) => {
                write!(f, "asset dependency {:#x} is not loaded", guid.0)
            }
            Self::DifferentBytes => write!(f, "asset guid is already loaded with other bytes"),
            Self::DifferentDependencies => {
                write!(f, "asset guid is already loaded with other dependencies")
            }
            Self::Stale => write!(f, "asset handle does not match the resident generation"),
            Self::StillUsed => write!(f, "asset is still used by a loaded dependent"),
            Self::Budget(fault) => write!(f, "{fault}"),
        }
    }
}

impl std::error::Error for AssetError {}

#[derive(Debug)]
struct Resident {
    generation: u64,
    bytes: Vec<u8>,
    dependencies: Vec<AssetGuid>,
    dependents: usize,
    charge: torse_types::MemoryChargeId,
    keys: Vec<ContentKey>,
    paths: Vec<String>,
    _memory: Option<MemoryLease>,
}

/// Resident CPU copies and the resource-class ledger that accounts them.
#[derive(Debug)]
pub struct AssetCatalog {
    ledger: MemoryLedger,
    resident: BTreeMap<u128, Resident>,
    keys: BTreeMap<ContentKey, u128>,
    paths: BTreeMap<String, u128>,
    /// Monotonic revision also used for new handles, avoiding an ever-growing
    /// tombstone map for unloaded guids. Idempotent loads keep this revision.
    generation: u64,
    memory: Option<MemoryPool>,
}

impl AssetCatalog {
    /// Empty catalog. `budget` limits the sum of resident CPU copies.
    pub fn new(budget: MemoryBudget) -> Self {
        Self {
            ledger: MemoryLedger::new(budget),
            resident: BTreeMap::new(),
            keys: BTreeMap::new(),
            paths: BTreeMap::new(),
            generation: 0,
            memory: None,
        }
    }
    pub fn with_memory(budget: MemoryBudget, memory: MemoryPool) -> Self {
        Self {
            memory: Some(memory),
            ..Self::new(budget)
        }
    }
    /// Attach an existing catalog atomically, before sharing it with consumers.
    pub fn attach_memory(&mut self, memory: MemoryPool) -> Result<(), AssetError> {
        if self.memory.is_some() {
            return Err(AssetError::Budget(Fault::invalid_data(
                "asset memory account already attached",
            )));
        }
        let mut leases = Vec::new();
        for (id, resident) in &self.resident {
            leases.push((
                *id,
                memory
                    .reserve(
                        MemoryClass::Resource,
                        resident.bytes.len() as u64,
                        EvictionRank::ReloadableResource,
                    )
                    .map_err(AssetError::Budget)?,
            ));
        }
        for (id, lease) in leases {
            self.resident.get_mut(&id).expect("resident asset")._memory = Some(lease);
        }
        self.memory = Some(memory);
        Ok(())
    }

    /// Value hosts copy into their asset-generation stamp.
    pub const fn asset_generation(&self) -> u64 {
        self.generation
    }

    /// Bytes currently charged to the resource class.
    pub fn charged_bytes(&self) -> u64 {
        self.ledger.usage(MemoryClass::Resource)
    }

    /// Load `bytes` for `guid`.
    ///
    /// Dependencies must already be resident. A second load of the same bytes
    /// returns the existing handle and does not change aliases. Different
    /// bytes or dependencies for a resident guid are rejected, so this is not
    /// a hot reload. Dependency order and duplicates do not change identity.
    pub fn load(
        &mut self,
        guid: AssetGuid,
        bytes: &[u8],
        aliases: &[AssetAlias],
        dependencies: &[AssetGuid],
    ) -> Result<AssetHandle, AssetError> {
        if bytes.is_empty() {
            return Err(AssetError::Empty);
        }
        let dependencies: Vec<_> = dependencies
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for dependency in &dependencies {
            if !self.resident.contains_key(&dependency.0) {
                return Err(AssetError::MissingDependency(*dependency));
            }
        }
        if let Some(resident) = self.resident.get(&guid.0) {
            if resident.bytes.as_slice() != bytes {
                return Err(AssetError::DifferentBytes);
            }
            if resident.dependencies != dependencies {
                return Err(AssetError::DifferentDependencies);
            }
            return Ok(AssetHandle {
                guid,
                generation: resident.generation,
            });
        }
        self.claim_aliases(guid, aliases)?;
        let generation = self.next_generation()?;
        for dependency in &dependencies {
            if self.resident[&dependency.0].dependents == usize::MAX {
                return Err(AssetError::Budget(Fault::resource_limit(
                    "asset dependent count exhausted",
                )));
            }
        }
        let memory = self
            .memory
            .as_ref()
            .map(|pool| {
                pool.reserve(
                    MemoryClass::Resource,
                    bytes.len() as u64,
                    EvictionRank::ReloadableResource,
                )
            })
            .transpose()
            .map_err(AssetError::Budget)?;
        let charge = self
            .ledger
            .charge(MemoryResident {
                class: MemoryClass::Resource,
                bytes: bytes.len() as u64,
                rank: EvictionRank::ReloadableResource,
            })
            .map_err(AssetError::Budget)?;
        let mut keys = Vec::new();
        let mut paths = Vec::new();
        for alias in aliases {
            match alias {
                AssetAlias::Key(key) => {
                    self.keys.insert(key.clone(), guid.0);
                    keys.push(key.clone());
                }
                AssetAlias::Path(path) => {
                    self.paths.insert(path.clone(), guid.0);
                    paths.push(path.clone());
                }
            }
        }
        for dependency in &dependencies {
            if let Some(resident) = self.resident.get_mut(&dependency.0) {
                resident.dependents += 1;
            }
        }
        self.resident.insert(
            guid.0,
            Resident {
                generation,
                bytes: bytes.to_vec(),
                dependencies,
                dependents: 0,
                charge,
                keys,
                paths,
                _memory: memory,
            },
        );
        self.generation = generation;
        Ok(AssetHandle { guid, generation })
    }

    /// Drop a resident copy and release its resource-class charge.
    pub fn unload(&mut self, handle: AssetHandle) -> Result<(), AssetError> {
        let Some(resident) = self.resident.get(&handle.guid.0) else {
            return Err(AssetError::Stale);
        };
        if resident.generation != handle.generation || resident.dependents > 0 {
            return Err(if resident.generation != handle.generation {
                AssetError::Stale
            } else {
                AssetError::StillUsed
            });
        }
        let generation = self.next_generation()?;
        let dependencies = resident.dependencies.clone();
        let charge = resident.charge;
        let keys = resident.keys.clone();
        let paths = resident.paths.clone();
        self.ledger.release(charge).map_err(AssetError::Budget)?;
        for key in keys {
            self.keys.remove(&key);
        }
        for path in paths {
            self.paths.remove(&path);
        }
        self.resident.remove(&handle.guid.0);
        for dependency in dependencies {
            if let Some(resident) = self.resident.get_mut(&dependency.0) {
                resident.dependents = resident.dependents.saturating_sub(1);
            }
        }
        self.generation = generation;
        Ok(())
    }

    /// Borrow the resident bytes when the handle generation is current.
    pub fn get(&self, handle: AssetHandle) -> Option<&[u8]> {
        let resident = self.resident.get(&handle.guid.0)?;
        if resident.generation != handle.generation {
            return None;
        }
        Some(&resident.bytes)
    }

    /// Current resident handle for a guid resolved through an alias.
    pub fn handle(&self, guid: AssetGuid) -> Option<AssetHandle> {
        let resident = self.resident.get(&guid.0)?;
        Some(AssetHandle {
            guid,
            generation: resident.generation,
        })
    }

    /// Guid currently named by a content key, if that copy is resident.
    pub fn guid_for_key(&self, key: &ContentKey) -> Option<AssetGuid> {
        self.keys.get(key).copied().map(AssetGuid)
    }

    /// Guid currently named by a relative path, if that copy is resident.
    pub fn guid_for_path(&self, path: &str) -> Option<AssetGuid> {
        self.paths.get(path).copied().map(AssetGuid)
    }

    fn claim_aliases(&self, guid: AssetGuid, aliases: &[AssetAlias]) -> Result<(), AssetError> {
        for alias in aliases {
            let taken = match alias {
                AssetAlias::Key(key) => self.keys.get(key).copied(),
                AssetAlias::Path(path) => {
                    AssetAlias::path(path)?;
                    self.paths.get(path).copied()
                }
            };
            if taken.is_some_and(|owner| owner != guid.0) {
                return Err(AssetError::AliasUsed);
            }
        }
        Ok(())
    }

    fn next_generation(&self) -> Result<u64, AssetError> {
        self.generation
            .checked_add(1)
            .ok_or_else(|| AssetError::Budget(Fault::resource_limit("asset generation exhausted")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_resource_pressure_retries_and_releases_payloads() {
        let pool = MemoryPool::new(MemoryBudget::new(64));
        let mut catalog = AssetCatalog::with_memory(MemoryBudget::new(128), pool.clone());
        let temporary = pool
            .reserve(MemoryClass::Codec, 33, EvictionRank::Pinned)
            .unwrap();
        assert!(catalog.load(guid(1), &[1; 32], &[], &[]).is_err());
        assert!(catalog.resident.is_empty());
        assert_eq!(catalog.asset_generation(), 0);
        drop(temporary);
        let handle = catalog.load(guid(1), &[1; 32], &[], &[]).unwrap();
        assert_eq!(pool.usage().current[MemoryClass::Resource.index()], 32);
        catalog.unload(handle).unwrap();
        assert_eq!(pool.usage().total, 0);
        catalog.load(guid(1), &[2; 64], &[], &[]).unwrap();
        drop(catalog);
        assert_eq!(pool.usage().total, 0);
    }

    fn guid(value: u128) -> AssetGuid {
        AssetGuid(value)
    }

    fn catalog() -> AssetCatalog {
        AssetCatalog::new(MemoryBudget::new(64))
    }

    #[test]
    fn load_resolves_aliases_and_unload_invalidates_the_handle() {
        let mut catalog = catalog();
        let key = AssetAlias::key("base:texture/atlas").unwrap();
        let path = AssetAlias::path("textures/atlas.png").unwrap();
        let handle = catalog.load(guid(1), b"atlas", &[key, path], &[]).unwrap();
        assert_eq!(catalog.get(handle), Some(b"atlas".as_slice()));
        assert_eq!(
            catalog.guid_for_key(&ContentKey::parse("base:texture/atlas").unwrap()),
            Some(guid(1))
        );
        assert_eq!(catalog.guid_for_path("textures/atlas.png"), Some(guid(1)));
        assert_eq!(catalog.charged_bytes(), 5);
        assert_eq!(catalog.asset_generation(), 1);
        catalog.unload(handle).unwrap();
        assert_eq!(catalog.get(handle), None);
        assert_eq!(catalog.charged_bytes(), 0);
        assert_eq!(catalog.guid_for_path("textures/atlas.png"), None);
        assert_eq!(catalog.asset_generation(), 2);
        let again = catalog.load(guid(1), b"atlas", &[], &[]).unwrap();
        assert_ne!(again.generation(), handle.generation());
        assert_eq!(catalog.get(handle), None);
        assert_eq!(catalog.get(again), Some(b"atlas".as_slice()));
    }

    #[test]
    fn dependencies_block_unload_and_a_missing_one_loads_nothing() {
        let mut catalog = catalog();
        let base = catalog.load(guid(1), b"base", &[], &[]).unwrap();
        let missing = catalog.load(guid(2), b"child", &[], &[guid(9)]);
        assert!(matches!(missing, Err(AssetError::MissingDependency(_))));
        assert_eq!(catalog.charged_bytes(), 4);
        let child = catalog.load(guid(2), b"child", &[], &[guid(1)]).unwrap();
        assert!(matches!(catalog.unload(base), Err(AssetError::StillUsed)));
        assert_eq!(catalog.get(base), Some(b"base".as_slice()));
        catalog.unload(child).unwrap();
        catalog.unload(base).unwrap();
        assert_eq!(catalog.charged_bytes(), 0);
    }

    #[test]
    fn a_full_budget_and_a_changed_payload_leave_the_catalog() {
        let mut catalog = catalog();
        let handle = catalog.load(guid(1), &[7; 64], &[], &[]).unwrap();
        let refused = catalog.load(guid(2), b"more", &[], &[]);
        assert!(matches!(refused, Err(AssetError::Budget(_))));
        assert_eq!(catalog.charged_bytes(), 64);
        assert!(catalog.get(handle).is_some());
        let same = catalog.load(guid(1), &[7; 64], &[], &[]).unwrap();
        assert_eq!(same, handle);
        assert!(matches!(
            catalog.load(guid(1), b"other", &[], &[]),
            Err(AssetError::DifferentBytes)
        ));
        assert_eq!(catalog.asset_generation(), 1);
        assert!(AssetAlias::path("../secret").is_err());
        assert!(AssetAlias::path("/absolute").is_err());
    }

    #[test]
    fn raw_path_aliases_cannot_bypass_validation() {
        let mut catalog = catalog();
        for path in ["../secret", "/absolute", concat!("C:", "/outside"), "a\\b"] {
            assert!(matches!(
                catalog.load(guid(1), b"copy", &[AssetAlias::Path(path.into())], &[]),
                Err(AssetError::BadPath)
            ));
            assert_eq!(catalog.charged_bytes(), 0);
            assert_eq!(catalog.asset_generation(), 0);
            assert!(catalog.guid_for_path(path).is_none());
        }
    }

    #[test]
    fn resident_load_cannot_silently_change_dependencies() {
        let mut catalog = catalog();
        let base = catalog.load(guid(1), b"base", &[], &[]).unwrap();
        catalog.load(guid(2), b"child", &[], &[]).unwrap();
        let before = catalog.asset_generation();
        assert!(catalog.load(guid(2), b"child", &[], &[guid(1)]).is_err());
        assert_eq!(catalog.asset_generation(), before);
        catalog.unload(base).unwrap();
    }

    #[test]
    fn exhausted_generation_rejects_load_and_unload_atomically() {
        let mut catalog = catalog();
        catalog.generation = u64::MAX;
        assert!(catalog.load(guid(1), b"copy", &[], &[]).is_err());
        assert_eq!(catalog.charged_bytes(), 0);
        assert!(catalog.resident.is_empty());

        catalog.generation = 0;
        let handle = catalog.load(guid(1), b"copy", &[], &[]).unwrap();
        catalog.generation = u64::MAX;
        assert!(catalog.unload(handle).is_err());
        assert_eq!(catalog.get(handle), Some(b"copy".as_slice()));
        assert_eq!(catalog.charged_bytes(), 4);
    }

    #[test]
    fn class_limit_and_alias_conflicts_leave_no_partial_registration() {
        let mut catalog =
            AssetCatalog::new(MemoryBudget::new(64).with_class_limit(MemoryClass::Resource, 4));
        let alias = AssetAlias::path("a.png").unwrap();
        let first = catalog
            .load(guid(1), b"abcd", std::slice::from_ref(&alias), &[])
            .unwrap();
        assert!(matches!(
            catalog.load(guid(2), b"x", std::slice::from_ref(&alias), &[]),
            Err(AssetError::AliasUsed)
        ));
        let other = AssetAlias::path("b.png").unwrap();
        assert!(matches!(
            catalog.load(guid(2), b"x", &[other], &[]),
            Err(AssetError::Budget(_))
        ));
        assert_eq!(catalog.asset_generation(), 1);
        assert_eq!(catalog.guid_for_path("a.png"), Some(guid(1)));
        assert!(catalog.guid_for_path("b.png").is_none());
        assert!(catalog.handle(guid(2)).is_none());
        assert_eq!(catalog.get(first), Some(b"abcd".as_slice()));
    }

    #[test]
    fn dependency_duplicates_are_idempotent_and_generations_survive_unload() {
        let mut catalog = catalog();
        let base = catalog.load(guid(1), b"base", &[], &[]).unwrap();
        let child = catalog
            .load(guid(2), b"child", &[], &[guid(1), guid(1)])
            .unwrap();
        assert_eq!(
            catalog.load(guid(2), b"child", &[], &[guid(1)]).unwrap(),
            child
        );
        assert_eq!(catalog.handle(guid(2)), Some(child));
        assert!(matches!(catalog.unload(base), Err(AssetError::StillUsed)));
        catalog.unload(child).unwrap();
        catalog.unload(base).unwrap();
        assert!(catalog.resident.is_empty());
        let again = catalog.load(guid(2), b"new", &[], &[]).unwrap();
        assert!(again.generation() > child.generation());
        assert!(catalog.get(child).is_none());
    }
}
