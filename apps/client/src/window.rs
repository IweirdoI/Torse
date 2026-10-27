//! Interactive windowed client.
//!
//! One winit window driven by [`ApplicationHandler`]. Each frame pumps a fixed
//! logic step from measured time, streams and uploads on the presentation
//! frame, and draws through the shared [`GpuContext`]. The draw does not
//! advance logic. Sections outside the radius unload, so sustained flight
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
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::camera::{FlyCamera, MoveInput};
use crate::input::InputState;
use crate::pick::PickOutcome;
use crate::renderer::GpuContext;
use crate::scene::Scene;
use crate::texture::Palette;
use torse_sim::PresentationClock;
use torse_types::{BlockPos, ContentKey, SectionPos};
use torse_world::Position;

/// How far the pick ray reaches, in blocks.
const PICK_RANGE: f32 = 64.0;
/// Sections generated per frame while streaming; spreading the work keeps
/// the frame time bounded at the cost of populating a window over a few
/// frames.
const STREAM_BUDGET: usize = 4;
/// Fixed logic step in seconds. A presentation frame may run several of these
/// and must not stretch one step to match the frame.
const LOGIC_STEP_SECONDS: f64 = 1.0 / 20.0;
/// Most logic steps one presentation frame may catch up.
const MAX_LOGIC_STEPS: u32 = 8;

/// Autoplay fires one demonstration action every this many frames.
const AUTOPLAY_PERIOD: u32 = 24;
/// Camera eye height above the walker's body centre.
const WALKER_EYE_OFFSET: f64 = 0.7;

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
    /// Start in walk mode with a player entity on the ground.
    pub walk: bool,
    /// Connect to a standalone server and mirror its authorized projection.
    pub connect: Option<String>,
    /// Captured-look sensitivity multiplier; scroll steps multiply it in
    /// walk mode.
    pub sensitivity: f64,
    /// Send one edit request far outside the approved scope, for acceptance
    /// evidence that the server rejects out-of-scope targets.
    pub probe_outside: bool,
    /// Initial UI screen; machine mode is explicitly a sample-data preview.
    pub ui_mode: crate::ui::Mode,
    /// Data-driven content pack directory; `None` keeps the fixture content.
    pub content: Option<String>,
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
    /// Actions planned and committed through the action service.
    pub actions_ok: u32,
    /// Actions rejected by the action service or the commit.
    pub actions_rejected: u32,
    /// Walk ticks committed while in walk mode.
    pub walk_ticks: u32,
    /// Final walker position, when walk mode was entered.
    pub walker: Option<[f64; 3]>,
    /// Server session, when a connection was established.
    pub session: Option<u128>,
    /// Replica increments applied.
    pub batches: u32,
    /// Edit requests sent over the connection.
    pub requests_sent: u32,
    /// Requests the server rejected at admission.
    pub requests_rejected: u32,
    /// Sections received in the baseline.
    pub baseline_sections: usize,
    /// Resident sections at exit.
    pub sections: usize,
    /// Resident meshes at exit.
    pub meshes: usize,
    /// Final vertex and face counts.
    pub vertices: usize,
    pub faces: usize,
    /// Wall time from resume to exit.
    pub elapsed_ms: u128,
    /// Mean frame interval in microseconds.
    pub mean_frame_us: u64,
    /// Longest frame interval in microseconds.
    pub max_frame_us: u64,
    /// Mean frame time while streaming new sections.
    pub stream_frame_us: u64,
    /// Mean frame time containing mesh rebuilds.
    pub rebuild_frame_us: u64,
    pub cpu: crate::scene::BackgroundStats,
}

struct Gpu {
    ui: crate::ui::Layer,
    context: GpuContext,
    surface: Surface<'static>,
    config: SurfaceConfiguration,
    depth: Texture,
    depth_view: TextureView,
    window: Arc<Window>,
    backend_name: String,
}

struct WindowApp {
    ui: crate::ui::State,
    config: WindowConfig,
    scene: Option<Scene>,
    camera: Option<FlyCamera>,
    input: InputState,
    gpu: Option<Gpu>,
    materials: Vec<ContentKey>,
    material: usize,
    /// Whether the hotbar names come from a pack instead of the packaged icons.
    pack: bool,
    center: Option<SectionPos>,
    uploaded: bool,
    frames: u32,
    streams: usize,
    rebuilds: usize,
    uploads: usize,
    screenshots: usize,
    actions_ok: u32,
    actions_rejected: u32,
    autoplay_break: bool,
    net: Option<crate::net::Connection<torse_net::TcpTransport>>,
    session: Option<u128>,
    grabbed: bool,
    focused: bool,
    last_cursor: Option<(f64, f64)>,
    look_sensitivity: f64,
    recentring: bool,
    probe_sent: bool,
    batches: u32,
    requests_sent: u32,
    requests_rejected: u32,
    walk: bool,
    presentation: PresentationClock,
    walk_ticks: u32,
    /// Eye at the last completed logic step.
    present_prev: [f32; 3],
    /// Eye at the logic step the frame interpolates toward.
    present_next: [f32; 3],
    /// Autoplay sets this when the walker stopped against a terrain step.
    walk_blocked: bool,
    last_result: String,
    adapter: String,
    started: Option<Instant>,
    last_frame: Option<Instant>,
    fps_origin: Instant,
    fps_frames: u32,
    frame_total_us: u128,
    frame_max_us: u128,
    frame_count: u32,
    streams_mark: usize,
    stream_total_us: u128,
    stream_count: u32,
    rebuilds_mark: usize,
    rebuild_total_us: u128,
    rebuild_count: u32,
    pending_shot: Option<String>,
    /// Palette waiting for the next frame to be uploaded, set when a scene is
    /// installed before the GPU context exists or from a network baseline.
    pending_palette: Option<Palette>,
    summary: Option<WindowSummary>,
    failure: Option<String>,
}

