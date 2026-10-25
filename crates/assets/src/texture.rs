//! PNG atlas decoding, material-pack validation, and mipmap generation.
//!
//! A texture pack maps material keys to rectangles inside one RGBA8 atlas. The
//! pack does not decide blending: alpha modes belong to content and are applied
//! by the renderer. Each game declares its own pack; the pack format is code,
//! not a file, until a general resource format exists.
//!
//! Sprites must leave a one-pixel gutter around every rectangle. Mipmap
//! generation extrudes each sprite into its gutter and fills transparent pixels
//! with the nearest opaque colour, so filtering at lower levels cannot pull in
//! a neighbour or a black fringe.

use std::collections::BTreeMap;
use std::collections::VecDeque;
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

    /// Tight RGBA8 rows of the base level.
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

/// One level of the generated mip chain.
#[derive(Clone, Debug, PartialEq)]
pub struct MipLevel {
    /// Level width in texels.
    pub width: u32,
    /// Level height in texels.
    pub height: u32,
    /// Tight RGBA8 rows.
    pub pixels: Vec<u8>,
}

/// Atlas plus the material rectangles it contains.
#[derive(Clone, Debug)]
pub struct TexturePack {
    atlas: Atlas,
    materials: BTreeMap<String, MaterialEntry>,
    missing: [u32; 4],
    white: [u32; 4],
}

impl TexturePack {
    /// Build a pack, checking rectangles against the atlas and each other.
    ///
    /// Every rectangle needs a one-pixel gutter, because mipmap generation
    /// extrudes sprites into it. Rectangles must not touch or overlap.
    pub fn new(
        atlas: Atlas,
        materials: Vec<MaterialEntry>,
        missing: [u32; 4],
        white: [u32; 4],
    ) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for entry in materials {
            check_rect(&atlas, entry.rect, &format!("material `{}`", entry.key))?;
            if entries.insert(entry.key.to_string(), entry).is_some() {
                return Err(format!("material `{}` is declared twice", entry.key));
            }
        }
        check_rect(&atlas, missing, "missing sprite")?;
        check_rect(&atlas, white, "white sprite")?;

        let mut rects: Vec<([u32; 4], String)> = entries
            .values()
            .map(|entry| (entry.rect, format!("material `{}`", entry.key)))
            .collect();
        rects.push((missing, "missing sprite".to_string()));
        rects.push((white, "white sprite".to_string()));
        for (index, (left, name)) in rects.iter().enumerate() {
            for (right, other) in &rects[index + 1..] {
                if contact(*left, *right) {
                    return Err(format!(
                        "{name} touches {other}; every sprite needs a one-pixel gutter"
                    ));
                }
            }
        }

