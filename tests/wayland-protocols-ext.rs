//! Real `wayland-server` `socketpair` wire-protocol integration tests for all 10 Wayland
//! desktop extension protocols:
//! 1. `wp_cursor_shape_v1` (+ `CursorShape::Hidden`)
//! 2. `ext_session_lock_v1` (atomic `ack_configure` on buffer commit, zero `commit_before_first_ack`)
//! 3. `ext_image_copy_capture_v1` + `ext_output_image_capture_source_manager_v1` (with `Transform`)
//! 4. `zwlr_screencopy_manager_v1` (with `y_invert` + pixel format swizzle)
//! 5. `zwlr_foreign_toplevel_management_v1` (coalesced snapshot + control actions)
//! 6. `ext_foreign_toplevel_list_v1` (read-only fallback)
//! 7. `ext_workspace_v1` (coalesced `done` + batched single-`commit` mutations)
//! 8. `wl_data_device_manager` Drag & Drop (zero-thread calloop `finish`-after-EOF + strict MIME rejection)
//! 9. `zxdg_decoration_manager_v1` (default `Server` + CSD/SSD negotiation)
//! 10. `zwp_idle_inhibit_manager_v1` (per-surface inhibitor lifecycle)
//! 11. `zwp_virtual_keyboard_v1` (rewound XKB keymap memfd + key + modifiers)
//! 12. `zwp_pointer_constraints_v1` & `zwp_relative_pointer_v1`

mod common;

use std::collections::HashMap;
#[cfg(feature = "virtual-keyboard")]
use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(feature = "screencopy")]
use common::write_shm_buffer;
use common::{BufferInfo, DummyClientData, PoolInfo, bind_mock_output};
use wayland_client::Connection;
#[cfg(feature = "foreign-toplevel")]
use wayland_protocols::ext::foreign_toplevel_list::v1::server::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
#[cfg(feature = "screencopy")]
use wayland_protocols::ext::image_capture_source::v1::server::{
    ext_image_capture_source_v1, ext_output_image_capture_source_manager_v1,
};
#[cfg(feature = "screencopy")]
use wayland_protocols::ext::image_copy_capture::v1::server::{
    ext_image_copy_capture_frame_v1, ext_image_copy_capture_manager_v1,
    ext_image_copy_capture_session_v1,
};
#[cfg(feature = "session-lock")]
use wayland_protocols::ext::session_lock::v1::server::{
    ext_session_lock_manager_v1, ext_session_lock_surface_v1, ext_session_lock_v1,
};
#[cfg(feature = "ext-workspace")]
use wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1, ext_workspace_handle_v1, ext_workspace_manager_v1,
};
#[cfg(feature = "cursor-shape")]
use wayland_protocols::wp::cursor_shape::v1::server::{
    wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1,
};
#[cfg(feature = "idle-inhibit")]
use wayland_protocols::wp::idle_inhibit::zv1::server::{
    zwp_idle_inhibit_manager_v1, zwp_idle_inhibitor_v1,
};
#[cfg(feature = "pointer-constraints")]
use wayland_protocols::wp::pointer_constraints::zv1::server::{
    zwp_confined_pointer_v1, zwp_locked_pointer_v1, zwp_pointer_constraints_v1,
};
#[cfg(feature = "pointer-constraints")]
use wayland_protocols::wp::relative_pointer::zv1::server::{
    zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1,
};
use wayland_protocols::xdg::decoration::zv1::server::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use wayland_protocols::xdg::shell::server::{xdg_surface, xdg_toplevel, xdg_wm_base};
#[cfg(feature = "virtual-keyboard")]
use wayland_protocols_misc::zwp_virtual_keyboard_v1::server::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
#[cfg(feature = "foreign-toplevel")]
use wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};
use wayland_protocols_wlr::layer_shell::v1::server::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
#[cfg(feature = "screencopy")]
use wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1,
};
use wayland_server::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer,
    wl_data_source, wl_keyboard, wl_output, wl_pointer, wl_region, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use waywi::prelude::*;

#[derive(Default, Clone, Copy)]
struct ExtServerCfg {
    enable_xdg_decoration: bool,
    enable_dnd: bool,
    send_dnd_drop: bool,
    dnd_offer_mime: Option<&'static str>,
    send_cursor_motion: bool,
    #[cfg(feature = "cursor-shape")]
    enable_cursor_shape: bool,
    #[cfg(feature = "session-lock")]
    enable_session_lock: bool,
    #[cfg(feature = "screencopy")]
    enable_ext_screencopy: bool,
    #[cfg(feature = "screencopy")]
    ext_screencopy_transform: Option<wl_output::Transform>,
    #[cfg(feature = "screencopy")]
    enable_wlr_screencopy: bool,
    #[cfg(feature = "foreign-toplevel")]
    enable_wlr_foreign_toplevel: bool,
    #[cfg(feature = "foreign-toplevel")]
    enable_ext_foreign_toplevel: bool,
    #[cfg(feature = "ext-workspace")]
    enable_ext_workspace: bool,
    #[cfg(feature = "idle-inhibit")]
    enable_idle_inhibit: bool,
    #[cfg(feature = "virtual-keyboard")]
    enable_virtual_keyboard: bool,
    #[cfg(feature = "pointer-constraints")]
    enable_pointer_constraints: bool,
    #[cfg(feature = "pointer-constraints")]
    send_relative_motion: bool,
}

#[allow(dead_code)]
#[derive(Default, Debug)]
struct ExtTrace {
    decoration_modes: Vec<u32>,
    cursor_shapes: Vec<u32>,
    cursor_hidden_count: usize,
    session_locks_created: usize,
    session_lock_surfaces_acked: usize,
    session_lock_commit_before_ack: usize,
    session_unlock_and_destroy: usize,
    idle_inhibitors_created: usize,
    idle_inhibitors_destroyed: usize,
    pointer_locks_created: usize,
    pointer_locks_destroyed: usize,
    pointer_confines_created: usize,
    vk_keymaps_received: usize,
    vk_keymap_starts_with_xkb: bool,
    vk_keys_received: Vec<(u32, u32)>,
    vk_modifiers_received: Vec<(u32, u32, u32, u32)>,
    dnd_accepted_mimes: Vec<Option<String>>,
    dnd_receives: usize,
    dnd_finished: usize,
    dnd_finished_after_pipe_closed: bool,
    ft_activated: usize,
    ft_maximized: usize,
    ft_closed: usize,
    ws_activated: usize,
    ws_created: Vec<String>,
    ws_commits: usize,
}

