//! Wayland screen/output pixel capture backend supporting:
//! 1. `ext_image_copy_capture_v1` + `ext_output_image_capture_source_manager_v1` (preferred)
//! 2. `zwlr_screencopy_manager_v1` (fallback & native sub-region capture)

use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::AsFd;

use memmap2::Mmap;
use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_buffer, wl_output, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
    ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1},
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1, Options},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, Flags as WlrFlags, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};

use crate::capabilities::Capability;
use crate::error::{Result, WaywiError};
use crate::geometry::{Bounds, LogicalSize};
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;
use crate::wayland_ext::ScreencopyFrame;

const MAX_CAPTURE_BYTES: usize = 128 * 1024 * 1024;

/// User data tagging a temporary `wl_buffer` created solely for one screencopy capture.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScreencopyBufferData;

struct TempShmBuffer {
    buffer: wl_buffer::WlBuffer,
    pool: wl_shm_pool::WlShmPool,
    mmap: Mmap,
    format: wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
}

impl Drop for TempShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

struct WlrPending {
    frame: ZwlrScreencopyFrameV1,
    chosen_format: Option<(wl_shm::Format, u32, u32, u32)>,
    shm: Option<TempShmBuffer>,
    y_invert: bool,
    copied: bool,
}

impl Drop for WlrPending {
    fn drop(&mut self) {
        self.frame.destroy();
    }
}

struct ExtPending {
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    frame: Option<ExtImageCopyCaptureFrameV1>,
    width: u32,
    height: u32,
    chosen_format: Option<wl_shm::Format>,
    shm: Option<TempShmBuffer>,
    transform: wl_output::Transform,
    crop: Option<Bounds>,
    output_logical_size: LogicalSize,
}

impl Drop for ExtPending {
    fn drop(&mut self) {
        if let Some(f) = self.frame.take() {
            f.destroy();
        }
        self.session.destroy();
        self.source.destroy();
    }
}

/// Manages asynchronous non-blocking screencopy requests across `ext` and `wlr` backends.
pub(crate) struct WaylandScreencopyState {
    ext_mgr: Option<ExtImageCopyCaptureManagerV1>,
    ext_output_src_mgr: Option<ExtOutputImageCaptureSourceManagerV1>,
    wlr_mgr: Option<ZwlrScreencopyManagerV1>,
    wlr_pending: HashMap<u64, WlrPending>,
    ext_pending: HashMap<u64, ExtPending>,
}

impl Drop for WaylandScreencopyState {
    fn drop(&mut self) {
        self.wlr_pending.clear();
        self.ext_pending.clear();
        if let Some(m) = self.ext_mgr.take() {
            m.destroy();
        }
        if let Some(m) = self.ext_output_src_mgr.take() {
            m.destroy();
        }
        if let Some(m) = self.wlr_mgr.take() {
            m.destroy();
        }
    }
}

impl WaylandScreencopyState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let ext_mgr = globals
            .bind::<ExtImageCopyCaptureManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        let ext_output_src_mgr = globals
            .bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        let wlr_mgr = globals
            .bind::<ZwlrScreencopyManagerV1, _, _>(qh, 1..=3, ())
            .ok();
        Self {
            ext_mgr,
            ext_output_src_mgr,
            wlr_mgr,
            wlr_pending: HashMap::new(),
            ext_pending: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        (self.ext_mgr.is_some() && self.ext_output_src_mgr.is_some()) || self.wlr_mgr.is_some()
    }

    /// Starts a non-blocking capture for `request_id` on `output` (optional logical `region`).
    pub(crate) fn start_capture(
        &mut self,
        request_id: u64,
        output: &wl_output::WlOutput,
        region: Option<Bounds>,
        output_logical_size: LogicalSize,
        overlay_cursor: bool,
        qh: &QueueHandle<WaylandState>,
    ) -> Result<()> {
        // Prefer `ext_image_copy_capture_v1` for full-output capture (or when `wlr` is absent).
        if let (Some(ext_mgr), Some(src_mgr)) = (&self.ext_mgr, &self.ext_output_src_mgr)
            && (region.is_none() || self.wlr_mgr.is_none())
        {
            let source = src_mgr.create_source(output, qh, ());
            let opts = if overlay_cursor {
                Options::PaintCursors
            } else {
                Options::empty()
            };
            let session = ext_mgr.create_session(&source, opts, qh, request_id);
            self.ext_pending.insert(
                request_id,
                ExtPending {
                    source,
                    session,
                    frame: None,
                    width: 0,
                    height: 0,
                    chosen_format: None,
                    shm: None,
                    transform: wl_output::Transform::Normal,
                    crop: region,
                    output_logical_size,
                },
            );
            return Ok(());
        }

        if let Some(ref wlr) = self.wlr_mgr {
            let oc = i32::from(overlay_cursor);
            let frame = match region {
                Some(b) => wlr.capture_output_region(
                    oc,
                    output,
                    b.x.0.floor() as i32,
                    b.y.0.floor() as i32,
                    b.width.0.ceil().max(1.0) as i32,
                    b.height.0.ceil().max(1.0) as i32,
                    qh,
                    request_id,
                ),
                None => wlr.capture_output(oc, output, qh, request_id),
            };
            self.wlr_pending.insert(
                request_id,
                WlrPending {
                    frame,
                    chosen_format: None,
                    shm: None,
                    y_invert: false,
                    copied: false,
                },
            );
            return Ok(());
        }

        Err(WaywiError::UnsupportedCapability(Capability::Screencopy))
    }
}

