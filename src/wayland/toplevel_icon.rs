//! Wayland `xdg_toplevel_icon_v1` backend for declarative [`Surface::window`](crate::surface::Surface::window) icons.

use std::collections::HashMap;
use std::ffi::CString;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};

use rustix::fs::{MemfdFlags, memfd_create};
use smithay_client_toolkit::shm::Shm;
use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1::{self, XdgToplevelIconManagerV1},
    xdg_toplevel_icon_v1::{self, XdgToplevelIconV1},
};

use crate::asset::AssetCache;
use crate::surface::WindowIcon;
use crate::wayland::connection::WaylandState;

/// Marker user-data type for one-shot `wl_buffer`s attached to `xdg_toplevel_icon_v1`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToplevelIconBufferData;

struct RetainedIconBuffer {
    buffer: wl_buffer::WlBuffer,
    pool: wl_shm_pool::WlShmPool,
    _fd: OwnedFd,
}

impl Drop for RetainedIconBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

struct AppliedToplevelIcon {
    spec: Option<WindowIcon>,
    _buffer: Option<RetainedIconBuffer>,
}

/// Manages `xdg_toplevel_icon_v1` icons attached to live `xdg_toplevel` windows.
pub(crate) struct WaylandToplevelIconState {
    manager: Option<XdgToplevelIconManagerV1>,
    applied: HashMap<ObjectId, AppliedToplevelIcon>,
}

impl Drop for WaylandToplevelIconState {
    fn drop(&mut self) {
        self.clear_all();
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandToplevelIconState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<XdgToplevelIconManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        Self {
            manager,
            applied: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    /// Synchronizes an `xdg_toplevel` window's icon cleanly (idempotent; skips wire traffic when
    /// unchanged). Pads non-square assets into a square buffer per the `xdg_toplevel_icon_v1` spec.
    pub(crate) fn sync_toplevel(
        &mut self,
        toplevel: &XdgToplevel,
        icon: Option<&WindowIcon>,
        shm: &Shm,
        assets: &AssetCache,
        qh: &QueueHandle<WaylandState>,
    ) {
        let Some(ref mgr) = self.manager else {
            return;
        };
        let tid = toplevel.id();
        if self
            .applied
            .get(&tid)
            .is_some_and(|cur| cur.spec.as_ref() == icon)
        {
            return;
        }

        match icon {
            None => {
                if self.applied.remove(&tid).is_some() {
                    mgr.set_icon(toplevel, None);
                }
            }
            Some(WindowIcon::Name(name)) => {
                if name.is_empty() {
                    if self.applied.remove(&tid).is_some() {
                        mgr.set_icon(toplevel, None);
                    }
                    return;
                }
                let wl_icon = mgr.create_icon(qh, ());
                wl_icon.set_name(name.as_str().to_owned());
                mgr.set_icon(toplevel, Some(&wl_icon));
                wl_icon.destroy();
                self.applied.insert(
                    tid,
                    AppliedToplevelIcon {
                        spec: Some(WindowIcon::Name(name.clone())),
                        _buffer: None,
                    },
                );
            }
            Some(WindowIcon::Asset(asset)) => {
                let Some(decoded) = assets.get(asset.id) else {
                    return;
                };
                let Some(buf_entry) = create_square_icon_buffer(
                    shm,
                    decoded.width,
                    decoded.height,
                    &decoded.pixels,
                    qh,
                ) else {
                    return;
                };
                let wl_icon = mgr.create_icon(qh, ());
                wl_icon.add_buffer(&buf_entry.buffer, 1);
                mgr.set_icon(toplevel, Some(&wl_icon));
                wl_icon.destroy();
                self.applied.insert(
                    tid,
                    AppliedToplevelIcon {
                        spec: Some(WindowIcon::Asset(*asset)),
                        _buffer: Some(buf_entry),
                    },
                );
            }
        }
    }

    pub(crate) fn clear_toplevel(&mut self, toplevel_id: &ObjectId) {
        self.applied.remove(toplevel_id);
    }

    pub(crate) fn clear_all(&mut self) {
        self.applied.clear();
    }
}

fn create_square_icon_buffer(
    shm: &Shm,
    width: u32,
    height: u32,
    rgba_premul: &[u8],
    qh: &QueueHandle<WaylandState>,
) -> Option<RetainedIconBuffer> {
    if width == 0 || height == 0 {
        return None;
    }
    // `xdg_toplevel_icon_v1` requires square buffers; center-pad if non-square and clamp huge images.
    let side = width.max(height).min(512);
    let stride = side.checked_mul(4)?;
    let byte_len = (stride as usize).checked_mul(side as usize)?;
    let mut argb = vec![0u8; byte_len];

    let copy_w = width.min(side) as usize;
    let copy_h = height.min(side) as usize;
    let off_x = ((side as usize) - copy_w) / 2;
    let off_y = ((side as usize) - copy_h) / 2;
    let src_stride = (width as usize).checked_mul(4)?;

    for y in 0..copy_h {
        let src_row = rgba_premul.get(y * src_stride..y * src_stride + copy_w * 4)?;
        let dst_start = (off_y + y) * (stride as usize) + off_x * 4;
        let dst_row = argb.get_mut(dst_start..dst_start + copy_w * 4)?;
        let (src_px, _) = src_row.as_chunks::<4>();
        let (dst_px, _) = dst_row.as_chunks_mut::<4>();
        for (s, d) in src_px.iter().zip(dst_px.iter_mut()) {
            // Premultiplied RGBA8 -> little-endian WL_SHM_FORMAT_ARGB8888 ([B, G, R, A]).
            *d = [s[2], s[1], s[0], s[3]];
        }
    }

    let name = CString::new("waywi-toplevel-icon").ok()?;
    let fd = memfd_create(&name, MemfdFlags::CLOEXEC).ok()?;
    let mut file = std::fs::File::from(fd);
    file.write_all(&argb).ok()?;
    let owned_fd: OwnedFd = file.into();

    let pool = shm
        .wl_shm()
        .create_pool(owned_fd.as_fd(), byte_len as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        side as i32,
        side as i32,
        stride as i32,
        wl_shm::Format::Argb8888,
        qh,
        ToplevelIconBufferData,
    );
    Some(RetainedIconBuffer {
        buffer,
        pool,
        _fd: owned_fd,
    })
}

impl Dispatch<XdgToplevelIconManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &XdgToplevelIconManagerV1,
        _event: xdg_toplevel_icon_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgToplevelIconV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &XdgToplevelIconV1,
        _event: xdg_toplevel_icon_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ToplevelIconBufferData> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_buffer::WlBuffer,
        _event: wl_buffer::Event,
        _data: &ToplevelIconBufferData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
