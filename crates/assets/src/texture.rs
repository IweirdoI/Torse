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
use std::time::Duration;

use torse_types::{BlockPos, ContentKey, SectionPos};

/// Decoded RGBA8 texture atlas.
#[derive(Clone, Debug)]
pub struct Atlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Bounds checked before parsing chunks or allocating decoded pixels.
#[derive(Clone, Copy, Debug)]
pub struct DecodeLimits {
    pub max_source_bytes: usize,
    pub max_rgba_bytes: usize,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 8 * 1024 * 1024,
            max_rgba_bytes: 64 * 1024 * 1024,
        }
    }
}

impl Atlas {
    /// Decode a PNG atlas into tight RGBA8 rows.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        Self::decode_with_limits(bytes, DecodeLimits::default())
    }

    /// Decode with caller-supplied source and RGBA bounds. Decoder scratch is
    /// limited to twice the RGBA size plus 64 KiB. Text and ICC data are unused.
    pub fn decode_with_limits(bytes: &[u8], limits: DecodeLimits) -> Result<Self, String> {
        if bytes.len() > limits.max_source_bytes {
            return Err("PNG source exceeds byte limit".into());
        }
        if bytes.len() < 33
            || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
            || bytes[8..12] != 13_u32.to_be_bytes()
            || &bytes[12..16] != b"IHDR"
        {
            return Err("invalid PNG header".into());
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().expect("header length"));
        let height = u32::from_be_bytes(bytes[20..24].try_into().expect("header length"));
        let rgba = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .and_then(|n| n.checked_mul(4))
            .filter(|n| width > 0 && height > 0 && *n <= limits.max_rgba_bytes)
            .ok_or("decoded image exceeds byte limit")?;
        let auxiliary = rgba
            .checked_mul(2)
            .and_then(|n| n.checked_add(65_536))
            .ok_or("PNG scratch bound overflow")?;
        let mut decoder =
            png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: auxiliary });
        decoder.set_ignore_text_chunk(true);
        decoder.set_ignore_iccp_chunk(true);
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
        if reader.output_buffer_size() > rgba {
            return Err("PNG output exceeds RGBA bound".into());
        }
        let mut data = vec![0; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut data)
            .map_err(|error| error.to_string())?;
        if info.width == 0 || info.height == 0 {
            return Err("texture atlas must not be empty".to_string());
        }
        let channels = info.color_type.samples();
        let mut pixels = Vec::with_capacity(rgba);
        for pixel in data[..info.buffer_size()].chunks_exact(channels) {
            pixels.extend_from_slice(&match info.color_type {
                png::ColorType::Rgba => [pixel[0], pixel[1], pixel[2], pixel[3]],
                png::ColorType::Rgb => [pixel[0], pixel[1], pixel[2], 255],
                png::ColorType::Grayscale => [pixel[0], pixel[0], pixel[0], 255],
                png::ColorType::GrayscaleAlpha => [pixel[0], pixel[0], pixel[0], pixel[1]],
                png::ColorType::Indexed => return Err("unexpanded atlas palette".to_string()),
            });
        }
        if info.width != width || info.height != height || pixels.len() != rgba {
            return Err("PNG frame dimensions differ from header".into());
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

/// Source category for a tint sampled from world-position data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TintSource {
    Grass,
    Foliage,
    Water,
}

/// Strategy for constructing mip levels for one material.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MipmapStrategy {
    /// Average the four source texels without adjusting alpha coverage.
    #[default]
    Average,
    /// Scale mip alpha to preserve the base level's mask coverage.
    CutoutCoverage,
    /// Fill transparent RGB with the darkest opaque colour and preserve coverage.
    DarkCutout,
}

/// Timing and playback order for a vertical strip animation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialAnimation {
    /// Number of equal-height frames in the texture rectangle.
    pub frame_count: u32,
    /// Duration used when `frame_durations` is absent.
    pub frame_duration: Duration,
    /// Frame indices in playback order; `None` plays `0..frame_count`.
    pub frame_order: Option<&'static [u32]>,
    /// Optional duration per playback entry.
    pub frame_durations: Option<&'static [Duration]>,
    /// Blend each frame toward the next during its display interval.
    pub interpolate: bool,
}

/// A source for location-dependent material colours.
///
/// Providers return sRGB bytes. The renderer converts them to linear RGB before
/// multiplying the sampled texture.
pub trait PositionTintProvider: Send + Sync {
    /// Resolve the tint at one block coordinate.
    fn sample(&self, source: TintSource, position: BlockPos) -> [u8; 3];