struct ExtMockCompositor {
    cfg: ExtServerCfg,
    trace: Arc<Mutex<ExtTrace>>,
    last_wl_surface: Option<wl_surface::WlSurface>,
    wl_pointer: Option<wl_pointer::WlPointer>,
    data_device: Option<wl_data_device::WlDataDevice>,
    pending_frame_cbs: Vec<wl_callback::WlCallback>,
    pools: HashMap<wayland_server::backend::ObjectId, PoolInfo>,
    buffers: HashMap<wayland_server::backend::ObjectId, BufferInfo>,
    cursor_motion_sent: bool,
    dnd_sent: bool,
    dnd_pipe_closed: bool,
    #[cfg(feature = "session-lock")]
    lock_wl_surface_id: Option<wayland_server::backend::ObjectId>,
    #[cfg(feature = "session-lock")]
    lock_acked: bool,
    #[cfg(feature = "pointer-constraints")]
    rel_pointer: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    #[cfg(feature = "pointer-constraints")]
    rel_motion_sent: bool,
    #[cfg(feature = "screencopy")]
    ext_attached_buf: Option<wayland_server::backend::ObjectId>,
}

// --- Core globals (shared boilerplate via tests/common/mod.rs macros) ---

impl_simple_global!(ExtMockCompositor, wl_compositor::WlCompositor);
impl_simple_global!(ExtMockCompositor, zwlr_layer_shell_v1::ZwlrLayerShellV1);
impl_simple_global!(ExtMockCompositor, xdg_wm_base::XdgWmBase);
impl_simple_global!(
    ExtMockCompositor,
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1
);
impl_simple_global!(
    ExtMockCompositor,
    wl_data_device_manager::WlDataDeviceManager
);

impl_noop_dispatch!(ExtMockCompositor, wl_callback::WlCallback);
impl_noop_dispatch!(ExtMockCompositor, wl_region::WlRegion);
impl_noop_dispatch!(ExtMockCompositor, wl_buffer::WlBuffer);
impl_noop_dispatch!(ExtMockCompositor, wl_output::WlOutput);
impl_noop_dispatch!(ExtMockCompositor, wl_keyboard::WlKeyboard);
impl_noop_dispatch!(ExtMockCompositor, zwlr_layer_surface_v1::ZwlrLayerSurfaceV1);
impl_noop_dispatch!(ExtMockCompositor, xdg_toplevel::XdgToplevel);
impl_noop_dispatch!(ExtMockCompositor, wl_data_device::WlDataDevice);
impl_noop_dispatch!(ExtMockCompositor, wl_data_source::WlDataSource);

impl Dispatch<wl_compositor::WlCompositor, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _c: &Client,
        _r: &wl_compositor::WlCompositor,
        req: wl_compositor::Request,
        _: &(),
        _dh: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_compositor::Request::CreateSurface { id } => {
                state.last_wl_surface = Some(di.init(id, ()));
            }
            wl_compositor::Request::CreateRegion { id } => {
                di.init(id, ());
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_surface::WlSurface, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wl_surface::WlSurface,
        req: wl_surface::Request,
        _: &(),
        dh: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_surface::Request::Frame { callback } => {
                state.pending_frame_cbs.push(di.init(callback, ()));
            }
            wl_surface::Request::Commit => {
                #[cfg(feature = "session-lock")]
                if state.lock_wl_surface_id.as_ref() == Some(&resource.id()) && !state.lock_acked {
                    state.trace.lock().unwrap().session_lock_commit_before_ack += 1;
                }
                if state.cfg.send_cursor_motion
                    && !state.cursor_motion_sent
                    && let Some(ptr) = state.wl_pointer.clone()
                {
                    state.cursor_motion_sent = true;
                    ptr.enter(10, resource, 15.0, 15.0);
                    ptr.frame();
                    ptr.motion(11, 120.0, 15.0);
                    ptr.frame();
                    ptr.motion(12, 220.0, 15.0);
                    ptr.frame();
                }
                if state.cfg.send_dnd_drop
                    && !state.dnd_sent
                    && let Some(dd) = state.data_device.clone()
                {
                    state.dnd_sent = true;
                    let mime = state.cfg.dnd_offer_mime.unwrap_or("text/uri-list");
                    if let Ok(offer) =
                        client.create_resource::<wl_data_offer::WlDataOffer, _, Self>(dh, 3, ())
                    {
                        dd.data_offer(&offer);
                        offer.offer(mime.into());
                        offer.source_actions(wl_data_device_manager::DndAction::Copy);
                        offer.action(wl_data_device_manager::DndAction::Copy);
                        dd.enter(55, resource, 24.0, 32.0, Some(&offer));
                        dd.motion(56, 30.0, 40.0);
                        dd.drop();
                    }
                }
                #[cfg(feature = "pointer-constraints")]
                if state.cfg.send_relative_motion
                    && !state.rel_motion_sent
                    && let (Some(ptr), Some(rp)) =
                        (state.wl_pointer.clone(), state.rel_pointer.clone())
                {
                    state.rel_motion_sent = true;
                    ptr.enter(77, resource, 10.0, 10.0);
                    ptr.frame();
                    rp.relative_motion(0, 123456, 4.5, -2.5, 9.0, -5.0);
                }
            }
            _ => {}
        }
    }
}

impl GlobalDispatch<wl_shm::WlShm, ()> for ExtMockCompositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        r: New<wl_shm::WlShm>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        let shm = di.init(r, ());
        shm.format(wl_shm::Format::Argb8888);
        shm.format(wl_shm::Format::Xrgb8888);
    }
}
impl Dispatch<wl_shm::WlShm, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_shm::WlShm,
        req: wl_shm::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let wl_shm::Request::CreatePool { id, fd, .. } = req {
            let pool = di.init(id, ());
            state.pools.insert(pool.id(), PoolInfo { fd });
        }
    }
}
impl Dispatch<wl_shm_pool::WlShmPool, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &wl_shm_pool::WlShmPool,
        req: wl_shm_pool::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let wl_shm_pool::Request::CreateBuffer {
            id,
            offset,
            width,
            height,
            stride,
            ..
        } = req
        {
            let buf = di.init(id, ());
            state.buffers.insert(
                buf.id(),
                BufferInfo {
                    pool_id: resource.id(),
                    offset: offset as usize,
                    width: width as u32,
                    height: height as u32,
                    stride: stride as u32,
                },
            );
        }
    }
}

impl GlobalDispatch<wl_output::WlOutput, ()> for ExtMockCompositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        r: New<wl_output::WlOutput>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        bind_mock_output(r, di);
    }
}

