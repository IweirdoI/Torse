//! Cooked runtime tables.
//!
//! Author files such as locale text stay outside the hot path. `cook` turns a
//! compiled content pack, image identities, and locale maps into one table.
//! Image bytes stay in the asset catalog; this table stores their guid.
//! Authority rows cover occlusion and collision. Presentation rows cover
//! material slots, image identities, and locale text. The two digests move
//! independently.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest as ShaDigest, Sha256};
use torse_assets::AssetGuid;
use torse_types::{ContentKey, Digest};

use crate::compiler::{CompiledContent, MAX_FINITE_STATES};
use crate::descriptor::{BoxBounds, Face, Shape, StateProfile};

const MAGIC: &[u8; 8] = b"TCOOK001";
const VERSION: u32 = 1;

/// One image identity recorded in the cooked table.
///
/// The pixels themselves are not copied here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CookImage {
    /// Assigned asset guid.
    pub guid: AssetGuid,
    /// Content-key alias for the image.
    pub key: ContentKey,
    /// Relative pack path alias for the image.
    pub path: String,
}

/// One language table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CookedLocale {
    /// Language tag such as `en` or `zh-Hans`.
    pub language: String,
    /// Message key to display text.
    pub entries: BTreeMap<String, String>,
}

/// One expanded block state.
#[derive(Clone, Debug, PartialEq)]
pub struct CookedState {
    /// Block identity. This remains a content key, not an asset guid.
    pub block: ContentKey,
    /// Index of this state within the block, last field varying fastest.
    pub index: u16,
    /// Finite field values in schema order.
    pub values: Vec<u16>,
    /// Six-bit occlusion mask.
    pub occlusion: u8,
    /// Authoritative collision shape, if any.
    pub collision: Option<Shape>,
    /// Material key for each face, in `-x, +x, -y, +y, -z, +z` order.
    pub materials: [ContentKey; 6],
}

/// Runtime table produced from author input.
#[derive(Clone, Debug, PartialEq)]
pub struct CookedTables {
    /// Digest of occlusion and collision rows.
    pub authority_digest: Digest,
    /// Digest of material slots, image identities, and locale text.
    pub presentation_digest: Digest,
    /// Expanded states in block-key order.
    pub states: Vec<CookedState>,
    /// Images sorted by guid.
    pub images: Vec<CookImage>,
    /// Locales sorted by language tag.
    pub locales: Vec<CookedLocale>,
}

/// Why a cook or a read did not produce a table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CookError {
    /// A block names a state schema that is not in the compiled pack.
    MissingSchema(ContentKey),
    /// A finite field has no legal values.
    EmptyField(ContentKey),
    /// The state product exceeds the content limit.
    StateLimit(ContentKey),
    /// Collision uses a visual model.
    VisualCollision(ContentKey),
    /// Two images share a guid or a key.
    DuplicateImage,
    /// Two locale tables share a language tag.
    DuplicateLanguage(String),
    /// A locale line is missing `=` or repeats a key.
    Locale(String),
    /// The byte length does not match a cooked table.
    Truncated,
    /// Bytes remain after a complete table.
    Trailing,
    /// A stored digest does not match the rows.
    DigestMismatch,
    /// The magic or version is not this table.
    Header,
}

impl std::fmt::Display for CookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSchema(key) => write!(f, "missing state schema {key}"),
            Self::EmptyField(key) => write!(f, "state field of {key} has no values"),
            Self::StateLimit(key) => write!(f, "state expansion of {key} exceeds the limit"),
            Self::VisualCollision(key) => write!(f, "collision of {key} uses a visual model"),
            Self::DuplicateImage => write!(f, "duplicate cooked image identity"),
            Self::DuplicateLanguage(language) => write!(f, "duplicate locale {language}"),
            Self::Locale(message) => write!(f, "{message}"),
            Self::Truncated => write!(f, "cooked table ended early"),
            Self::Trailing => write!(f, "cooked table has trailing bytes"),
            Self::DigestMismatch => write!(f, "cooked table digest does not match its rows"),
            Self::Header => write!(f, "cooked table header is not recognized"),
        }
    }
}

impl std::error::Error for CookError {}

/// Parse one locale file body.
///
/// Blank lines are skipped. Each remaining line is `key=value`.
pub fn parse_locale(text: &str) -> Result<BTreeMap<String, String>, CookError> {
    let mut entries = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(CookError::Locale(format!(
                "line {} is missing a separator",
                index + 1
            )));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(CookError::Locale(format!(
                "line {} has an empty key",
                index + 1
            )));
        }
        if entries
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(CookError::Locale(format!("duplicate locale key {key}")));
        }
    }
    Ok(entries)
}

