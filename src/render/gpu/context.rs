//! WGPU device/instance management, offscreen readback, and native Wayland surface targets.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;

use femtovg::renderer::{WGPURenderOutput, WGPURenderer};
use femtovg::{Canvas, Color as FemtoColor};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};
use wgpu::rwh::{
    DisplayHandle, HandleError, RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle,
    WaylandWindowHandle,
};

use crate::app::GpuMemoryPolicy;
use crate::error::{Result, WaywiError};
use crate::reconcile::RetainedSurfaceKey;

#[derive(Debug)]
struct OwnedWaylandDisplay {
    conn: Connection,
}

fn block_on_ready<F: std::future::Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(val) => return val,
            std::task::Poll::Pending => std::thread::yield_now(),
        }
    }
}

impl wgpu::rwh::HasDisplayHandle for OwnedWaylandDisplay {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        let ptr = self.conn.backend().display_ptr().cast::<c_void>();
        let nn = NonNull::new(ptr).ok_or(HandleError::Unavailable)?;
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(nn));
        // SAFETY: `self.conn` outlives the borrowed `DisplayHandle<'_>` tied to `&self`.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

pub(crate) struct WaylandGpuTarget {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
}

pub(crate) struct GpuContext {
    pub(crate) instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) canvas: Canvas<WGPURenderer>,
    offscreen_tex: Option<(u32, u32, wgpu::Texture)>,
    staging_buf: Option<(u64, wgpu::Buffer)>,
    surfaces: HashMap<RetainedSurfaceKey, WaylandGpuTarget>,
}

impl GpuContext {
    pub(crate) fn memory_hints_for(policy: GpuMemoryPolicy) -> wgpu::MemoryHints {
        const MIB: u64 = 1024 * 1024;
        match policy {
            GpuMemoryPolicy::MemoryUsage => wgpu::MemoryHints::MemoryUsage,
            GpuMemoryPolicy::Minimal => wgpu::MemoryHints::Manual {
                suballocated_device_memory_block_size: (4 * MIB)..(16 * MIB),
            },
            GpuMemoryPolicy::Performance => wgpu::MemoryHints::Performance,
        }
    }