impl GlobalDispatch<wl_seat::WlSeat, ()> for ExtMockCompositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        r: New<wl_seat::WlSeat>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        let seat = di.init(r, ());
        seat.capabilities(wl_seat::Capability::Pointer | wl_seat::Capability::Keyboard);
        if seat.version() >= 2 {
            seat.name("seat0".into());
        }
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_seat::WlSeat,
        req: wl_seat::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_seat::Request::GetPointer { id } => {
                state.wl_pointer = Some(di.init(id, ()));
            }
            wl_seat::Request::GetKeyboard { id } => {
                di.init(id, ());
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_pointer::WlPointer,
        req: wl_pointer::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let wl_pointer::Request::SetCursor { surface: None, .. } = req {
            state.trace.lock().unwrap().cursor_hidden_count += 1;
        }
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for ExtMockCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        req: zwlr_layer_shell_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_layer_shell_v1::Request::GetLayerSurface { id, .. } = req {
            let ls = di.init(id, ());
            ls.configure(1, 300, 100);
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for ExtMockCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &xdg_wm_base::XdgWmBase,
        req: xdg_wm_base::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let xdg_wm_base::Request::GetXdgSurface { id, .. } = req {
            let xs = di.init(id, ());
            xs.configure(1);
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for ExtMockCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        resource: &xdg_surface::XdgSurface,
        req: xdg_surface::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let xdg_surface::Request::GetToplevel { id } = req {
            let tl = di.init(id, ());
            tl.configure(400, 300, Vec::new());
            resource.configure(2);
        }
    }
}
impl Dispatch<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1, ()> for ExtMockCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
        req: zxdg_decoration_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let zxdg_decoration_manager_v1::Request::GetToplevelDecoration { id, .. } = req {
            let d = di.init(id, ());
            d.configure(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        }
    }
}
impl Dispatch<zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
        req: zxdg_toplevel_decoration_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Request::SetMode { mode } = req
            && let WEnum::Value(m) = mode
        {
            state.trace.lock().unwrap().decoration_modes.push(m as u32);
        }
    }
}

// --- wl_data_device DnD ---

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_data_device_manager::WlDataDeviceManager,
        req: wl_data_device_manager::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_data_device_manager::Request::GetDataDevice { id, .. } => {
                state.data_device = Some(di.init(id, ()));
            }
            wl_data_device_manager::Request::CreateDataSource { id } => {
                di.init(id, ());
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_data_offer::WlDataOffer, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_data_offer::WlDataOffer,
        req: wl_data_offer::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_data_offer::Request::Accept { mime_type, .. } => {
                state
                    .trace
                    .lock()
                    .unwrap()
                    .dnd_accepted_mimes
                    .push(mime_type);
            }
            wl_data_offer::Request::Receive { fd, .. } => {
                state.trace.lock().unwrap().dnd_receives += 1;
                {
                    let mut f = std::fs::File::from(fd);
                    let _ = f.write_all(b"file:///tmp/waywi-dnd-ok.txt");
                }
                state.dnd_pipe_closed = true;
            }
            wl_data_offer::Request::Finish => {
                let mut tr = state.trace.lock().unwrap();
                tr.dnd_finished += 1;
                tr.dnd_finished_after_pipe_closed = state.dnd_pipe_closed;
            }
            _ => {}
        }
    }
}

// --- wp_cursor_shape_v1 ---

#[cfg(feature = "cursor-shape")]
impl_simple_global!(
    ExtMockCompositor,
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1
);
#[cfg(feature = "cursor-shape")]
impl Dispatch<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, ()> for ExtMockCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
        req: wp_cursor_shape_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let wp_cursor_shape_manager_v1::Request::GetPointer {
            cursor_shape_device,
            ..
        } = req
        {
            di.init(cursor_shape_device, ());
        }
    }
}
#[cfg(feature = "cursor-shape")]
impl Dispatch<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
        req: wp_cursor_shape_device_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let wp_cursor_shape_device_v1::Request::SetShape { shape, .. } = req
            && let WEnum::Value(sh) = shape
        {
            state.trace.lock().unwrap().cursor_shapes.push(sh as u32);
        }
    }
}

// --- ext_session_lock_v1 ---

#[cfg(feature = "session-lock")]
impl_simple_global!(
    ExtMockCompositor,
    ext_session_lock_manager_v1::ExtSessionLockManagerV1
);
#[cfg(feature = "session-lock")]
impl Dispatch<ext_session_lock_manager_v1::ExtSessionLockManagerV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ext_session_lock_manager_v1::ExtSessionLockManagerV1,
        req: ext_session_lock_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let ext_session_lock_manager_v1::Request::Lock { id } = req {
            let lock = di.init(id, ());
            state.trace.lock().unwrap().session_locks_created += 1;
            lock.locked();
        }
    }
}
#[cfg(feature = "session-lock")]
impl Dispatch<ext_session_lock_v1::ExtSessionLockV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ext_session_lock_v1::ExtSessionLockV1,
        req: ext_session_lock_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            ext_session_lock_v1::Request::GetLockSurface { id, surface, .. } => {
                state.lock_wl_surface_id = Some(surface.id());
                state.lock_acked = false;
                let ls = di.init(id, ());
                ls.configure(42, 1920, 1080);
            }
            ext_session_lock_v1::Request::UnlockAndDestroy => {
                state.trace.lock().unwrap().session_unlock_and_destroy += 1;
            }
            _ => {}
        }
    }
}
#[cfg(feature = "session-lock")]
impl Dispatch<ext_session_lock_surface_v1::ExtSessionLockSurfaceV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
        req: ext_session_lock_surface_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let ext_session_lock_surface_v1::Request::AckConfigure { .. } = req {
            state.lock_acked = true;
            state.trace.lock().unwrap().session_lock_surfaces_acked += 1;
        }
    }
}

// --- ext_image_copy_capture_v1 & zwlr_screencopy_manager_v1 ---

#[cfg(feature = "screencopy")]
impl_simple_global!(
    ExtMockCompositor,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1
);
#[cfg(feature = "screencopy")]
impl_simple_global!(
    ExtMockCompositor,
    ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1
);
#[cfg(feature = "screencopy")]
impl_simple_global!(
    ExtMockCompositor,
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1
);
#[cfg(feature = "screencopy")]
impl_noop_dispatch!(
    ExtMockCompositor,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1
);

#[cfg(feature = "screencopy")]
impl Dispatch<ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1, ()>
    for ExtMockCompositor
{
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
        req: ext_output_image_capture_source_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource {
            source, ..
        } = req
        {
            di.init(source, ());
        }
    }
}
#[cfg(feature = "screencopy")]
impl Dispatch<ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1, ()>
    for ExtMockCompositor
{
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
        req: ext_image_copy_capture_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_manager_v1::Request::CreateSession { session, .. } = req {
            let s = di.init(session, ());
            s.buffer_size(2, 2);
            s.shm_format(wl_shm::Format::Xrgb8888);
            s.done();
        }
    }
}
#[cfg(feature = "screencopy")]
impl Dispatch<ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1, ()>
    for ExtMockCompositor
{
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
        req: ext_image_copy_capture_session_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_session_v1::Request::CreateFrame { frame } = req {
            di.init(frame, ());
        }
    }
}
#[cfg(feature = "screencopy")]
impl Dispatch<ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1, ()>
    for ExtMockCompositor
{
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
        req: ext_image_copy_capture_frame_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                state.ext_attached_buf = Some(buffer.id());
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                if let Some(bid) = state.ext_attached_buf.take() {
                    // XRGB8888 little-endian memory bytes: [B, G, R, X]
                    write_shm_buffer(&state.pools, &state.buffers, &bid, |x, y| {
                        [30 + x as u8, 20 + y as u8, 10, 0]
                    });
                }
                if let Some(t) = state.cfg.ext_screencopy_transform {
                    resource.transform(t);
                }
                resource.ready();
            }
            _ => {}
        }
    }
}