impl WindowApp {
    fn new(config: WindowConfig) -> Self {
        let materials = ["base:stone", "base:glass_slab", "base:plant"]
            .into_iter()
            .map(|name| ContentKey::parse(name).expect("static material key"))
            .collect();
        Self {
            ui: {
                let mut state = crate::ui::State::default();
                state.set_mode(config.ui_mode);
                state
            },
            config,
            scene: None,
            camera: None,
            input: InputState::new(),
            gpu: None,
            materials,
            material: 0,
            pack: false,
            center: None,
            uploaded: false,
            frames: 0,
            streams: 0,
            rebuilds: 0,
            uploads: 0,
            screenshots: 0,
            actions_ok: 0,
            actions_rejected: 0,
            autoplay_break: false,
            net: None,
            session: None,
            grabbed: false,
            focused: true,
            last_cursor: None,
            look_sensitivity: 1.0,
            recentring: false,
            probe_sent: false,
            batches: 0,
            requests_sent: 0,
            requests_rejected: 0,
            walk: false,
            presentation: PresentationClock::new(LOGIC_STEP_SECONDS, MAX_LOGIC_STEPS)
                .expect("logic step and catch-up cap are positive"),
            walk_ticks: 0,
            present_prev: [0.0; 3],
            present_next: [0.0; 3],
            walk_blocked: false,
            last_result: String::new(),
            adapter: String::new(),
            started: None,
            last_frame: None,
            fps_origin: Instant::now(),
            fps_frames: 0,
            frame_total_us: 0,
            frame_max_us: 0,
            frame_count: 0,
            streams_mark: 0,
            stream_total_us: 0,
            stream_count: 0,
            rebuilds_mark: 0,
            rebuild_total_us: 0,
            rebuild_count: 0,
            pending_shot: None,
            pending_palette: None,
            summary: None,
            failure: None,
        }
    }

    fn poll_network(&mut self, event_loop: &ActiveEventLoop) -> bool {
        // Drain the connection first: the baseline creates the scene and
        // increments refresh it before this frame's meshes are captured.
        let events = match self.net.as_mut().map(crate::net::Connection::poll) {
            Some(Ok(events)) => events,
            Some(Err(error)) => {
                self.failure = Some(format!("connection failed: {error}"));
                event_loop.exit();
                return false;
            }
            None => Vec::new(),
        };
        for event in events {
            if let Err(error) = self.apply_net_event(event, event_loop) {
                self.failure = Some(error);
                event_loop.exit();
                return false;
            }
        }

        true
    }

    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        // The HUD needs the pack labels before the GPU borrow starts.
        let names = self.hotbar_names();
        let frame_started = Instant::now();
        if !self.poll_network(event_loop) {
            return;
        }

        if self.camera.is_none() {
            return;
        }
        if self.scene.is_none() {
            // Connected but no baseline yet: keep presenting cleared frames
            // so polling continues and the window does not sit on an
            // uninitialized surface.
            if let Some(gpu) = self.gpu.as_mut() {
                match gpu.surface.get_current_texture() {
                    CurrentSurfaceTexture::Success(frame)
                    | CurrentSurfaceTexture::Suboptimal(frame) => {
                        let view = frame.texture.create_view(&TextureViewDescriptor::default());
                        let (device, queue) = gpu.context.parts();
                        let mut encoder =
                            device.create_command_encoder(&CommandEncoderDescriptor {
                                label: Some("client idle frame"),
                            });
                        gpu.context
                            .encode_draw(&mut encoder, &view, &gpu.depth_view);
                        gpu.ui.prepare(
                            device,
                            queue,
                            [gpu.config.width, gpu.config.height],
                            &self.ui,
                            crate::ui::Hud {
                                material: self.material,
                                network: self.net.is_some(),
                                ready: false,
                                result: &self.last_result,
                                names: names.clone(),
                                sensitivity: self.look_sensitivity,
                            },
                        );
                        gpu.ui.draw(&mut encoder, &view);
                        queue.submit([encoder.finish()]);
                        queue.present(frame);
                    }
                    _ => {}
                }
                gpu.window.request_redraw();
            }
            return;
        }
        let dt = self.frame_interval(frame_started);
        let speed_now = self.camera.as_ref().expect("camera").speed;
        let (mut motion, speed) = self.input.take_input(speed_now);
        self.camera.as_mut().expect("camera").speed = speed;
        if self.config.autoplay && !self.ui.modal() {
            motion.forward = 1.0;
        }
        if let Err(reason) = self.pump_logic(dt, motion) {
            self.failure = Some(reason);
            event_loop.exit();
            return;
        }

        let center = self
            .scene
            .as_ref()
            .expect("camera implies scene")
            .center_section(self.camera.as_ref().expect("camera").eye);
        // Local scenes stream continuously with a per-frame generation
        // budget: a crossing no longer stalls one frame with a whole ring of
        // fresh sections. Network scenes keep the subscribed sections.
        if self.scene.as_ref().is_some_and(|scene| !scene.is_network()) {
            let Some(scene) = self.scene.as_mut() else {
                return;
            };
            if let Err(error) = scene.stream_around(center, STREAM_BUDGET) {
                self.failure = Some(format!("streaming failed: {error}"));
                event_loop.exit();
                return;
            }
            if self.center != Some(center) {
                self.center = Some(center);
                self.streams += 1;
            }
        } else {
            self.center = None;
        }