fn format_priority(fmt: wl_shm::Format) -> Option<u8> {
    match fmt {
        wl_shm::Format::Argb8888 => Some(4),
        wl_shm::Format::Xrgb8888 => Some(3),
        wl_shm::Format::Abgr8888 => Some(2),
        wl_shm::Format::Xbgr8888 => Some(1),
        _ => None,
    }
}

fn allocate_capture_shm(
    wl_shm: &wl_shm::WlShm,
    qh: &QueueHandle<WaylandState>,
    format: wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
) -> Result<TempShmBuffer> {
    let size = (stride as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| WaywiError::SurfaceError("screencopy buffer dimensions overflow".into()))?;
    if size == 0 || size > MAX_CAPTURE_BYTES {
        return Err(WaywiError::SurfaceError(format!(
            "screencopy buffer size {size} out of bounds"
        )));
    }
    let name = CString::new("waywi-screencopy").expect("valid cstr");
    let fd = memfd_create(&name, MemfdFlags::CLOEXEC)
        .map_err(|e| WaywiError::SurfaceError(format!("screencopy memfd_create failed: {e}")))?;
    ftruncate(&fd, size as u64)
        .map_err(|e| WaywiError::SurfaceError(format!("screencopy ftruncate failed: {e}")))?;
    let pool = wl_shm.create_pool(fd.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        format,
        qh,
        ScreencopyBufferData,
    );
    // SAFETY: `fd` is a freshly created `memfd` sized to `size` bytes; read-only mapping is
    // accessed only after the compositor signals `ready`.
    let mmap = unsafe {
        Mmap::map(&fd)
            .map_err(|e| WaywiError::SurfaceError(format!("screencopy mmap failed: {e}")))?
    };
    Ok(TempShmBuffer {
        buffer,
        pool,
        mmap,
        format,
        width,
        height,
        stride,
    })
}

/// Returns the upright `(width, height)` after normalizing `transform`.
fn upright_dimensions(bw: u32, bh: u32, transform: wl_output::Transform) -> (u32, u32) {
    match transform {
        wl_output::Transform::_90
        | wl_output::Transform::_270
        | wl_output::Transform::Flipped90
        | wl_output::Transform::Flipped270 => (bh, bw),
        _ => (bw, bh),
    }
}

/// Maps an upright pixel coordinate `(ux, uy)` into source buffer pixel coordinates `(bx, by)`
/// according to the Wayland `wl_output::Transform` applied to the buffer.
#[inline]
fn map_upright_to_buffer(
    ux: u32,
    uy: u32,
    bw: u32,
    bh: u32,
    transform: wl_output::Transform,
) -> (u32, u32) {
    match transform {
        wl_output::Transform::Normal => (ux, uy),
        wl_output::Transform::_90 => (uy, bh - 1 - ux),
        wl_output::Transform::_180 => (bw - 1 - ux, bh - 1 - uy),
        wl_output::Transform::_270 => (bw - 1 - uy, ux),
        wl_output::Transform::Flipped => (bw - 1 - ux, uy),
        wl_output::Transform::Flipped90 => (uy, ux),
        wl_output::Transform::Flipped180 => (ux, bh - 1 - uy),
        wl_output::Transform::Flipped270 => (bw - 1 - uy, bh - 1 - ux),
        _ => (ux, uy),
    }
}

