//! Texture atlas and material palette for the terrain pass.
//!
//! Content selects one material per block face. This module resolves those
//! material keys to a rectangle in a texture atlas, a tint colour, and an alpha
//! mode, and lays the result out as a fixed-size material table for the shader.
//! The table is a hard budget: a session with more distinct materials fails at
//! startup instead of silently drawing the wrong surface.

use std::collections::BTreeMap;
use std::io::Cursor;

use torse_content::{AlphaMode, Face};
use torse_types::ContentKey;
use torse_world::{BlockStateId, BlockStateRegistry};

/// Maximum materials addressable by one terrain vertex.
pub const MAX_MATERIALS: usize = 256;

/// Decoded RGBA8 texture atlas.
#[derive(Clone, Debug)]
pub struct Atlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Atlas {
    /// Decode a PNG atlas into tight RGBA8 rows.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
        let mut data = vec![0; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut data)
            .map_err(|error| error.to_string())?;
        if info.width == 0 || info.height == 0 {
            return Err("texture atlas must not be empty".to_string());
        }
        let channels = info.color_type.samples();
        let mut pixels = Vec::with_capacity((info.width * info.height * 4) as usize);
        for pixel in data[..info.buffer_size()].chunks_exact(channels) {
            pixels.extend_from_slice(&match info.color_type {
                png::ColorType::Rgba => [pixel[0], pixel[1], pixel[2], pixel[3]],
                png::ColorType::Rgb => [pixel[0], pixel[1], pixel[2], 255],
                png::ColorType::Grayscale => [pixel[0], pixel[0], pixel[0], 255],
                png::ColorType::GrayscaleAlpha => [pixel[0], pixel[0], pixel[0], pixel[1]],
                png::ColorType::Indexed => return Err("unexpanded atlas palette".to_string()),
            });
        }
        Ok(Self {
            width: info.width,
            height: info.height,
            pixels,
        })
    }

    /// Atlas width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Atlas height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Tight RGBA8 rows.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    fn rect(&self, rect: [u32; 4]) -> [f32; 4] {
        let width = self.width as f32;
        let height = self.height as f32;
        [
            rect[0] as f32 / width,
            rect[1] as f32 / height,
            rect[2] as f32 / width,
            rect[3] as f32 / height,
        ]
    }
}

/// One material entry of a pack.
#[derive(Clone, Copy, Debug)]
pub struct MaterialEntry {
    /// Content key of the material.
    pub key: &'static str,
    /// Texture rectangle in pixels: x, y, width, height.
    pub rect: [u32; 4],
    /// Optional sRGB tint colour multiplied with the sampled texture.
    pub tint: Option<[f32; 3]>,
}

/// Atlas plus the material rectangles it contains.
#[derive(Debug)]
pub struct TexturePack {
    atlas: Atlas,
    materials: BTreeMap<String, MaterialEntry>,
    fallback: [u32; 2],
}

