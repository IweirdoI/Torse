//! wgpu rendering for the client.
//!
//! [`GpuContext`] owns the device-facing half: pipelines, the camera uniform,
//! the material table, and terrain buffers. Both targets reuse it: [`Renderer`]
//! draws into an offscreen texture so a frame can be captured without a window,
//! and the windowed app draws into a surface view. The pass is identical, only
//! the target differs.
//!
//! Terrain vertices carry a material slot. The fragment shader looks the slot up
//! in a fixed-size material table, samples the texture atlas through the
//! stored rectangle, multiplies the result by the material tint, and applies the
//! material's alpha mode. Materials come from [`crate::texture::Palette`].

use std::collections::BTreeMap;
use std::fs::File;
use std::time::{Duration, Instant};

use wgpu::{
    AddressMode, BindGroup, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    BindingType, BlendState, Buffer, BufferBindingType, BufferDescriptor, BufferUsages, Color,
    ColorTargetState, ColorWrites, CommandEncoder, CommandEncoderDescriptor, Device,
    DeviceDescriptor, Extent3d, FilterMode, FragmentState, Instance, InstanceDescriptor, LoadOp,
    MapMode, MultisampleState, Operations, Origin3d, PollType, PrimitiveState, Queue,
    RenderPassColorAttachment, RenderPassDepthStencilAttachment, RenderPassDescriptor,
    RenderPipeline, RenderPipelineDescriptor, RequestAdapterOptions, SamplerBindingType,
    SamplerDescriptor, ShaderModuleDescriptor, ShaderSource, ShaderStages, StoreOp,
    TexelCopyBufferInfo, TexelCopyBufferLayout, TexelCopyTextureInfo, Texture, TextureAspect,
    TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
    TextureView, TextureViewDescriptor, VertexState,
};

use crate::texture::{MAX_MATERIALS, Palette};

const TERRAIN_SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};

struct Surface {
    rect: vec4<f32>,
    tint: vec4<f32>,
    next_rect: vec4<f32>,
    frame: vec4<f32>,
};

struct Materials {
    entries: array<Surface, 512>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(1) @binding(0)
var<uniform> materials: Materials;

@group(1) @binding(1)
var atlas: texture_2d<f32>;

@group(1) @binding(2)
var atlas_sampler: sampler;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) material: u32,
    @location(4) overlay_material: u32,
    @location(5) tint: vec3<f32>,
    @location(6) overlay_tint: vec3<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) material: u32,
    @location(3) @interpolate(flat) overlay_material: u32,
    @location(4) tint: vec3<f32>,
    @location(5) overlay_tint: vec3<f32>,
    @location(6) world: vec3<f32>,
};

fn sample_surface(surface: Surface, uv: vec2<f32>) -> vec4<f32> {
    let texel = 0.5 / vec2<f32>(textureDimensions(atlas, 0));
    let minimum = surface.rect.xy + texel;
    let maximum = surface.rect.xy + surface.rect.zw - texel;
    let first_uv = clamp(surface.rect.xy + uv * surface.rect.zw, minimum, maximum);
    var first: vec4<f32>;
    if (surface.frame.y > 0.5) {
        first = textureSampleLevel(atlas, atlas_sampler, first_uv, 0.0);
    } else {
        first = textureSample(atlas, atlas_sampler, first_uv);
    }
    if (surface.frame.x <= 0.0) {
        return first;
    }
    let second_uv = clamp(
        surface.next_rect.xy + uv * surface.next_rect.zw,
        surface.next_rect.xy + texel,
        surface.next_rect.xy + surface.next_rect.zw - texel,
    );
    let second = textureSampleLevel(atlas, atlas_sampler, second_uv, 0.0);
    return mix(first, second, surface.frame.x);
}

fn srgb_encode(color: vec3<f32>) -> vec3<f32> {
    let high = 1.055 * pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, color * 12.92, color <= vec3<f32>(0.0031308));
}

