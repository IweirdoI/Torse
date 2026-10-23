//! M0 experiment: create an invisible native window, surface, adapter, and device.
//!
//! The window is hidden and exists only long enough to prove native windowing and
//! backend initialization on the current machine. No grid is submitted and no
//! frame is timed, so these numbers must not be used as rendering performance
//! evidence. The probe also documents the platform backend selected by `wgpu`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use wgpu::{DeviceDescriptor, Instance, InstanceDescriptor, PollType, RequestAdapterOptions};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

/// Observable result of one graphics initialization.
#[derive(Clone, Debug)]
pub struct GpuProbeReport {
    /// Human-readable adapter name reported by the selected backend.
    pub adapter_name: String,
    /// Backend identifier such as `Metal`, `Vulkan`, or `Dx12`.
    pub backend: String,
    /// Adapter class reported by the backend.
    pub device_type: String,
    /// Texture format selected for the configured surface.
    pub surface_format: String,
    /// Time spent creating the surface, device, and configuration.
    pub elapsed: Duration,
}

/// Minimal winit application that performs initialization once and exits.
struct ProbeApp {
    /// Initialization result moved out after the event loop returns.
    result: Option<Result<GpuProbeReport, String>>,
}

impl ApplicationHandler for ProbeApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Keep the window invisible so the probe is safe during local development
        // and does not require a visible desktop session.
        let attributes = Window::default_attributes()
            .with_title("Torse M0 GPU probe")
            .with_visible(false)
            .with_inner_size(LogicalSize::new(64, 64));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.result = Some(Err(format!("window creation failed: {error}")));
                event_loop.exit();
                return;
            }
        };
        self.result = Some(initialize_device(&window));
        // One initialization is enough; exiting immediately avoids a steady-state
        // event loop and makes the command suitable for automation.
        event_loop.exit();
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }
}

/// Create the native event loop and run the one-shot graphics probe.
///
/// Errors are owned strings because the render crate does not define the final
/// platform error contract yet.
pub fn run_gpu_probe() -> Result<GpuProbeReport, String> {
    let event_loop = EventLoop::new().map_err(|error| format!("event loop failed: {error}"))?;
    let mut app = ProbeApp { result: None };
    event_loop
        .run_app(&mut app)
        .map_err(|error| format!("event loop failed: {error}"))?;
    app.result
        .unwrap_or_else(|| Err("event loop exited before initialization".to_owned()))
}

/// Create a surface, choose an adapter, request a device, and configure the surface.
///
/// The surface configuration is validated before returning, which catches common
/// backend/driver failures without requiring a rendering pass.
fn initialize_device(window: &Window) -> Result<GpuProbeReport, String> {
    let start = Instant::now();
    let instance = Instance::new(InstanceDescriptor::new_without_display_handle());
    let surface = instance
        .create_surface(window)
        .map_err(|error| format!("surface creation failed: {error}"))?;
    // Supplying the compatible surface lets the adapter be selected with the
    // backend and capabilities needed by the actual window.
    let options = RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..RequestAdapterOptions::default()
    };
    let adapter = pollster::block_on(instance.request_adapter(&options))
        .map_err(|error| format!("adapter unavailable: {error}"))?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))
        .map_err(|error| format!("device unavailable: {error}"))?;

    let size = window.inner_size();
    let width = size.width.max(1);
    let height = size.height.max(1);
    // `get_default_config` chooses a supported present mode and color format for
    // this adapter. Clearing view formats keeps the probe independent of optional
    // color-space reinterpretation.
    let mut config = surface
        .get_default_config(&adapter, width, height)
        .ok_or_else(|| "surface has no supported configuration".to_owned())?;
    config.view_formats.clear();
    let format = format!("{:?}", config.format);
    // A successful configure call is the serviceable proof for this M0 probe.
    // Frame acquisition is intentionally omitted because hidden windows may be
    // treated as occluded by the compositor.
    surface.configure(&device, &config);
    queue.submit([]);
    device
        .poll(PollType::wait_indefinitely())
        .map_err(|error| format!("device poll failed: {error}"))?;

    Ok(GpuProbeReport {
        adapter_name: info.name,
        backend: format!("{:?}", info.backend),
        device_type: format!("{:?}", info.device_type),
        surface_format: format,
        elapsed: start.elapsed(),
    })
}
