//! Material palette for the terrain pass.
//!
//! Content selects one material per block face. This module resolves those keys
//! through the base game's texture pack and lays the result out as a fixed-size
//! material table for the shader. The table is a hard budget: a session with
//! more distinct materials fails at startup instead of silently drawing the
//! wrong surface.

use std::collections::BTreeMap;

pub use torse_assets::texture::TexturePack;
use torse_assets::texture::{Atlas, MaterialEntry, MipLevel};
use torse_content::{AlphaMode, Face};
use torse_types::ContentKey;
use torse_world::{BlockStateId, BlockStateRegistry};

/// Maximum materials addressable by one terrain vertex.
pub const MAX_MATERIALS: usize = 256;

/// Mip levels uploaded for the terrain atlas, including the base level.
pub const MIP_LEVELS: u32 = 3;

/// One material as consumed by the terrain shader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Surface {
    /// Atlas rectangle as origin and size in normalised coordinates.
    pub rect: [f32; 4],
    /// Linear RGB tint multiplied with the sampled texture.
    pub tint: [f32; 3],
    /// Blending mode used by the fragment shader.
    pub alpha: AlphaMode,
}

/// Materials used by one session, addressed by a small integer.
#[derive(Clone, Debug)]
pub struct Palette {
    slots: BTreeMap<ContentKey, u16>,
    surfaces: Vec<Surface>,
    mips: Vec<MipLevel>,
}

impl Palette {
    /// Resolve every material referenced by the content registry.
    pub fn build(registry: &BlockStateRegistry, pack: &TexturePack) -> Result<Self, String> {
        if u64::from(registry.state_count()) > u64::from(u16::MAX) + 1 {
            return Err(
                "content has more block states than a material slot index can carry".into(),
            );
        }
        let mut materials: BTreeMap<ContentKey, AlphaMode> = BTreeMap::new();
        for index in 0..registry.state_count() {
            let Some(definition) = registry.definition(BlockStateId(index as u16)) else {
                continue;
            };
            let profile = &definition.render_profile;
            for face in Face::ALL {
                let material = profile.face_materials.resolve(face, &profile.material);
                match materials.entry(material.clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        if *entry.get() != profile.alpha_mode {
                            return Err(format!(
                                "material `{material}` is declared with both {:?} and {:?} alpha modes",
                                entry.get(),
                                profile.alpha_mode
                            ));
                        }
                    }
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(profile.alpha_mode);
                    }
                }
            }
        }
        if materials.len() + 1 > MAX_MATERIALS {
            return Err(format!(
                "content uses {} materials, above the {} material budget",
                materials.len() + 1,
                MAX_MATERIALS
            ));
        }

        let mut slots = BTreeMap::new();
        let mut surfaces = Vec::with_capacity(materials.len() + 1);
        for (key, alpha) in materials {
            // Uncovered materials show the missing sprite so a pack that lags
            // behind content is visible instead of silently grey.
            let surface = pack.surface(&key).map_or(
                Surface {
                    rect: pack.missing_rect(),
                    tint: [1.0, 1.0, 1.0],
                    alpha,
                },
                |resolved| Surface {
                    rect: resolved.rect,
                    tint: resolved.tint,
                    alpha,
                },
            );
            slots.insert(key, surfaces.len() as u16);
            surfaces.push(surface);
        }
        // One extra surface draws entities and other tinted debug bodies.
        surfaces.push(Surface {
            rect: pack.white_rect(),
            tint: [0.78, 0.55, 0.35],
            alpha: AlphaMode::Opaque,
        });

        Ok(Self {
            slots,
            surfaces,
            mips: pack.mips(MIP_LEVELS)?,
        })
    }

    /// Slot of one material, if content references it.
    pub fn slot(&self, material: &ContentKey) -> Option<u16> {
        self.slots.get(material).copied()
    }

    /// Material table written to the GPU.
    pub fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }

    /// Uploaded mip chain; the first level is the base atlas.
    pub fn mips(&self) -> &[MipLevel] {
        &self.mips
    }

    /// Slot reserved for entity debug bodies.
    pub fn entity_slot(&self) -> u16 {
        (self.surfaces.len() - 1) as u16
    }
}

