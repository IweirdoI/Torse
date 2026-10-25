//! Compact runtime identity for compiled block content.
//!
//! Sections store a small integer per cell instead of a name or a pointer. The
//! registry turns [`CompiledContent`] into that integer space once, at startup,
//! and then answers storage, meshing, and collision queries in constant time.
//!
//! Two sentinels must not be conflated:
//!
//! - [`BlockStateId::EMPTY`] means the cell is known to be empty (air).
//! - `NotLoaded` is a terrain query result for a section that has no
//!   authoritative data at all. It is defined in `torse-world::store`, not here.
//!
//! Finite state fields are expanded at registration time. A schema with one
//! four-valued field therefore occupies four consecutive state identifiers, and
//! a plant with growth `2` is `default_state + 2`. Expansion is bounded by
//! [`MAX_FINITE_STATES`], so a malformed schema cannot allocate an unbounded
//! registry.

use std::collections::BTreeMap;

use torse_content::{BlockDefinition, CompiledContent, FiniteField, MAX_FINITE_STATES};
use torse_types::ContentKey;

/// Content key reserved for empty occupancy.
///
/// The framework, not a content pack, owns this identity. A pack that declares
/// it is rejected so that "empty cell" always has exactly one meaning.
pub const EMPTY_BLOCK_KEY: &str = "core:empty";

/// Compact block-state identifier stored in a section cell.
///
/// Zero is reserved for empty occupancy. The remaining range is dense: every
/// identifier in `1..state_count` belongs to exactly one compiled definition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlockStateId(pub u16);

impl BlockStateId {
    /// The reserved empty-occupancy state.
    pub const EMPTY: Self = Self(0);

    /// Return whether this cell contains no block.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// One block definition together with its slice of the state space.
#[derive(Clone, Debug)]
struct BlockEntry {
    /// Compiled definition shared by every state of this block.
    definition: BlockDefinition,
    /// First state identifier owned by this definition.
    first_state: u16,
    /// Number of finite state combinations.
    state_count: u16,
    /// Finite field identities in canonical order.
    fields: Vec<ContentKey>,
    /// Per-state field values, `state_count * fields.len()` entries.
    values: Vec<u16>,
    /// State identifier used for newly placed blocks.
    default_state: u16,
}

/// Dense state table produced by [`BlockStateRegistry::build`].
///
/// The registry is immutable after construction. Runtime edits only change which
/// state identifier a cell holds; they never add or remove definitions.
#[derive(Clone, Debug, Default)]
pub struct BlockStateRegistry {
    /// Definitions in registration order.
    entries: Vec<BlockEntry>,
    /// Stable identity of each entry, parallel to `entries`.
    keys: Vec<ContentKey>,
    /// Lookup from stable content identity to entries index.
    by_key: BTreeMap<ContentKey, u32>,
    /// Dense mapping from state identifier to entries index.
    slots: Vec<u32>,
    /// Number of allocated state identifiers including the empty sentinel.
    state_count: u32,
}

impl BlockStateRegistry {
    /// Register every compiled block definition.
    ///
    /// Definitions are registered in [`BTreeMap`] order, which makes identifier
    /// assignment independent of loader order. The empty sentinel is always
    /// identifier zero and is never assigned to a definition.
    pub fn build(content: &CompiledContent) -> Result<Self, RegistryError> {
        let mut registry = Self {
            state_count: 1,
            slots: vec![u32::MAX],
            ..Self::default()
        };

        for (key, definition) in &content.blocks {
            if key.to_string() == EMPTY_BLOCK_KEY {
                return Err(RegistryError::ReservedKey { key: key.clone() });
            }
            let schema = content
                .state_schemas
                .get(&definition.state_schema)
                .ok_or_else(|| RegistryError::UnknownStateSchema {
                    block: key.clone(),
                    schema: definition.state_schema.clone(),
                })?;

            let mut combinations: u64 = 1;
            for field in &schema.finite_fields {
                if field.values.is_empty() {
                    return Err(RegistryError::EmptyFieldValues {
                        block: key.clone(),
                        field: field.id.clone(),
                    });
                }
                combinations = combinations.saturating_mul(field.values.len() as u64);
            }
            if combinations > MAX_FINITE_STATES {
                return Err(RegistryError::StateExpansionOverflow {
                    block: key.clone(),
                    combinations,
                    limit: MAX_FINITE_STATES,
                });
            }

            let state_count =
                u16::try_from(combinations).map_err(|_| RegistryError::StateExpansionOverflow {
                    block: key.clone(),
                    combinations,
                    limit: u64::from(u16::MAX),
                })?;
            let first_state = u16::try_from(registry.state_count).map_err(|_| {
                RegistryError::StateSpaceExhausted {
                    block: key.clone(),
                    needed: combinations,
                }
            })?;
            if registry.state_count + u32::from(state_count) > u32::from(u16::MAX) + 1 {
                return Err(RegistryError::StateSpaceExhausted {
                    block: key.clone(),
                    needed: combinations,
                });
            }

            let fields: Vec<ContentKey> = schema
                .finite_fields
                .iter()
                .map(|field| field.id.clone())
                .collect();
            let values = expand_values(&schema.finite_fields, usize::from(state_count));
            let default_state = first_state + default_offset(&schema.finite_fields) as u16;

            let index = u32::try_from(registry.entries.len()).map_err(|_| {
                RegistryError::StateSpaceExhausted {
                    block: key.clone(),
                    needed: combinations,
                }
            })?;
            registry.by_key.insert(key.clone(), index);
            registry.keys.push(key.clone());
            registry
                .slots
                .extend(std::iter::repeat_n(index, usize::from(state_count)));
            registry.entries.push(BlockEntry {
                definition: definition.clone(),
                first_state,
                state_count,
                fields,
                values,
                default_state,
            });
            registry.state_count += u32::from(state_count);
        }

        if registry.state_count > u32::from(u16::MAX) + 1 {
            return Err(RegistryError::StateSpaceExhausted {
                block: ContentKey::parse(EMPTY_BLOCK_KEY).expect("reserved key is valid"),
                needed: u64::from(registry.state_count),
            });
        }
        Ok(registry)
    }

