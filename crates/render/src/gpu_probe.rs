//! M0 experiment: create an invisible native window, surface, adapter, and device.
//!
//! The window is hidden and exists only long enough to prove native windowing and
//! backend initialization on the current machine. No grid is submitted and no
//! frame is timed, so these numbers must not be used as rendering performance
//! evidence. The probe also documents the platform backend selected by `wgpu`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gpu::{apply_present_mode, open_for_window, startup_line};
use wgpu::PollType;
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
    /// Present mode selected for the surface.
    pub present_mode: String,
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
        self.result = Some(initialize_device(window));
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
fn initialize_device(window: Arc<Window>) -> Result<GpuProbeReport, String> {
    let start = Instant::now();
    // The surface has to be created from the same instance as the adapter.
    let (session, surface) = open_for_window(|instance| {
        instance
            .create_surface(window.clone())
            .map_err(|error| format!("{error}"))
    })?;
    let info = session.info();
    let size = window.inner_size();
    let width = size.width.max(1);
    let height = size.height.max(1);
    let capabilities = surface.get_capabilities(&session.adapter);
    let mut config = surface
        .get_default_config(&session.adapter, width, height)
        .ok_or_else(|| "surface has no supported configuration".to_owned())?;
    config.view_formats.clear();
    let present = apply_present_mode(&mut config, &capabilities.present_modes)?;
    let format = format!("{:?}", config.format);
    surface.configure(&session.device, &config);
    session.queue.submit([]);
    session
        .device
        .poll(PollType::wait_indefinitely())
        .map_err(|error| format!("device poll failed: {error}"))?;
    eprintln!("{}", startup_line(&info, Some(present)));

    Ok(GpuProbeReport {
        adapter_name: info.name,
        backend: format!("{:?}", info.backend),
        device_type: format!("{:?}", info.device_type),
        surface_format: format,
        present_mode: format!("{present:?}"),
        elapsed: start.elapsed(),
    })
}
