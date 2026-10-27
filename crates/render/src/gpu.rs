//! One device selection path for a window, an offscreen target, and the probe.
//!
//! Windows tries Vulkan, then Dx12. Linux tries Vulkan. macOS tries Metal.
//! Within a backend the high-performance adapter is requested first and the
//! low-power adapter second. A failed backend does not enlarge the request.
//! Presentation prefers mailbox and otherwise fifo. Headless devices have no
//! present mode.

use wgpu::{
    Adapter, AdapterInfo, Backends, Device, DeviceDescriptor, Instance, InstanceDescriptor,
    PowerPreference, PresentMode, Queue, RequestAdapterOptions, Surface, SurfaceConfiguration,
};

/// Adapter, device, and queue opened by [`open_headless`] or [`open_for_window`].
pub struct GpuSession {
    /// Instance whose backend set is the one that succeeded.
    pub instance: Instance,
    /// Selected adapter.
    pub adapter: Adapter,
    /// Device created with the default descriptor.
    pub device: Device,
    /// Queue for that device.
    pub queue: Queue,
}

impl GpuSession {
    /// Adapter name, backend, and device type.
    pub fn info(&self) -> AdapterInfo {
        self.adapter.get_info()
    }
}

/// Backends for this operating system, in the order they are tried.
pub fn backend_order() -> &'static [Backends] {
    if cfg!(windows) {
        &[Backends::VULKAN, Backends::DX12]
    } else if cfg!(target_os = "macos") {
        &[Backends::METAL]
    } else {
        &[Backends::VULKAN]
    }
}

/// Power preferences tried for one backend, high performance first.
pub fn power_order() -> [PowerPreference; 2] {
    [PowerPreference::HighPerformance, PowerPreference::LowPower]
}

/// Mailbox when the surface lists it, otherwise fifo.
///
/// Returns `None` when neither mode is listed. The caller fails initialization
/// instead of picking an unlisted mode.
pub fn select_present_mode(modes: &[PresentMode]) -> Option<PresentMode> {
    if modes.contains(&PresentMode::Mailbox) {
        Some(PresentMode::Mailbox)
    } else if modes.contains(&PresentMode::Fifo) {
        Some(PresentMode::Fifo)
    } else {
        None
    }
}

/// One line naming the adapter, backend, and present mode.
///
/// `present` is `None` for a headless device.
pub fn startup_line(info: &AdapterInfo, present: Option<PresentMode>) -> String {
    match present {
        Some(mode) => format!(
            "adapter={} backend={:?} present={mode:?}",
            info.name, info.backend
        ),
        None => format!(
            "adapter={} backend={:?} present=none",
            info.name, info.backend
        ),
    }
}

/// Open a device with no surface.
pub fn open_headless() -> Result<GpuSession, String> {
    let mut errors = Vec::new();
    for backends in backend_order() {
        let instance = instance_for(*backends);
        match open_on(&instance, None) {
            Ok((adapter, device, queue)) => {
                return Ok(GpuSession {
                    instance,
                    adapter,
                    device,
                    queue,
                });
            }
            Err(error) => errors.push(format!("{backends:?}: {error}")),
        }
    }
    Err(format!("no usable graphics device ({})", errors.join("; ")))
}

/// Open a device whose surface is created from the same instance.
///
/// `create_surface` is called once per backend. A backend is abandoned when
/// the surface, the adapter, or the device cannot be created.
pub fn open_for_window(
    create_surface: impl Fn(&Instance) -> Result<Surface<'static>, String>,
) -> Result<(GpuSession, Surface<'static>), String> {
    let mut errors = Vec::new();
    for backends in backend_order() {
        let instance = instance_for(*backends);
        let surface = match create_surface(&instance) {
            Ok(surface) => surface,
            Err(error) => {
                errors.push(format!("{backends:?} surface: {error}"));
                continue;
            }
        };
        match open_on(&instance, Some(&surface)) {
            Ok((adapter, device, queue)) => {
                return Ok((
                    GpuSession {
                        instance,
                        adapter,
                        device,
                        queue,
                    },
                    surface,
                ));
            }
            Err(error) => errors.push(format!("{backends:?}: {error}")),
        }
    }
    Err(format!("no usable graphics device ({})", errors.join("; ")))
}

/// Write the selected present mode into `config`.
pub fn apply_present_mode(
    config: &mut SurfaceConfiguration,
    modes: &[PresentMode],
) -> Result<PresentMode, String> {
    let mode = select_present_mode(modes)
        .ok_or_else(|| "surface supports neither mailbox nor fifo presentation".to_owned())?;
    config.present_mode = mode;
    Ok(mode)
}

fn instance_for(backends: Backends) -> Instance {
    let mut descriptor = InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    Instance::new(descriptor)
}

fn open_on(
    instance: &Instance,
    surface: Option<&Surface<'_>>,
) -> Result<(Adapter, Device, Queue), String> {
    let mut errors = Vec::new();
    for power in power_order() {
        let options = RequestAdapterOptions {
            power_preference: power,
            force_fallback_adapter: false,
            compatible_surface: surface,
            apply_limit_buckets: false,
        };
        let adapter = match pollster::block_on(instance.request_adapter(&options)) {
            Ok(adapter) => adapter,
            Err(error) => {
                errors.push(format!("{power:?} adapter: {error}"));
                continue;
            }
        };
        match pollster::block_on(adapter.request_device(&DeviceDescriptor::default())) {
            Ok((device, queue)) => return Ok((adapter, device, queue)),
            Err(error) => errors.push(format!("{power:?} device: {error}")),
        }
    }
    Err(errors.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_order_is_fixed_for_the_host() {
        let order = backend_order();
        if cfg!(windows) {
            assert_eq!(order, &[Backends::VULKAN, Backends::DX12]);
        } else if cfg!(target_os = "macos") {
            assert_eq!(order, &[Backends::METAL]);
        } else {
            assert_eq!(order, &[Backends::VULKAN]);
        }
    }

    #[test]
    fn present_mode_prefers_mailbox_and_rejects_an_empty_list() {
        assert_eq!(
            select_present_mode(&[PresentMode::Fifo, PresentMode::Mailbox]),
            Some(PresentMode::Mailbox)
        );
        assert_eq!(
            select_present_mode(&[PresentMode::Fifo]),
            Some(PresentMode::Fifo)
        );
        assert_eq!(select_present_mode(&[PresentMode::Immediate]), None);
        assert_eq!(power_order()[0], PowerPreference::HighPerformance);
        assert_eq!(power_order()[1], PowerPreference::LowPower);
    }
}