#[cfg(feature = "screencopy")]
impl Dispatch<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, ()> for ExtMockCompositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
        req: zwlr_screencopy_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        let frame = match req {
            zwlr_screencopy_manager_v1::Request::CaptureOutput { frame, .. }
            | zwlr_screencopy_manager_v1::Request::CaptureOutputRegion { frame, .. } => {
                di.init(frame, ())
            }
            _ => return,
        };
        frame.buffer(wl_shm::Format::Argb8888, 2, 2, 8);
        frame.buffer_done();
    }
}
#[cfg(feature = "screencopy")]
impl Dispatch<zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
        req: zwlr_screencopy_frame_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_screencopy_frame_v1::Request::Copy { buffer } = req {
            write_shm_buffer(&state.pools, &state.buffers, &buffer.id(), |_x, y| {
                if y == 1 {
                    [0, 0, 200, 255]
                } else {
                    [200, 0, 0, 255]
                }
            });
            resource.flags(zwlr_screencopy_frame_v1::Flags::YInvert);
            resource.ready(0, 1, 0);
        }
    }
}

// --- Foreign toplevel (wlr + ext) ---

#[cfg(feature = "foreign-toplevel")]
impl_noop_dispatch!(
    ExtMockCompositor,
    zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1
);
#[cfg(feature = "foreign-toplevel")]
impl_noop_dispatch!(
    ExtMockCompositor,
    ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1
);
#[cfg(feature = "foreign-toplevel")]
impl_noop_dispatch!(
    ExtMockCompositor,
    ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1
);

#[cfg(feature = "foreign-toplevel")]
impl GlobalDispatch<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()>
    for ExtMockCompositor
{
    fn bind(
        _: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        r: New<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        let mgr = di.init(r, ());
        if let Ok(h) = client
            .create_resource::<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, _, Self>(
                dh,
                3,
                (),
            )
        {
            mgr.toplevel(&h);
            h.title("Terminal".into());
            h.app_id("foot".into());
            let activated = (zwlr_foreign_toplevel_handle_v1::State::Activated as u32)
                .to_ne_bytes()
                .to_vec();
            h.state(activated);
            h.done();
        }
    }
}
#[cfg(feature = "foreign-toplevel")]
impl Dispatch<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()>
    for ExtMockCompositor
{
    fn request(
        state: &mut Self,
        _: &Client,
        _: &zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        req: zwlr_foreign_toplevel_handle_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            zwlr_foreign_toplevel_handle_v1::Request::Activate { .. } => {
                state.trace.lock().unwrap().ft_activated += 1;
            }
            zwlr_foreign_toplevel_handle_v1::Request::SetMaximized => {
                state.trace.lock().unwrap().ft_maximized += 1;
            }
            zwlr_foreign_toplevel_handle_v1::Request::Close => {
                state.trace.lock().unwrap().ft_closed += 1;
            }
            _ => {}
        }
    }
}

#[cfg(feature = "foreign-toplevel")]
impl GlobalDispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()>
    for ExtMockCompositor
{
    fn bind(
        _: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        r: New<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        let list = di.init(r, ());
        if let Ok(h) = client
            .create_resource::<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, _, Self>(
                dh,
                1,
                (),
            )
        {
            list.toplevel(&h);
            h.identifier("ext-win-42".into());
            h.title("Browser".into());
            h.app_id("firefox".into());
            h.done();
        }
    }
}

// --- ext_workspace_v1 ---

#[cfg(feature = "ext-workspace")]
impl GlobalDispatch<ext_workspace_manager_v1::ExtWorkspaceManagerV1, ()> for ExtMockCompositor {
    fn bind(
        _: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        r: New<ext_workspace_manager_v1::ExtWorkspaceManagerV1>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        let mgr = di.init(r, ());
        if let (Ok(g), Ok(w)) = (
            client.create_resource::<ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1, _, Self>(
                dh, 1, (),
            ),
            client.create_resource::<ext_workspace_handle_v1::ExtWorkspaceHandleV1, _, Self>(
                dh, 1, (),
            ),
        ) {
            mgr.workspace_group(&g);
            mgr.workspace(&w);
            g.capabilities(ext_workspace_group_handle_v1::GroupCapabilities::CreateWorkspace);
            g.workspace_enter(&w);
            w.id("ws-1".into());
            w.name("1".into());
            w.state(ext_workspace_handle_v1::State::Active);
            w.capabilities(
                ext_workspace_handle_v1::WorkspaceCapabilities::Activate
                    | ext_workspace_handle_v1::WorkspaceCapabilities::Remove,
            );
            mgr.done();
        }
    }
}
#[cfg(feature = "ext-workspace")]
impl Dispatch<ext_workspace_manager_v1::ExtWorkspaceManagerV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ext_workspace_manager_v1::ExtWorkspaceManagerV1,
        req: ext_workspace_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let ext_workspace_manager_v1::Request::Commit = req {
            state.trace.lock().unwrap().ws_commits += 1;
        }
    }
}
#[cfg(feature = "ext-workspace")]
impl Dispatch<ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
        req: ext_workspace_group_handle_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let ext_workspace_group_handle_v1::Request::CreateWorkspace { workspace } = req {
            state.trace.lock().unwrap().ws_created.push(workspace);
        }
    }
}
#[cfg(feature = "ext-workspace")]
impl Dispatch<ext_workspace_handle_v1::ExtWorkspaceHandleV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ext_workspace_handle_v1::ExtWorkspaceHandleV1,
        req: ext_workspace_handle_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let ext_workspace_handle_v1::Request::Activate = req {
            state.trace.lock().unwrap().ws_activated += 1;
        }
    }
}

// --- zwp_idle_inhibit_manager_v1 ---

#[cfg(feature = "idle-inhibit")]
impl_simple_global!(
    ExtMockCompositor,
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1
);
#[cfg(feature = "idle-inhibit")]
impl Dispatch<zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
        req: zwp_idle_inhibit_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let zwp_idle_inhibit_manager_v1::Request::CreateInhibitor { id, .. } = req {
            di.init(id, ());
            state.trace.lock().unwrap().idle_inhibitors_created += 1;
        }
    }
}
#[cfg(feature = "idle-inhibit")]
impl Dispatch<zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
        req: zwp_idle_inhibitor_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zwp_idle_inhibitor_v1::Request::Destroy = req {
            state.trace.lock().unwrap().idle_inhibitors_destroyed += 1;
        }
    }
}

// --- zwp_virtual_keyboard_v1 ---