fn decode_raw_shm_to_rgba(
    src: &[u8],
    format: wl_shm::Format,
    bw: u32,
    bh: u32,
    stride: u32,
    transform: wl_output::Transform,
    crop: Option<(u32, u32, u32, u32)>,
) -> ScreencopyFrame {
    let (up_w, up_h) = upright_dimensions(bw, bh, transform);
    let (cx, cy, cw, ch) = match crop {
        Some((x, y, w, h)) => {
            let x = x.min(up_w.saturating_sub(1));
            let y = y.min(up_h.saturating_sub(1));
            let w = w.min(up_w.saturating_sub(x)).max(1);
            let h = h.min(up_h.saturating_sub(y)).max(1);
            (x, y, w, h)
        }
        None => (0, 0, up_w, up_h),
    };

    let dst_stride = cw * 4;
    let mut rgba = vec![0u8; (dst_stride as usize) * (ch as usize)];

    for dy in 0..ch {
        let uy = cy + dy;
        let dst_row_off = (dy as usize) * (dst_stride as usize);
        for dx in 0..cw {
            let ux = cx + dx;
            let (bx, by) = map_upright_to_buffer(ux, uy, bw, bh, transform);
            let sp = (by as usize) * (stride as usize) + (bx as usize) * 4;
            let dp = dst_row_off + (dx as usize) * 4;
            if sp + 4 > src.len() {
                continue;
            }
            let b0 = src[sp];
            let b1 = src[sp + 1];
            let b2 = src[sp + 2];
            let b3 = src[sp + 3];
            // Little-endian DRM/wl_shm 32-bit layouts:
            // ARGB8888 / XRGB8888 -> memory bytes [B, G, R, A/X]
            // ABGR8888 / XBGR8888 -> memory bytes [R, G, B, A/X]
            let (r, g, b, a) = match format {
                wl_shm::Format::Argb8888 => (b2, b1, b0, b3),
                wl_shm::Format::Xrgb8888 => (b2, b1, b0, 255),
                wl_shm::Format::Abgr8888 => (b0, b1, b2, b3),
                wl_shm::Format::Xbgr8888 => (b0, b1, b2, 255),
                _ => (b2, b1, b0, 255),
            };
            rgba[dp] = r;
            rgba[dp + 1] = g;
            rgba[dp + 2] = b;
            rgba[dp + 3] = a;
        }
    }

    ScreencopyFrame {
        width: cw,
        height: ch,
        stride: dst_stride,
        rgba,
    }
}

fn decode_shm_to_rgba(
    shm: &TempShmBuffer,
    transform: wl_output::Transform,
    crop: Option<(u32, u32, u32, u32)>,
) -> ScreencopyFrame {
    decode_raw_shm_to_rgba(
        &shm.mmap[..],
        shm.format,
        shm.width,
        shm.height,
        shm.stride,
        transform,
        crop,
    )
}