/// Build a cooked table. The result is independent of input list order.
pub fn cook(
    content: &CompiledContent,
    images: &[CookImage],
    locales: &[CookedLocale],
) -> Result<CookedTables, CookError> {
    let mut states = Vec::new();
    for block in content.blocks.values() {
        let Some(schema) = content.state_schemas.get(&block.state_schema) else {
            return Err(CookError::MissingSchema(block.state_schema.clone()));
        };
        let count = state_count(&schema.finite_fields).map_err(|_| {
            if schema
                .finite_fields
                .iter()
                .any(|field| field.values.is_empty())
            {
                CookError::EmptyField(block.id.clone())
            } else {
                CookError::StateLimit(block.id.clone())
            }
        })?;
        for index in 0..count {
            let values = state_values(&schema.finite_fields, index);
            let profile = matching_profile(&block.state_overrides, &values);
            let collision = match profile.and_then(|item| item.collision_shape.clone()) {
                Some(shape) => Some(shape),
                None => block.collision_profile.shape.clone(),
            };
            if matches!(collision, Some(Shape::Model(_))) {
                return Err(CookError::VisualCollision(block.id.clone()));
            }
            let occlusion = profile
                .and_then(|item| item.occlusion)
                .unwrap_or(block.occlusion_profile.face_mask);
            let materials = core::array::from_fn(|slot| {
                let face = Face::ALL[slot];
                block
                    .render_profile
                    .face_materials
                    .resolve(face, &block.render_profile.material)
                    .clone()
            });
            states.push(CookedState {
                block: block.id.clone(),
                index: index as u16,
                values,
                occlusion,
                collision,
                materials,
            });
        }
    }

    let mut images = images.to_vec();
    images.sort_by(|left, right| left.guid.cmp(&right.guid).then(left.key.cmp(&right.key)));
    let mut seen_guid = BTreeSet::new();
    let mut seen_key = BTreeSet::new();
    for image in &images {
        if !seen_guid.insert(image.guid) || !seen_key.insert(image.key.clone()) {
            return Err(CookError::DuplicateImage);
        }
    }

    let mut locales = locales.to_vec();
    locales.sort_by(|left, right| left.language.cmp(&right.language));
    let mut seen_language = BTreeSet::new();
    for locale in &locales {
        if !seen_language.insert(locale.language.clone()) {
            return Err(CookError::DuplicateLanguage(locale.language.clone()));
        }
    }

    let authority_digest = digest_authority(&states);
    let presentation_digest = digest_presentation(&states, &images, &locales);
    Ok(CookedTables {
        authority_digest,
        presentation_digest,
        states,
        images,
        locales,
    })
}

/// Encode a table. The bytes are not an author format.
pub fn write_cooked(tables: &CookedTables) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&tables.authority_digest.0);
    out.extend_from_slice(&tables.presentation_digest.0);
    push_u32(&mut out, tables.states.len() as u32);
    for state in &tables.states {
        push_str(&mut out, &state.block.to_string());
        out.extend_from_slice(&state.index.to_le_bytes());
        push_u32(&mut out, state.values.len() as u32);
        for value in &state.values {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.push(state.occlusion);
        push_shape(&mut out, state.collision.as_ref());
        for material in &state.materials {
            push_str(&mut out, &material.to_string());
        }
    }
    push_u32(&mut out, tables.images.len() as u32);
    for image in &tables.images {
        out.extend_from_slice(&image.guid.0.to_le_bytes());
        push_str(&mut out, &image.key.to_string());
        push_str(&mut out, &image.path);
    }
    push_u32(&mut out, tables.locales.len() as u32);
    for locale in &tables.locales {
        push_str(&mut out, &locale.language);
        push_u32(&mut out, locale.entries.len() as u32);
        for (key, value) in &locale.entries {
            push_str(&mut out, key);
            push_str(&mut out, value);
        }
    }
    out
}