#[cfg(feature = "virtual-keyboard")]
impl_simple_global!(
    ExtMockCompositor,
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1
);
#[cfg(feature = "virtual-keyboard")]
impl Dispatch<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ()>
    for ExtMockCompositor
{
    fn request(
        _: &mut Self,
        _: &Client,
        _: &zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        req: zwp_virtual_keyboard_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let zwp_virtual_keyboard_manager_v1::Request::CreateVirtualKeyboard { id, .. } = req {
            di.init(id, ());
        }
    }
}
#[cfg(feature = "virtual-keyboard")]
impl Dispatch<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        req: zwp_virtual_keyboard_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            zwp_virtual_keyboard_v1::Request::Keymap { fd, size, .. } => {
                assert!(size > 16);
                let mut text = String::new();
                let mut f = std::fs::File::from(fd);
                let _ = f.read_to_string(&mut text);
                let mut tr = state.trace.lock().unwrap();
                tr.vk_keymaps_received += 1;
                tr.vk_keymap_starts_with_xkb = text.trim_start().starts_with("xkb_keymap");
            }
            zwp_virtual_keyboard_v1::Request::Key { key, state: st, .. } => {
                state.trace.lock().unwrap().vk_keys_received.push((key, st));
            }
            zwp_virtual_keyboard_v1::Request::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                state.trace.lock().unwrap().vk_modifiers_received.push((
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    group,
                ));
            }
            _ => {}
        }
    }
}

// --- zwp_pointer_constraints_v1 & zwp_relative_pointer_v1 ---

#[cfg(feature = "pointer-constraints")]
impl_simple_global!(
    ExtMockCompositor,
    zwp_pointer_constraints_v1::ZwpPointerConstraintsV1
);
#[cfg(feature = "pointer-constraints")]
impl_simple_global!(
    ExtMockCompositor,
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1
);
#[cfg(feature = "pointer-constraints")]
impl_noop_dispatch!(
    ExtMockCompositor,
    zwp_confined_pointer_v1::ZwpConfinedPointerV1
);
#[cfg(feature = "pointer-constraints")]
impl_noop_dispatch!(
    ExtMockCompositor,
    zwp_relative_pointer_v1::ZwpRelativePointerV1
);

#[cfg(feature = "pointer-constraints")]
impl Dispatch<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &zwp_pointer_constraints_v1::ZwpPointerConstraintsV1,
        req: zwp_pointer_constraints_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            zwp_pointer_constraints_v1::Request::LockPointer { id, .. } => {
                let lp = di.init(id, ());
                state.trace.lock().unwrap().pointer_locks_created += 1;
                lp.locked();
            }
            zwp_pointer_constraints_v1::Request::ConfinePointer { id, .. } => {
                let cp = di.init(id, ());
                state.trace.lock().unwrap().pointer_confines_created += 1;
                cp.confined();
            }
            _ => {}
        }
    }
}
#[cfg(feature = "pointer-constraints")]
impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ()> for ExtMockCompositor {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &zwp_locked_pointer_v1::ZwpLockedPointerV1,
        req: zwp_locked_pointer_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zwp_locked_pointer_v1::Request::Destroy = req {
            state.trace.lock().unwrap().pointer_locks_destroyed += 1;
        }
    }
}
#[cfg(feature = "pointer-constraints")]
impl Dispatch<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, ()>
    for ExtMockCompositor
{
    fn request(
        state: &mut Self,
        _: &Client,
        _: &zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        req: zwp_relative_pointer_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        if let zwp_relative_pointer_manager_v1::Request::GetRelativePointer { id, .. } = req {
            state.rel_pointer = Some(di.init(id, ()));
        }
    }
}