    /// Number of distinct state identifiers, including the empty sentinel.
    pub fn state_count(&self) -> u32 {
        self.state_count
    }

    /// Number of registered block definitions.
    pub fn block_count(&self) -> usize {
        self.entries.len()
    }

    /// Return the empty-occupancy state.
    pub const fn empty(&self) -> BlockStateId {
        BlockStateId::EMPTY
    }

    /// Return the state used when a block is newly placed.
    ///
    /// The default value of each finite field is honoured when it is declared;
    /// otherwise the first legal value is used.
    pub fn default_state(&self, key: &ContentKey) -> Option<BlockStateId> {
        let entry = self.entry(key)?;
        Some(BlockStateId(entry.default_state))
    }

    /// Resolve explicit finite values to a state identifier.
    ///
    /// `values` follow the schema's canonical field order, which content
    /// compilation defines by sorting finite fields by identity. A length
    /// mismatch or an undeclared value returns `None`.
    pub fn state_of(&self, key: &ContentKey, values: &[u16]) -> Option<BlockStateId> {
        let entry = self.entry(key)?;
        if values.len() != entry.fields.len() {
            return None;
        }
        self.scan_state(entry, values)
    }

    /// Return the definition that owns a state identifier.
    pub fn definition(&self, state: BlockStateId) -> Option<&BlockDefinition> {
        self.entry_of(state).map(|entry| &entry.definition)
    }

    /// Return the block identity that owns a state identifier.
    pub fn definition_key(&self, state: BlockStateId) -> Option<&ContentKey> {
        let index = usize::try_from(*self.slots.get(usize::from(state.0))?).ok()?;
        self.keys.get(index).filter(|_| !state.is_empty())
    }

    /// Return the finite field values of a state identifier.
    ///
    /// The slice follows the schema's canonical field order. Empty cells have
    /// no values and return `None`.
    pub fn state_values(&self, state: BlockStateId) -> Option<&[u16]> {
        let entry = self.entry_of(state)?;
        let width = entry.fields.len();
        let offset = usize::from(state.0 - entry.first_state) * width;
        entry.values.get(offset..offset + width)
    }