    /// Revision covering all tint data a mesh in `section` may sample.
    fn revision(&self, section: SectionPos) -> u64;
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
    /// Optional world-position tint source.
    pub tint_source: Option<TintSource>,
    /// Optional vertical-strip animation.
    pub animation: Option<MaterialAnimation>,
    /// Mipmap generation strategy for this material.
    pub mipmap_strategy: MipmapStrategy,
}

impl MaterialEntry {
    /// Declare a static material with default white tint and average mipmaps.
    pub const fn new(key: &'static str, rect: [u32; 4]) -> Self {
        Self {
            key,
            rect,
            tint: None,
            tint_source: None,
            animation: None,
            mipmap_strategy: MipmapStrategy::Average,
        }
    }
}

/// Resolved appearance of one material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialSurface {
    /// Atlas rectangle as origin and size in normalised coordinates.
    pub rect: [f32; 4],
    /// Linear RGB tint multiplied with the sampled texture.
    pub tint: [f32; 3],
    /// Optional world-position tint source.
    pub tint_source: Option<TintSource>,
    /// Optional vertical-strip animation.
    pub animation: Option<MaterialAnimation>,
}

/// Two frame rectangles and their interpolation weight at one time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialFrameSample {
    /// Current frame rectangle.
    pub current: [f32; 4],
    /// Next frame rectangle, equal to `current` for a static sample.
    pub next: [f32; 4],
    /// Blend amount from current to next.
    pub blend: f32,
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
            validate_material(&entry)?;
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
            tint_source: entry.tint_source,
            animation: entry.animation,
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
        let mut strategies: Vec<([u32; 4], u32, MipmapStrategy)> = self
            .materials
            .values()
            .map(|entry| {
                (
                    entry.rect,
                    entry.animation.map_or(1, |animation| animation.frame_count),
                    entry.mipmap_strategy,
                )
            })
            .collect();
        for rect in [self.missing, self.white] {
            strategies.push((rect, 1, MipmapStrategy::Average));
        }
        for (rect, frame_count, strategy) in &strategies {
            let frame_height = rect[3] / frame_count;
            for frame in 0..*frame_count {
                let frame_rect = [
                    rect[0],
                    rect[1] + frame * frame_height,
                    rect[2],
                    frame_height,
                ];
                match strategy {
                    MipmapStrategy::DarkCutout => {
                        solidify_dark(&mut pixels, self.atlas.width, frame_rect)
                    }
                    MipmapStrategy::Average | MipmapStrategy::CutoutCoverage => {
                        solidify(&mut pixels, self.atlas.width, frame_rect)
                    }
                }
            }
            extrude(&mut pixels, self.atlas.width, *rect);
        }
        let base = MipLevel {
            width: self.atlas.width,
            height: self.atlas.height,
            pixels,
        };
        let coverage: Vec<Vec<f32>> = strategies
            .iter()
            .map(|(rect, frame_count, strategy)| {
                if *strategy == MipmapStrategy::Average {
                    return Vec::new();
                }
                (0..*frame_count)
                    .map(|frame| {
                        rect_coverage(
                            &base.pixels,
                            base.width,
                            [
                                rect[0],
                                rect[1] + frame * (rect[3] / frame_count),
                                rect[2],
                                rect[3] / frame_count,
                            ],
                        )
                    })
                    .collect()
            })
            .collect();
        let mut chain = vec![base];
        for level_index in 1..levels {
            let previous = chain.last().expect("chain is not empty");
            if previous.width == 1 && previous.height == 1 {
                break;
            }
            let mut next = halve(previous);
            for (index, (rect, frame_count, strategy)) in strategies.iter().enumerate() {
                if *strategy == MipmapStrategy::Average {
                    continue;
                }
                let scale = 1u32.checked_shl(level_index).unwrap_or(u32::MAX);
                let scaled_height = rect[3] / scale;
                if scaled_height < *frame_count || !scaled_height.is_multiple_of(*frame_count) {
                    continue;
                }
                let frame_height = scaled_height / frame_count;
                for frame in 0..*frame_count {
                    let scaled = [
                        rect[0] / scale,
                        rect[1] / scale + frame * frame_height,
                        (rect[2] / scale).max(1),
                        frame_height,
                    ];
                    preserve_coverage(
                        &mut next.pixels,
                        next.width,
                        scaled,
                        coverage[index][frame as usize],
                    );
                }
            }
            chain.push(next);
        }
        Ok(chain)
    }
}