fn srgb_decode(color: vec3<f32>) -> vec3<f32> {
    let high = pow((color + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, color / 12.92, color <= vec3<f32>(0.04045));
}

// Brightness of one face. A block surface is lit by its axis, not by a dot
// product with a sun direction, which keeps every voxel face equally readable.
fn face_shade(normal: vec3<f32>) -> f32 {
    if (normal.y > 0.5) {
        return 1.0;
    }
    if (normal.y < -0.5) {
        return 0.5;
    }
    if (abs(normal.x) > 0.5) {
        return 0.6;
    }
    return 0.8;
}

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = camera.view_proj * vec4<f32>(input.position, 1.0);
    out.normal = input.normal;
    out.uv = input.uv;
    out.material = input.material;
    out.overlay_material = input.overlay_material;
    out.tint = input.tint;
    out.overlay_tint = input.overlay_tint;
    out.world = input.position;
    return out;
}

@fragment
fn fs_main(
    input: VertexOut,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let surface = materials.entries[input.material];
    let texel = sample_surface(surface, input.uv);

    // tint.w encodes the alpha mode: 0 opaque, 1 mask, 2 blend.
    let mode = surface.tint.w;
    if (mode > 0.5 && mode < 1.5 && texel.a < 0.5) {
        discard;
    }
    // Colours combine in sRGB space, the space the textures were authored in;
    // the linear result is re-encoded for the sRGB target.
    var color = srgb_encode(texel.rgb) * srgb_encode(surface.tint.rgb) * srgb_encode(input.tint);
    var alpha = select(1.0, texel.a, mode > 1.5);
    if (input.overlay_material != 0xffffffffu) {
        let overlay = materials.entries[input.overlay_material];
        let overlay_texel = sample_surface(overlay, input.uv);
        let overlay_mode = overlay.tint.w;
        let overlay_color = srgb_encode(overlay_texel.rgb)
            * srgb_encode(overlay.tint.rgb)
            * srgb_encode(input.overlay_tint);
        if (overlay_mode < 0.5) {
            color = overlay_color;
            alpha = 1.0;
        } else if (overlay_mode < 1.5) {
            if (overlay_texel.a >= 0.5) {
                color = overlay_color;
                alpha = 1.0;
            }
        } else {
            let combined_alpha = overlay_texel.a + alpha * (1.0 - overlay_texel.a);
            if (combined_alpha > 0.0) {
                color = (overlay_color * overlay_texel.a
                    + color * alpha * (1.0 - overlay_texel.a))
                    / combined_alpha;
            }
            alpha = combined_alpha;
        }
    }
    let normal = select(-input.normal, input.normal, front_facing);
    let sky = srgb_encode(vec3<f32>(0.66, 0.74, 0.80));
    let distance = length(input.world - camera.eye.xyz);
    let fog = clamp(1.0 - exp(-distance * 0.0028), 0.0, 0.72);
    let lit = color * face_shade(normal);
    return vec4<f32>(srgb_decode(mix(lit, sky, fog)), alpha);
}
"#;

const LINE_SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return camera.view_proj * vec4<f32>(position, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.05, 0.05, 0.05, 1.0);
}
"#;

/// Sky colour shared by both targets.
pub const SKY: Color = Color {
    r: 0.66,
    g: 0.74,
    b: 0.80,
    a: 1.0,
};

/// Selection outline capacity: 12 cube edges of 2 vertices each.
const MAX_LINE_VERTICES: usize = 24;

/// Bytes per terrain vertex: position, normal, uv, and one material slot.
const VERTEX_STRIDE: usize = 64;

/// Material table bytes: rect plus tint per surface.
const SURFACE_BYTES: u64 = 64;

/// Synthetic key carrying the entity box buffers; retention never drops it.
pub const ENTITY_SLOT: torse_types::SectionPos =
    torse_types::SectionPos::new(i64::MAX, 0, i64::MAX);

/// Device, queue, pipelines, camera uniform, and terrain buffers.
pub struct GpuContext {
    device: Device,
    queue: Queue,
    pipeline: RenderPipeline,
    line_pipeline: RenderPipeline,
    camera: Buffer,
    camera_bind: BindGroup,
    palette_layout: BindGroupLayout,
    palette: Option<PaletteBinding>,
    #[allow(dead_code)]
    camera_layout: BindGroupLayout,
    /// One vertex/index buffer pair per resident section; edits rewrite only
    /// the changed sections instead of re-uploading the whole scene.
    terrain: BTreeMap<torse_types::SectionPos, SectionBuffers>,
    line_buffer: Buffer,
    line_count: u32,
    animation_origin: Instant,
}