    /// Initializes a WGPU instance, adapter, device, and `femtovg::Canvas` cleanly.
    pub(crate) fn new(
        wayland_conn: Option<&Connection>,
        memory_policy: GpuMemoryPolicy,
    ) -> Option<Self> {
        let desc =
            match wayland_conn {
                Some(conn) => wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
                    OwnedWaylandDisplay { conn: conn.clone() },
                )),
                None => wgpu::InstanceDescriptor::new_without_display_handle(),
            };
        let instance = wgpu::Instance::new(desc);
        let adapter = block_on_ready(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            ..Default::default()
        }))
        .ok()?;

        let (device, queue) = block_on_ready(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("waywi-gpu-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: Self::memory_hints_for(memory_policy),
            ..Default::default()
        }))
        .ok()?;

        let renderer = WGPURenderer::new(device.clone(), queue.clone());
        let canvas = Canvas::new(renderer).ok()?;

        Some(Self {
            instance,
            adapter,
            device,
            queue,
            canvas,
            offscreen_tex: None,
            staging_buf: None,
            surfaces: HashMap::new(),
        })
    }

    pub(crate) fn begin_frame(&mut self, width: u32, height: u32, bg: FemtoColor) {
        self.canvas.set_size(width, height, 1.0);
        self.canvas.clear_rect(0, 0, width, height, bg);
    }

    pub(crate) fn flush_offscreen_pixels(
        &mut self,
        width: u32,
        height: u32,
        dst: &mut [u8],
    ) -> Result<()> {
        let recreate_tex = self
            .offscreen_tex
            .as_ref()
            .is_none_or(|(w, h, _)| *w != width || *h != height);
        if recreate_tex {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("waywi-offscreen-rgba"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            self.offscreen_tex = Some((width, height, tex));
        }

        let Some((_, _, ref tex)) = self.offscreen_tex else {
            return Err(WaywiError::RenderError(
                "offscreen GPU texture unavailable".into(),
            ));
        };
        let output = WGPURenderOutput::from(tex);
        let cmd_buf = self.canvas.flush_to_output(output);

        let unpadded_bpr = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bpr = unpadded_bpr.div_ceil(align) * align;
        let total_bytes = u64::from(padded_bpr) * u64::from(height);

        let recreate_buf = self
            .staging_buf
            .as_ref()
            .is_none_or(|(sz, _)| *sz < total_bytes);
        if recreate_buf {
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("waywi-offscreen-readback"),
                size: total_bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.staging_buf = Some((total_bytes, buf));
        }

        let Some((_, ref staging)) = self.staging_buf else {
            return Err(WaywiError::RenderError(
                "offscreen readback buffer unavailable".into(),
            ));
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("waywi-offscreen-copy"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let copy_cmd = encoder.finish();

        if let Some(draw_cmd) = cmd_buf {
            self.queue.submit([draw_cmd, copy_cmd]);
        } else {
            self.queue.submit([copy_cmd]);
        }

        let slice = staging.slice(0..total_bytes);
        let (map_tx, map_rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = map_tx.send(res);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        map_rx
            .recv()
            .map_err(|_| WaywiError::RenderError("GPU readback channel dropped".into()))?
            .map_err(|e| WaywiError::RenderError(format!("GPU buffer map failed: {e}")))?;

        let mapped = slice
            .get_mapped_range()
            .map_err(|e| WaywiError::RenderError(format!("GPU mapped range failed: {e}")))?;
        let row_bytes = (width as usize) * 4;
        for row in 0..(height as usize) {
            let src_off = row * (padded_bpr as usize);
            let dst_off = row * row_bytes;
            let src_row = &mapped[src_off..src_off + row_bytes];
            let dst_row = &mut dst[dst_off..dst_off + row_bytes];
            for (s, d) in src_row
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dst_row.as_chunks_mut::<4>().0)
            {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
        }
        drop(mapped);
        staging.unmap();
        Ok(())
    }

    pub(crate) fn ensure_wayland_target(
        &mut self,
        conn: &Connection,
        wl_surface: &WlSurface,
        key: RetainedSurfaceKey,
        width: u32,
        height: u32,
    ) -> bool {
        if width == 0 || height == 0 {
            return false;
        }
        if !self.surfaces.contains_key(&key) {
            let Some(disp_ptr) = NonNull::new(conn.backend().display_ptr().cast::<c_void>()) else {
                return false;
            };
            let Some(surf_ptr) = NonNull::new(wl_surface.id().as_ptr().cast::<c_void>()) else {
                return false;
            };
            let raw_display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(disp_ptr));
            let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(surf_ptr));

            // SAFETY: `self._owned_display` keeps the `wl_display` connection alive for `'static`,
            // and `WaylandGpuTarget` for `key` is dropped when the `wl_surface` is destroyed.
            let Ok(surface) = (unsafe {
                self.instance
                    .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                        raw_display_handle: Some(raw_display_handle),
                        raw_window_handle,
                    })
            }) else {
                return false;
            };

            let caps = surface.get_capabilities(&self.adapter);
            if caps.formats.is_empty() || caps.present_modes.is_empty() {
                return false;
            }
            // Prefer non-sRGB formats matching sRGB byte output, and require PreMultiplied alpha
            // if available (otherwise let wl_shm handle transparent layer-shell alpha cleanly).
            let Some(&alpha_mode) = caps
                .alpha_modes
                .iter()
                .find(|&&m| m == wgpu::CompositeAlphaMode::PreMultiplied)
            else {
                return false;
            };
            let format = caps
                .formats
                .iter()
                .copied()
                .find(|f| {
                    matches!(
                        f,
                        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                    )
                })
                .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
                .unwrap_or(caps.formats[0]);

            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                color_space: wgpu::SurfaceColorSpace::Auto,
                width,
                height,
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode,
                view_formats: vec![],
            };
            surface.configure(&self.device, &config);
            self.surfaces
                .insert(key, WaylandGpuTarget { surface, config });
            return true;
        }

        if let Some(target) = self.surfaces.get_mut(&key)
            && (target.config.width != width || target.config.height != height)
        {
            target.config.width = width;
            target.config.height = height;
            target.surface.configure(&self.device, &target.config);
        }
        true
    }

    pub(crate) fn has_wayland_target(&self, key: RetainedSurfaceKey) -> bool {
        self.surfaces.contains_key(&key)
    }

    /// Acquires the next swapchain texture BEFORE encoding any `femtovg::Canvas` draw commands.
    ///
    /// Handles `Outdated` via reconfigure and `Lost` via full `wgpu::Surface` recreation; returns
    /// `None` on transient `Timeout`/`Occluded` (e.g. DPMS off) without leaving dirty commands
    /// queued in `self.canvas`.
    pub(crate) fn acquire_surface_frame(
        &mut self,
        conn: &Connection,
        wl_surface: &WlSurface,
        key: RetainedSurfaceKey,
        width: u32,
        height: u32,
    ) -> Option<wgpu::SurfaceTexture> {
        let state = {
            let target = self.surfaces.get_mut(&key)?;
            target.surface.get_current_texture()
        };
        match state {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
            wgpu::CurrentSurfaceTexture::Outdated => {
                let target = self.surfaces.get_mut(&key)?;
                target.surface.configure(&self.device, &target.config);
                match target.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
                    _ => None,
                }
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surfaces.remove(&key);
                if !self.ensure_wayland_target(conn, wl_surface, key, width, height) {
                    return None;
                }
                let target = self.surfaces.get_mut(&key)?;
                match target.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub(crate) fn present_acquired_frame(&mut self, frame: wgpu::SurfaceTexture) {
        if let Some(cmd_buf) = self
            .canvas
            .flush_to_output(WGPURenderOutput::from(&frame.texture))
        {
            self.queue.submit([cmd_buf]);
        }
        self.queue.present(frame);
    }

    pub(crate) fn drop_surface(&mut self, key: RetainedSurfaceKey) {
        self.surfaces.remove(&key);
    }

    pub(crate) fn trim_offscreen(&mut self) {
        self.offscreen_tex = None;
        self.staging_buf = None;
        let _ = self.device.poll(wgpu::PollType::Poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_memory_policy_maps_to_expected_wgpu_memory_hints() {
        const MIB: u64 = 1024 * 1024;
        assert!(matches!(
            GpuContext::memory_hints_for(GpuMemoryPolicy::MemoryUsage),
            wgpu::MemoryHints::MemoryUsage
        ));
        assert!(matches!(
            GpuContext::memory_hints_for(GpuMemoryPolicy::Minimal),
            wgpu::MemoryHints::Manual {
                suballocated_device_memory_block_size,
            } if suballocated_device_memory_block_size == ((4 * MIB)..(16 * MIB))
        ));
        assert!(matches!(
            GpuContext::memory_hints_for(GpuMemoryPolicy::Performance),
            wgpu::MemoryHints::Performance
        ));
    }
}