/// Decode a table. A short or extended buffer produces no rows.
pub fn read_cooked(bytes: &[u8]) -> Result<CookedTables, CookError> {
    let mut cursor = Cursor { bytes, at: 0 };
    let magic = cursor.bytes_exact(8)?;
    if magic != MAGIC {
        return Err(CookError::Header);
    }
    if cursor.u32()? != VERSION {
        return Err(CookError::Header);
    }
    let mut authority = [0u8; 32];
    authority.copy_from_slice(cursor.bytes_exact(32)?);
    let mut presentation = [0u8; 32];
    presentation.copy_from_slice(cursor.bytes_exact(32)?);
    let state_count = cursor.u32()? as usize;
    let mut states = Vec::with_capacity(state_count);
    for _ in 0..state_count {
        let block = ContentKey::parse(&cursor.string()?).map_err(|_| CookError::Header)?;
        let index = cursor.u16()?;
        let value_count = cursor.u32()? as usize;
        let mut values = Vec::with_capacity(value_count);
        for _ in 0..value_count {
            values.push(cursor.u16()?);
        }
        let occlusion = cursor.byte()?;
        let collision = cursor.shape()?;
        let mut materials = core::array::from_fn(|_| block.clone());
        for slot in &mut materials {
            *slot = ContentKey::parse(&cursor.string()?).map_err(|_| CookError::Header)?;
        }
        states.push(CookedState {
            block,
            index,
            values,
            occlusion,
            collision,
            materials,
        });
    }
    let image_count = cursor.u32()? as usize;
    let mut images = Vec::with_capacity(image_count);
    for _ in 0..image_count {
        let guid = AssetGuid(u128::from_le_bytes(cursor.array()?));
        let key = ContentKey::parse(&cursor.string()?).map_err(|_| CookError::Header)?;
        let path = cursor.string()?;
        images.push(CookImage { guid, key, path });
    }
    let locale_count = cursor.u32()? as usize;
    let mut locales = Vec::with_capacity(locale_count);
    for _ in 0..locale_count {
        let language = cursor.string()?;
        let entry_count = cursor.u32()? as usize;
        let mut entries = BTreeMap::new();
        for _ in 0..entry_count {
            let key = cursor.string()?;
            let value = cursor.string()?;
            entries.insert(key, value);
        }
        locales.push(CookedLocale { language, entries });
    }
    if cursor.at != bytes.len() {
        return Err(CookError::Trailing);
    }
    let tables = CookedTables {
        authority_digest: Digest(authority),
        presentation_digest: Digest(presentation),
        states,
        images,
        locales,
    };
    if tables.authority_digest != digest_authority(&tables.states)
        || tables.presentation_digest
            != digest_presentation(&tables.states, &tables.images, &tables.locales)
    {
        return Err(CookError::DigestMismatch);
    }
    Ok(tables)
}

fn state_count(fields: &[crate::descriptor::FiniteField]) -> Result<usize, ()> {
    if fields.is_empty() {
        return Ok(1);
    }
    let mut count = 1u64;
    for field in fields {
        if field.values.is_empty() {
            return Err(());
        }
        count = count.checked_mul(field.values.len() as u64).ok_or(())?;
        if count > MAX_FINITE_STATES {
            return Err(());
        }
    }
    Ok(count as usize)
}

fn state_values(fields: &[crate::descriptor::FiniteField], index: usize) -> Vec<u16> {
    if fields.is_empty() {
        return Vec::new();
    }
    let mut remainder = index;
    let mut row = vec![0u16; fields.len()];
    for (slot, field) in fields.iter().enumerate().rev() {
        let size = field.values.len();
        row[slot] = field.values[remainder % size];
        remainder /= size;
    }
    row
}

fn matching_profile<'a>(profiles: &'a [StateProfile], values: &[u16]) -> Option<&'a StateProfile> {
    profiles.iter().find(|profile| profile.values == values)
}

fn digest_authority(states: &[CookedState]) -> Digest {
    let mut hasher = Sha256::new();
    for state in states {
        hasher.update(state.block.to_string().as_bytes());
        hasher.update(state.index.to_le_bytes());
        hasher.update([state.occlusion]);
        let mut encoded = Vec::new();
        push_shape(&mut encoded, state.collision.as_ref());
        hasher.update(encoded);
    }
    Digest(hasher.finalize().into())
}

