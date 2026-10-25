//! PNG atlas decoding and material pack validation.
//!
//! A texture pack maps material keys to rectangles inside one RGBA8 atlas. The
//! pack does not decide blending: alpha modes belong to content and are applied
//! by the renderer. Each game declares its own pack; the pack format is code,
//! not a file, until a general resource format exists.

use std::collections::BTreeMap;
use std::io::Cursor;

use torse_types::ContentKey;

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

    /// Build an atlas of one opaque white pixel; useful for tests.
    pub fn white_pixel() -> Self {
        Self {
            width: 1,
            height: 1,
            pixels: vec![255, 255, 255, 255],
        }
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

    /// Normalise a pixel rectangle to origin and size in `0.0..=1.0`.
    pub fn rect(&self, rect: [u32; 4]) -> [f32; 4] {
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
    /// Optional sRGB tint multiplied with the sampled texture.
    pub tint: Option<[f32; 3]>,
}

/// Resolved appearance of one material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialSurface {
    /// Atlas rectangle as origin and size in normalised coordinates.
    pub rect: [f32; 4],
    /// Linear RGB tint multiplied with the sampled texture.
    pub tint: [f32; 3],
}

/// Atlas plus the material rectangles it contains.
#[derive(Clone, Debug)]
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

    /// Atlas held by this pack.
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Appearance of one material, if the pack covers it.
    pub fn surface(&self, key: &ContentKey) -> Option<MaterialSurface> {
        let entry = self.materials.get(&key.to_string())?;
        Some(MaterialSurface {
            rect: self.atlas.rect(entry.rect),
            tint: entry.tint.map_or([1.0, 1.0, 1.0], srgb_to_linear),
        })
    }

    /// Rectangle of the 1x1 white fallback pixel.
    pub fn fallback_rect(&self) -> [f32; 4] {
        self.atlas.rect([self.fallback[0], self.fallback[1], 1, 1])
    }
}

/// Convert one sRGB colour to linear RGB.
pub fn srgb_to_linear(color: [f32; 3]) -> [f32; 3] {
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
    use super::*;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key")
    }

    #[test]
    fn pack_resolves_rectangles_and_tints() {
        let pack = TexturePack::new(
            Atlas::white_pixel(),
            vec![MaterialEntry {
                key: "base:material/stone",
                rect: [0, 0, 1, 1],
                tint: Some([0.5, 0.5, 0.5]),
            }],
            [0, 0],
        )
        .expect("pack builds");
        let surface = pack.surface(&key("base:material/stone")).expect("covered");
        assert_eq!(surface.rect, [0.0, 0.0, 1.0, 1.0]);
        assert!((surface.tint[0] - 0.214).abs() < 0.01);
        assert_eq!(pack.fallback_rect(), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn uncovered_material_has_no_surface() {
        let pack = TexturePack::new(Atlas::white_pixel(), Vec::new(), [0, 0]).expect("pack builds");
        assert!(pack.surface(&key("base:material/missing")).is_none());
    }

    #[test]
    fn pack_rejects_rectangles_outside_the_atlas() {
        let error = TexturePack::new(
            Atlas::white_pixel(),
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
        let error = TexturePack::new(Atlas::white_pixel(), vec![entry, entry], [0, 0]).unwrap_err();
        assert!(
            error.contains("declared twice"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn pack_rejects_fallback_outside_the_atlas() {
        let error = TexturePack::new(Atlas::white_pixel(), Vec::new(), [4, 4]).unwrap_err();
        assert!(error.contains("fallback"), "unexpected error: {error}");
    }

    #[test]
    fn srgb_tint_is_converted_to_linear() {
        assert_eq!(srgb_to_linear([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
        assert!((srgb_to_linear([1.0, 1.0, 1.0])[0] - 1.0).abs() < 1e-6);
    }
}
