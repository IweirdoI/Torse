//! Budgeted runtime consumption of cooked metadata and verified PNG sources.

use crate::{AssetAlias, AssetCatalog, AssetGuid, AssetHandle, texture::Atlas};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use torse_content::{
    CookLimits, CookedMaterial, CookedState, CookedTables, read_cooked_with_limits,
};
use torse_types::{ContentKey, EvictionRank, MemoryClass, MemoryLease, MemoryPool};

/// Bounds apply before source reads and PNG decoding, in addition to table limits.
#[derive(Clone, Copy, Debug)]
pub struct LoadLimits {
    pub table: CookLimits,
    pub max_source_bytes: usize,
    pub max_image_bytes: usize,
}
impl Default for LoadLimits {
    fn default() -> Self {
        Self {
            table: CookLimits::default(),
            max_source_bytes: 8 * 1024 * 1024,
            max_image_bytes: 64 * 1024 * 1024,
        }
    }
}

/// A read callback must enforce its byte limit while reading, including errors.
pub trait ImageSource {
    fn read(&mut self, path: &str, max_bytes: usize) -> Result<Vec<u8>, String>;
}
impl<F: FnMut(&str, usize) -> Result<Vec<u8>, String>> ImageSource for F {
    fn read(&mut self, path: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        self(path, max_bytes)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ImageInfo {
    pub handle: AssetHandle,
    pub width: u32,
    pub height: u32,
}

/// One atomic initial load. An error drops all unpublished tables and images.
/// The catalog stores decoded RGBA; source hashes refer to the external PNG.
pub struct CookedResources {
    tables: CookedTables,
    assets: AssetCatalog,
    images: BTreeMap<AssetGuid, ImageInfo>,
    derived: BTreeMap<AssetGuid, MemoryLease>,
    memory: MemoryPool,
    _metadata: MemoryLease,
    limits: LoadLimits,
}
impl CookedResources {
    pub fn load(
        bytes: &[u8],
        memory: MemoryPool,
        limits: LoadLimits,
        source: &mut impl ImageSource,
    ) -> Result<Self, String> {
        if bytes.len() > limits.table.max_file_bytes {
            return Err("cooked input exceeds byte limit".into());
        }
        // Wire rows are smaller than their owned representations. Reserve a
        // conservative expansion before parsing, then shrink to reader accounting.
        let bound = bytes
            .len()
            .checked_mul(64)
            .and_then(|n| n.checked_add(4096))
            .ok_or("cooked storage bound overflow")?
            .min(limits.table.max_owned_bytes);
        let mut metadata = memory
            .reserve(MemoryClass::Resource, bound as u64, EvictionRank::Pinned)
            .map_err(|e| e.to_string())?;
        let tables = read_cooked_with_limits(bytes, limits.table).map_err(|e| e.to_string())?;
        let retained = tables
            .accounted_bytes()
            .map_err(|e| e.to_string())?
            .checked_add(
                tables
                    .images
                    .len()
                    .checked_mul(1024)
                    .ok_or("image metadata overflow")?,
            )
            .and_then(|n| n.checked_add(4096))
            .ok_or("cooked metadata overflow")?;
        metadata
            .resize(retained as u64)
            .map_err(|e| e.to_string())?;
        let assets = AssetCatalog::with_memory(memory.budget(), memory.clone());
        let mut loaded = Self {
            tables,
            assets,
            images: BTreeMap::new(),
            derived: BTreeMap::new(),
            memory,
            _metadata: metadata,
            limits,
        };
        for index in 0..loaded.tables.images.len() {
            loaded.load_image(loaded.tables.images[index].guid, source)?;
        }
        Ok(loaded)
    }

    pub fn tables(&self) -> &CookedTables {
        &self.tables
    }
    pub fn catalog(&self) -> &AssetCatalog {
        &self.assets
    }
    pub fn image(&self, guid: AssetGuid) -> Option<(ImageInfo, &[u8])> {
        let info = *self.images.get(&guid)?;
        Some((info, self.assets.get(info.handle)?))
    }
    pub fn state(&self, block: &ContentKey, index: u16) -> Option<&CookedState> {
        let index = self
            .tables
            .states
            .binary_search_by(|row| row.block.cmp(block).then(row.index.cmp(&index)))
            .ok()?;
        self.tables.states.get(index)
    }
    pub fn material(&self, slot: u32) -> Option<&CookedMaterial> {
        self.tables.materials.get(slot as usize)
    }
    pub fn message(&self, language: &str, key: &str) -> Option<&str> {
        let index = self
            .tables
            .locales
            .binary_search_by(|locale| locale.language.as_str().cmp(language))
            .ok()?;
        self.tables.locales[index]
            .entries
            .get(key)
            .map(String::as_str)
    }

    /// Reload a declared image after unloading. Identical resident data reuses its handle.
    pub fn load_image(
        &mut self,
        guid: AssetGuid,
        source: &mut impl ImageSource,
    ) -> Result<ImageInfo, String> {
        let row = self
            .tables
            .images
            .binary_search_by_key(&guid, |row| row.guid)
            .ok()
            .and_then(|index| self.tables.images.get(index))
            .ok_or("unknown cooked image")?;
        let source_bound = self
            .limits
            .max_source_bytes
            .checked_add(1)
            .and_then(|n| n.checked_mul(2))
            .ok_or("image source limit overflow")?;
        let mut scratch = self
            .memory
            .reserve(
                MemoryClass::Codec,
                source_bound as u64,
                EvictionRank::Pinned,
            )
            .map_err(|e| e.to_string())?;
        let bytes = source.read(&row.path, self.limits.max_source_bytes)?;
        if bytes.len() > self.limits.max_source_bytes || bytes.capacity() > source_bound {
            return Err(format!("{}: source exceeds byte limit", row.path));
        }
        if Sha256::digest(&bytes).as_slice() != row.source_digest.0 {
            return Err(format!("{}: source digest mismatch", row.path));
        }
        if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
            return Err(format!("{}: invalid PNG header", row.path));
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().expect("header length"));
        let height = u32::from_be_bytes(bytes[20..24].try_into().expect("header length"));
        let rgba = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .filter(|n| *n > 0 && *n <= self.limits.max_image_bytes && *n <= u32::MAX as usize)
            .ok_or("decoded image exceeds byte limit")?;
        let temporary = rgba
            .checked_mul(4)
            .and_then(|n| n.checked_add(bytes.capacity()))
            .and_then(|n| n.checked_add(65_536))
            .ok_or("image scratch bound overflow")?;
        scratch
            .resize(temporary as u64)
            .map_err(|e| e.to_string())?;
        let atlas = Atlas::decode_with_limits(
            &bytes,
            crate::texture::DecodeLimits {
                max_source_bytes: self.limits.max_source_bytes,
                max_rgba_bytes: self.limits.max_image_bytes,
            },
        )
        .map_err(|e| format!("{}: {e}", row.path))?;
        if atlas.width() != width || atlas.height() != height || atlas.pixels().len() != rgba {
            return Err("decoded PNG dimensions differ from header".into());
        }
        let aliases = [
            AssetAlias::Key(row.key.clone()),
            AssetAlias::path(&row.path).map_err(|e| e.to_string())?,
        ];
        let handle = self
            .assets
            .load(guid, atlas.pixels(), &aliases, &[])
            .map_err(|e| e.to_string())?;
        let info = ImageInfo {
            handle,
            width,
            height,
        };
        self.images.insert(guid, info);
        Ok(info)
    }

    /// Derived CPU data pins its declared sources until the derived handle is unloaded.
    pub fn load_derived(
        &mut self,
        guid: AssetGuid,
        bytes: &[u8],
        aliases: &[AssetAlias],
        dependencies: &[AssetGuid],
    ) -> Result<AssetHandle, String> {
        if self
            .tables
            .images
            .binary_search_by_key(&guid, |row| row.guid)
            .is_ok()
        {
            return Err("derived identity collides with a declared image".into());
        }
        let metadata = if self.assets.handle(guid).is_none() {
            let bytes = aliases
                .iter()
                .try_fold(256_u64, |n, alias| {
                    let len = match alias {
                        AssetAlias::Key(k) => k.to_string().len(),
                        AssetAlias::Path(p) => p.len(),
                    };
                    n.checked_add(len as u64 + 128)
                })
                .and_then(|n| n.checked_add((dependencies.len() as u64).checked_mul(32)?))
                .ok_or("derived metadata overflow")?;
            Some(
                self.memory
                    .reserve(MemoryClass::Resource, bytes, EvictionRank::Pinned)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let handle = self
            .assets
            .load(guid, bytes, aliases, dependencies)
            .map_err(|e| e.to_string())?;
        if let Some(metadata) = metadata {
            self.derived.insert(guid, metadata);
        }
        Ok(handle)
    }
    pub fn unload(&mut self, handle: AssetHandle) -> Result<(), String> {
        self.assets.unload(handle).map_err(|e| e.to_string())?;
        self.images.remove(&handle.guid());
        self.derived.remove(&handle.guid());
        Ok(())
    }
}