impl TexturePack {
    /// Build a pack, checking every rectangle against the atlas.
    pub fn new(
        atlas: Atlas,
        materials: Vec<MaterialEntry>,
        fallback: [u32; 2],
    ) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for entry in materials {
            let [x, y, width, height] = entry.rect;
            if width == 0 || height == 0 {
                return Err(format!("material `{}` has an empty rectangle", entry.key));
            }
            if x + width > atlas.width() || y + height > atlas.height() {
                return Err(format!(
                    "material `{}` rectangle exceeds the {}x{} atlas",
                    entry.key,
                    atlas.width(),
                    atlas.height()
                ));
            }
            if entries.insert(entry.key.to_string(), entry).is_some() {
                return Err(format!("material `{}` is declared twice", entry.key));
            }
        }
        if fallback[0] >= atlas.width() || fallback[1] >= atlas.height() {
            return Err("fallback atlas pixel is outside the atlas".to_string());
        }
        Ok(Self {
            atlas,
            materials: entries,
            fallback,
        })
    }

    /// The pack shipped with the base game.
    ///
    /// The three textures are self-drawn by `make-textures.py` in the same
    /// directory; the last pixel is a white fallback sampled by materials that
    /// no pack entry covers.
    pub fn embedded() -> Result<Self, String> {
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
        Self::new(
            atlas,
            vec![
                MaterialEntry {
                    key: "base:material/stone",
                    rect: [0, 0, 16, 16],
                    tint: None,
                },
                MaterialEntry {
                    key: "base:material/glass_slab",
                    rect: [16, 0, 16, 16],
                    tint: None,
                },
                MaterialEntry {
                    key: "base:material/plant",
                    rect: [32, 0, 16, 16],
                    tint: None,
                },
            ],
            [48, 0],
        )
    }

    /// Atlas held by this pack.
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    fn surface(&self, key: &ContentKey) -> Option<Surface> {
        let entry = self.materials.get(&key.to_string())?;
        Some(Surface {
            rect: self.atlas.rect(entry.rect),
            tint: entry.tint.map_or([1.0, 1.0, 1.0], srgb_to_linear),
            alpha: AlphaMode::Opaque,
        })
    }

    fn fallback_rect(&self) -> [f32; 4] {
        self.atlas.rect([self.fallback[0], self.fallback[1], 1, 1])
    }
}

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
    atlas: Atlas,
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
            let mut surface = pack.surface(&key).unwrap_or(Surface {
                rect: pack.fallback_rect(),
                tint: [0.7, 0.7, 0.7],
                alpha,
            });
            // The declared blending mode is a content decision; a pack entry
            // only supplies the image.
            surface.alpha = alpha;
            slots.insert(key, surfaces.len() as u16);
            surfaces.push(surface);
        }
        // One extra surface draws entities and other debug bodies.
        surfaces.push(Surface {
            rect: pack.fallback_rect(),
            tint: [0.78, 0.55, 0.35],
            alpha: AlphaMode::Opaque,
        });

        Ok(Self {
            slots,
            surfaces,
            atlas: pack.atlas().clone(),
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

    /// Atlas sampled by the terrain shader.
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Slot reserved for entity debug bodies.
    pub fn entity_slot(&self) -> u16 {
        (self.surfaces.len() - 1) as u16
    }
}

#[cfg(test)]
impl Palette {
    /// Palette without materials, for table-shape tests.
    pub fn empty_for_tests() -> Self {
        Self {
            slots: BTreeMap::new(),
            surfaces: Vec::new(),
            atlas: Atlas {
                width: 1,
                height: 1,
                pixels: vec![255; 4],
            },
        }
    }

    /// Palette with a single surface, for byte-layout tests.
    pub fn single_for_tests(rect: [f32; 4], tint: [f32; 3], alpha: AlphaMode) -> Self {
        Self {
            slots: BTreeMap::new(),
            surfaces: vec![Surface { rect, tint, alpha }],
            atlas: Atlas {
                width: 1,
                height: 1,
                pixels: vec![255; 4],
            },
        }
    }
}

fn srgb_to_linear(color: [f32; 3]) -> [f32; 3] {
    color.map(|channel| {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    })
}

#[cfg(test)]
mod tests {
    use torse_content::{compile, m0_fixture_pack};
    use torse_world::BlockStateRegistry;

    use super::*;

    fn pack(entries: Vec<MaterialEntry>, atlas: Atlas, fallback: [u32; 2]) -> TexturePack {
        match TexturePack::new(atlas, entries, fallback) {
            Ok(pack) => pack,
            Err(error) => panic!("pack rejected: {error}"),
        }
    }

    fn unit_atlas() -> Atlas {
        Atlas {
            width: 1,
            height: 1,
            pixels: vec![255, 255, 255, 255],
        }
    }

    #[test]
    fn embedded_pack_covers_the_base_materials() {
        let pack = TexturePack::embedded().expect("embedded pack decodes");
        let content = compile(m0_fixture_pack()).expect("fixture compiles");
        let registry = BlockStateRegistry::build(&content).expect("registry builds");
        let palette = Palette::build(&registry, &pack).expect("palette builds");

        for material in [
            "base:material/stone",
            "base:material/glass_slab",
            "base:material/plant",
        ] {
            let key = ContentKey::parse(material).expect("valid key");
            let slot = palette
                .slot(&key)
                .unwrap_or_else(|| panic!("missing {material}"));
            let surface = palette.surfaces()[usize::from(slot)];
            assert_ne!(
                surface.rect,
                pack.fallback_rect(),
                "{material} used the fallback"
            );
        }
        assert_eq!(
            palette.surfaces()[usize::from(
                palette
                    .slot(&ContentKey::parse("base:material/glass_slab").unwrap())
                    .unwrap()
            )]
            .alpha,
            AlphaMode::Blend
        );
        assert_eq!(
            palette.surfaces()[usize::from(
                palette
                    .slot(&ContentKey::parse("base:material/plant").unwrap())
                    .unwrap()
            )]
            .alpha,
            AlphaMode::Mask
        );
        assert_eq!(palette.entity_slot() as usize, palette.surfaces().len() - 1);
    }

    #[test]
    fn pack_rejects_rectangles_outside_the_atlas() {
        let error = TexturePack::new(
            unit_atlas(),
            vec![MaterialEntry {
                key: "base:material/oops",
                rect: [8, 8, 16, 16],
                tint: None,
            }],
            [0, 0],
        )
        .unwrap_err();
        assert!(error.contains("exceeds the"), "unexpected error: {error}");
    }

    #[test]
    fn pack_rejects_duplicate_materials() {
        let entry = MaterialEntry {
            key: "base:material/stone",
            rect: [0, 0, 1, 1],
            tint: None,
        };
        let error = TexturePack::new(unit_atlas(), vec![entry, entry], [0, 0]).unwrap_err();
        assert!(
            error.contains("declared twice"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn uncovered_material_uses_the_grey_fallback_pixel() {
        let pack = pack(Vec::new(), unit_atlas(), [0, 0]);
        let content = compile(m0_fixture_pack()).expect("fixture compiles");
        let registry = BlockStateRegistry::build(&content).expect("registry builds");
        let palette = Palette::build(&registry, &pack).expect("palette builds");
        let stone = palette
            .slot(&ContentKey::parse("base:material/stone").unwrap())
            .unwrap();
        let surface = palette.surfaces()[usize::from(stone)];
        assert_eq!(surface.rect, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(surface.tint, [0.7, 0.7, 0.7]);
    }

    #[test]
    fn shared_material_with_two_alpha_modes_is_rejected() {
        let mut content_pack = m0_fixture_pack();
        let stone_material = content_pack
            .blocks
            .iter()
            .find(|block| block.value.id.to_string() == "base:stone")
            .unwrap()
            .value
            .render_profile
            .material
            .clone();
        let glass = content_pack
            .blocks
            .iter_mut()
            .find(|block| block.value.id.to_string() == "base:glass_slab")
            .unwrap();
        glass.value.render_profile.material = stone_material;
        let content = compile(content_pack).expect("content compiles");
        let registry = BlockStateRegistry::build(&content).expect("registry builds");
        let pack = TexturePack::embedded().expect("embedded pack decodes");
        let error = Palette::build(&registry, &pack).unwrap_err();
        assert!(error.contains("alpha modes"), "unexpected error: {error}");
    }

    #[test]
    fn srgb_tint_is_converted_to_linear() {
        assert_eq!(srgb_to_linear([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
        assert!((srgb_to_linear([1.0, 1.0, 1.0])[0] - 1.0).abs() < 1e-6);
        let mid = srgb_to_linear([0.5, 0.5, 0.5])[0];
        assert!((mid - 0.214).abs() < 0.01, "mid grey: {mid}");
    }
}