fn spawn_ext_mock(
    cfg: ExtServerCfg,
) -> (
    Connection,
    Arc<Mutex<ExtTrace>>,
    Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let (server_sock, client_sock) = UnixStream::pair().expect("socketpair");
    let trace = Arc::new(Mutex::new(ExtTrace::default()));
    let trace_srv = Arc::clone(&trace);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_srv = Arc::clone(&stop);

    let handle = std::thread::spawn(move || {
        let mut display: Display<ExtMockCompositor> = Display::new().unwrap();
        let mut dh = display.handle();
        dh.create_global::<ExtMockCompositor, wl_compositor::WlCompositor, _>(5, ());
        dh.create_global::<ExtMockCompositor, wl_shm::WlShm, _>(1, ());
        dh.create_global::<ExtMockCompositor, wl_output::WlOutput, _>(4, ());
        dh.create_global::<ExtMockCompositor, wl_seat::WlSeat, _>(7, ());
        dh.create_global::<ExtMockCompositor, zwlr_layer_shell_v1::ZwlrLayerShellV1, _>(4, ());
        dh.create_global::<ExtMockCompositor, xdg_wm_base::XdgWmBase, _>(5, ());

        if cfg.enable_xdg_decoration {
            dh.create_global::<ExtMockCompositor, zxdg_decoration_manager_v1::ZxdgDecorationManagerV1, _>(1, ());
        }
        if cfg.enable_dnd {
            dh.create_global::<ExtMockCompositor, wl_data_device_manager::WlDataDeviceManager, _>(
                3,
                (),
            );
        }
        #[cfg(feature = "cursor-shape")]
        if cfg.enable_cursor_shape {
            dh.create_global::<ExtMockCompositor, wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, _>(2, ());
        }
        #[cfg(feature = "session-lock")]
        if cfg.enable_session_lock {
            dh.create_global::<ExtMockCompositor, ext_session_lock_manager_v1::ExtSessionLockManagerV1, _>(1, ());
        }
        #[cfg(feature = "screencopy")]
        if cfg.enable_ext_screencopy {
            dh.create_global::<ExtMockCompositor, ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1, _>(1, ());
            dh.create_global::<ExtMockCompositor, ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1, _>(1, ());
        }
        #[cfg(feature = "screencopy")]
        if cfg.enable_wlr_screencopy {
            dh.create_global::<ExtMockCompositor, zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, _>(3, ());
        }
        #[cfg(feature = "foreign-toplevel")]
        if cfg.enable_wlr_foreign_toplevel {
            dh.create_global::<ExtMockCompositor, zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, _>(3, ());
        }
        #[cfg(feature = "foreign-toplevel")]
        if cfg.enable_ext_foreign_toplevel {
            dh.create_global::<ExtMockCompositor, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, _>(1, ());
        }
        #[cfg(feature = "ext-workspace")]
        if cfg.enable_ext_workspace {
            dh.create_global::<ExtMockCompositor, ext_workspace_manager_v1::ExtWorkspaceManagerV1, _>(1, ());
        }
        #[cfg(feature = "idle-inhibit")]
        if cfg.enable_idle_inhibit {
            dh.create_global::<ExtMockCompositor, zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, _>(1, ());
        }
        #[cfg(feature = "virtual-keyboard")]
        if cfg.enable_virtual_keyboard {
            dh.create_global::<ExtMockCompositor, zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, _>(1, ());
        }
        #[cfg(feature = "pointer-constraints")]
        if cfg.enable_pointer_constraints {
            dh.create_global::<ExtMockCompositor, zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, _>(1, ());
            dh.create_global::<ExtMockCompositor, zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, _>(1, ());
        }

        let _ = dh.insert_client(server_sock, Arc::new(DummyClientData));
        let mut state = ExtMockCompositor {
            cfg,
            trace: trace_srv,
            last_wl_surface: None,
            wl_pointer: None,
            data_device: None,
            pending_frame_cbs: Vec::new(),
            pools: HashMap::new(),
            buffers: HashMap::new(),
            cursor_motion_sent: false,
            dnd_sent: false,
            dnd_pipe_closed: false,
            #[cfg(feature = "session-lock")]
            lock_wl_surface_id: None,
            #[cfg(feature = "session-lock")]
            lock_acked: false,
            #[cfg(feature = "pointer-constraints")]
            rel_pointer: None,
            #[cfg(feature = "pointer-constraints")]
            rel_motion_sent: false,
            #[cfg(feature = "screencopy")]
            ext_attached_buf: None,
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while !stop_srv.load(std::sync::atomic::Ordering::Relaxed)
            && std::time::Instant::now() < deadline
        {
            let _ = display.dispatch_clients(&mut state);
            for cb in state.pending_frame_cbs.drain(..) {
                cb.done(16);
            }
            let _ = display.flush_clients();
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = display.dispatch_clients(&mut state);
        let _ = display.flush_clients();
    });

    let conn = Connection::from_socket(client_sock).expect("client connection");
    (conn, trace, stop, handle)
}

#[cfg(feature = "cursor-shape")]
#[test]
fn cursor_shape_and_hidden_cursor_wire_protocol() {
    struct State;
    #[derive(Clone)]
    enum Msg {
        Quit,
    }
    struct CursorApp;
    impl Application for CursorApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::CursorShape));
            ctx.after(Duration::from_millis(120), Msg::Quit);
            Ok(State)
        }
        fn update(
            _: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::Quit => ctx.quit(),
            }
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(
                Surface::floating("cur", Output::Primary)
                    .size((300.logical_px(), 100.logical_px()))
                    .cursor(CursorShape::Crosshair)
                    .child(
                        Rect::new()
                            .position(Position::Absolute {
                                x: 100.logical_px(),
                                y: 0.logical_px(),
                            })
                            .size((80.logical_px(), 100.logical_px()))
                            .cursor(CursorShape::Pointer),
                    )
                    .child(
                        Rect::new()
                            .position(Position::Absolute {
                                x: 200.logical_px(),
                                y: 0.logical_px(),
                            })
                            .size((80.logical_px(), 100.logical_px()))
                            .cursor(CursorShape::Hidden),
                    ),
            );
            s
        }
    }

    let (conn, trace, stop, srv) = spawn_ext_mock(ExtServerCfg {
        enable_cursor_shape: true,
        send_cursor_motion: true,
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<CursorApp>(conn)
        .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv.join();
    let tr = trace.lock().unwrap();
    assert!(
        tr.cursor_shapes
            .contains(&(wp_cursor_shape_device_v1::Shape::Crosshair as u32))
    );
    assert!(
        tr.cursor_shapes
            .contains(&(wp_cursor_shape_device_v1::Shape::Pointer as u32))
    );
    assert!(tr.cursor_hidden_count >= 1);
}

#[cfg(feature = "session-lock")]
#[test]
fn session_lock_locks_acks_on_commit_and_unlocks_cleanly() {
    static LOCKED_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    struct State {
        locked: bool,
    }
    #[derive(Clone)]
    enum Msg {
        Lock(SessionLockEvent),
        Unlock,
        Quit,
    }
    struct LockApp;
    impl Application for LockApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::SessionLock));
            Ok(State { locked: true })
        }
        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::Lock(SessionLockEvent::Locked) => {
                    LOCKED_SEEN.store(true, std::sync::atomic::Ordering::SeqCst);
                    ctx.after(Duration::from_millis(60), Msg::Unlock);
                }
                Msg::Lock(SessionLockEvent::Finished) => ctx.quit(),
                Msg::Unlock => {
                    state.locked = false;
                    ctx.after(Duration::from_millis(40), Msg::Quit);
                }
                Msg::Quit => ctx.quit(),
            }
            Ok(())
        }
        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            if state.locked {
                s.push(Surface::session_lock("lock", Output::All).background(Color::BLACK));
            }
            s
        }
        fn subscriptions(_: &Self::State) -> Subscriptions<Self::Message> {
            let mut subs = Subscriptions::new();
            subs.push(SessionLock::subscribe(Msg::Lock));
            subs
        }
    }

    let (conn, trace, stop, srv) = spawn_ext_mock(ExtServerCfg {
        enable_session_lock: true,
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<LockApp>(conn)
        .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv.join();
    assert!(LOCKED_SEEN.load(std::sync::atomic::Ordering::SeqCst));
    let tr = trace.lock().unwrap();
    assert_eq!(tr.session_locks_created, 1);
    assert_eq!(tr.session_lock_surfaces_acked, 1);
    assert_eq!(
        tr.session_lock_commit_before_ack, 0,
        "ext_session_lock_surface_v1 must never commit before ack_configure"
    );
    assert_eq!(tr.session_unlock_and_destroy, 1);
}

#[cfg(feature = "screencopy")]
#[test]
fn screencopy_ext_with_transform_and_wlr_y_invert() {
    static EXT_PIXEL: Mutex<Option<[u8; 4]>> = Mutex::new(None);
    static WLR_TOP_PIXEL: Mutex<Option<[u8; 4]>> = Mutex::new(None);

    struct State;
    #[derive(Clone)]
    enum Msg {
        Captured(std::result::Result<ScreencopyFrame, String>),
    }
    struct ExtCapApp;
    impl Application for ExtCapApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::Screencopy));
            ctx.capture_screen(ScreencopyTarget::Output(Output::Primary), true, |res| {
                Msg::Captured(res.map_err(|e| e.to_string()))
            });
            Ok(State)
        }
        fn update(
            _: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            let Msg::Captured(res) = msg;
            let frame = res.expect("ext screencopy ok");
            // With Transform::_180 on 2x2 buffer where buffer(x,y) = [B=30+x, G=20+y, R=10, X=0]:
            // upright (0,0) reads buffer (1,1) = [B=31, G=21, R=10] -> RGBA [10, 21, 31, 255].
            *EXT_PIXEL.lock().unwrap() = frame.pixel_rgba(0, 0);
            ctx.quit();
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::new()
        }
    }

    let (conn, _trace, stop, srv) = spawn_ext_mock(ExtServerCfg {
        enable_ext_screencopy: true,
        ext_screencopy_transform: Some(wl_output::Transform::_180),
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<ExtCapApp>(conn)
        .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv.join();
    assert_eq!(*EXT_PIXEL.lock().unwrap(), Some([10, 21, 31, 255]));

    struct WlrCapApp;
    impl Application for WlrCapApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::Screencopy));
            ctx.capture_screen(ScreencopyTarget::Output(Output::Primary), false, |res| {
                Msg::Captured(res.map_err(|e| e.to_string()))
            });
            Ok(State)
        }
        fn update(
            _: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            let Msg::Captured(res) = msg;
            let frame = res.expect("wlr screencopy ok");
            *WLR_TOP_PIXEL.lock().unwrap() = frame.pixel_rgba(0, 0);
            ctx.quit();
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::new()
        }
    }

    let (conn2, _trace2, stop2, srv2) = spawn_ext_mock(ExtServerCfg {
        enable_wlr_screencopy: true,
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<WlrCapApp>(conn2)
        .unwrap();
    stop2.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv2.join();
    assert_eq!(*WLR_TOP_PIXEL.lock().unwrap(), Some([200, 0, 0, 255]));
}

#[cfg(feature = "foreign-toplevel")]
#[test]
fn foreign_toplevel_wlr_control_and_ext_list_fallback() {
    static WLR_SNAP: Mutex<Option<ForeignToplevelSnapshot>> = Mutex::new(None);
    static EXT_SNAP: Mutex<Option<ForeignToplevelSnapshot>> = Mutex::new(None);

    struct State {
        acted: bool,
    }
    #[derive(Clone)]
    enum Msg {
        Snap(ForeignToplevelSnapshot),
        Quit,
    }
    struct WlrFtApp;
    impl Application for WlrFtApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::ForeignToplevel));
            assert!(ctx.capabilities().has(Capability::ForeignToplevelControl));
            Ok(State { acted: false })
        }
        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::Snap(snap) => {
                    if !state.acted
                        && let Some(tl) = snap.toplevels.first()
                    {
                        state.acted = true;
                        ctx.activate_toplevel(tl.id)?;
                        ctx.set_toplevel_maximized(tl.id, true)?;
                        ctx.close_toplevel(tl.id)?;
                        *WLR_SNAP.lock().unwrap() = Some(snap);
                        ctx.after(Duration::from_millis(40), Msg::Quit);
                    }
                }
                Msg::Quit => ctx.quit(),
            }
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::new()
        }
        fn subscriptions(_: &Self::State) -> Subscriptions<Self::Message> {
            let mut s = Subscriptions::new();
            s.push(ForeignToplevel::subscribe(Msg::Snap));
            s
        }
    }

    let (conn, trace, stop, srv) = spawn_ext_mock(ExtServerCfg {
        enable_wlr_foreign_toplevel: true,
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<WlrFtApp>(conn)
        .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv.join();
    let snap = WLR_SNAP.lock().unwrap().clone().expect("wlr snapshot");
    assert!(snap.can_control);
    assert_eq!(snap.toplevels[0].title, "Terminal");
    assert_eq!(snap.toplevels[0].app_id, "foot");
    assert!(
        snap.toplevels[0]
            .state
            .contains(ForeignToplevelState::ACTIVATED)
    );
    let tr = trace.lock().unwrap();
    assert_eq!(tr.ft_activated, 1);
    assert_eq!(tr.ft_maximized, 1);
    assert_eq!(tr.ft_closed, 1);

    struct ExtFtApp;
    impl Application for ExtFtApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::ForeignToplevel));
            assert!(!ctx.capabilities().has(Capability::ForeignToplevelControl));
            Ok(State { acted: false })
        }
        fn update(
            _: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            if let Msg::Snap(snap) = msg
                && !snap.toplevels.is_empty()
            {
                *EXT_SNAP.lock().unwrap() = Some(snap);
                ctx.quit();
            }
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::new()
        }
        fn subscriptions(_: &Self::State) -> Subscriptions<Self::Message> {
            let mut s = Subscriptions::new();
            s.push(ForeignToplevel::subscribe(Msg::Snap));
            s
        }
    }

    let (conn2, _tr2, stop2, srv2) = spawn_ext_mock(ExtServerCfg {
        enable_ext_foreign_toplevel: true,
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<ExtFtApp>(conn2)
        .unwrap();
    stop2.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv2.join();
    let esnap = EXT_SNAP.lock().unwrap().clone().expect("ext snapshot");
    assert!(!esnap.can_control);
    assert_eq!(esnap.toplevels[0].identifier.as_deref(), Some("ext-win-42"));
    assert_eq!(esnap.toplevels[0].app_id, "firefox");
}

#[cfg(feature = "ext-workspace")]
#[test]
fn ext_workspace_atomic_snapshot_and_single_commit_batching() {
    static WS_SNAP: Mutex<Option<ExtWorkspaceSnapshot>> = Mutex::new(None);
    struct State {
        acted: bool,
    }
    #[derive(Clone)]
    enum Msg {
        Snap(ExtWorkspaceSnapshot),
        Quit,
    }
    struct WsApp;
    impl Application for WsApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::ExtWorkspace));
            Ok(State { acted: false })
        }
        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::Snap(snap) => {
                    if !state.acted && !snap.workspaces.is_empty() && !snap.groups.is_empty() {
                        state.acted = true;
                        ctx.activate_ext_workspace(snap.workspaces[0].id)?;
                        ctx.create_ext_workspace(snap.groups[0].id, "dev")?;
                        *WS_SNAP.lock().unwrap() = Some(snap);
                        ctx.after(Duration::from_millis(40), Msg::Quit);
                    }
                }
                Msg::Quit => ctx.quit(),
            }
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::new()
        }
        fn subscriptions(_: &Self::State) -> Subscriptions<Self::Message> {
            let mut s = Subscriptions::new();
            s.push(ExtWorkspace::subscribe(Msg::Snap));
            s
        }
    }

    let (conn, trace, stop, srv) = spawn_ext_mock(ExtServerCfg {
        enable_ext_workspace: true,
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<WsApp>(conn)
        .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv.join();
    let snap = WS_SNAP.lock().unwrap().clone().expect("ws snap");
    assert_eq!(snap.workspaces[0].name, "1");
    assert!(snap.workspaces[0].state.contains(ExtWorkspaceState::ACTIVE));
    assert!(snap.groups[0].can_create_workspace);
    let tr = trace.lock().unwrap();
    assert_eq!(tr.ws_activated, 1);
    assert_eq!(tr.ws_created, vec!["dev".to_string()]);
    assert_eq!(
        tr.ws_commits, 1,
        "multiple ext_workspace mutations in one update must batch into 1 atomic commit()"
    );
}

#[test]
fn dnd_finish_after_eof_rejection_and_default_server_decoration() {
    static DROPPED: Mutex<Option<(String, String)>> = Mutex::new(None);
    struct State {
        use_client_deco: bool,
    }
    #[derive(Clone)]
    enum Msg {
        Dnd(DndEvent),
        Quit,
    }
    struct DndDecoApp;
    impl Application for DndDecoApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::DragAndDrop));
            assert!(ctx.capabilities().has(Capability::XdgDecoration));
            ctx.after(Duration::from_millis(1500), Msg::Quit);
            Ok(State {
                use_client_deco: false,
            })
        }
        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::Dnd(DndEvent::Drop {
                    mime_type, data, ..
                }) => {
                    *DROPPED.lock().unwrap() =
                        Some((mime_type, String::from_utf8_lossy(&data).into_owned()));
                    state.use_client_deco = true;
                    ctx.after(Duration::from_millis(60), Msg::Quit);
                }
                Msg::Dnd(_) => {}
                Msg::Quit => ctx.quit(),
            }
            Ok(())
        }
        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            let mut win = Surface::window("drop-win")
                .size((200.logical_px(), 120.logical_px()))
                .dnd_mime_types(["text/uri-list"])
                .on_dnd(|ev| Some(Msg::Dnd(ev)));
            if state.use_client_deco {
                win = win.decoration(WindowDecoration::Client);
            }
            s.push(win);
            s
        }
    }

    let (conn, trace, stop, srv) = spawn_ext_mock(ExtServerCfg {
        enable_xdg_decoration: true,
        enable_dnd: true,
        send_dnd_drop: true,
        dnd_offer_mime: Some("text/uri-list"),
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<DndDecoApp>(conn)
        .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv.join();
    assert_eq!(
        *DROPPED.lock().unwrap(),
        Some((
            "text/uri-list".into(),
            "file:///tmp/waywi-dnd-ok.txt".into()
        ))
    );
    let tr = trace.lock().unwrap();
    // Default `Surface::window` MUST request ServerSide first, then switch to ClientSide!
    assert_eq!(
        tr.decoration_modes.first().copied(),
        Some(zxdg_toplevel_decoration_v1::Mode::ServerSide as u32),
        "default Surface::window decoration must request ServerSide (no regression)"
    );
    assert!(
        tr.decoration_modes
            .contains(&(zxdg_toplevel_decoration_v1::Mode::ClientSide as u32))
    );
    assert!(
        tr.dnd_accepted_mimes
            .contains(&Some("text/uri-list".into()))
    );
    assert_eq!(tr.dnd_receives, 1);
    assert_eq!(tr.dnd_finished, 1);
    assert!(
        tr.dnd_finished_after_pipe_closed,
        "wl_data_offer.finish() must be sent strictly AFTER pipe read EOF"
    );

    // Second check: surface WITHOUT `.on_dnd(...)` must reject offer (`accept(None)`), 0 receives, 0 finish!
    struct NoDndApp;
    impl Application for NoDndApp {
        type State = ();
        type Message = ();
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            ctx.after(Duration::from_millis(80), ());
            Ok(())
        }
        fn update(
            _: &mut Self::State,
            _: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            ctx.quit();
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(Surface::window("passive-win").size((200.logical_px(), 120.logical_px())));
            s
        }
    }

    let (conn2, trace2, stop2, srv2) = spawn_ext_mock(ExtServerCfg {
        enable_dnd: true,
        send_dnd_drop: true,
        dnd_offer_mime: Some("text/uri-list"),
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<NoDndApp>(conn2)
        .unwrap();
    stop2.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv2.join();
    let tr2 = trace2.lock().unwrap();
    assert_eq!(tr2.dnd_accepted_mimes, vec![None]);
    assert_eq!(tr2.dnd_receives, 0);
    assert_eq!(tr2.dnd_finished, 0);
}

#[cfg(all(
    feature = "idle-inhibit",
    feature = "virtual-keyboard",
    feature = "pointer-constraints"
))]
#[test]
fn idle_inhibit_virtual_keyboard_and_pointer_constraints_wire_protocols() {
    static REL_EVENT: Mutex<Option<RelativePointerEvent>> = Mutex::new(None);
    struct State {
        inhibit: bool,
        constraint: PointerConstraintMode,
        toggled: bool,
    }
    #[derive(Clone)]
    enum Msg {
        Rel(RelativePointerEvent),
        Quit,
    }
    struct MediumProtoApp;
    impl Application for MediumProtoApp {
        type State = State;
        type Message = Msg;
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::IdleInhibit));
            assert!(ctx.capabilities().has(Capability::VirtualKeyboard));
            assert!(ctx.capabilities().has(Capability::PointerConstraints));
            assert!(ctx.capabilities().has(Capability::RelativePointer));
            ctx.virtual_key(30, VirtualKeyState::Pressed)?;
            ctx.virtual_key(30, VirtualKeyState::Released)?;
            ctx.virtual_modifiers(1, 0, 0, 0)?;
            ctx.after(Duration::from_millis(1500), Msg::Quit);
            Ok(State {
                inhibit: true,
                constraint: PointerConstraintMode::Locked,
                toggled: false,
            })
        }
        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::Rel(ev) => {
                    *REL_EVENT.lock().unwrap() = Some(ev);
                    if !state.toggled {
                        state.toggled = true;
                        state.inhibit = false;
                        state.constraint = PointerConstraintMode::Confined;
                        ctx.after(Duration::from_millis(60), Msg::Quit);
                    }
                }
                Msg::Quit => ctx.quit(),
            }
            Ok(())
        }
        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(
                Surface::floating("ctrl", Output::Primary)
                    .size((200.logical_px(), 100.logical_px()))
                    .inhibit_idle(state.inhibit)
                    .pointer_constraint(state.constraint)
                    .on_relative_pointer(|ev| Some(Msg::Rel(ev))),
            );
            s
        }
    }

    let (conn, trace, stop, srv) = spawn_ext_mock(ExtServerCfg {
        enable_idle_inhibit: true,
        enable_virtual_keyboard: true,
        enable_pointer_constraints: true,
        send_relative_motion: true,
        ..Default::default()
    });
    App::new()
        .renderer(RendererPreference::Software)
        .run_on_connection::<MediumProtoApp>(conn)
        .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv.join();

    let rel = REL_EVENT.lock().unwrap().expect("relative motion received");
    assert_eq!(
        (rel.dx, rel.dy, rel.dx_unaccel, rel.dy_unaccel),
        (4.5, -2.5, 9.0, -5.0)
    );

    let tr = trace.lock().unwrap();
    assert_eq!(tr.idle_inhibitors_created, 1);
    assert_eq!(tr.idle_inhibitors_destroyed, 1);
    assert_eq!(tr.pointer_locks_created, 1);
    assert_eq!(tr.pointer_locks_destroyed, 1);
    assert_eq!(tr.pointer_confines_created, 1);
    assert_eq!(tr.vk_keymaps_received, 1);
    assert!(
        tr.vk_keymap_starts_with_xkb,
        "virtual keyboard keymap memfd must be rewound to offset 0 before vk.keymap()"
    );
    assert_eq!(tr.vk_keys_received, vec![(30, 1), (30, 0)]);
    assert_eq!(tr.vk_modifiers_received, vec![(1, 0, 0, 0)]);
}
