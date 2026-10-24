//! Interactive windowed client.
//!
//! One winit window driven by [`ApplicationHandler`]. Each frame integrates the
//! fly camera from aggregated input, streams scene sections around the camera,
//! rebuilds dirty meshes, and draws through the shared [`GpuContext`] into the
//! window surface. Sections outside the radius unload, so sustained flight
//! keeps the resident set bounded.

use std::sync::Arc;
use std::time::Instant;

use wgpu::{
    BufferDescriptor, BufferUsages, CommandEncoderDescriptor, CurrentSurfaceTexture, Device,
    DeviceDescriptor, Extent3d, Instance, InstanceDescriptor, PollType, Queue,
    RequestAdapterOptions, Surface, SurfaceConfiguration, SurfaceTexture, TexelCopyBufferInfo,
    TexelCopyBufferLayout, TexelCopyTextureInfo, Texture, TextureAspect, TextureDescriptor,
    TextureDimension, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::camera::FlyCamera;
use crate::input::InputState;
use crate::renderer::GpuContext;
use crate::scene::Scene;
use torse_types::SectionPos;

/// Startup parameters for the windowed run.
#[derive(Clone, Debug)]
pub struct WindowConfig {
    /// Section loading radius around the camera.
    pub radius: i64,
    /// Initial window size in logical pixels.
    pub width: u32,
    pub height: u32,
    /// Exit automatically after this many presented frames.
    pub frames: Option<u32>,
    /// Save the final presented frame to this path before exiting.
    pub screenshot: Option<String>,
    /// Fly forward automatically instead of waiting for keys.
    pub autoplay: bool,
}

/// Measurements reported when the window closes.
#[derive(Clone, Debug, Default)]
pub struct WindowSummary {
    /// Adapter name reported by the backend.
    pub adapter: String,
    /// Backend selected for the surface.
    pub backend: String,
    /// Surface colour format the pipeline was built for.
    pub format: String,
    /// Presented frames.
    pub frames: u32,
    /// Times the section window moved with the camera.
    pub streams: usize,
    /// Section meshes rebuilt.
    pub rebuilds: usize,
    /// Geometry uploads to the GPU.
    pub uploads: usize,
    /// Screenshots written.
    pub screenshots: usize,
    /// Resident sections at exit.
    pub sections: usize,
    /// Resident meshes at exit.
    pub meshes: usize,
    /// Final vertex and face counts.
    pub vertices: usize,
    pub faces: usize,
    /// Wall time from resume to exit.
    pub elapsed_ms: u128,
}

struct Gpu {
    context: GpuContext,
    surface: Surface<'static>,
    config: SurfaceConfiguration,
    depth: Texture,
    depth_view: TextureView,
    window: Arc<Window>,
    backend_name: String,
}

struct WindowApp {
    config: WindowConfig,
    scene: Option<Scene>,
    camera: Option<FlyCamera>,
    input: InputState,
    gpu: Option<Gpu>,
    center: Option<SectionPos>,
    uploaded: bool,
    frames: u32,
    streams: usize,
    rebuilds: usize,
    uploads: usize,
    screenshots: usize,
    adapter: String,
    started: Option<Instant>,
    last_frame: Option<Instant>,
    fps_origin: Instant,
    fps_frames: u32,
    pending_shot: Option<String>,
    summary: Option<WindowSummary>,
    failure: Option<String>,
}

impl WindowApp {
    fn new(config: WindowConfig) -> Self {
        Self {
            config,
            scene: None,
            camera: None,
            input: InputState::new(),
            gpu: None,
            center: None,
            uploaded: false,
            frames: 0,
            streams: 0,
            rebuilds: 0,
            uploads: 0,
            screenshots: 0,
            adapter: String::new(),
            started: None,
            last_frame: None,
            fps_origin: Instant::now(),
            fps_frames: 0,
            pending_shot: None,
            summary: None,
            failure: None,
        }
    }

    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        // First frame assumes 60 fps; afterwards integrate the real interval,
        // clamped so a pause (breakpoint, window drag) cannot teleport the camera.
        let step = self
            .last_frame
            .map_or(1.0 / 60.0, |last| now.duration_since(last).as_secs_f32())
            .clamp(0.0001, 0.25);
        self.last_frame = Some(now);

        let Some(camera) = self.camera.as_mut() else {
            return;
        };
        let (mut motion, speed) = self.input.take_input(camera.speed);
        camera.speed = speed;
        if self.config.autoplay {
            motion.forward = 1.0;
        }
        camera.advance(motion, step);

        let center = self
            .scene
            .as_ref()
            .expect("camera implies scene")
            .center_section(camera.eye);
        if self.center != Some(center) {
            let Some(scene) = self.scene.as_mut() else {
                return;
            };
            if let Err(error) = scene.load_around(center) {
                self.failure = Some(format!("streaming failed: {error}"));
                event_loop.exit();
                return;
            }
            self.center = Some(center);
            self.streams += 1;
        }

        let rebuilt = self
            .scene
            .as_mut()
            .expect("camera implies scene")
            .refresh_meshes();
        self.rebuilds += rebuilt;
        if rebuilt > 0 || !self.uploaded {
            let (vertices, indices, _) = self
                .scene
                .as_ref()
                .expect("camera implies scene")
                .geometry();
            let bytes = Scene::vertex_bytes(&vertices);
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.context.upload(&bytes, &indices);
            }
            self.uploaded = true;
            self.uploads += 1;
        }

        if self.input.take_screenshot() {
            self.pending_shot = Some(format!("torse-shot-{}.png", self.screenshots + 1));
        }

        let frame = match self.gpu.as_mut() {
            Some(gpu) => match gpu.surface.get_current_texture() {
                CurrentSurfaceTexture::Success(frame)
                | CurrentSurfaceTexture::Suboptimal(frame) => frame,
                CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                    gpu.window.request_redraw();
                    return;
                }
                CurrentSurfaceTexture::Outdated => {
                    let (device, _) = gpu.context.parts();
                    gpu.surface.configure(device, &gpu.config);
                    gpu.window.request_redraw();
                    return;
                }
                CurrentSurfaceTexture::Lost | CurrentSurfaceTexture::Validation => {
                    self.failure = Some("surface lost or failed validation".to_owned());
                    event_loop.exit();
                    return;
                }
            },
            None => return,
        };

        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let aspect = gpu.config.width as f32 / gpu.config.height.max(1) as f32;
        let (device, queue) = gpu.context.parts();
        gpu.context.write_camera(
            self.camera
                .as_ref()
                .expect("gpu implies camera")
                .view_projection(aspect),
        );
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("client window frame"),
        });
        gpu.context
            .encode_draw(&mut encoder, &view, &gpu.depth_view);
        queue.submit([encoder.finish()]);
        if let Some(path) = self.pending_shot.take() {
            // The copy must be queued after the draw pass, otherwise it reads
            // the presented texture before this frame has been drawn into it.
            match capture_frame(
                device,
                queue,
                &frame,
                gpu.config.width,
                gpu.config.height,
                gpu.config.format,
                &path,
            ) {
                Ok(()) => self.screenshots += 1,
                Err(error) => {
                    self.failure = Some(error);
                }
            }
        }
        queue.present(frame);

        self.frames += 1;
        self.fps_frames += 1;
        let title_span = self.fps_origin.elapsed().as_secs_f32();
        if title_span >= 0.5 {
            let fps = self.fps_frames as f32 / title_span;
            if let Some(scene) = self.scene.as_ref() {
                gpu.window.set_title(&format!(
                    "Torse client | {fps:.0} fps | sections {} | meshes {}",
                    scene.sections().len(),
                    scene.mesh_count(),
                ));
            }
            self.fps_origin = Instant::now();
            self.fps_frames = 0;
        }

        if let Some(limit) = self.config.frames
            && self.frames >= limit
        {
            if let Some(path) = self.config.screenshot.clone() {
                self.pending_shot = Some(path);
                // Present once more so the screenshot captures a real frame,
                // then stop on the following one.
                if self.frames == limit {
                    gpu.window.request_redraw();
                    return;
                }
            }
            self.finish(event_loop);
            return;
        }
        gpu.window.request_redraw();
    }

    fn finish(&mut self, event_loop: &ActiveEventLoop) {
        if let Some((scene, gpu)) = self.scene.as_ref().zip(self.gpu.as_ref()) {
            let (vertices, _, faces) = scene.geometry();
            self.summary = Some(WindowSummary {
                adapter: self.adapter.clone(),
                backend: gpu.backend_name.clone(),
                format: format!("{:?}", gpu.config.format),
                frames: self.frames,
                streams: self.streams,
                rebuilds: self.rebuilds,
                uploads: self.uploads,
                screenshots: self.screenshots,
                sections: scene.sections().len(),
                meshes: scene.mesh_count(),
                vertices: vertices.len(),
                faces,
                elapsed_ms: self
                    .started
                    .map(|start| start.elapsed().as_millis())
                    .unwrap_or_default(),
            });
        }
        event_loop.exit();
    }
}