fn digest_presentation(
    states: &[CookedState],
    images: &[CookImage],
    locales: &[CookedLocale],
) -> Digest {
    let mut hasher = Sha256::new();
    for state in states {
        hasher.update(state.block.to_string().as_bytes());
        hasher.update(state.index.to_le_bytes());
        for material in &state.materials {
            hasher.update(material.to_string().as_bytes());
            hasher.update([0]);
        }
    }
    for image in images {
        hasher.update(image.guid.0.to_le_bytes());
        hasher.update(image.key.to_string().as_bytes());
        hasher.update(image.path.as_bytes());
    }
    for locale in locales {
        hasher.update(locale.language.as_bytes());
        for (key, value) in &locale.entries {
            hasher.update(key.as_bytes());
            hasher.update([0]);
            hasher.update(value.as_bytes());
            hasher.update([0]);
        }
    }
    Digest(hasher.finalize().into())
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_str(out: &mut Vec<u8>, value: &str) {
    let len = u16::try_from(value.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&value.as_bytes()[..usize::from(len)]);
}

fn push_box(out: &mut Vec<u8>, bounds: &BoxBounds) {
    for value in bounds.min.into_iter().chain(bounds.max) {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn push_shape(out: &mut Vec<u8>, shape: Option<&Shape>) {
    match shape {
        None => out.push(0),
        Some(Shape::FullCube) => out.push(1),
        Some(Shape::LowerSlab) => out.push(2),
        Some(Shape::Panel) => out.push(3),
        Some(Shape::Cross) => out.push(4),
        Some(Shape::Corner) => out.push(5),
        Some(Shape::Stair) => out.push(6),
        Some(Shape::Box(bounds)) => {
            out.push(7);
            push_box(out, bounds);
        }
        Some(Shape::Compound(boxes)) => {
            out.push(8);
            let count = u16::try_from(boxes.len()).unwrap_or(u16::MAX);
            out.extend_from_slice(&count.to_le_bytes());
            for bounds in boxes.iter().take(usize::from(count)) {
                push_box(out, bounds);
            }
        }
        Some(Shape::Model(_)) => out.push(9),
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn bytes_exact(&mut self, len: usize) -> Result<&'a [u8], CookError> {
        let end = self.at.checked_add(len).ok_or(CookError::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(CookError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, CookError> {
        Ok(self.bytes_exact(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CookError> {
        let mut bytes = [0u8; 2];
        bytes.copy_from_slice(self.bytes_exact(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, CookError> {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(self.bytes_exact(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CookError> {
        let mut bytes = [0u8; N];
        bytes.copy_from_slice(self.bytes_exact(N)?);
        Ok(bytes)
    }

    fn string(&mut self) -> Result<String, CookError> {
        let len = usize::from(self.u16()?);
        let bytes = self.bytes_exact(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| CookError::Header)
    }

    fn bounds(&mut self) -> Result<BoxBounds, CookError> {
        let mut min = [0f32; 3];
        let mut max = [0f32; 3];
        for value in min.iter_mut().chain(max.iter_mut()) {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(self.bytes_exact(4)?);
            *value = f32::from_le_bytes(bytes);
        }
        Ok(BoxBounds { min, max })
    }

    fn shape(&mut self) -> Result<Option<Shape>, CookError> {
        Ok(match self.byte()? {
            0 => None,
            1 => Some(Shape::FullCube),
            2 => Some(Shape::LowerSlab),
            3 => Some(Shape::Panel),
            4 => Some(Shape::Cross),
            5 => Some(Shape::Corner),
            6 => Some(Shape::Stair),
            7 => Some(Shape::Box(self.bounds()?)),
            8 => {
                let count = usize::from(self.u16()?);
                let mut boxes = Vec::with_capacity(count);
                for _ in 0..count {
                    boxes.push(self.bounds()?);
                }
                Some(Shape::Compound(boxes))
            }
            _ => return Err(CookError::Header),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile, m0_fixture_pack};

    fn image() -> CookImage {
        CookImage {
            guid: AssetGuid(0x0001_0000_0000_0001),
            key: ContentKey::parse("base:texture/atlas").unwrap(),
            path: "textures/atlas.png".to_owned(),
        }
    }

    fn locales() -> Vec<CookedLocale> {
        let text = include_str!("../../../games/base/assets/locales/en.txt");
        vec![CookedLocale {
            language: "en".to_owned(),
            entries: parse_locale(text).unwrap(),
        }]
    }

    #[test]
    fn locale_text_changes_only_the_presentation_digest() {
        let content = compile(m0_fixture_pack()).unwrap();
        let original = cook(&content, &[image()], &locales()).unwrap();
        let mut changed = locales();
        changed[0]
            .entries
            .insert("extra".to_owned(), "value".to_owned());
        let presentation = cook(&content, &[image()], &changed).unwrap();
        assert_eq!(original.authority_digest, presentation.authority_digest);
        assert_ne!(
            original.presentation_digest,
            presentation.presentation_digest
        );
        assert!(
            original
                .states
                .iter()
                .any(|state| state.block.to_string() == "base:stone")
        );
        assert!(
            write_cooked(&original)
                .windows(8)
                .any(|window| window == MAGIC)
        );
        assert!(
            !write_cooked(&original)
                .windows(8)
                .any(|window| window == b"\x89PNG\r\n\x1a\n")
        );
    }

    #[test]
    fn occlusion_changes_only_the_authority_digest_and_bytes_roundtrip() {
        let mut pack = m0_fixture_pack();
        let before = cook(&compile(pack.clone()).unwrap(), &[image()], &[]).unwrap();
        pack.blocks[0].value.occlusion_profile.face_mask = 0;
        let after = cook(&compile(pack).unwrap(), &[image()], &[]).unwrap();
        assert_ne!(before.authority_digest, after.authority_digest);
        assert_eq!(before.presentation_digest, after.presentation_digest);
        let bytes = write_cooked(&before);
        assert_eq!(read_cooked(&bytes).unwrap(), before);
        assert_eq!(
            read_cooked(&bytes[..bytes.len() - 1]),
            Err(CookError::Truncated)
        );
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(read_cooked(&extra), Err(CookError::Trailing));
        assert!(parse_locale("a=1\na=2").is_err());
    }
}