        // Resolve the aim from the fresh camera, translate clicks and the
        // autoplay demo into place or break intents, and commit them before
        // this frame's meshes are captured.
        let aim = {
            let Some(scene) = self.scene.as_ref() else {
                return;
            };
            let Some(camera) = self.camera.as_ref() else {
                return;
            };
            scene.pick(camera.eye, camera.forward(), PICK_RANGE)
        };
        let mut intent = self
            .input
            .take_click()
            .and_then(|button| action_for(&aim, button));
        // The walk demo is about walking; firing place or break at the aim
        // while walking would block the walker's own path.
        let network = self.scene.as_ref().is_some_and(Scene::is_network);
        if network
            && self.config.probe_outside
            && !self.probe_sent
            && self.frames > 12
            && let Some(net) = self.net.as_mut()
        {
            // One deliberate out-of-scope target; the server should reject
            // it and neither world nor mirror changes.
            self.probe_sent = true;
            let call = crate::net::edit_call(
                false,
                &self.materials[self.material],
                BlockPos::new(200, 1, 1),
                0,
            );
            match net.request(call) {
                Ok(seq) => {
                    self.requests_sent += 1;
                    self.last_result = format!("outside probe seq {} sent", seq.0);
                }
                Err(error) => self.last_result = format!("probe failed: {error}"),
            }
        }
        if network
            && self.config.autoplay
            && !self.ui.modal()
            && self.frames.is_multiple_of(AUTOPLAY_PERIOD)
        {
            // Over a connection the demo exercises the request path: the
            // guest session should see its edit rejected by admission.
            let aim = {
                let Some(scene) = self.scene.as_ref() else {
                    return;
                };
                let Some(camera) = self.camera.as_ref() else {
                    return;
                };
                scene.pick(camera.eye, camera.forward(), PICK_RANGE)
            };
            if let Some((place, target)) = action_for(&aim, MouseButton::Left) {
                let material = &self.materials[self.material];
                let half = place_half(&aim);
                let call = crate::net::edit_call(place, material, target, half);
                if let Some(Ok(seq)) = self.net.as_mut().map(|net| net.request(call)) {
                    self.requests_sent += 1;
                    self.last_result = format!("seq {} sent", seq.0);
                }
            }
        }
        if intent.is_none()
            && self.config.autoplay
            && !self.ui.modal()
            && !self.walk
            && !network
            && self.frames.is_multiple_of(AUTOPLAY_PERIOD)
        {
            let button = if self.autoplay_break {
                MouseButton::Left
            } else {
                MouseButton::Right
            };
            intent = action_for(&aim, button);
            self.autoplay_break = !self.autoplay_break;
        }
        // Aiming at the exposed top of a half block places into the same
        // cell, where the action service merges the two halves into one
        // full block.
        if let Some((true, target)) = intent.as_mut()
            && let Some(merged) = merged_target(self.scene.as_ref(), &aim)
        {
            *target = merged;
        }
        if let Some((place, target)) = intent {
            if self.scene.as_ref().is_some_and(Scene::is_network) {
                // Edits go to the server, not the local mirror. The guest
                // session has no edit grants, so the admission reply carries
                // the rejection; the mirror never forks.
                let material = &self.materials[self.material];
                let half = place_half(&aim);
                let call = crate::net::edit_call(place, material, target, half);
                match self.net.as_mut().map(|net| net.request(call)) {
                    Some(Ok(seq)) => self.last_result = format!("seq {} sent", seq.0),
                    Some(Err(error)) => self.last_result = format!("send failed: {error}"),
                    None => self.last_result = "offline".to_owned(),
                }
            } else {
                let material = &self.materials[self.material];
                let half = place_half(&aim);
                let result = match self.scene.as_mut() {
                    Some(scene) => scene.apply_edit(place, material, target, half),
                    None => return,
                };
                match result {
                    Ok(()) => {
                        self.actions_ok += 1;
                        self.last_result.clear();
                    }
                    Err(reason) => {
                        self.actions_rejected += 1;
                        self.last_result = reason;
                    }
                }
            }
        }