    /// Conservative face coverage of a state.
    ///
    /// Empty cells cover nothing, which makes their neighbors render every face
    /// instead of accidentally culling it.
    pub fn occlusion_mask(&self, state: BlockStateId) -> u8 {
        self.definition(state)
            .map_or(0, |definition| definition.occlusion_profile.face_mask)
    }

    fn scan_state(&self, entry: &BlockEntry, values: &[u16]) -> Option<BlockStateId> {
        let width = entry.fields.len();
        (0..usize::from(entry.state_count)).find_map(|row| {
            let start = row * width;
            let candidate = entry.values.get(start..start + width)?;
            (candidate == values).then_some(BlockStateId(entry.first_state + row as u16))
        })
    }

    fn entry(&self, key: &ContentKey) -> Option<&BlockEntry> {
        self.entries
            .get(usize::try_from(*self.by_key.get(key)?).ok()?)
    }

    fn entry_of(&self, state: BlockStateId) -> Option<&BlockEntry> {
        if state.is_empty() {
            return None;
        }
        let index = usize::try_from(*self.slots.get(usize::from(state.0))?).ok()?;
        self.entries.get(index)
    }
}

/// Build the per-state table of finite values.
///
/// The layout is row-major over the canonical field order: state `s` stores one
/// value per field, with the last field varying fastest.
fn expand_values(fields: &[FiniteField], state_count: usize) -> Vec<u16> {
    let mut values = Vec::with_capacity(fields.len() * state_count);
    for state in 0..state_count {
        let mut remainder = state;
        let mut row = vec![0u16; fields.len()];
        for (index, field) in fields.iter().enumerate().rev() {
            let size = field.values.len();
            row[index] = field.values[remainder % size];
            remainder /= size;
        }
        values.extend_from_slice(&row);
    }
    values
}

/// Return the row-major offset of the declared default combination.
fn default_offset(fields: &[FiniteField]) -> usize {
    let mut offset = 0usize;
    for field in fields {
        let position = field
            .values
            .iter()
            .position(|value| *value == field.default_value)
            .unwrap_or(0);
        offset = offset * field.values.len() + position;
    }
    offset
}

/// Failure produced while building a state registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    /// A content pack tried to redefine the framework-owned empty sentinel.
    ReservedKey {
        /// Offending key.
        key: ContentKey,
    },
    /// A block referenced a state schema that is not registered.
    UnknownStateSchema {
        /// Block identity.
        block: ContentKey,
        /// Missing schema identity.
        schema: ContentKey,
    },
    /// A finite field declared no legal values.
    EmptyFieldValues {
        /// Block identity.
        block: ContentKey,
        /// Offending field.
        field: ContentKey,
    },
    /// One definition expanded past the per-schema state limit.
    StateExpansionOverflow {
        /// Block identity.
        block: ContentKey,
        /// Requested combination count.
        combinations: u64,
        /// Configured limit.
        limit: u64,
    },
    /// The global 16-bit state space is exhausted.
    StateSpaceExhausted {
        /// Block identity that no longer fits.
        block: ContentKey,
        /// Combinations that were requested.
        needed: u64,
    },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedKey { key } => {
                write!(f, "{key} is reserved for the framework empty sentinel")
            }
            Self::UnknownStateSchema { block, schema } => {
                write!(f, "block {block} references unknown state schema {schema}")
            }
            Self::EmptyFieldValues { block, field } => {
                write!(f, "block {block} state field {field} has no legal values")
            }
            Self::StateExpansionOverflow {
                block,
                combinations,
                limit,
            } => write!(
                f,
                "block {block} expands to {combinations} states, limit is {limit}"
            ),
            Self::StateSpaceExhausted { block, needed } => write!(
                f,
                "state space exhausted while registering {block}; needs {needed} identifiers"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use torse_content::{SourceRef, Sourced, compile, m0_fixture_pack};

    fn fixture_registry() -> BlockStateRegistry {
        let content = compile(m0_fixture_pack()).expect("fixture content compiles");
        BlockStateRegistry::build(&content).expect("fixture registry builds")
    }

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key is valid")
    }