/// Texture pack shipped with the base game.
///
/// The atlas is self-drawn by `make-textures.py` next to it. Tiles sit on a
/// 24-pixel pitch so mip extrusion has a gutter; the last two tiles are the
/// missing sprite and a white sprite for tinted debug surfaces.
pub fn base_pack() -> Result<TexturePack, String> {
    macro_rules! asset {
        ($name:literal) => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../games/base/assets/textures/",
                $name
            )) as &[u8]
        };
    }
    let atlas = Atlas::decode(asset!("atlas.png"))?;
    TexturePack::new(
        atlas,
        vec![
            MaterialEntry {
                key: "base:material/stone",
                rect: [4, 4, 16, 16],
                tint: None,
            },
            MaterialEntry {
                key: "base:material/glass",
                rect: [28, 4, 16, 16],
                tint: None,
            },
            MaterialEntry {
                key: "base:material/plant",
                rect: [52, 4, 16, 16],
                tint: None,
            },
        ],
        [76, 4, 16, 16],
        [100, 4, 16, 16],
    )
}

#[cfg(test)]
impl Palette {
    fn test_mips() -> Vec<MipLevel> {
        vec![MipLevel {
            width: 1,
            height: 1,
            pixels: vec![255; 4],
        }]
    }

    /// Palette without materials, for table-shape tests.
    pub fn empty_for_tests() -> Self {
        Self {
            slots: BTreeMap::new(),
            surfaces: Vec::new(),
            mips: Self::test_mips(),
        }
    }

    /// Palette with a single surface, for byte-layout tests.
    pub fn single_for_tests(rect: [f32; 4], tint: [f32; 3], alpha: AlphaMode) -> Self {
        Self {
            slots: BTreeMap::new(),
            surfaces: vec![Surface { rect, tint, alpha }],
            mips: Self::test_mips(),
        }
    }
}

#[cfg(test)]
mod tests {
    use torse_content::{ContentPack, compile, m0_fixture_pack};
    use torse_world::BlockStateRegistry;

    use super::*;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key")
    }

    fn registry() -> (ContentPack, BlockStateRegistry) {
        let content = m0_fixture_pack();
        let compiled = compile(content.clone()).expect("fixture compiles");
        let registry = BlockStateRegistry::build(&compiled).expect("registry builds");
        (content, registry)
    }

    fn base_atlas() -> Atlas {
        Atlas::decode(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../games/base/assets/textures/atlas.png"
        )))
        .expect("atlas decodes")
    }

    #[test]
    fn base_pack_covers_the_base_materials() {
        let pack = base_pack().expect("base pack decodes");
        let (_, registry) = registry();
        let palette = Palette::build(&registry, &pack).expect("palette builds");

        for material in [
            "base:material/stone",
            "base:material/glass",
            "base:material/plant",
        ] {
            let slot = palette
                .slot(&key(material))
                .unwrap_or_else(|| panic!("missing {material}"));
            let surface = palette.surfaces()[usize::from(slot)];
            assert_ne!(
                surface.rect,
                pack.missing_rect(),
                "{material} used the missing sprite"
            );
        }
        assert_eq!(
            palette.surfaces()[usize::from(palette.slot(&key("base:material/glass")).unwrap())]
                .alpha,
            AlphaMode::Blend
        );
        assert_eq!(
            palette.surfaces()[usize::from(palette.slot(&key("base:material/plant")).unwrap())]
                .alpha,
            AlphaMode::Mask
        );
        assert_eq!(palette.entity_slot() as usize, palette.surfaces().len() - 1);
        assert_eq!(palette.mips().len(), MIP_LEVELS as usize);
        assert_eq!(
            (palette.mips()[1].width, palette.mips()[1].height),
            (palette.mips()[0].width / 2, palette.mips()[0].height / 2)
        );
    }

    #[test]
    fn uncovered_material_shows_the_missing_sprite() {
        let pack = TexturePack::new(base_atlas(), Vec::new(), [76, 4, 16, 16], [100, 4, 16, 16])
            .expect("pack builds");
        let (_, registry) = registry();
        let palette = Palette::build(&registry, &pack).expect("palette builds");
        let stone = palette.slot(&key("base:material/stone")).unwrap();
        let surface = palette.surfaces()[usize::from(stone)];
        assert_eq!(surface.rect, pack.missing_rect());
        assert_eq!(surface.tint, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn shared_material_with_two_alpha_modes_is_rejected() {
        let (mut content, _) = registry();
        let stone_material = content
            .blocks
            .iter()
            .find(|block| block.value.id.to_string() == "base:stone")
            .unwrap()
            .value
            .render_profile
            .material
            .clone();
        let glass = content
            .blocks
            .iter_mut()
            .find(|block| block.value.id.to_string() == "base:glass_slab")
            .unwrap();
        glass.value.render_profile.material = stone_material;
        let compiled = compile(content).expect("content compiles");
        let registry = BlockStateRegistry::build(&compiled).expect("registry builds");
        let pack = base_pack().expect("base pack decodes");
        let error = Palette::build(&registry, &pack).unwrap_err();
        assert!(error.contains("alpha modes"), "unexpected error: {error}");
    }
}