impl MaterialSurface {
    /// Select animation frames for elapsed time since playback began.
    pub fn frame_sample(&self, elapsed: Duration) -> MaterialFrameSample {
        let Some(animation) = self.animation else {
            return MaterialFrameSample {
                current: self.rect,
                next: self.rect,
                blend: 0.0,
            };
        };
        let order_len = animation
            .frame_order
            .map_or(animation.frame_count as usize, <[u32]>::len);
        let total_nanos = (0..order_len).fold(0u128, |total, index| {
            total.saturating_add(animation_duration(animation, index).as_nanos())
        });
        if total_nanos == 0 {
            return MaterialFrameSample {
                current: self.rect,
                next: self.rect,
                blend: 0.0,
            };
        }
        let mut cursor = elapsed.as_nanos() % total_nanos;
        let mut current_index = 0;
        for index in 0..order_len {
            let duration = animation_duration(animation, index).as_nanos();
            if cursor < duration {
                current_index = index;
                break;
            }
            cursor -= duration;
        }
        let current_frame = animation
            .frame_order
            .map_or(current_index as u32, |order| order[current_index]);
        let next_index = (current_index + 1) % order_len;
        let next_frame = animation
            .frame_order
            .map_or(next_index as u32, |order| order[next_index]);
        let duration = animation_duration(animation, current_index).as_nanos();
        let blend = if animation.interpolate {
            (cursor as f64 / duration as f64) as f32
        } else {
            0.0
        };
        MaterialFrameSample {
            current: frame_rect(self.rect, animation.frame_count, current_frame),
            next: frame_rect(self.rect, animation.frame_count, next_frame),
            blend,
        }
    }
}

fn animation_duration(animation: MaterialAnimation, playback_index: usize) -> Duration {
    animation
        .frame_durations
        .map_or(animation.frame_duration, |durations| {
            durations[playback_index]
        })
}

fn frame_rect(rect: [f32; 4], frame_count: u32, frame: u32) -> [f32; 4] {
    let frame_height = rect[3] / frame_count as f32;
    [
        rect[0],
        rect[1] + frame_height * frame as f32,
        rect[2],
        frame_height,
    ]
}

fn validate_material(entry: &MaterialEntry) -> Result<(), String> {
    if let Some(animation) = entry.animation {
        if animation.frame_count == 0 || !entry.rect[3].is_multiple_of(animation.frame_count) {
            return Err(format!(
                "material `{}` animation frame count must divide its rectangle height",
                entry.key
            ));
        }
        if animation.frame_durations.is_none() && animation.frame_duration.is_zero() {
            return Err(format!(
                "material `{}` animation frame duration must be positive",
                entry.key
            ));
        }
        let order_len = animation
            .frame_order
            .map_or(animation.frame_count as usize, <[u32]>::len);
        if order_len == 0 {
            return Err(format!(
                "material `{}` animation order must not be empty",
                entry.key
            ));
        }
        if animation
            .frame_order
            .is_some_and(|order| order.iter().any(|frame| *frame >= animation.frame_count))
        {
            return Err(format!(
                "material `{}` animation order contains an invalid frame",
                entry.key
            ));
        }
        if animation
            .frame_durations
            .is_some_and(|durations| durations.len() != order_len)
        {
            return Err(format!(
                "material `{}` animation duration count does not match its playback order",
                entry.key
            ));
        }
        if animation
            .frame_durations
            .is_some_and(|durations| durations.iter().any(Duration::is_zero))
        {
            return Err(format!(
                "material `{}` animation frame durations must be positive",
                entry.key
            ));
        }
    }
    Ok(())
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

/// Fill fully transparent pixels with the darkest opaque colour in the sprite.
fn solidify_dark(pixels: &mut [u8], width: u32, rect: [u32; 4]) {
    let [rx, ry, rw, rh] = rect;
    let darkest = (ry..ry + rh)
        .flat_map(|y| (rx..rx + rw).map(move |x| (x, y)))
        .filter_map(|(x, y)| {
            let offset = index(width, x, y);
            (pixels[offset + 3] > 0).then(|| {
                let red = u32::from(pixels[offset]);
                let green = u32::from(pixels[offset + 1]);
                let blue = u32::from(pixels[offset + 2]);
                let luminance = 299 * red + 587 * green + 114 * blue;
                (
                    luminance,
                    [pixels[offset], pixels[offset + 1], pixels[offset + 2]],
                )
            })
        })
        .min_by_key(|(luminance, _)| *luminance)
        .map(|(_, color)| color);
    let Some(darkest) = darkest else {
        return;
    };
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            let offset = index(width, x, y);
            if pixels[offset + 3] == 0 {
                pixels[offset..offset + 3].copy_from_slice(&darkest);
            }
        }
    }
}