        // Stage the selection outline from the post-edit aim so it matches
        // what the next frame will show.
        let outline = {
            let Some(scene) = self.scene.as_ref() else {
                return;
            };
            let Some(camera) = self.camera.as_ref() else {
                return;
            };
            match scene.pick(camera.eye, camera.forward(), PICK_RANGE) {
                PickOutcome::Hit(hit) => scene.outline_bytes(&hit),
                PickOutcome::Miss => Vec::new(),
            }
        };
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.context.write_lines(&outline);
        }

        let rebuilt = match self
            .scene
            .as_mut()
            .expect("camera implies scene")
            .refresh_visuals(STREAM_BUDGET)
        {
            Ok(rebuilt) => rebuilt,
            Err(error) => {
                self.failure = Some(error);
                event_loop.exit();
                return;
            }
        };
        self.rebuilds += rebuilt.len();
        if !rebuilt.is_empty() || !self.uploaded {
            let scene = self.scene.as_ref().expect("camera implies scene");
            if let Some(gpu) = self.gpu.as_mut() {
                // Re-upload only the changed sections; untouched terrain keeps
                // its buffers, so an edit costs one small upload, not the
                // whole scene.
                for pos in &rebuilt {
                    if let Some((vertices, indices)) = scene.section_geometry(*pos) {
                        let bytes = Scene::vertex_bytes(&vertices);
                        gpu.context.upload_section(*pos, &bytes, &indices);
                    }
                }
                // Entity bodies change without mesh rebuilds; refresh them
                // alongside any terrain upload.
                let (entity_vertices, entity_indices) = scene.entity_geometry();
                let entity_bytes = Scene::vertex_bytes(&entity_vertices);
                gpu.context.upload_section(
                    crate::renderer::ENTITY_SLOT,
                    &entity_bytes,
                    &entity_indices,
                );
            }
            self.uploaded = true;
            self.uploads += rebuilt.len();
        }
        // Unloaded sections must stop drawing even when nothing rebuilt this
        // frame; retention is cheap and the entity slot survives it.
        if let Some(gpu) = self.gpu.as_mut()
            && let Some(scene) = self.scene.as_ref()
        {
            gpu.context.retain_sections(scene.sections());
        }
        // Belt and braces: some focus transitions skip the Focused event, and
        // a captured look with no focus moves the camera from outside.
        if self.grabbed
            && let Some(gpu) = self.gpu.as_ref()
            && !gpu.window.has_focus()
        {
            self.focused = false;
            self.input.reset();
            self.set_grab(false);
        }

        if self.input.take_screenshot() {
            self.pending_shot = Some(format!("torse-shot-{}.png", self.screenshots + 1));
        }

        self.draw(event_loop, &names, frame_started);
    }

    /// Present one frame. This does not pump logic steps.
    fn draw(&mut self, event_loop: &ActiveEventLoop, names: &[String], frame_started: Instant) {
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

        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        if let Some(palette) = self.pending_palette.take() {
            gpu.context.set_palette(&palette);
        }
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let aspect = gpu.config.width as f32 / gpu.config.height.max(1) as f32;
        let (device, queue) = gpu.context.parts();
        let view_projection = self
            .camera
            .as_ref()
            .expect("gpu implies camera")
            .view_projection(aspect);
        let eye = self.camera.as_ref().expect("gpu implies camera").eye;
        gpu.context.write_camera(view_projection, eye);
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("client window frame"),
        });
        gpu.context
            .encode_draw(&mut encoder, &view, &gpu.depth_view);
        gpu.ui.prepare(
            device,
            queue,
            [gpu.config.width, gpu.config.height],
            &self.ui,
            crate::ui::Hud {
                material: self.material,
                network: self.net.is_some(),
                ready: self.scene.is_some(),
                result: &self.last_result,
                names: names.to_vec(),
                sensitivity: self.look_sensitivity,
            },
        );
        gpu.ui.draw(&mut encoder, &view);
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

        let frame_us = frame_started.elapsed().as_micros();
        self.frame_total_us += frame_us;
        self.frame_max_us = self.frame_max_us.max(frame_us);
        self.frame_count += 1;
        if self.streams_mark != self.streams {
            self.streams_mark = self.streams;
            self.stream_total_us += frame_us;
            self.stream_count += 1;
        }
        if self.rebuilds_mark != self.rebuilds {
            self.rebuilds_mark = self.rebuilds;
            self.rebuild_total_us += frame_us;
            self.rebuild_count += 1;
        }

        self.frames += 1;
        self.fps_frames += 1;
        let title_span = self.fps_origin.elapsed().as_secs_f32();
        if title_span >= 0.5 {
            let fps = self.fps_frames as f32 / title_span;
            if let Some(scene) = self.scene.as_ref() {
                let material = self.materials[self.material].path();
                let status = if self.last_result.is_empty() {
                    "ok".to_owned()
                } else {
                    self.last_result.clone()
                };
                gpu.window.set_title(&format!(
                    "Torse client | {fps:.0} fps | sections {} | meshes {} | {material} | {status}",
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

    /// Run the fixed logic steps owed to `dt` seconds.
    ///
    /// Streaming and drawing stay outside this call. Modal frame intervals
    /// are discarded while the existing interpolation phase stays frozen.
    fn pump_logic(&mut self, dt: f64, motion: MoveInput) -> Result<(), String> {
        if self.ui.modal() {
            return Ok(());
        }
        let steps = self.presentation.pump_logic(dt);
        if self.walk {
            self.pump_walk(steps, motion)?;
        } else {
            self.pump_fly(steps, motion);
        }
        self.apply_presented_eye();
        Ok(())
    }

    /// Measure real elapsed time; the presentation clock alone caps catch-up.
    fn frame_interval(&mut self, now: Instant) -> f64 {
        self.last_frame
            .replace(now)
            .map_or(0.0, |last| now.duration_since(last).as_secs_f64())
    }

    fn set_ui_mode(&mut self, mode: crate::ui::Mode) {
        self.ui.set_mode(mode);
        // A transition can happen with no redraws during a long pause.
        self.last_frame = None;
    }

    /// Fraction of the current logic step, for read-only pose interpolation.
    fn presentation_alpha(&self) -> f32 {
        self.presentation.presentation_alpha() as f32
    }

    /// Snap the presented eye and drop the interpolation phase.
    ///
    /// Movement correction from the authority is not settled here. Callers use
    /// this when an external pose replaces the one the logic steps produced.
    fn reset_presentation_pose(&mut self, eye: [f32; 3]) {
        self.presentation.reset();
        self.last_frame = None;
        self.walk_blocked = false;
        self.present_prev = eye;
        self.present_next = eye;
        if let Some(camera) = self.camera.as_mut() {
            camera.eye = eye;
        }
    }

    fn apply_presented_eye(&mut self) {
        let alpha = self.presentation_alpha();
        let Some(camera) = self.camera.as_mut() else {
            return;
        };
        for axis in 0..3 {
            camera.eye[axis] = self.present_prev[axis]
                + (self.present_next[axis] - self.present_prev[axis]) * alpha;
        }
    }

    fn pump_walk(&mut self, steps: u32, motion: MoveInput) -> Result<(), String> {
        if let Some(camera) = self.camera.as_mut() {
            camera.look(motion.yaw_delta, motion.pitch_delta);
        }
        let yaw = f64::from(self.camera.as_ref().map(|camera| camera.yaw).unwrap_or(0.0));
        let forward = [-yaw.sin(), 0.0, -yaw.cos()];
        let right = [yaw.cos(), 0.0, -yaw.sin()];
        let mut wish = [0.0f64; 3];
        for axis in 0..3 {
            wish[axis] =
                forward[axis] * f64::from(motion.forward) + right[axis] * f64::from(motion.strafe);
        }
        let length = (wish[0] * wish[0] + wish[1] * wish[1] + wish[2] * wish[2]).sqrt();
        if length > 0.0 {
            for value in &mut wish {
                *value /= length;
            }
        }
        let step = LOGIC_STEP_SECONDS as f32;
        for _ in 0..steps {
            let jump = self.input.jump_held() || (self.config.autoplay && self.walk_blocked);
            let before = (
                f64::from(self.present_next[0]),
                f64::from(self.present_next[2]),
            );
            let scene = self.scene.as_mut().ok_or("walk mode has no scene")?;
            match scene.step_walker(wish, jump, step) {
                Ok((position, _)) => {
                    let wanted = wish[0].abs() + wish[2].abs() > 1e-9;
                    let drifted =
                        (position.x - before.0).abs() + (position.z - before.1).abs() > 0.02;
                    self.walk_blocked = wanted && !drifted;
                    self.walk_ticks += 1;
                    self.present_prev = self.present_next;
                    self.present_next = [
                        position.x as f32,
                        (position.y + WALKER_EYE_OFFSET) as f32,
                        position.z as f32,
                    ];
                }
                Err(reason) => return Err(reason),
            }
        }
        Ok(())
    }

    fn pump_fly(&mut self, steps: u32, mut motion: MoveInput) {
        let yaw_delta = motion.yaw_delta;
        let pitch_delta = motion.pitch_delta;
        motion.yaw_delta = 0.0;
        motion.pitch_delta = 0.0;
        let Some(camera) = self.camera.as_mut() else {
            return;
        };
        camera.look(yaw_delta, pitch_delta);
        let step = LOGIC_STEP_SECONDS as f32;
        for _ in 0..steps {
            camera.eye = self.present_next;
            camera.advance(motion, step);
            self.present_prev = self.present_next;
            self.present_next = camera.eye;
        }
    }

    /// Apply one decoded server event; errors stop the run.
    fn apply_net_event(
        &mut self,
        event: crate::net::NetEvent,
        _event_loop: &ActiveEventLoop,
    ) -> Result<(), String> {
        use crate::net::NetEvent;
        match event {
            NetEvent::Welcomed { session } => {
                self.session = Some(session.0);
            }
            NetEvent::Baseline(snapshot) => {
                // The mirror installs the y = 0 section layer; the eye must
                // stay inside it or the pick ray stops at the first unloaded
                // cell above the terrain.
                let loaded_top = (snapshot.side as i64).min(snapshot.bounds.max_y);
                let mut scene = Scene::from_snapshot(&snapshot)?;
                scene.enable_background(crate::scene::background_config())?;
                let eye = [4.0, (loaded_top as f32 - 2.0).max(4.0), 4.0];
                if let Some(camera) = self.camera.as_mut() {
                    camera.eye = eye;
                    // Look nearly straight down: the scope is 32 by 16 blocks,
                    // so a shallow view drifts out of the loaded area before
                    // the ray reaches the floor.
                    camera.pitch = -1.5;
                    camera.yaw = 0.6;
                }
                self.reset_presentation_pose(eye);
                self.scene = Some(scene);
                self.pending_palette = self.scene.as_ref().map(|scene| scene.palette().clone());
                self.center = None;
                self.uploaded = false;
            }
            NetEvent::Increment(changes) => {
                self.batches += 1;
                let Some(scene) = self.scene.as_mut() else {
                    return Err("increment before baseline".to_owned());
                };
                scene.apply_changes(&changes)?;
            }
            NetEvent::Admitted { seq, rejected } => {
                if rejected.is_some() {
                    self.requests_rejected += 1;
                }
                self.last_result = match rejected {
                    Some(code) => format!("seq {} rejected {code:?}", seq.0),
                    None => format!("seq {} admitted", seq.0),
                };
            }
            NetEvent::Finished { seq, committed } => {
                if !committed {
                    self.requests_rejected += 1;
                }
                self.last_result = format!(
                    "seq {} {}",
                    seq.0,
                    if committed { "committed" } else { "denied" }
                );
            }
            NetEvent::Closed(reason) => return Err(reason),
        }
        Ok(())
    }

    /// Capture or release the mouse.
    ///
    /// Captured means hidden and re-centred after every motion event: the
    /// deltas drive the camera directly, and any modal needs the real cursor
    /// back. winit's own grab is deliberately not used on Windows because
    /// both of its modes clip the cursor to a one-pixel rect there, which
    /// freezes the reported positions and with them the camera.
    /// Hotbar labels for a pack scene; an empty list keeps the packaged icons.
    fn hotbar_names(&self) -> Vec<String> {
        if !self.pack {
            return Vec::new();
        }
        self.materials
            .iter()
            .take(3)
            .map(|key| key.path().to_owned())
            .collect()
    }

    fn set_grab(&mut self, grab: bool) {
        // Never capture while the window is unfocused; the next focus event
        // or the per-frame check re-arms capture.
        let grab = grab && self.focused;
        self.grabbed = grab;
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.set_cursor_visible(!grab);
            if grab {
                let center = winit::dpi::Position::Physical(winit::dpi::PhysicalPosition::new(
                    gpu.config.width as i32 / 2,
                    gpu.config.height as i32 / 2,
                ));
                let _ = gpu.window.set_cursor_position(center);
                // Deltas are measured from this baseline until real motion
                // or the drift allowance moves the cursor. The first event
                // after the programmatic jump is synthetic and skipped.
                self.last_cursor = Some((
                    f64::from(gpu.config.width / 2),
                    f64::from(gpu.config.height / 2),
                ));
                self.recentring = true;
            } else {
                self.last_cursor = None;
            }
        }
    }

    /// Switch to walk mode by spawning the player entity under the camera.
    ///
    /// `Scene::eye` sits 1.4 blocks above the surface cell's top, and the
    /// walker's centre must stand 0.9 above that top for the collider to
    /// clear the ground, hence the 0.5 drop. Movement preparation rejects any
    /// entity that already overlaps terrain, so a low spawn point freezes the
    /// walker until it is toggled again from higher up.
    fn enter_walk(&mut self, eye: [f32; 3]) {
        let under = Position::new(
            f64::from(eye[0]),
            f64::from(eye[1]) - 0.5,
            f64::from(eye[2]),
        );
        match self.scene.as_mut() {
            Some(scene) => {
                if let Err(reason) = scene.spawn_walker(under) {
                    self.last_result = reason;
                    return;
                }
            }
            None => return,
        }
        self.walk = true;
        // Seed the interpolation endpoints with the spawn position so the
        // first frames do not glide in from the origin.
        let start = [
            under.x as f32,
            (under.y + WALKER_EYE_OFFSET) as f32,
            under.z as f32,
        ];
        self.reset_presentation_pose(start);
    }

    /// F5 flips between fly and walk; the walker entity stays in the world.
    fn toggle_walk(&mut self) {
        if self.walk {
            self.walk = false;
            if let Some(eye) = self.camera.as_ref().map(|camera| camera.eye) {
                self.reset_presentation_pose(eye);
            }
            return;
        }
        if let Some(camera) = self.camera.as_ref() {
            self.enter_walk(camera.eye);
        }
    }

    fn finish(&mut self, event_loop: &ActiveEventLoop) {
        if let Some((scene, gpu)) = self.scene.as_ref().zip(self.gpu.as_ref()) {
            let (vertices, _, faces) = scene.geometry();
            self.summary = Some(WindowSummary {
                cpu: scene.background_stats(),
                adapter: self.adapter.clone(),
                backend: gpu.backend_name.clone(),
                format: format!("{:?}", gpu.config.format),
                frames: self.frames,
                streams: self.streams,
                rebuilds: self.rebuilds,
                uploads: self.uploads,
                screenshots: self.screenshots,
                actions_ok: self.actions_ok,
                actions_rejected: self.actions_rejected,
                session: self.session,
                batches: self.batches,
                requests_sent: self.requests_sent,
                requests_rejected: self.requests_rejected,
                baseline_sections: self
                    .scene
                    .as_ref()
                    .map_or(0, |scene| scene.sections().len()),
                walk_ticks: self.walk_ticks,
                walker: self
                    .scene
                    .as_ref()
                    .and_then(Scene::walker_position)
                    .map(|position| [position.x, position.y, position.z]),
                sections: scene.sections().len(),
                meshes: scene.mesh_count(),
                vertices: vertices.len(),
                faces,
                elapsed_ms: self
                    .started
                    .map(|start| start.elapsed().as_millis())
                    .unwrap_or_default(),
                mean_frame_us: if self.frame_count == 0 {
                    0
                } else {
                    u64::try_from(self.frame_total_us / u128::from(self.frame_count)).unwrap_or(0)
                },
                max_frame_us: u64::try_from(self.frame_max_us).unwrap_or(0),
                stream_frame_us: if self.stream_count == 0 {
                    0
                } else {
                    u64::try_from(self.stream_total_us / u128::from(self.stream_count)).unwrap_or(0)
                },
                rebuild_frame_us: if self.rebuild_count == 0 {
                    0
                } else {
                    u64::try_from(self.rebuild_total_us / u128::from(self.rebuild_count))
                        .unwrap_or(0)
                },
            });
        }
        event_loop.exit();
    }
}

impl ApplicationHandler for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes()
            .with_title("Torse client")
            .with_min_inner_size(LogicalSize::new(640, 360))
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

        let scene = if self.config.connect.is_some() {
            // Network scenes arrive with the first baseline; the window stays
            // empty until then.
            None
        } else {
            // A pack scene generates on the foreground thread; the built-in
            // fixture scene keeps the bounded background pool.
            let built = match self.config.content.as_deref() {
                Some(directory) => {
                    Scene::new_game(self.config.radius, std::path::Path::new(directory))
                }
                None => Scene::new_background(self.config.radius),
            };
            match built {
                Ok(scene) => Some(scene),
                Err(error) => {
                    self.failure = Some(error);
                    event_loop.exit();
                    return;
                }
            }
        };
        let mut camera = FlyCamera::new([8.0, 26.0, 8.0]);
        if let Some(scene) = scene.as_ref() {
            camera = FlyCamera::new(scene.eye());
            if self.config.content.is_some() {
                // A pack scene is a landscape overview: stand above the spawn
                // column and look across the terrain instead of into it.
                camera.eye[1] = (camera.eye[1] + 12.0).min(56.0);
                camera.eye[2] = (camera.eye[2] - 26.0).max(2.0);
                camera.pitch = -0.28;
                camera.yaw = 0.0;
            } else {
                // Only the y = 0 section layer is loaded, and picking terminates
                // at the first unloaded cell. Keep the starting eye inside that
                // layer so the aim works no matter how high the spawn surface is.
                camera.eye[1] = camera.eye[1].min(14.0);
            }
        } else {
            // Overlook the subscribed scope until the baseline lands.
            camera.pitch = -0.7;
            camera.yaw = 0.6;
        }
        if let Some(address) = self.config.connect.clone() {
            let compatibility = match Scene::compatibility() {
                Ok(value) => value,
                Err(error) => {
                    self.failure = Some(error);
                    event_loop.exit();
                    return;
                }
            };
            let mut connection: crate::net::Connection<torse_net::TcpTransport> =
                match crate::net::Connection::connect(&address) {
                    Ok(connection) => connection,
                    Err(error) => {
                        self.failure = Some(format!("connect failed: {error}"));
                        event_loop.exit();
                        return;
                    }
                };
            let handshake = connection
                .hello(compatibility)
                .and_then(|()| connection.subscribe(demo_scope().to_vec()));
            if let Err(error) = handshake {
                self.failure = Some(format!("handshake failed: {error}"));
                event_loop.exit();
                return;
            }
            self.net = Some(connection);
        }
        self.adapter = info.name;
        let ui = {
            let (device, _) = context.parts();
            match crate::ui::Layer::new(device, config.format) {
                Ok(ui) => ui,
                Err(error) => {
                    self.failure = Some(error);
                    event_loop.exit();
                    return;
                }
            }
        };
        self.gpu = Some(Gpu {
            ui,
            backend_name: format!("{:?}", info.backend),
            context,
            surface,
            config,
            depth,
            depth_view,
            window,
        });
        self.scene = scene;
        if let Some(keys) = self.scene.as_ref().and_then(Scene::placeable_blocks) {
            self.materials = keys.iter().take(3).cloned().collect();
            self.material = 0;
            self.pack = true;
        }
        self.pending_palette = self.scene.as_ref().map(|scene| scene.palette().clone());
        self.present_prev = camera.eye;
        self.present_next = camera.eye;
        self.camera = Some(camera);
        // Enter walk mode only after the scene exists; enter_walk spawns the
        // walker through it.
        if self.config.walk {
            let eye = self.camera.as_ref().expect("camera was just installed").eye;
            self.enter_walk(eye);
        }
        self.look_sensitivity = self.config.sensitivity;
        self.started = Some(Instant::now());
        self.fps_origin = Instant::now();
        // Capture the mouse right away; Esc opens the pause menu with the
        // real cursor back.
        self.set_grab(true);
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
                use winit::keyboard::KeyCode;
                if event.state == ElementState::Pressed
                    && let winit::keyboard::PhysicalKey::Code(code) = event.physical_key
                {
                    match code {
                        KeyCode::Escape if !event.repeat => {
                            let mode = if self.ui.modal() {
                                crate::ui::Mode::Game
                            } else {
                                crate::ui::Mode::Pause
                            };
                            self.set_ui_mode(mode);
                            self.input.reset();
                            self.set_grab(!self.ui.modal());
                            return;
                        }
                        KeyCode::KeyE if !event.repeat => {
                            let mode = if self.ui.modal() {
                                crate::ui::Mode::Game
                            } else {
                                crate::ui::Mode::MachinePreview
                            };
                            self.set_ui_mode(mode);
                            self.input.reset();
                            self.set_grab(!self.ui.modal());
                            return;
                        }
                        KeyCode::F12 => {
                            self.input.key(event.physical_key, event.state);
                            return;
                        }
                        _ if self.ui.modal() => return,
                        KeyCode::Digit1 => self.material = 0,
                        KeyCode::Digit2 => self.material = 1,
                        KeyCode::Digit3 => self.material = 2,
                        KeyCode::F5 => self.toggle_walk(),
                        _ => {}
                    }
                }
                if !self.ui.modal() {
                    self.input.key(event.physical_key, event.state);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if self.ui.modal() {
                    if button == MouseButton::Left {
                        let size = self
                            .gpu
                            .as_ref()
                            .map_or([960, 540], |g| [g.config.width, g.config.height]);
                        if let Some(action) = self.ui.pointer(state == ElementState::Pressed, size)
                        {
                            match action {
                                crate::ui::Action::Quit => self.finish(event_loop),
                                crate::ui::Action::Resume => {
                                    self.set_ui_mode(crate::ui::Mode::Game)
                                }
                                crate::ui::Action::Preview => {
                                    self.set_ui_mode(crate::ui::Mode::MachinePreview)
                                }
                                crate::ui::Action::SensDown => {
                                    self.look_sensitivity =
                                        (self.look_sensitivity - 0.1).clamp(0.2, 5.0);
                                    self.last_result = format!("sens {:.2}", self.look_sensitivity);
                                }
                                crate::ui::Action::SensUp => {
                                    self.look_sensitivity =
                                        (self.look_sensitivity + 0.1).clamp(0.2, 5.0);
                                    self.last_result = format!("sens {:.2}", self.look_sensitivity);
                                }
                            }
                            self.input.reset();
                            self.set_grab(!self.ui.modal());
                        }
                    }
                } else if self.grabbed {
                    // Captured mouse: a press is a click, motion is look.
                    if state == ElementState::Pressed {
                        self.input.queue_click(button);
                    }
                } else {
                    self.input.mouse(button, state);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.ui.cursor = [position.x, position.y];
                if self.grabbed && self.focused && !self.ui.modal() {
                    if let Some(gpu) = self.gpu.as_ref() {
                        let center = (
                            f64::from(gpu.config.width / 2),
                            f64::from(gpu.config.height / 2),
                        );
                        // Deltas come from the previous delivered position:
                        // per-event re-centring would feed rounding jitter
                        // into the camera every event.
                        let (last_x, last_y) = self.last_cursor.unwrap_or(center);
                        // The OS cursor lands a pixel off after re-centring;
                        // that synthetic jump must not reach the camera.
                        let recentred = self.recentring;
                        self.recentring = false;
                        let dx = position.x - last_x;
                        let dy = position.y - last_y;
                        if !recentred && dx.abs() + dy.abs() > 0.5 {
                            self.input
                                .mouse_look(dx * self.look_sensitivity, dy * self.look_sensitivity);
                        }
                        // Recentre only on real drift; the hidden cursor may
                        // wander a little without anyone noticing. The next
                        // CursorMoved after a recentre is the synthetic jump,
                        // so it is skipped above.
                        if (position.x - center.0).abs() > 24.0
                            || (position.y - center.1).abs() > 24.0
                        {
                            let point =
                                winit::dpi::Position::Physical(winit::dpi::PhysicalPosition::new(
                                    gpu.config.width as i32 / 2,
                                    gpu.config.height as i32 / 2,
                                ));
                            let _ = gpu.window.set_cursor_position(point);
                            self.last_cursor = Some(center);
                            self.recentring = true;
                        } else {
                            self.last_cursor = Some((position.x, position.y));
                        }
                    }
                } else if !self.ui.modal() {
                    self.input.cursor(position);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.ui.modal() && self.walk {
                    // Fly speed is meaningless on foot; the wheel tunes the
                    // look sensitivity instead.
                    let notches = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, lines) => f64::from(lines),
                        winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 48.0,
                    };
                    if notches.is_finite() && notches != 0.0 {
                        self.look_sensitivity = (self.look_sensitivity
                            * 1.08f64.powf(notches.clamp(-8.0, 8.0)))
                        .clamp(0.2, 5.0);
                        self.last_result = format!("sens {:.2}", self.look_sensitivity);
                    }
                } else if !self.ui.modal() {
                    self.input.wheel(delta);
                }
            }
            WindowEvent::Focused(false) => {
                self.focused = false;
                self.input.reset();
                self.set_grab(false);
            }
            WindowEvent::Focused(true) => {
                self.focused = true;
                if !self.ui.modal() {
                    self.set_grab(true);
                }
            }
            WindowEvent::RedrawRequested => {
                if self.gpu.is_some() && self.camera.is_some() {
                    self.frame(event_loop);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.poll_network(event_loop) {
            return;
        }
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.request_redraw();
        }
    }
}

/// The demo server's host-approved scope; subscriptions must be a subset.
const fn demo_scope() -> [SectionPos; 2] {
    [SectionPos::new(0, 0, 0), SectionPos::new(1, 0, 0)]
}

/// Translate one aim and mouse button into a place or break intent.
///
/// Left breaks the hit cell; right places the selected material against the
/// entered face. A hit inside a solid cell carries no face, so placement is
/// dropped rather than guessed.
/// The half a placement produces, decided by the aimed face: a click on a
/// ceiling underside places the upper half, a click on a floor top the lower
/// half, and a side click follows the aim point's height within the targeted
/// cell.
fn place_half(aim: &PickOutcome) -> u32 {
    let PickOutcome::Hit(hit) = aim else {
        return 0;
    };
    match hit.face {
        [0, -1, 0] => 1,
        [0, 1, 0] => 0,
        _ => {
            let fraction = hit.entry[1] - hit.block.y as f32;
            u32::from(fraction > 0.5)
        }
    }
}

fn action_for(outcome: &PickOutcome, button: MouseButton) -> Option<(bool, BlockPos)> {
    let PickOutcome::Hit(hit) = outcome else {
        return None;
    };
    match button {
        MouseButton::Left => Some((false, hit.block)),
        MouseButton::Right => {
            let (face_x, face_y, face_z) = (hit.face[0], hit.face[1], hit.face[2]);
            if (face_x, face_y, face_z) == (0, 0, 0) {
                return None;
            }
            Some((
                true,
                BlockPos::new(
                    hit.block.x + i64::from(face_x),
                    hit.block.y + i64::from(face_y),
                    hit.block.z + i64::from(face_z),
                ),
            ))
        }
        _ => None,
    }
}

/// Placement aimed at the exposed top of a completable half block targets
/// the same cell instead of the cell above; the action service decides
/// whether the pair merges into one full block.
fn merged_target(scene: Option<&Scene>, outcome: &PickOutcome) -> Option<BlockPos> {
    let PickOutcome::Hit(hit) = outcome else {
        return None;
    };
    (hit.face == [0, 1, 0] && scene.is_some_and(|scene| scene.completes_at(hit.block)))
        .then_some(hit.block)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn app() -> WindowApp {
        let mut app = WindowApp::new(WindowConfig {
            radius: 1,
            width: 960,
            height: 540,
            frames: None,
            screenshot: None,
            autoplay: false,
            walk: false,
            connect: None,
            sensitivity: 1.0,
            probe_outside: false,
            ui_mode: crate::ui::Mode::Game,
            content: None,
        });
        app.camera = Some(FlyCamera::new([8.5, 12.7, 8.5]));
        app.reset_presentation_pose([8.5, 12.7, 8.5]);
        app
    }

    fn forward() -> MoveInput {
        MoveInput {
            forward: 1.0,
            ..MoveInput::default()
        }
    }

    #[test]
    fn frame_intervals_use_wall_time_without_fabrication_or_clamping() {
        let mut app = app();
        let now = Instant::now();
        assert_eq!(app.frame_interval(now), 0.0);
        assert_eq!(app.frame_interval(now), 0.0);
        assert_eq!(app.frame_interval(now + Duration::from_micros(10)), 0.00001);
        let dt = app.frame_interval(now + Duration::from_secs(1) + Duration::from_micros(10));
        assert_eq!(dt, 1.0);
        app.pump_logic(dt, forward()).expect("fly");
        // The single catch-up cap allows eight 50 ms steps after a hitch.
        assert!((app.present_next[2] - (8.5 - 8.0 * 12.0 * 0.05)).abs() < 1e-5);
        assert!(app.presentation_alpha() < 1e-6);
    }

    #[test]
    fn fly_pose_and_look_are_independent_of_frame_rate() {
        for fps in [30, 60, 144, 1000] {
            let mut app = app();
            for frame in 0..fps {
                let motion = MoveInput {
                    yaw_delta: if frame == 0 { 0.5 } else { 0.0 },
                    ..forward()
                };
                app.pump_logic(1.0 / f64::from(fps), motion).expect("fly");
            }
            let camera = app.camera.expect("camera");
            assert_eq!(camera.yaw, 0.5);
            assert!((app.present_next[0] - (8.5 - 12.0 * 0.5f32.sin())).abs() < 1e-4);
            assert!((app.present_next[2] - (8.5 - 12.0 * 0.5f32.cos())).abs() < 1e-4);
            assert!(app.presentation_alpha() < 1e-6);
        }
    }

    #[test]
    fn pause_freezes_the_pose_and_resumes_without_replaying_wall_time() {
        let mut app = app();
        app.pump_logic(0.075, forward()).expect("fly");
        let eye = app.camera.expect("camera").eye;
        let target = app.present_next;
        let alpha = app.presentation_alpha();
        app.set_ui_mode(crate::ui::Mode::Pause);
        app.pump_logic(10.0, forward()).expect("pause");
        assert_eq!(app.camera.expect("camera").eye, eye);
        assert_eq!(app.present_next, target);
        assert_eq!(app.presentation_alpha(), alpha);

        let now = Instant::now();
        app.frame_interval(now);
        app.set_ui_mode(crate::ui::Mode::Game);
        let dt = app.frame_interval(now + Duration::from_secs(30));
        assert_eq!(dt, 0.0);
        app.pump_logic(dt, forward()).expect("resume");
        assert_eq!(app.camera.expect("camera").eye, eye);
        app.pump_logic(0.025, forward()).expect("next tick");
        assert!(app.camera.expect("camera").eye[2] < eye[2]);
    }

    #[test]
    fn correction_replaces_both_endpoints_and_discards_old_time() {
        let mut app = app();
        app.pump_logic(0.075, forward()).expect("fly");
        app.last_frame = Some(Instant::now());
        app.walk_blocked = true;
        let corrected = [100.0, 20.0, 100.0];
        app.reset_presentation_pose(corrected);
        assert_eq!(app.presentation_alpha(), 0.0);
        assert!(app.last_frame.is_none());
        assert!(!app.walk_blocked);
        app.pump_logic(0.025, forward()).expect("partial frame");
        assert_eq!(app.camera.expect("camera").eye, corrected);
        assert_eq!(app.present_prev, corrected);
        assert_eq!(app.present_next, corrected);
        app.pump_logic(0.025, forward()).expect("step");
        assert_eq!(app.present_prev, corrected);
        assert!((app.present_next[2] - 99.4).abs() < 1e-5);
    }

    #[test]
    fn walking_uses_fixed_steps_and_presentation_does_not_move_the_entity() {
        let mut expected = None;
        for fps in [30, 60, 144] {
            let mut app = app();
            app.scene = Some(Scene::new(1).expect("scene"));
            app.enter_walk(app.camera.expect("camera").eye);
            assert!(app.walk);
            for _ in 0..fps {
                app.pump_logic(1.0 / f64::from(fps), forward())
                    .expect("walk");
            }
            assert_eq!(app.walk_ticks, 20);
            let position = app
                .scene
                .as_ref()
                .expect("scene")
                .walker_position()
                .expect("walker");
            if let Some(expected) = expected {
                assert_eq!(position, expected);
            } else {
                expected = Some(position);
            }
            app.apply_presented_eye();
            app.apply_presented_eye();
            app.set_ui_mode(crate::ui::Mode::Pause);
            app.pump_logic(30.0, forward()).expect("pause");
            assert_eq!(app.walk_ticks, 20);
            assert_eq!(
                app.scene.as_ref().expect("scene").walker_position(),
                Some(position)
            );
            // Switching modes drops stale interpolation and autoplay state.
            let eye = app.camera.expect("camera").eye;
            app.toggle_walk();
            assert!(!app.walk);
            assert_eq!(app.present_prev, eye);
            assert_eq!(app.present_next, eye);
            assert_eq!(app.presentation_alpha(), 0.0);
        }
    }

    #[test]
    fn network_walk_does_not_create_a_local_authority() {
        let mut app = app();
        let snapshot = torse_sim::replica::ReplicaSnapshot {
            side: 16,
            bounds: torse_world::WorldBounds::new(0, 32).expect("bounds"),
            states: Vec::new(),
            sections: Vec::new(),
            entities: Vec::new(),
        };
        app.scene = Some(Scene::from_snapshot(&snapshot).expect("replica"));
        app.enter_walk(app.camera.expect("camera").eye);
        assert!(!app.walk);
        app.pump_logic(1.0, forward()).expect("network fly");
        assert_eq!(app.walk_ticks, 0);
        assert!(
            app.scene
                .as_ref()
                .expect("scene")
                .walker_position()
                .is_none()
        );
        assert_eq!(app.last_result, "network walking is not supported yet");
    }
}