/// GPU resources for the material table and atlas.
struct PaletteBinding {
    #[allow(dead_code)]
    materials: Buffer,
    #[allow(dead_code)]
    texture: Texture,
    surfaces: Vec<crate::texture::Surface>,
    bind: BindGroup,
}

/// GPU buffers for one section's mesh.
struct SectionBuffers {
    vertices: Buffer,
    indices: Buffer,
    index_count: u32,
}

impl GpuContext {
    /// Build the pipelines and camera binding on an already connected device.
    ///
    /// `color_format` is the format of the views the pass will draw into; the
    /// offscreen target picks it itself while a surface inherits it from the
    /// adapter.
    pub fn new(device: Device, queue: Queue, color_format: TextureFormat) -> Self {
        let camera_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("client camera layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let palette_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("client palette layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let camera = device.create_buffer(&BufferDescriptor {
            label: Some("client camera uniform"),
            size: 80,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("client camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            }],
        });

        let terrain_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("client terrain pipeline layout"),
                bind_group_layouts: &[Some(&camera_layout), Some(&palette_layout)],
                immediate_size: 0,
            });
        let line_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("client outline pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            immediate_size: 0,
        });

        let terrain_shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("client terrain shader"),
            source: ShaderSource::Wgsl(TERRAIN_SHADER.into()),
        });
        let line_shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("client outline shader"),
            source: ShaderSource::Wgsl(LINE_SHADER.into()),
        });
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 24,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 32,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 36,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 40,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 52,
                shader_location: 6,
            },
        ];
        let terrain_vertex_state = VertexState {
            module: &terrain_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: VERTEX_STRIDE as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        };
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("client terrain pipeline"),
            layout: Some(&terrain_pipeline_layout),
            vertex: terrain_vertex_state,
            fragment: Some(FragmentState {
                module: &terrain_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format: color_format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // The outline only reads depth, so it never occludes the terrain it
        // marks. It ignores texture coordinates and material slots.
        let line_pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("client outline pipeline"),
            layout: Some(&line_pipeline_layout),
            vertex: VertexState {
                module: &line_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    }],
                })],
            },
            fragment: Some(FragmentState {
                module: &line_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format: color_format,
                    blend: Some(BlendState::REPLACE),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..PrimitiveState::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let line_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("client outline vertices"),
            size: (MAX_LINE_VERTICES * VERTEX_STRIDE) as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            device,
            queue,
            pipeline,
            line_pipeline,
            camera,
            camera_bind,
            palette_layout,
            palette: None,
            camera_layout,
            terrain: BTreeMap::new(),
            line_buffer,
            line_count: 0,
            animation_origin: Instant::now(),
        }
    }

    /// Connected device and queue.
    pub fn parts(&self) -> (&Device, &Queue) {
        (&self.device, &self.queue)
    }

    /// Upload the atlas and material table used by the terrain pass.
    ///
    /// Calling this again replaces the previous pack. Sections keep their
    /// vertex buffers because material slots are session-stable; content must
    /// be recompiled when the pack changes its material set.
    pub fn set_palette(&mut self, palette: &Palette) {
        self.animation_origin = Instant::now();
        let materials = self.device.create_buffer(&BufferDescriptor {
            label: Some("client material table"),
            size: MAX_MATERIALS as u64 * SURFACE_BYTES,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(
            &materials,
            0,
            &surface_bytes(palette.surfaces(), Duration::ZERO),
        );

        let mips = palette.mips();
        let base = mips.first().expect("palette always carries the base level");
        let texture = self.device.create_texture(&TextureDescriptor {
            label: Some("client texture atlas"),
            size: Extent3d {
                width: base.width,
                height: base.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mips.len() as u32,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level_index, level) in mips.iter().enumerate() {
            let row_bytes = level.width * 4;
            let padded = align_256(row_bytes);
            let mut pixels = vec![0u8; (padded * level.height) as usize];
            for row in 0..level.height {
                let source = (row * row_bytes) as usize;
                let target = (row * padded) as usize;
                pixels[target..target + row_bytes as usize]
                    .copy_from_slice(&level.pixels[source..source + row_bytes as usize]);
            }
            self.queue.write_texture(
                TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level_index as u32,
                    origin: Origin3d::ZERO,
                    aspect: TextureAspect::All,
                },
                &pixels,
                TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(level.height),
                },
                Extent3d {
                    width: level.width,
                    height: level.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        let view = texture.create_view(&TextureViewDescriptor::default());
        let sampler = self.device.create_sampler(&SamplerDescriptor {
            label: Some("client atlas sampler"),
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            address_mode_w: AddressMode::ClampToEdge,
            // Nearest magnification keeps the pixel-art look; linear
            // minification blends between the generated mip levels.
            mag_filter: FilterMode::Nearest,
            min_filter: FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..SamplerDescriptor::default()
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("client palette bind group"),
            layout: &self.palette_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: materials.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.palette = Some(PaletteBinding {
            materials,
            texture,
            surfaces: palette.surfaces().to_vec(),
            bind,
        });
    }

    /// Replace one section's terrain buffers, keeping other sections as-is.
    ///
    /// Empty content removes the entry instead: wgpu forbids slicing a
    /// zero-size buffer, and an empty mesh simply has nothing to draw.
    pub fn upload_section(
        &mut self,
        section: torse_types::SectionPos,
        vertex_bytes: &[u8],
        indices: &[u32],
    ) {
        if vertex_bytes.is_empty() || indices.is_empty() {
            self.terrain.remove(&section);
            return;
        }
        use wgpu::util::DeviceExt;
        let buffers = SectionBuffers {
            vertices: self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("client terrain vertices"),
                    contents: vertex_bytes,
                    usage: BufferUsages::VERTEX,
                }),
            indices: self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("client terrain indices"),
                    contents: &by_index_bytes(indices),
                    usage: BufferUsages::INDEX,
                }),
            index_count: indices.len() as u32,
        };
        self.terrain.insert(section, buffers);
    }

    /// Drop the buffers of unloaded sections.
    ///
    /// The entity slot is synthetic and carries no section coordinate, so it
    /// survives every retention; callers re-upload its contents when entity
    /// positions change.
    pub fn retain_sections(&mut self, keep: &[torse_types::SectionPos]) {
        self.terrain
            .retain(|pos, _| keep.contains(pos) || *pos == ENTITY_SLOT);
    }

    /// Stage the camera matrix for the next frame.
    pub fn write_camera(&self, view_projection: [f32; 16], eye: [f32; 3]) {
        self.queue
            .write_buffer(&self.camera, 0, &matrix_bytes(view_projection));
        let mut position = [0f32; 4];
        position[..3].copy_from_slice(&eye);
        let mut bytes = Vec::with_capacity(16);
        for value in position {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        self.queue.write_buffer(&self.camera, 64, &bytes);
    }

    /// Replace the selection outline. `vertex_bytes` uses the terrain layout
    /// and is truncated to the outline capacity.
    pub fn write_lines(&mut self, vertex_bytes: &[u8]) {
        let limit = vertex_bytes.len().min(MAX_LINE_VERTICES * VERTEX_STRIDE);
        self.queue
            .write_buffer(&self.line_buffer, 0, &vertex_bytes[..limit]);
        self.line_count = (limit / VERTEX_STRIDE) as u32;
    }

    /// Encode the one terrain pass onto any colour and depth view pair.
    pub fn encode_draw(
        &self,
        encoder: &mut CommandEncoder,
        color: &TextureView,
        depth: &TextureView,
    ) {
        if let Some(palette) = &self.palette {
            self.queue.write_buffer(
                &palette.materials,
                0,
                &surface_bytes(&palette.surfaces, self.animation_origin.elapsed()),
            );
        }
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("client terrain pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: color,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(SKY),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(Operations {
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(palette) = &self.palette {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind, &[]);
            pass.set_bind_group(1, &palette.bind, &[]);
            for section in self.terrain.values() {
                if section.index_count == 0 {
                    continue;
                }
                pass.set_vertex_buffer(0, section.vertices.slice(..));
                pass.set_index_buffer(section.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..section.index_count, 0, 0..1);
            }
        }
        if self.line_count > 0 {
            pass.set_pipeline(&self.line_pipeline);
            pass.set_bind_group(0, &self.camera_bind, &[]);
            pass.set_vertex_buffer(0, self.line_buffer.slice(..));
            pass.draw(0..self.line_count, 0..1);
        }
    }
}

/// Offscreen renderer.
pub struct Renderer {
    context: GpuContext,
    color: Texture,
    color_view: TextureView,
    depth_view: TextureView,
    width: u32,
    height: u32,
}

impl Renderer {
    /// Create a headless renderer.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let instance = Instance::new(InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default()))
                .map_err(|error| format!("adapter unavailable: {error}"))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))
                .map_err(|error| format!("device unavailable: {error}"))?;

        let color = device.create_texture(&TextureDescriptor {
            label: Some("client color target"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color.create_view(&TextureViewDescriptor::default());
        let depth = device.create_texture(&TextureDescriptor {
            label: Some("client depth target"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Depth32Float,
            usage: TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&TextureViewDescriptor::default());

        let context = GpuContext::new(device, queue, TextureFormat::Rgba8UnormSrgb);
        Ok(Self {
            context,
            color,
            color_view,
            depth_view,
            width,
            height,
        })
    }

    /// Upload the palette used by the terrain pass.
    pub fn set_palette(&mut self, palette: &Palette) {
        self.context.set_palette(palette);
    }

    /// Replace one section's terrain buffers.
    pub fn upload_section(
        &mut self,
        section: torse_types::SectionPos,
        vertex_bytes: &[u8],
        indices: &[u32],
    ) {
        self.context.upload_section(section, vertex_bytes, indices);
    }

    /// Render one frame into the offscreen target.
    pub fn render(&mut self, view_projection: [f32; 16], eye: [f32; 3]) {
        self.context.write_camera(view_projection, eye);
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("client frame"),
            });
        self.context
            .encode_draw(&mut encoder, &self.color_view, &self.depth_view);
        self.context.queue.submit([encoder.finish()]);
    }

    /// Draw the same UI used by the window into an offscreen capture.
    pub fn render_ui(&self, mode: crate::ui::Mode) -> Result<(), String> {
        let (device, queue) = self.context.parts();
        let mut layer = crate::ui::Layer::new(device, TextureFormat::Rgba8UnormSrgb)?;
        let mut state = crate::ui::State::default();
        state.set_mode(mode);
        layer.prepare(
            device,
            queue,
            [self.width, self.height],
            &state,
            crate::ui::Hud {
                material: 0,
                network: false,
                ready: true,
                result: "",
                sensitivity: 1.0,
            },
        );
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("UI capture"),
        });
        layer.draw(&mut encoder, &self.color_view);
        queue.submit([encoder.finish()]);
        Ok(())
    }

    /// Copy the rendered frame into a PNG file.
    pub fn save_png(&self, path: &str) -> Result<(), String> {
        let padded_row = align_256(self.width * 4);
        let buffer_size = u64::from(padded_row) * u64::from(self.height);
        let readback = self.context.device.create_buffer(&BufferDescriptor {
            label: Some("client readback"),
            size: buffer_size,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("client readback"),
            });
        encoder.copy_texture_to_buffer(
            TexelCopyTextureInfo {
                texture: &self.color,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            TexelCopyBufferInfo {
                buffer: &readback,
                layout: TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(self.height),
                },
            },
            Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.context.queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        slice.map_async(MapMode::Read, |_| {});
        self.context
            .device
            .poll(PollType::wait_indefinitely())
            .map_err(|error| format!("device poll failed: {error}"))?;
        let view = slice
            .get_mapped_range()
            .map_err(|error| format!("map failed: {error}"))?;
        let mut pixels = Vec::with_capacity((self.width * self.height * 4) as usize);
        for row in 0..self.height {
            let start = (row * padded_row) as usize;
            let end = start + (self.width * 4) as usize;
            pixels.extend_from_slice(&view[start..end]);
        }
        drop(view);
        readback.unmap();

        write_png(path, self.width, self.height, &pixels)
    }
}

/// Encode tight RGBA rows into a PNG file.
pub fn write_png(path: &str, width: u32, height: u32, pixels: &[u8]) -> Result<(), String> {
    let file = File::create(path).map_err(|error| format!("create failed: {error}"))?;
    let mut writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(&mut writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder
        .write_header()
        .map_err(|error| format!("png header failed: {error}"))?;
    png_writer
        .write_image_data(pixels)
        .map_err(|error| format!("png write failed: {error}"))?;
    Ok(())
}

fn matrix_bytes(matrix: [f32; 16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    for value in matrix {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn by_index_bytes(indices: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(indices.len() * 4);
    for value in indices {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn surface_bytes(surfaces: &[crate::texture::Surface], elapsed: Duration) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAX_MATERIALS * SURFACE_BYTES as usize);
    for surface in surfaces {
        let resolved = torse_assets::texture::MaterialSurface {
            rect: surface.rect,
            tint: surface.tint,
            tint_source: surface.tint_source,
            animation: surface.animation,
        };
        let frames = resolved.frame_sample(elapsed);
        for value in frames.current {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for value in surface.tint {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let mode: f32 = match surface.alpha {
            torse_content::AlphaMode::Opaque => 0.0,
            torse_content::AlphaMode::Mask => 1.0,
            torse_content::AlphaMode::Blend => 2.0,
        };
        bytes.extend_from_slice(&mode.to_le_bytes());
        for value in frames.next {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let animated = if surface.animation.is_some() {
            1.0
        } else {
            0.0
        };
        for value in [frames.blend, animated, 0.0, 0.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes.resize(MAX_MATERIALS * SURFACE_BYTES as usize, 0);
    bytes
}

const fn align_256(value: u32) -> u32 {
    (value + 255) & !255
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_png_reports_file_errors() {
        // A path with a missing directory must surface the OS error, not panic.
        let result = write_png("no-such-dir/frame.png", 1, 1, &[0, 0, 0, 255]);
        assert!(result.is_err());
    }

    #[test]
    fn surface_table_is_padded_to_the_budget() {
        let palette = Palette::empty_for_tests();
        let bytes = surface_bytes(palette.surfaces(), Duration::ZERO);
        assert_eq!(bytes.len(), MAX_MATERIALS * SURFACE_BYTES as usize);
    }

    #[test]
    fn surface_bytes_encode_rect_tint_and_alpha_mode() {
        let palette = Palette::single_for_tests(
            [0.0, 0.0, 0.5, 0.5],
            [0.2, 0.4, 0.6],
            torse_content::AlphaMode::Blend,
        );
        let bytes = surface_bytes(palette.surfaces(), Duration::ZERO);
        let read =
            |offset: usize| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert_eq!(read(0), 0.0);
        assert_eq!(read(8), 0.5);
        assert_eq!(read(12), 0.5);
        assert_eq!(read(16), 0.2);
        assert_eq!(read(24), 0.6);
        assert_eq!(read(28), 2.0);
        assert_eq!(read(32), 0.0);
        assert_eq!(read(48), 0.0);
        assert_eq!(read(52), 0.0);
    }

    #[test]
    fn animated_surface_table_updates_frame_rectangles_and_blend() {
        const FRAME_DURATION: Duration = Duration::from_millis(100);
        let surface = crate::texture::Surface {
            rect: [0.25, 0.0, 0.5, 1.0],
            tint: [1.0; 3],
            tint_source: None,
            animation: Some(torse_assets::texture::MaterialAnimation {
                frame_count: 4,
                frame_duration: FRAME_DURATION,
                frame_order: None,
                frame_durations: None,
                interpolate: true,
            }),
            alpha: torse_content::AlphaMode::Opaque,
        };
        let initial = surface_bytes(&[surface], Duration::ZERO);
        let halfway = surface_bytes(&[surface], Duration::from_millis(50));
        let read = |bytes: &[u8], offset: usize| {
            f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
        };
        assert_eq!(read(&initial, 12), 0.25);
        assert_eq!(read(&initial, 44), 0.25);
        assert_eq!(read(&initial, 48), 0.0);
        assert_eq!(read(&initial, 52), 1.0);
        assert!((read(&halfway, 48) - 0.5).abs() < 1e-6);
    }
}