        Ok(Self {
            atlas,
            materials: entries,
            missing,
            white,
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

    /// Rectangle of the missing sprite, shown for uncovered materials.
    pub fn missing_rect(&self) -> [f32; 4] {
        self.atlas.rect(self.missing)
    }

    /// Rectangle of the white sprite, used by tinted debug surfaces.
    pub fn white_rect(&self) -> [f32; 4] {
        self.atlas.rect(self.white)
    }

    /// Generate a mip chain, including the prepared base level.
    ///
    /// Transparent pixels receive the nearest opaque colour and every sprite is
    /// extruded one pixel into its gutter before halving. The returned chain
    /// always contains at least the base level and never goes below one texel.
    pub fn mips(&self, levels: u32) -> Result<Vec<MipLevel>, String> {
        if levels == 0 {
            return Err("a mip chain needs at least one level".to_string());
        }
        let mut pixels = self.atlas.pixels.clone();
        for rect in self.rects() {
            solidify(&mut pixels, self.atlas.width, rect);
            extrude(&mut pixels, self.atlas.width, rect);
        }
        let mut chain = vec![MipLevel {
            width: self.atlas.width,
            height: self.atlas.height,
            pixels,
        }];
        for _ in 1..levels {
            let previous = chain.last().expect("chain is not empty");
            if previous.width == 1 && previous.height == 1 {
                break;
            }
            chain.push(halve(previous));
        }
        Ok(chain)
    }

    fn rects(&self) -> Vec<[u32; 4]> {
        let mut rects: Vec<[u32; 4]> = self.materials.values().map(|entry| entry.rect).collect();
        rects.push(self.missing);
        rects.push(self.white);
        rects
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

fn check_rect(atlas: &Atlas, rect: [u32; 4], name: &str) -> Result<(), String> {
    let [x, y, width, height] = rect;
    if width == 0 || height == 0 {
        return Err(format!("{name} has an empty rectangle"));
    }
    // One spare pixel all around so extrusion stays inside the atlas.
    if x == 0 || y == 0 || x + width >= atlas.width() || y + height >= atlas.height() {
        return Err(format!(
            "{name} rectangle exceeds the {}x{} atlas or lacks a gutter",
            atlas.width(),
            atlas.height()
        ));
    }
    Ok(())
}

/// Whether two rectangles overlap or touch, including their one-pixel gutter.
fn contact(left: [u32; 4], right: [u32; 4]) -> bool {
    let left_end = [left[0] + left[2] + 1, left[1] + left[3] + 1];
    let right_end = [right[0] + right[2] + 1, right[1] + right[3] + 1];
    let overlap_x = left[0] < right_end[0] && right[0] < left_end[0];
    let overlap_y = left[1] < right_end[1] && right[1] < left_end[1];
    overlap_x && overlap_y
}

fn index(width: u32, x: u32, y: u32) -> usize {
    ((y * width + x) * 4) as usize
}

/// Fill transparent pixels with the nearest opaque colour, keeping alpha.
fn solidify(pixels: &mut [u8], width: u32, rect: [u32; 4]) {
    let [rx, ry, rw, rh] = rect;
    let mut queue = VecDeque::new();
    let mut visited = vec![false; (rw * rh) as usize];
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            let offset = index(width, x, y);
            if pixels[offset + 3] > 0 {
                let slot = ((y - ry) * rw + (x - rx)) as usize;
                visited[slot] = true;
                queue.push_back((x, y, offset));
            }
        }
    }
    while let Some((x, y, source)) = queue.pop_front() {
        let color = [pixels[source], pixels[source + 1], pixels[source + 2]];
        for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
            let nx = i64::from(x) + dx;
            let ny = i64::from(y) + dy;
            if nx < i64::from(rx)
                || ny < i64::from(ry)
                || nx >= i64::from(rx + rw)
                || ny >= i64::from(ry + rh)
            {
                continue;
            }
            let (nx, ny) = (nx as u32, ny as u32);
            let slot = ((ny - ry) * rw + (nx - rx)) as usize;
            if visited[slot] {
                continue;
            }
            visited[slot] = true;
            let target = index(width, nx, ny);
            pixels[target] = color[0];
            pixels[target + 1] = color[1];
            pixels[target + 2] = color[2];
            queue.push_back((nx, ny, target));
        }
    }
}

/// Copy the outer ring of a sprite one pixel into its gutter.
fn extrude(pixels: &mut [u8], width: u32, rect: [u32; 4]) {
    let [rx, ry, rw, rh] = rect;
    let copy = |pixels: &mut [u8], from: (u32, u32), to: (u32, u32)| {
        let source = index(width, from.0, from.1);
        let color = [
            pixels[source],
            pixels[source + 1],
            pixels[source + 2],
            pixels[source + 3],
        ];
        let target = index(width, to.0, to.1);
        pixels[target..target + 4].copy_from_slice(&color);
    };
    for y in ry..ry + rh {
        copy(pixels, (rx, y), (rx - 1, y));
        copy(pixels, (rx + rw - 1, y), (rx + rw, y));
    }
    for x in rx..rx + rw {
        copy(pixels, (x, ry), (x, ry - 1));
        copy(pixels, (x, ry + rh - 1), (x, ry + rh));
    }
    copy(pixels, (rx, ry), (rx - 1, ry - 1));
    copy(pixels, (rx + rw - 1, ry), (rx + rw, ry - 1));
    copy(pixels, (rx, ry + rh - 1), (rx - 1, ry + rh));
    copy(pixels, (rx + rw - 1, ry + rh - 1), (rx + rw, ry + rh));
}

fn halve(previous: &MipLevel) -> MipLevel {
    let width = (previous.width / 2).max(1);
    let height = (previous.height / 2).max(1);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let sx = (x * 2).min(previous.width - 1);
            let sy = (y * 2).min(previous.height - 1);
            let sx1 = (sx + 1).min(previous.width - 1);
            let sy1 = (sy + 1).min(previous.height - 1);
            let samples = [
                index(previous.width, sx, sy),
                index(previous.width, sx1, sy),
                index(previous.width, sx, sy1),
                index(previous.width, sx1, sy1),
            ];
            let mut channels = [0u32; 4];
            for sample in samples {
                for (channel, value) in channels.iter_mut().enumerate() {
                    *value += u32::from(previous.pixels[sample + channel]);
                }
            }
            let target = index(width, x, y);
            for (channel, value) in channels.iter().enumerate() {
                pixels[target + channel] = (value / 4) as u8;
            }
        }
    }
    MipLevel {
        width,
        height,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> ContentKey {
        ContentKey::parse(value).expect("test key")
    }

    fn solid_atlas(width: u32, height: u32, color: [u8; 4]) -> Atlas {
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
        Atlas {
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn pack_resolves_rectangles_and_tints() {
        let pack = TexturePack::new(
            solid_atlas(10, 10, [255, 255, 255, 255]),
            vec![MaterialEntry {
                key: "base:material/stone",
                rect: [2, 2, 4, 4],
                tint: Some([0.5, 0.5, 0.5]),
            }],
            [8, 8, 1, 1],
            [8, 2, 1, 1],
        )
        .expect("pack builds");
        let surface = pack.surface(&key("base:material/stone")).expect("covered");
        assert_eq!(surface.rect, [0.2, 0.2, 0.4, 0.4]);
        assert!((surface.tint[0] - 0.214).abs() < 0.01);
    }

    #[test]
    fn uncovered_material_has_no_surface() {
        let pack = TexturePack::new(
            solid_atlas(10, 10, [255, 255, 255, 255]),
            Vec::new(),
            [8, 8, 1, 1],
            [8, 2, 1, 1],
        )
        .expect("pack builds");
        assert!(pack.surface(&key("base:material/missing")).is_none());
    }

    #[test]
    fn pack_rejects_rectangles_outside_the_atlas() {
        let error = TexturePack::new(
            solid_atlas(10, 10, [255, 255, 255, 255]),
            vec![MaterialEntry {
                key: "base:material/oops",
                rect: [4, 4, 16, 16],
                tint: None,
            }],
            [8, 8, 1, 1],
            [8, 2, 1, 1],
        )
        .unwrap_err();
        assert!(error.contains("exceeds the"), "unexpected error: {error}");
    }

    #[test]
    fn pack_rejects_touching_sprites() {
        let error = TexturePack::new(
            solid_atlas(16, 8, [255, 255, 255, 255]),
            vec![
                MaterialEntry {
                    key: "base:material/left",
                    rect: [1, 1, 4, 4],
                    tint: None,
                },
                MaterialEntry {
                    key: "base:material/right",
                    rect: [5, 1, 4, 4],
                    tint: None,
                },
            ],
            [1, 6, 1, 1],
            [14, 6, 1, 1],
        )
        .unwrap_err();
        assert!(error.contains("gutter"), "unexpected error: {error}");
    }

    #[test]
    fn mips_halve_and_keep_transparent_neighbours_coloured() {
        let mut pixels = vec![0u8; 12 * 12 * 4];
        for y in 0..4 {
            for x in 0..4 {
                let offset = index(12, 2 + x, 2 + y);
                pixels[offset..offset + 4].copy_from_slice(&[200, 40, 30, 255]);
            }
        }
        let pack = TexturePack::new(
            Atlas {
                width: 12,
                height: 12,
                pixels,
            },
            vec![MaterialEntry {
                key: "base:material/red",
                rect: [2, 2, 4, 4],
                tint: None,
            }],
            [8, 8, 1, 1],
            [8, 2, 1, 1],
        )
        .expect("pack builds");
        let chain = pack.mips(3).expect("mips build");
        assert_eq!(chain.len(), 3);
        assert_eq!((chain[1].width, chain[1].height), (6, 6));
        assert_eq!((chain[2].width, chain[2].height), (3, 3));
        let transparent = index(12, 2, 2);
        assert_eq!(
            &chain[0].pixels[transparent..transparent + 3],
            &[200, 40, 30]
        );
        let gutter = index(12, 1, 2);
        assert_eq!(&chain[0].pixels[gutter..gutter + 3], &[200, 40, 30]);
        let corner = index(6, 1, 1);
        assert!(chain[1].pixels[corner] > 100, "level 1 turned black");
    }

    #[test]
    fn mip_chain_stops_at_one_texel() {
        let pack = TexturePack::new(
            solid_atlas(6, 6, [255, 255, 255, 255]),
            Vec::new(),
            [1, 1, 1, 1],
            [4, 4, 1, 1],
        )
        .expect("pack builds");
        let chain = pack.mips(8).expect("mips build");
        assert_eq!(chain.len(), 3);
        assert_eq!((chain[1].width, chain[1].height), (3, 3));
        assert_eq!((chain[2].width, chain[2].height), (1, 1));
    }

    #[test]
    fn srgb_tint_is_converted_to_linear() {
        assert_eq!(srgb_to_linear([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
        assert!((srgb_to_linear([1.0, 1.0, 1.0])[0] - 1.0).abs() < 1e-6);
    }
}