fn trigger_wlr_copy(state: &mut WaylandState, request_id: u64, qh: &QueueHandle<WaylandState>) {
    let Ok(wl_shm) = smithay_client_toolkit::globals::ProvidesBoundGlobal::bound_global(&state.shm)
    else {
        state.screencopy.wlr_pending.remove(&request_id);
        state.events.push_back(WaylandEvent::ScreencopyDone {
            request_id,
            result: Err(WaywiError::WaylandConnection("wl_shm not bound".into())),
        });
        return;
    };

    let Some(entry) = state.screencopy.wlr_pending.get_mut(&request_id) else {
        return;
    };
    if entry.copied {
        return;
    }
    let Some((fmt, w, h, stride)) = entry.chosen_format else {
        state.screencopy.wlr_pending.remove(&request_id);
        state.events.push_back(WaylandEvent::ScreencopyDone {
            request_id,
            result: Err(WaywiError::SurfaceError(
                "screencopy compositor offered no supported SHM format".into(),
            )),
        });
        return;
    };
    match allocate_capture_shm(&wl_shm, qh, fmt, w, h, stride) {
        Ok(shm_buf) => {
            entry.frame.copy(&shm_buf.buffer);
            entry.shm = Some(shm_buf);
            entry.copied = true;
        }
        Err(err) => {
            state.screencopy.wlr_pending.remove(&request_id);
            state.events.push_back(WaylandEvent::ScreencopyDone {
                request_id,
                result: Err(err),
            });
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ScreencopyBufferData> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_buffer::WlBuffer,
        _event: wl_buffer::Event,
        _data: &ScreencopyBufferData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrScreencopyManagerV1,
        _event: zwlr_screencopy_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, u64> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        &request_id: &u64,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                if let WEnum::Value(fmt) = format
                    && let Some(prio) = format_priority(fmt)
                    && let Some(entry) = state.screencopy.wlr_pending.get_mut(&request_id)
                {
                    let better = entry
                        .chosen_format
                        .and_then(|(cur, _, _, _)| format_priority(cur))
                        .is_none_or(|cur_p| prio > cur_p);
                    if better {
                        entry.chosen_format = Some((fmt, width, height, stride));
                    }
                }
                // Version < 3 sends a single `buffer` event without `buffer_done`; trigger
                // immediately (emitting an honest error if the format was unsupported).
                if proxy.version() < 3 {
                    trigger_wlr_copy(state, request_id, qh);
                }
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => {
                trigger_wlr_copy(state, request_id, qh);
            }
            zwlr_screencopy_frame_v1::Event::Flags { flags } => {
                if let WEnum::Value(f) = flags
                    && let Some(entry) = state.screencopy.wlr_pending.get_mut(&request_id)
                {
                    entry.y_invert = f.contains(WlrFlags::YInvert);
                }
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                if let Some(entry) = state.screencopy.wlr_pending.remove(&request_id) {
                    let transform = if entry.y_invert {
                        wl_output::Transform::Flipped180
                    } else {
                        wl_output::Transform::Normal
                    };
                    let result = match entry.shm.as_ref() {
                        Some(shm) => Ok(decode_shm_to_rgba(shm, transform, None)),
                        None => Err(WaywiError::SurfaceError(
                            "screencopy ready without SHM buffer".into(),
                        )),
                    };
                    state
                        .events
                        .push_back(WaylandEvent::ScreencopyDone { request_id, result });
                }
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                state.screencopy.wlr_pending.remove(&request_id);
                state.events.push_back(WaylandEvent::ScreencopyDone {
                    request_id,
                    result: Err(WaywiError::SurfaceError(
                        "zwlr_screencopy_frame_v1 capture failed".into(),
                    )),
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtImageCopyCaptureManagerV1,
        _event: ext_image_copy_capture_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtOutputImageCaptureSourceManagerV1,
        _event: ext_output_image_capture_source_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCaptureSourceV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtImageCaptureSourceV1,
        _event: ext_image_capture_source_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, u64> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        &request_id: &u64,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                if let Some(entry) = state.screencopy.ext_pending.get_mut(&request_id) {
                    entry.width = width;
                    entry.height = height;
                }
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                if let WEnum::Value(fmt) = format
                    && let Some(prio) = format_priority(fmt)
                    && let Some(entry) = state.screencopy.ext_pending.get_mut(&request_id)
                {
                    let better = entry
                        .chosen_format
                        .and_then(format_priority)
                        .is_none_or(|cur_p| prio > cur_p);
                    if better {
                        entry.chosen_format = Some(fmt);
                    }
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                let Ok(wl_shm) =
                    smithay_client_toolkit::globals::ProvidesBoundGlobal::bound_global(&state.shm)
                else {
                    state.screencopy.ext_pending.remove(&request_id);
                    state.events.push_back(WaylandEvent::ScreencopyDone {
                        request_id,
                        result: Err(WaywiError::WaylandConnection("wl_shm not bound".into())),
                    });
                    return;
                };
                let Some(entry) = state.screencopy.ext_pending.get_mut(&request_id) else {
                    return;
                };
                if entry.frame.is_some() {
                    return;
                }
                let (Some(fmt), w, h) = (entry.chosen_format, entry.width, entry.height) else {
                    state.screencopy.ext_pending.remove(&request_id);
                    state.events.push_back(WaylandEvent::ScreencopyDone {
                        request_id,
                        result: Err(WaywiError::SurfaceError(
                            "ext_image_copy_capture_v1 offered no supported SHM format".into(),
                        )),
                    });
                    return;
                };
                let stride = w.saturating_mul(4);
                match allocate_capture_shm(&wl_shm, qh, fmt, w, h, stride) {
                    Ok(shm_buf) => {
                        let frame = proxy.create_frame(qh, request_id);
                        frame.attach_buffer(&shm_buf.buffer);
                        frame.damage_buffer(0, 0, w as i32, h as i32);
                        frame.capture();
                        entry.frame = Some(frame);
                        entry.shm = Some(shm_buf);
                    }
                    Err(err) => {
                        state.screencopy.ext_pending.remove(&request_id);
                        state.events.push_back(WaylandEvent::ScreencopyDone {
                            request_id,
                            result: Err(err),
                        });
                    }
                }
            }
            ext_image_copy_capture_session_v1::Event::Stopped
                if state.screencopy.ext_pending.remove(&request_id).is_some() =>
            {
                state.events.push_back(WaylandEvent::ScreencopyDone {
                    request_id,
                    result: Err(WaywiError::SurfaceError(
                        "ext_image_copy_capture_v1 session stopped by compositor".into(),
                    )),
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, u64> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        &request_id: &u64,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Transform { transform } => {
                if let WEnum::Value(t) = transform
                    && let Some(entry) = state.screencopy.ext_pending.get_mut(&request_id)
                {
                    entry.transform = t;
                }
            }
            ext_image_copy_capture_frame_v1::Event::Ready => {
                if let Some(entry) = state.screencopy.ext_pending.remove(&request_id) {
                    let (up_w, up_h) =
                        upright_dimensions(entry.width, entry.height, entry.transform);
                    let sx = if entry.output_logical_size.width.0 > 0.0 {
                        up_w as f32 / entry.output_logical_size.width.0
                    } else {
                        1.0
                    };
                    let sy = if entry.output_logical_size.height.0 > 0.0 {
                        up_h as f32 / entry.output_logical_size.height.0
                    } else {
                        1.0
                    };
                    let crop = entry.crop.map(|b| {
                        (
                            (b.x.0 * sx).floor().max(0.0) as u32,
                            (b.y.0 * sy).floor().max(0.0) as u32,
                            (b.width.0 * sx).ceil().max(1.0) as u32,
                            (b.height.0 * sy).ceil().max(1.0) as u32,
                        )
                    });
                    let result = match entry.shm.as_ref() {
                        Some(shm) => Ok(decode_shm_to_rgba(shm, entry.transform, crop)),
                        None => Err(WaywiError::SurfaceError(
                            "ext_image_copy_capture_v1 ready without SHM buffer".into(),
                        )),
                    };
                    state
                        .events
                        .push_back(WaylandEvent::ScreencopyDone { request_id, result });
                }
            }
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                state.screencopy.ext_pending.remove(&request_id);
                state.events.push_back(WaylandEvent::ScreencopyDone {
                    request_id,
                    result: Err(WaywiError::SurfaceError(format!(
                        "ext_image_copy_capture_v1 frame failed: {reason:?}"
                    ))),
                });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_normalizes_all_wl_output_transforms() {
        // Upright 2x2 image:
        //   (0,0) = Red   (1,0) = Green
        //   (0,1) = Blue  (1,1) = White
        // Little-endian ARGB8888 stores [B, G, R, A].
        let px = |r: u8, g: u8, b: u8| [b, g, r, 255u8];
        let red = px(255, 0, 0);
        let green = px(0, 255, 0);
        let blue = px(0, 0, 255);
        let white = px(255, 255, 255);

        let expected = vec![
            255, 0, 0, 255, 0, 255, 0, 255, // row 0: R, G
            0, 0, 255, 255, 255, 255, 255, 255, // row 1: B, W
        ];

        let cases: [(wl_output::Transform, [[u8; 4]; 4]); 8] = [
            (wl_output::Transform::Normal, [red, green, blue, white]),
            (wl_output::Transform::_90, [green, white, red, blue]),
            (wl_output::Transform::_180, [white, blue, green, red]),
            (wl_output::Transform::_270, [blue, red, white, green]),
            (wl_output::Transform::Flipped, [green, red, white, blue]),
            (wl_output::Transform::Flipped90, [red, blue, green, white]),
            (wl_output::Transform::Flipped180, [blue, white, red, green]),
            (wl_output::Transform::Flipped270, [white, green, blue, red]),
        ];

        for (transform, buf_pixels) in cases {
            let raw: Vec<u8> = buf_pixels.into_iter().flatten().collect();
            let frame =
                decode_raw_shm_to_rgba(&raw, wl_shm::Format::Argb8888, 2, 2, 8, transform, None);
            assert_eq!(frame.width, 2, "transform={transform:?}");
            assert_eq!(frame.height, 2, "transform={transform:?}");
            assert_eq!(frame.rgba, expected, "transform={transform:?}");
        }
    }
}