fn rect_coverage(pixels: &[u8], width: u32, rect: [u32; 4]) -> f32 {
    let [x, y, rect_width, rect_height] = rect;
    let mut covered = 0usize;
    for row in y..y + rect_height {
        for column in x..x + rect_width {
            covered += usize::from(pixels[index(width, column, row) + 3] >= 128);
        }
    }
    covered as f32 / (rect_width * rect_height) as f32
}

fn preserve_coverage(pixels: &mut [u8], width: u32, rect: [u32; 4], target: f32) {
    let [x, y, rect_width, rect_height] = rect;
    if rect_width == 0 || rect_height == 0 {
        return;
    }
    let count = (rect_width * rect_height) as usize;
    let target_count = (target * count as f32).round() as usize;
    let alphas: Vec<u8> = (y..y + rect_height)
        .flat_map(|row| (x..x + rect_width).map(move |column| index(width, column, row) + 3))
        .map(|offset| pixels[offset])
        .collect();
    let mut low = 0.0f32;
    let mut high = 16.0f32;
    for _ in 0..16 {
        let scale = (low + high) * 0.5;
        let covered = alphas
            .iter()
            .filter(|alpha| f32::from(**alpha) * scale >= 128.0)
            .count();
        if covered > target_count {
            high = scale;
        } else {
            low = scale;
        }
    }
    let coverage_at = |scale: f32| {
        alphas
            .iter()
            .filter(|alpha| f32::from(**alpha) * scale >= 128.0)
            .count()
    };
    let low_error = coverage_at(low).abs_diff(target_count);
    let high_error = coverage_at(high).abs_diff(target_count);
    let scale = if low_error <= high_error { low } else { high };
    for row in y..y + rect_height {
        for column in x..x + rect_width {
            let offset = index(width, column, row) + 3;
            let alpha = f32::from(pixels[offset]);
            pixels[offset] = (alpha * scale).round().clamp(0.0, 255.0) as u8;
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

    #[test]
    fn png_preflight_rejects_large_dimensions_before_parsing() {
        // A header is sufficient to reject the request before CRC parsing or
        // decoder allocation. No large image or output buffer is constructed.
        for (width, height) in [(16384_u32, 16384_u32), (u32::MAX, u32::MAX), (0, 1)] {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend_from_slice(&13_u32.to_be_bytes());
            bytes.extend_from_slice(b"IHDR");
            bytes.extend_from_slice(&width.to_be_bytes());
            bytes.extend_from_slice(&height.to_be_bytes());
            bytes.resize(33, 0);
            assert!(
                Atlas::decode(&bytes)
                    .unwrap_err()
                    .contains("decoded image exceeds")
            );
        }
        assert!(Atlas::decode(b"invalid").unwrap_err().contains("header"));
    }

    #[test]
    fn png_colour_conversion_and_exact_limits() {
        for (colour, input, expected) in [
            (png::ColorType::Rgba, vec![1, 2, 3, 4], [1, 2, 3, 4]),
            (png::ColorType::Rgb, vec![1, 2, 3], [1, 2, 3, 255]),
            (png::ColorType::Grayscale, vec![7], [7, 7, 7, 255]),
            (png::ColorType::GrayscaleAlpha, vec![7, 8], [7, 7, 7, 8]),
        ] {
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
                encoder.set_color(colour);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&input)
                    .unwrap();
            }
            let limits = DecodeLimits {
                max_source_bytes: bytes.len(),
                max_rgba_bytes: 4,
            };
            assert_eq!(
                Atlas::decode_with_limits(&bytes, limits).unwrap().pixels(),
                expected
            );
            assert!(
                Atlas::decode_with_limits(
                    &bytes,
                    DecodeLimits {
                        max_rgba_bytes: 3,
                        ..limits
                    }
                )
                .unwrap_err()
                .contains("decoded image exceeds")
            );
            assert!(
                Atlas::decode_with_limits(
                    &bytes,
                    DecodeLimits {
                        max_source_bytes: bytes.len() - 1,
                        ..limits
                    }
                )
                .unwrap_err()
                .contains("source exceeds")
            );
            bytes.truncate(bytes.len() / 2);
            assert!(Atlas::decode(&bytes).is_err());
        }
    }

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
                ..MaterialEntry::new("", [0; 4])
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
                ..MaterialEntry::new("", [0; 4])
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
                    ..MaterialEntry::new("", [0; 4])
                },
                MaterialEntry {
                    key: "base:material/right",
                    rect: [5, 1, 4, 4],
                    tint: None,
                    ..MaterialEntry::new("", [0; 4])
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
                ..MaterialEntry::new("", [0; 4])
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
    fn animation_selects_ordered_frames_and_interpolates_by_duration() {
        const ORDER: [u32; 4] = [2, 0, 3, 1];
        const DURATIONS: [Duration; 4] = [
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
            Duration::from_secs(4),
        ];
        let pack = TexturePack::new(
            solid_atlas(32, 24, [255, 255, 255, 255]),
            vec![MaterialEntry {
                key: "base:material/animated",
                rect: [2, 2, 4, 16],
                animation: Some(MaterialAnimation {
                    frame_count: 4,
                    frame_duration: Duration::from_secs(1),
                    frame_order: Some(&ORDER),
                    frame_durations: Some(&DURATIONS),
                    interpolate: true,
                }),
                ..MaterialEntry::new("", [0; 4])
            }],
            [10, 2, 1, 1],
            [14, 2, 1, 1],
        )
        .expect("pack builds");
        let surface = pack
            .surface(&key("base:material/animated"))
            .expect("animated material exists");
        let first = surface.frame_sample(Duration::ZERO);
        assert!((first.current[1] - 10.0 / 24.0).abs() < 1e-6);
        assert!((first.next[1] - 2.0 / 24.0).abs() < 1e-6);
        assert_eq!(first.blend, 0.0);
        let middle = surface.frame_sample(Duration::from_millis(500));
        assert!((middle.blend - 0.5).abs() < 1e-6);
        let boundary = surface.frame_sample(Duration::from_secs(1));
        assert!((boundary.current[1] - 2.0 / 24.0).abs() < 1e-6);
        assert_eq!(boundary.blend, 0.0);
    }

    #[test]
    fn static_material_keeps_one_frame_for_all_times() {
        let pack = TexturePack::new(
            solid_atlas(12, 12, [255, 255, 255, 255]),
            vec![MaterialEntry::new("base:material/static", [2, 2, 4, 4])],
            [8, 2, 1, 1],
            [8, 8, 1, 1],
        )
        .expect("pack builds");
        let surface = pack
            .surface(&key("base:material/static"))
            .expect("static material exists");
        assert_eq!(
            surface.frame_sample(Duration::from_secs(100)),
            surface.frame_sample(Duration::ZERO)
        );
    }

    #[test]
    fn cutout_mips_keep_coverage_and_dark_mips_use_dark_transparent_rgb() {
        let mut pixels = vec![0u8; 32 * 32 * 4];
        for y in 4..12 {
            for x in 4..12 {
                let offset = index(32, x, y);
                let alpha = if y < 8 { 255 } else { 0 };
                pixels[offset..offset + 4].copy_from_slice(&[220, 180, 100, alpha]);
            }
        }
        for y in 4..12 {
            for x in 16..24 {
                let offset = index(32, x, y);
                let alpha = if y < 8 { 255 } else { 0 };
                pixels[offset..offset + 4].copy_from_slice(&[70, 60, 50, alpha]);
            }
        }
        let pack = TexturePack::new(
            Atlas {
                width: 32,
                height: 32,
                pixels,
            },
            vec![
                MaterialEntry {
                    key: "base:material/leaves",
                    rect: [4, 4, 8, 8],
                    mipmap_strategy: MipmapStrategy::CutoutCoverage,
                    ..MaterialEntry::new("", [0; 4])
                },
                MaterialEntry {
                    key: "base:material/dark_leaves",
                    rect: [16, 4, 8, 8],
                    mipmap_strategy: MipmapStrategy::DarkCutout,
                    ..MaterialEntry::new("", [0; 4])
                },
            ],
            [4, 16, 1, 1],
            [8, 16, 1, 1],
        )
        .expect("pack builds");
        let chain = pack.mips(3).expect("mips build");
        assert!((rect_coverage(&chain[0].pixels, 32, [4, 4, 8, 8]) - 0.5).abs() < 0.01);
        assert!((rect_coverage(&chain[1].pixels, 16, [2, 2, 4, 4]) - 0.5).abs() < 0.1);
        assert!((rect_coverage(&chain[2].pixels, 8, [1, 1, 2, 2]) - 0.5).abs() < 0.1);
        let dark_transparent = index(32, 16, 10);
        assert_eq!(
            &chain[0].pixels[dark_transparent..dark_transparent + 3],
            &[70, 60, 50]
        );
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