impl ApplicationHandler for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes()
            .with_title("Torse client")
            .with_inner_size(LogicalSize::new(self.config.width, self.config.height));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(format!("window creation failed: {error}"));
                event_loop.exit();
                return;
            }
        };

        let instance = Instance::new(InstanceDescriptor::new_without_display_handle());
        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                self.failure = Some(format!("surface creation failed: {error}"));
                event_loop.exit();
                return;
            }
        };
        let options = RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..RequestAdapterOptions::default()
        };
        let adapter = match pollster::block_on(instance.request_adapter(&options)) {
            Ok(adapter) => adapter,
            Err(error) => {
                self.failure = Some(format!("adapter unavailable: {error}"));
                event_loop.exit();
                return;
            }
        };
        let info = adapter.get_info();
        let (device, queue) =
            match pollster::block_on(adapter.request_device(&DeviceDescriptor::default())) {
                Ok(pair) => pair,
                Err(error) => {
                    self.failure = Some(format!("device unavailable: {error}"));
                    event_loop.exit();
                    return;
                }
            };

        let size = window.inner_size();
        let mut config =
            match surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)) {
                Some(config) => config,
                None => {
                    self.failure = Some("surface has no supported configuration".to_owned());
                    event_loop.exit();
                    return;
                }
            };
        // Screenshots read back from the presented texture.
        config.usage |= TextureUsages::COPY_SRC;
        surface.configure(&device, &config);
        let depth = create_depth(&device, config.width, config.height);
        let depth_view = depth.create_view(&TextureViewDescriptor::default());
        let context = GpuContext::new(device, queue, config.format);

        let scene = match Scene::new(self.config.radius) {
            Ok(scene) => scene,
            Err(error) => {
                self.failure = Some(error);
                event_loop.exit();
                return;
            }
        };
        let camera = FlyCamera::new(scene.eye());
        self.adapter = info.name;
        self.gpu = Some(Gpu {
            backend_name: format!("{:?}", info.backend),
            context,
            surface,
            config,
            depth,
            depth_view,
            window,
        });
        self.scene = Some(scene);
        self.camera = Some(camera);
        self.started = Some(Instant::now());
        self.fps_origin = Instant::now();
        // Prime one frame immediately instead of waiting for the first event.
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.finish(event_loop);
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut()
                    && size.width > 0
                    && size.height > 0
                {
                    gpu.config.width = size.width;
                    gpu.config.height = size.height;
                    let (device, _) = gpu.context.parts();
                    gpu.surface.configure(device, &gpu.config);
                    gpu.depth = create_depth(device, size.width, size.height);
                    gpu.depth_view = gpu.depth.create_view(&TextureViewDescriptor::default());
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && let winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Escape) =
                        event.physical_key
                {
                    self.finish(event_loop);
                    return;
                }
                self.input.key(event.physical_key, event.state);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.input.mouse(button, state);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.input.cursor(position);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.input.wheel(delta);
            }
            WindowEvent::RedrawRequested => {
                if self.gpu.is_some() && self.scene.is_some() && self.camera.is_some() {
                    self.frame(event_loop);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.request_redraw();
        }
    }
}

