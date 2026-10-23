//! M0 experiment: create an invisible native window, surface, adapter, and device.

use std::sync::Arc;
use std::time::{Duration, Instant};

use wgpu::{DeviceDescriptor, Instance, InstanceDescriptor, PollType, RequestAdapterOptions};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Clone, Debug)]
pub struct GpuProbeReport {
    pub adapter_name: String,
    pub backend: String,
    pub device_type: String,
    pub surface_format: String,
    pub elapsed: Duration,
}

struct ProbeApp {
    result: Option<Result<GpuProbeReport, String>>,
}

impl ApplicationHandler for ProbeApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
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

pub fn run_gpu_probe() -> Result<GpuProbeReport, String> {
    let event_loop = EventLoop::new().map_err(|error| format!("event loop failed: {error}"))?;
    let mut app = ProbeApp { result: None };
    event_loop
        .run_app(&mut app)
        .map_err(|error| format!("event loop failed: {error}"))?;
    app.result
        .unwrap_or_else(|| Err("event loop exited before initialization".to_owned()))
}

fn initialize_device(window: &Window) -> Result<GpuProbeReport, String> {
    let start = Instant::now();
    let instance = Instance::new(InstanceDescriptor::new_without_display_handle());
    let surface = instance
        .create_surface(window)
        .map_err(|error| format!("surface creation failed: {error}"))?;
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
    let mut config = surface
        .get_default_config(&adapter, width, height)
        .ok_or_else(|| "surface has no supported configuration".to_owned())?;
    config.view_formats.clear();
    let format = format!("{:?}", config.format);
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