    #[test]
    fn empty_sentinel_is_zero_and_not_a_block() {
        let registry = fixture_registry();
        assert_eq!(registry.empty(), BlockStateId(0));
        assert!(registry.empty().is_empty());
        assert!(registry.definition(BlockStateId::EMPTY).is_none());
        assert_eq!(registry.occlusion_mask(BlockStateId::EMPTY), 0);
    }

    #[test]
    fn default_states_cover_every_block() {
        let registry = fixture_registry();
        assert_eq!(registry.block_count(), 5);
        for block in [
            key("base:stone"),
            key("base:glass_slab"),
            key("base:glass_slab_upper"),
            key("base:plant"),
            key("base:glass"),
        ] {
            let state = registry
                .default_state(&block)
                .expect("block has a default state");
            assert!(!state.is_empty());
            assert_eq!(
                registry.definition(state).map(|definition| &definition.id),
                Some(&block)
            );
            assert_eq!(registry.definition_key(state), Some(&block));
        }
        assert!(registry.default_state(&key("base:missing")).is_none());
    }

    #[test]
    fn finite_fields_expand_into_consecutive_states() {
        let registry = fixture_registry();
        let plant = key("base:plant");
        let first = registry.default_state(&plant).expect("plant default state");
        let last = registry
            .state_of(&plant, &[3])
            .expect("growth 3 is declared");
        assert_eq!(last.0, first.0 + 3);
        assert_eq!(registry.state_values(last), Some(&[3][..]));
        assert_eq!(registry.state_values(first), Some(&[0][..]));
        assert!(registry.state_of(&plant, &[4]).is_none());
        assert!(registry.state_of(&plant, &[]).is_none());
    }

    #[test]
    fn identifiers_do_not_depend_on_registration_order() {
        // Two builds from the same content must assign identical numbers, which
        // is what lets a save file or a network message use them transiently.
        let first = fixture_registry();
        let second = fixture_registry();
        assert_eq!(first.state_count(), second.state_count());
        for block in [
            key("base:stone"),
            key("base:glass_slab"),
            key("base:plant"),
            key("base:glass"),
        ] {
            assert_eq!(first.default_state(&block), second.default_state(&block));
        }
    }

    #[test]
    fn reserved_empty_key_is_rejected() {
        let mut pack = m0_fixture_pack();
        let mut block = pack.blocks[0].value.clone();
        block.id = key(EMPTY_BLOCK_KEY);
        pack.blocks.push(Sourced::new(
            SourceRef::new("fixtures/m0/core_empty.json", 1),
            block,
        ));
        let content = compile(pack).expect("the pack still compiles");
        let error = BlockStateRegistry::build(&content).expect_err("reserved key is rejected");
        assert!(matches!(error, RegistryError::ReservedKey { .. }));
    }

    #[test]
    fn two_field_schema_uses_canonical_row_major_order() {
        // Content compilation sorts finite fields by identity, so `axis`
        // precedes `growth` even though it was appended last. The final field in
        // that canonical order varies fastest inside the state space.
        let mut pack = m0_fixture_pack();
        let mut schema = pack.state_schemas[1].value.clone();
        schema.finite_fields.push(FiniteField {
            id: key("base:field/axis"),
            default_value: 1,
            values: vec![0, 1],
        });
        pack.state_schemas[1].value = schema;
        let content = compile(pack).expect("two-field schema compiles");
        let registry = BlockStateRegistry::build(&content).expect("registry builds");
        let plant = key("base:plant");
        let combined = registry.state_of(&plant, &[0, 2]).expect("declared pair");
        let base = registry
            .state_of(&plant, &[0, 0])
            .expect("first declared pair");
        assert_eq!(combined.0 - base.0, 2);
        assert_eq!(registry.state_values(combined), Some(&[0, 2][..]));
        // `axis` only declares 0 and 1, so swapping the values is not a state.
        assert!(registry.state_of(&plant, &[2, 0]).is_none());
    }
}