fn create_depth(device: &Device, width: u32, height: u32) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: Some("client window depth"),
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
    })
}

/// Read one presented frame back and write it as a PNG.
fn capture_frame(
    device: &Device,
    queue: &Queue,
    frame: &SurfaceTexture,
    width: u32,
    height: u32,
    format: TextureFormat,
    path: &str,
) -> Result<(), String> {
    let bytes_per_pixel = 4;
    let padded_row = (width * bytes_per_pixel).next_multiple_of(256);
    let readback = device.create_buffer(&BufferDescriptor {
        label: Some("client window readback"),
        size: u64::from(padded_row) * u64::from(height),
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("client window readback"),
    });
    encoder.copy_texture_to_buffer(
        TexelCopyTextureInfo {
            texture: &frame.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        TexelCopyBufferInfo {
            buffer: &readback,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(height),
            },
        },
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(PollType::wait_indefinitely())
        .map_err(|error| format!("device poll failed: {error}"))?;
    let view = slice
        .get_mapped_range()
        .map_err(|error| format!("map failed: {error}"))?;
    let mut pixels = Vec::with_capacity((width * height * bytes_per_pixel) as usize);
    for row in 0..height {
        let start = (row * padded_row) as usize;
        let end = start + (width * bytes_per_pixel) as usize;
        pixels.extend_from_slice(&view[start..end]);
    }
    drop(view);
    readback.unmap();

    if matches!(
        format,
        TextureFormat::Bgra8UnormSrgb | TextureFormat::Bgra8Unorm
    ) {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    } else if !matches!(
        format,
        TextureFormat::Rgba8UnormSrgb | TextureFormat::Rgba8Unorm
    ) {
        return Err(format!("unsupported screenshot format {format:?}"));
    }
    crate::renderer::write_png(path, width, height, &pixels)
}

/// Open the interactive window and block until it closes.
///
/// Returns the run summary, or the first failure that stopped the loop.
pub fn run_window(config: WindowConfig) -> Result<WindowSummary, String> {
    let event_loop = EventLoop::new().map_err(|error| format!("event loop failed: {error}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = WindowApp::new(config);
    event_loop
        .run_app(&mut app)
        .map_err(|error| format!("event loop failed: {error}"))?;
    if let Some(failure) = app.failure {
        return Err(failure);
    }
    app.summary
        .ok_or_else(|| "window closed before the first frame".to_owned())
}
