use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wayland_client::Connection;
#[cfg(feature = "ime")]
use wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use wayland_protocols::xdg::activation::v1::server::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols::xdg::shell::server::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};
use wayland_protocols_wlr::layer_shell::v1::server::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use wayland_server::backend::ClientData;
use wayland_server::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_region, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
};
use waywi::capabilities::Capability;
use waywi::display::Output;
use waywi::effect::EffectCtx;
use waywi::error::{Result, WaywiError};
use waywi::geometry::{Bounds, LogicalPxExt, ScaleFactor};
#[cfg(feature = "ime")]
use waywi::input::{ImeConfig, ImeContentPurpose, ImeEvent, TextInputState};
use waywi::surface::{
    Anchor, PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupPositioner, Surface,
    Surfaces,
};
use waywi::{App, Application};

#[cfg(feature = "ime")]
#[derive(Default)]
struct MockImeTrace {
    enabled_commits: usize,
    disabled_commits: usize,
    cursor_rects: Vec<(i32, i32, i32, i32)>,
    surrounding_updates: Vec<(String, i32, i32)>,
    content_purposes: Vec<u32>,
    sent_batches: bool,
}

#[derive(Default, Clone, Debug)]
struct MockPositionerSnapshot {
    size: Option<(i32, i32)>,
    anchor_rect: Option<(i32, i32, i32, i32)>,
    anchor: Option<u32>,
    gravity: Option<u32>,
    offset: Option<(i32, i32)>,
    constraint_adjustment: Option<u32>,
    reactive: bool,
}

#[derive(Default, Debug)]
struct MockXdgTrace {
    token_app_ids: Vec<String>,
    token_commits: usize,
    activations: Vec<String>,
    layer_get_popup_before_commit: Option<bool>,
    popup_created_positioners: Vec<MockPositionerSnapshot>,
    popup_reposition_tokens: Vec<u32>,
    popup_reposition_offsets: Vec<Option<(i32, i32)>>,
    popup_destroyed: bool,
    popup_null_buffer_attached: bool,
    layer_surfaces_created: usize,
    layer_closed_sent: usize,
    buffer_commits: usize,
    frame_callbacks_created: usize,
}

#[derive(Default, Clone, Copy)]
struct MockServerConfig {
    enable_xdg_shell: bool,
    enable_layer_shell: bool,
    enable_xdg_activation: bool,
    simulate_output_hotplug_on_two_layers: bool,
    #[cfg(feature = "ime")]
    enable_ime: bool,
}

struct DummyClientData;
impl ClientData for DummyClientData {
    fn initialized(&self, _client_id: wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        _client_id: wayland_server::backend::ClientId,
        _reason: wayland_server::backend::DisconnectReason,
    ) {
    }
}

struct MockCompositor {
    wl_surfaces_created: usize,
    last_wl_surface: Option<wl_surface::WlSurface>,
    popup_wl_surface_id: Option<wayland_server::backend::ObjectId>,
    popup_committed: bool,
    popup_xdg_surface: Option<xdg_surface::XdgSurface>,
    popup_resource: Option<xdg_popup::XdgPopup>,
    popup_done_sent: bool,
    layer_surfaces: Vec<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    output_global: Option<wayland_server::backend::GlobalId>,
    hotplug_triggered: bool,
    simulate_output_hotplug_on_two_layers: bool,
    current_positioner: MockPositionerSnapshot,
    positioners:
        std::collections::HashMap<wayland_server::backend::ObjectId, MockPositionerSnapshot>,
    xdg_surface_to_wl: std::collections::HashMap<
        wayland_server::backend::ObjectId,
        wayland_server::backend::ObjectId,
    >,
    pending_token_app_id: Option<String>,
    xdg_trace: Arc<Mutex<MockXdgTrace>>,
    #[cfg(feature = "ime")]
    text_inputs: Vec<zwp_text_input_v3::ZwpTextInputV3>,
    #[cfg(feature = "ime")]
    pending_enabled: Option<bool>,
    #[cfg(feature = "ime")]
    pending_cursor_rect: Option<(i32, i32, i32, i32)>,
    #[cfg(feature = "ime")]
    pending_surrounding: Option<(String, i32, i32)>,
    #[cfg(feature = "ime")]
    pending_purpose: Option<u32>,
    #[cfg(feature = "ime")]
    ime_trace: Arc<Mutex<MockImeTrace>>,
}

impl GlobalDispatch<wl_compositor::WlCompositor, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<wl_compositor::WlCompositor>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                state.wl_surfaces_created += 1;
                let surf = data_init.init(id, ());
                state.last_wl_surface = Some(surf);
            }
            wl_compositor::Request::CreateRegion { id } => {
                data_init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &wl_surface::WlSurface,
        request: wl_surface::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_surface::Request::Frame { callback } => {
                data_init.init(callback, ());
                state.xdg_trace.lock().unwrap().frame_callbacks_created += 1;
            }
            wl_surface::Request::Attach {
                buffer: Some(_), ..
            } => {
                state.xdg_trace.lock().unwrap().buffer_commits += 1;
            }
            wl_surface::Request::Attach { buffer: None, .. }
                if state.popup_wl_surface_id.as_ref() == Some(&resource.id()) =>
            {
                state.xdg_trace.lock().unwrap().popup_null_buffer_attached = true;
            }
            wl_surface::Request::Commit
                if state.popup_wl_surface_id.as_ref() == Some(&resource.id()) =>
            {
                state.popup_committed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wayland_server::protocol::wl_callback::WlCallback, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wayland_server::protocol::wl_callback::WlCallback,
        _request: wayland_server::protocol::wl_callback::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<wl_region::WlRegion, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_region::WlRegion,
        _request: wl_region::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<wl_shm::WlShm, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<wl_shm::WlShm>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let shm = data_init.init(resource, ());
        shm.format(wl_shm::Format::Argb8888);
    }
}

impl Dispatch<wl_shm::WlShm, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_shm::WlShm,
        request: wl_shm::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_shm::Request::CreatePool { id, .. } = request {
            data_init.init(id, ());
        }
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_shm_pool::WlShmPool,
        request: wl_shm_pool::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_shm_pool::Request::CreateBuffer { id, .. } = request {
            data_init.init(id, ());
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_buffer::WlBuffer,
        _request: wl_buffer::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<wl_output::WlOutput, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<wl_output::WlOutput>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let out = data_init.init(resource, ());
        out.geometry(
            0,
            0,
            340,
            190,
            wl_output::Subpixel::Unknown,
            "MockMake".into(),
            "MockPanel".into(),
            wl_output::Transform::Normal,
        );
        out.mode(
            wl_output::Mode::Current | wl_output::Mode::Preferred,
            1920,
            1080,
            60_000,
        );
        if out.version() >= 2 {
            out.scale(2);
        }
        if out.version() >= 4 {
            out.name("eDP-mock".into());
            out.description("Mock Display".into());
        }
        if out.version() >= 2 {
            out.done();
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_output::WlOutput,
        _request: wl_output::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<wl_seat::WlSeat, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<wl_seat::WlSeat>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let seat = data_init.init(resource, ());
        seat.capabilities(wl_seat::Capability::Pointer | wl_seat::Capability::Keyboard);
        if seat.version() >= 2 {
            seat.name("seat0".into());
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_seat::WlSeat,
        request: wl_seat::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_seat::Request::GetPointer { id } => {
                data_init.init(id, ());
            }
            wl_seat::Request::GetKeyboard { id } => {
                data_init.init(id, ());
            }
            wl_seat::Request::GetTouch { id } => {
                data_init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<wayland_server::protocol::wl_pointer::WlPointer, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wayland_server::protocol::wl_pointer::WlPointer,
        _request: wayland_server::protocol::wl_pointer::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<wayland_server::protocol::wl_keyboard::WlKeyboard, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wayland_server::protocol::wl_keyboard::WlKeyboard,
        _request: wayland_server::protocol::wl_keyboard::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<wayland_server::protocol::wl_touch::WlTouch, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wayland_server::protocol::wl_touch::WlTouch,
        _request: wayland_server::protocol::wl_touch::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<xdg_wm_base::XdgWmBase, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<xdg_wm_base::XdgWmBase>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &xdg_wm_base::XdgWmBase,
        request: xdg_wm_base::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_wm_base::Request::CreatePositioner { id } => {
                let pos = data_init.init(id, ());
                state
                    .positioners
                    .insert(pos.id(), MockPositionerSnapshot::default());
            }
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                let xdg_surf = data_init.init(id, ());
                state.xdg_surface_to_wl.insert(xdg_surf.id(), surface.id());
            }
            xdg_wm_base::Request::Pong { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<xdg_positioner::XdgPositioner, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &xdg_positioner::XdgPositioner,
        request: xdg_positioner::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let entry = state.positioners.entry(resource.id()).or_default();
        match request {
            xdg_positioner::Request::SetSize { width, height } => {
                entry.size = Some((width, height));
            }
            xdg_positioner::Request::SetAnchorRect {
                x,
                y,
                width,
                height,
            } => {
                entry.anchor_rect = Some((x, y, width, height));
            }
            xdg_positioner::Request::SetAnchor {
                anchor: wayland_server::WEnum::Value(a),
            } => {
                entry.anchor = Some(a as u32);
            }
            xdg_positioner::Request::SetGravity {
                gravity: wayland_server::WEnum::Value(g),
            } => {
                entry.gravity = Some(g as u32);
            }
            xdg_positioner::Request::SetOffset { x, y } => {
                entry.offset = Some((x, y));
            }
            xdg_positioner::Request::SetConstraintAdjustment {
                constraint_adjustment,
            } => {
                entry.constraint_adjustment = Some(constraint_adjustment.into());
            }
            xdg_positioner::Request::SetReactive => {
                entry.reactive = true;
            }
            _ => {}
        }
        state.current_positioner = entry.clone();
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &xdg_surface::XdgSurface,
        request: xdg_surface::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_surface::Request::GetToplevel { id } => {
                let toplevel = data_init.init(id, ());
                toplevel.configure(320, 64, Vec::new());
                resource.configure(1);
            }
            xdg_surface::Request::GetPopup {
                id,
                parent,
                positioner,
            } => {
                let popup = data_init.init(id, ());
                let snap = state
                    .positioners
                    .get(&positioner.id())
                    .cloned()
                    .unwrap_or_default();
                let (w, h) = snap.size.unwrap_or((180, 120));
                state
                    .xdg_trace
                    .lock()
                    .unwrap()
                    .popup_created_positioners
                    .push(snap);
                if let Some(wl_id) = state.xdg_surface_to_wl.get(&resource.id()).cloned() {
                    state.popup_wl_surface_id = Some(wl_id);
                    state.popup_committed = false;
                }
                state.popup_xdg_surface = Some(resource.clone());
                state.popup_resource = Some(popup.clone());
                if parent.is_some() {
                    popup.configure(0, 0, w, h);
                    resource.configure(10);
                }
            }
            xdg_surface::Request::AckConfigure { .. } => {
                #[cfg(feature = "ime")]
                if let Some(ref wl_surf) = state.last_wl_surface {
                    for ti in &state.text_inputs {
                        ti.enter(wl_surf);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &xdg_toplevel::XdgToplevel,
        _request: xdg_toplevel::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<xdg_popup::XdgPopup, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &xdg_popup::XdgPopup,
        request: xdg_popup::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_popup::Request::Reposition { positioner, token } => {
                let snap = state
                    .positioners
                    .get(&positioner.id())
                    .cloned()
                    .unwrap_or_default();
                let (w, h) = snap.size.unwrap_or((180, 120));
                {
                    let mut trace = state.xdg_trace.lock().unwrap();
                    trace.popup_reposition_tokens.push(token);
                    trace.popup_reposition_offsets.push(snap.offset);
                }
                resource.repositioned(token);
                resource.configure(24, 48, w, h);
                if let Some(ref xdg_surf) = state.popup_xdg_surface {
                    xdg_surf.configure(11);
                }
                if !state.popup_done_sent {
                    state.popup_done_sent = true;
                    resource.popup_done();
                }
            }
            xdg_popup::Request::Destroy => {
                state.xdg_trace.lock().unwrap().popup_destroyed = true;
            }
            _ => {}
        }
    }
}

impl GlobalDispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        request: zwlr_layer_shell_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_layer_shell_v1::Request::GetLayerSurface { id, .. } = request {
            let layer = data_init.init(id, ());
            layer.configure(1, 1920, 32);
            state.layer_surfaces.push(layer);
            state.xdg_trace.lock().unwrap().layer_surfaces_created += 1;
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        request: zwlr_layer_surface_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_layer_surface_v1::Request::GetPopup { popup } = request {
            state
                .xdg_trace
                .lock()
                .unwrap()
                .layer_get_popup_before_commit = Some(!state.popup_committed);
            let (w, h) = state.current_positioner.size.unwrap_or((180, 120));
            popup.configure(16, 40, w, h);
            if let Some(ref xdg_surf) = state.popup_xdg_surface {
                xdg_surf.configure(10);
            }
        }
    }
}

impl GlobalDispatch<xdg_activation_v1::XdgActivationV1, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<xdg_activation_v1::XdgActivationV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<xdg_activation_v1::XdgActivationV1, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &xdg_activation_v1::XdgActivationV1,
        request: xdg_activation_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_activation_v1::Request::GetActivationToken { id } => {
                data_init.init(id, ());
            }
            xdg_activation_v1::Request::Activate { token, surface: _ } => {
                state.xdg_trace.lock().unwrap().activations.push(token);
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &xdg_activation_token_v1::XdgActivationTokenV1,
        request: xdg_activation_token_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_activation_token_v1::Request::SetAppId { app_id } => {
                state.pending_token_app_id = Some(app_id);
            }
            xdg_activation_token_v1::Request::Commit => {
                let mut trace = state.xdg_trace.lock().unwrap();
                trace.token_commits += 1;
                if let Some(app_id) = state.pending_token_app_id.take() {
                    trace.token_app_ids.push(app_id);
                }
                resource.done("waywi-test-token-77".into());
            }
            _ => {}
        }
    }
}

#[cfg(feature = "ime")]
impl GlobalDispatch<zwp_text_input_manager_v3::ZwpTextInputManagerV3, ()> for MockCompositor {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<zwp_text_input_manager_v3::ZwpTextInputManagerV3>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

#[cfg(feature = "ime")]
impl Dispatch<zwp_text_input_manager_v3::ZwpTextInputManagerV3, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &zwp_text_input_manager_v3::ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_text_input_manager_v3::Request::GetTextInput { id, seat: _ } = request {
            let ti = data_init.init(id, ());
            state.text_inputs.push(ti);
        }
    }
}

#[cfg(feature = "ime")]
impl Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for MockCompositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &zwp_text_input_v3::ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_text_input_v3::Request::Enable => {
                state.pending_enabled = Some(true);
            }
            zwp_text_input_v3::Request::Disable => {
                state.pending_enabled = Some(false);
            }
            zwp_text_input_v3::Request::SetCursorRectangle {
                x,
                y,
                width,
                height,
            } => {
                state.pending_cursor_rect = Some((x, y, width, height));
            }
            zwp_text_input_v3::Request::SetSurroundingText {
                text,
                cursor,
                anchor,
            } => {
                state.pending_surrounding = Some((text, cursor, anchor));
            }
            zwp_text_input_v3::Request::SetContentType {
                hint: _,
                purpose: wayland_server::WEnum::Value(p),
            } => {
                state.pending_purpose = Some(p as u32);
            }
            zwp_text_input_v3::Request::Commit => {
                let mut trace = state.ime_trace.lock().unwrap();
                match state.pending_enabled {
                    Some(true) => trace.enabled_commits += 1,
                    Some(false) => trace.disabled_commits += 1,
                    None => {}
                }
                if let Some(rect) = state.pending_cursor_rect.take() {
                    trace.cursor_rects.push(rect);
                }
                if let Some(sur) = state.pending_surrounding.take() {
                    trace.surrounding_updates.push(sur);
                }
                if let Some(p) = state.pending_purpose.take() {
                    trace.content_purposes.push(p);
                }
                if state.pending_enabled == Some(true) && !trace.sent_batches {
                    trace.sent_batches = true;
                    // Batch 1: double-buffered preedit only committed on done(1).
                    resource.preedit_string(Some("にほん".into()), 0, 9);
                    resource.done(1);
                    // Batch 2: omits preedit_string (clears preedit per spec) and commits "日本".
                    resource.commit_string(Some("日本".into()));
                    resource.done(1);
                }
            }
            _ => {}
        }
    }
}

fn spawn_mock_compositor() -> (Connection, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let (conn, stop, handle, _, _) = spawn_mock_compositor_configured(MockServerConfig::default());
    (conn, stop, handle)
}

#[cfg(feature = "ime")]
type OptTrace = Option<Arc<Mutex<MockImeTrace>>>;
#[cfg(not(feature = "ime"))]
type OptTrace = Option<()>;

#[cfg(feature = "ime")]
fn spawn_mock_compositor_with(
    enable_shell_and_ime: bool,
) -> (
    Connection,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
    OptTrace,
) {
    let (conn, stop, handle, ime_trace, _) = spawn_mock_compositor_configured(MockServerConfig {
        enable_xdg_shell: enable_shell_and_ime,
        enable_ime: enable_shell_and_ime,
        ..MockServerConfig::default()
    });
    (conn, stop, handle, ime_trace)
}

fn spawn_mock_compositor_configured(
    cfg: MockServerConfig,
) -> (
    Connection,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
    OptTrace,
    Arc<Mutex<MockXdgTrace>>,
) {
    let (server_sock, client_sock) = UnixStream::pair().expect("socketpair");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = Arc::clone(&stop);
    let xdg_trace = Arc::new(Mutex::new(MockXdgTrace::default()));
    let xdg_trace_srv = Arc::clone(&xdg_trace);
    #[cfg(feature = "ime")]
    let ime_trace = cfg
        .enable_ime
        .then(|| Arc::new(Mutex::new(MockImeTrace::default())));
    #[cfg(not(feature = "ime"))]
    let ime_trace: OptTrace = None;
    #[cfg(feature = "ime")]
    let ime_trace_srv = ime_trace.as_ref().map(Arc::clone);

    let handle = std::thread::spawn(move || {
        let mut display: Display<MockCompositor> = Display::new().expect("wayland Display");
        let mut dh = display.handle();
        dh.create_global::<MockCompositor, wl_compositor::WlCompositor, _>(6, ());
        dh.create_global::<MockCompositor, wl_shm::WlShm, _>(1, ());
        let initial_output_global =
            dh.create_global::<MockCompositor, wl_output::WlOutput, _>(4, ());
        dh.create_global::<MockCompositor, wl_seat::WlSeat, _>(7, ());
        if cfg.enable_xdg_shell {
            dh.create_global::<MockCompositor, xdg_wm_base::XdgWmBase, _>(5, ());
        }
        if cfg.enable_layer_shell {
            dh.create_global::<MockCompositor, zwlr_layer_shell_v1::ZwlrLayerShellV1, _>(4, ());
        }
        if cfg.enable_xdg_activation {
            dh.create_global::<MockCompositor, xdg_activation_v1::XdgActivationV1, _>(1, ());
        }
        #[cfg(feature = "ime")]
        if cfg.enable_ime {
            dh.create_global::<MockCompositor, zwp_text_input_manager_v3::ZwpTextInputManagerV3, _>(
                1,
                (),
            );
        }
        dh.insert_client(server_sock, Arc::new(DummyClientData))
            .expect("insert_client");

        let mut state = MockCompositor {
            wl_surfaces_created: 0,
            last_wl_surface: None,
            popup_wl_surface_id: None,
            popup_committed: false,
            popup_xdg_surface: None,
            popup_resource: None,
            popup_done_sent: false,
            layer_surfaces: Vec::new(),
            output_global: Some(initial_output_global),
            hotplug_triggered: false,
            simulate_output_hotplug_on_two_layers: cfg.simulate_output_hotplug_on_two_layers,
            current_positioner: MockPositionerSnapshot::default(),
            positioners: std::collections::HashMap::new(),
            xdg_surface_to_wl: std::collections::HashMap::new(),
            pending_token_app_id: None,
            xdg_trace: xdg_trace_srv,
            #[cfg(feature = "ime")]
            text_inputs: Vec::new(),
            #[cfg(feature = "ime")]
            pending_enabled: None,
            #[cfg(feature = "ime")]
            pending_cursor_rect: None,
            #[cfg(feature = "ime")]
            pending_surrounding: None,
            #[cfg(feature = "ime")]
            pending_purpose: None,
            #[cfg(feature = "ime")]
            ime_trace: ime_trace_srv
                .unwrap_or_else(|| Arc::new(Mutex::new(MockImeTrace::default()))),
        };
        while !stop_srv.load(Ordering::Relaxed) {
            let _ = display.dispatch_clients(&mut state);
            if state.simulate_output_hotplug_on_two_layers
                && !state.hotplug_triggered
                && state.layer_surfaces.len() >= 2
                && state.popup_resource.is_some()
            {
                state.hotplug_triggered = true;
                let closing = std::mem::take(&mut state.layer_surfaces);
                for layer in closing {
                    layer.closed();
                    state.xdg_trace.lock().unwrap().layer_closed_sent += 1;
                }
                if let Some(old_out) = state.output_global.take() {
                    dh.remove_global::<MockCompositor>(old_out);
                }
                let _ = display.flush_clients();
                std::thread::sleep(Duration::from_millis(15));
                let new_out = dh.create_global::<MockCompositor, wl_output::WlOutput, _>(4, ());
                state.output_global = Some(new_out);
            }
            let _ = display.flush_clients();
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    let conn = Connection::from_socket(client_sock).expect("Connection::from_socket");
    (conn, stop, handle, ime_trace, xdg_trace)
}

#[test]
fn socketpair_probes_outputs_capabilities_and_exits_cleanly() {
    struct ProbeApp;
    impl Application for ProbeApp {
        type State = ();
        type Message = ();
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            let caps = ctx.capabilities();
            assert!(caps.has(Capability::Wayland));
            assert!(!caps.has(Capability::LayerShell));
            assert!(!caps.has(Capability::XdgShell));

            let outputs = ctx.outputs();
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].name.as_deref(), Some("eDP-mock"));
            assert_eq!(outputs[0].scale_factor, ScaleFactor::from_integer(2));
            assert_eq!(outputs[0].refresh_mhz, 60_000);

            ctx.quit();
            Ok(())
        }
        fn update(_: &mut (), _: (), _: &mut EffectCtx<'_, Self::Message>) -> Result<()> {
            Ok(())
        }
        fn view(_: &()) -> Surfaces<()> {
            Surfaces::new()
        }
    }

    let (conn, stop, srv) = spawn_mock_compositor();
    let res = App::new().run_on_connection::<ProbeApp>(conn);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
    assert!(res.is_ok(), "ProbeApp failed: {res:?}");
}

#[test]
fn missing_layer_shell_fails_honestly_without_silent_fallback() {
    struct LayerApp;
    impl Application for LayerApp {
        type State = ();
        type Message = ();
        fn init(_: &mut EffectCtx<'_, Self::Message>) -> Result<()> {
            Ok(())
        }
        fn update(_: &mut (), _: (), _: &mut EffectCtx<'_, Self::Message>) -> Result<()> {
            Ok(())
        }
        fn view(_: &()) -> Surfaces<()> {
            let mut s = Surfaces::new();
            s.push(
                Surface::floating("osd", Output::Primary).size((120.logical_px(), 40.logical_px())),
            );
            s
        }
    }

    let (conn, stop, srv) = spawn_mock_compositor();
    let res = App::new().run_on_connection::<LayerApp>(conn);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
    assert!(matches!(
        res,
        Err(WaywiError::UnsupportedCapability(Capability::LayerShell))
    ));
}

#[test]
fn missing_xdg_shell_for_window_fails_honestly() {
    struct WindowApp;
    impl Application for WindowApp {
        type State = ();
        type Message = ();
        fn init(_: &mut EffectCtx<'_, Self::Message>) -> Result<()> {
            Ok(())
        }
        fn update(_: &mut (), _: (), _: &mut EffectCtx<'_, Self::Message>) -> Result<()> {
            Ok(())
        }
        fn view(_: &()) -> Surfaces<()> {
            let mut s = Surfaces::new();
            s.push(Surface::window("win").size((200.logical_px(), 100.logical_px())));
            s
        }
    }

    let (conn, stop, srv) = spawn_mock_compositor();
    let res = App::new().run_on_connection::<WindowApp>(conn);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
    assert!(matches!(
        res,
        Err(WaywiError::UnsupportedCapability(Capability::XdgShell))
    ));
}

#[cfg(feature = "ime")]
#[test]
fn socketpair_text_input_v3_roundtrip_preedit_commit_and_surrounding() {
    #[derive(Default)]
    struct ImeProtoState {
        input: TextInputState,
        events: Vec<ImeEvent>,
        final_text: Option<String>,
    }

    #[derive(Clone)]
    enum ImeProtoMsg {
        Ime(ImeEvent),
        Quit,
    }

    static OBSERVED_TEXT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    static OBSERVED_EVENTS: std::sync::Mutex<Vec<ImeEvent>> = std::sync::Mutex::new(Vec::new());

    struct ImeProtoApp;

    impl Application for ImeProtoApp {
        type State = ImeProtoState;
        type Message = ImeProtoMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::Wayland));
            assert!(ctx.capabilities().has(Capability::XdgShell));
            assert!(ctx.capabilities().has(Capability::TextInput));
            Ok(ImeProtoState::default())
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                ImeProtoMsg::Ime(ev) => {
                    state.events.push(ev.clone());
                    state.input.handle_ime(&ev);
                    if state.input.text() == "日本" {
                        state.final_text = Some(state.input.text().to_owned());
                        // Give the view -> Wayland update + socket flush one tick before exit.
                        ctx.after(Duration::from_millis(40), ImeProtoMsg::Quit);
                    }
                }
                ImeProtoMsg::Quit => {
                    *OBSERVED_TEXT.lock().unwrap() = state.final_text.clone();
                    *OBSERVED_EVENTS.lock().unwrap() = state.events.clone();
                    ctx.quit();
                }
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            let cursor_x = 10 + (state.input.before_cursor().chars().count() as i32 * 8);
            s.push(
                Surface::window("ime-win")
                    .size((320.logical_px(), 64.logical_px()))
                    .ime(
                        ImeConfig::enabled()
                            .from_text_input(&state.input)
                            .content_purpose(ImeContentPurpose::Email)
                            .cursor_rect(Bounds::new(
                                (cursor_x as f32).logical_px(),
                                20.logical_px(),
                                2.logical_px(),
                                18.logical_px(),
                            )),
                    )
                    .on_ime(|ev| Some(ImeProtoMsg::Ime(ev))),
            );
            s
        }
    }

    let (conn, stop, srv, trace_opt) = spawn_mock_compositor_with(true);
    let trace = trace_opt.expect("trace present");
    let res = App::new().run_on_connection::<ImeProtoApp>(conn);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
    assert!(res.is_ok(), "ImeProtoApp failed: {res:?}");

    assert_eq!(OBSERVED_TEXT.lock().unwrap().as_deref(), Some("日本"));
    let events = OBSERVED_EVENTS.lock().unwrap();
    assert!(matches!(events.first(), Some(ImeEvent::Enter)));
    assert!(events.iter().any(|ev| matches!(
        ev,
        ImeEvent::Update {
            commit: None,
            preedit: Some(p),
            ..
        } if p.text == "にほん"
    )));
    assert!(events.iter().any(|ev| matches!(
        ev,
        ImeEvent::Update {
            commit: Some(c),
            preedit: None,
            ..
        } if c == "日本"
    )));

    let recorded = trace.lock().unwrap();
    assert!(recorded.enabled_commits >= 2);
    assert!(recorded.content_purposes.contains(&6)); // ContentPurpose::Email
    assert!(recorded.cursor_rects.contains(&(10, 20, 2, 18)));
    assert!(recorded.cursor_rects.contains(&(26, 20, 2, 18)));
    assert!(
        recorded
            .surrounding_updates
            .iter()
            .any(|(txt, cur, anc)| txt == "日本" && *cur == 6 && *anc == 6)
    );
}

#[test]
fn socketpair_xdg_activation_v1_token_and_activate_roundtrip() {
    #[derive(Clone)]
    enum ActMsg {
        RequestToken,
        GotToken(Option<String>),
        Quit,
    }

    static OBSERVED_TOKEN: Mutex<Option<String>> = Mutex::new(None);

    struct ActApp;

    impl Application for ActApp {
        type State = ();
        type Message = ActMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::XdgShell));
            assert!(ctx.capabilities().has(Capability::XdgActivation));
            // Trigger token request once the initial toplevel surface is mapped.
            ctx.after(Duration::from_millis(20), ActMsg::RequestToken);
            // Failsafe timeout so protocol regressions fail assertions instead of hanging.
            ctx.after(Duration::from_secs(5), ActMsg::Quit);
            Ok(())
        }

        fn update(
            _state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                ActMsg::RequestToken => {
                    ctx.request_activation_token(Some("org.waywi.TestLauncher"), |res| {
                        ActMsg::GotToken(res.ok())
                    });
                }
                ActMsg::GotToken(tok) => {
                    *OBSERVED_TOKEN.lock().unwrap() = tok.clone();
                    if let Some(t) = tok {
                        ctx.activate_surface("act-win", t)?;
                    }
                    ctx.after(Duration::from_millis(30), ActMsg::Quit);
                }
                ActMsg::Quit => {
                    ctx.quit();
                }
            }
            Ok(())
        }

        fn view(_state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(Surface::window("act-win").size((240.logical_px(), 80.logical_px())));
            s
        }
    }

    let (conn, stop, srv, _, xdg_trace) = spawn_mock_compositor_configured(MockServerConfig {
        enable_xdg_shell: true,
        enable_xdg_activation: true,
        ..MockServerConfig::default()
    });
    let res = App::new().run_on_connection::<ActApp>(conn);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
    assert!(res.is_ok(), "ActApp failed: {res:?}");

    assert_eq!(
        OBSERVED_TOKEN.lock().unwrap().as_deref(),
        Some("waywi-test-token-77")
    );
    let trace = xdg_trace.lock().unwrap();
    assert_eq!(trace.token_commits, 1);
    assert_eq!(trace.token_app_ids, vec!["org.waywi.TestLauncher"]);
    assert_eq!(trace.activations, vec!["waywi-test-token-77"]);
}

#[test]
fn socketpair_missing_xdg_activation_delivers_unsupported_capability() {
    #[derive(Clone)]
    enum MissingActMsg {
        GotResult(bool),
    }

    static SAW_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

    struct MissingActApp;

    impl Application for MissingActApp {
        type State = ();
        type Message = MissingActMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(!ctx.capabilities().has(Capability::XdgActivation));
            ctx.request_activation_token(Some("org.waywi.Launcher"), |res| {
                MissingActMsg::GotResult(matches!(
                    res,
                    Err(WaywiError::UnsupportedCapability(Capability::XdgActivation))
                ))
            });
            Ok(())
        }

        fn update(
            _state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            let MissingActMsg::GotResult(is_unsupported) = msg;
            SAW_UNSUPPORTED.store(is_unsupported, Ordering::Relaxed);
            ctx.quit();
            Ok(())
        }

        fn view(_state: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::new()
        }
    }

    let (conn, stop, srv) = spawn_mock_compositor();
    let res = App::new().run_on_connection::<MissingActApp>(conn);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
    assert!(res.is_ok(), "MissingActApp failed: {res:?}");
    assert!(SAW_UNSUPPORTED.load(Ordering::Relaxed));
}

#[test]
fn socketpair_layer_xdg_popup_positioner_reposition_and_done() {
    #[derive(Clone)]
    enum PopupMsg {
        Reposition,
        Dismissed,
        Quit,
    }

    struct PopupState {
        open: bool,
        offset_y: i32,
        dismissed: bool,
    }

    static OBSERVED_DISMISSED: AtomicBool = AtomicBool::new(false);

    struct LayerPopupApp;

    impl Application for LayerPopupApp {
        type State = PopupState;
        type Message = PopupMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::LayerShell));
            assert!(ctx.capabilities().has(Capability::XdgShell));
            // Give initial layer + popup creation & configure one tick before repositioning.
            ctx.after(Duration::from_millis(30), PopupMsg::Reposition);
            // Failsafe timeout so protocol regressions fail assertions instead of hanging.
            ctx.after(Duration::from_secs(5), PopupMsg::Quit);
            Ok(PopupState {
                open: true,
                offset_y: 8,
                dismissed: false,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                PopupMsg::Reposition => {
                    state.offset_y = 20;
                }
                PopupMsg::Dismissed => {
                    state.dismissed = true;
                    state.open = false;
                    OBSERVED_DISMISSED.store(true, Ordering::Relaxed);
                    // Wait briefly so view sync + destroy flushes cleanly before quitting.
                    ctx.after(Duration::from_millis(30), PopupMsg::Quit);
                }
                PopupMsg::Quit => {
                    ctx.quit();
                }
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(
                Surface::panel("bar", Output::Primary)
                    .anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)
                    .size((1920.logical_px(), 32.logical_px())),
            );
            if state.open {
                s.push(
                    Surface::popup("menu", "bar")
                        .size((180.logical_px(), 120.logical_px()))
                        .positioner(
                            PopupPositioner::new(Bounds::from_xywh(12.0, 0.0, 80.0, 32.0))
                                .anchor(PopupAnchor::BottomLeft)
                                .gravity(PopupGravity::BottomRight)
                                .offset(4.logical_px(), state.offset_y.logical_px())
                                .constraint_adjustment(
                                    PopupConstraintAdjustment::FLIP_Y
                                        | PopupConstraintAdjustment::SLIDE_X,
                                )
                                .reactive(true),
                        )
                        .on_close(PopupMsg::Dismissed),
                );
            }
            s
        }
    }

    let (conn, stop, srv, _, xdg_trace) = spawn_mock_compositor_configured(MockServerConfig {
        enable_xdg_shell: true,
        enable_layer_shell: true,
        ..MockServerConfig::default()
    });
    let res = App::new().run_on_connection::<LayerPopupApp>(conn);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
    assert!(res.is_ok(), "LayerPopupApp failed: {res:?}");
    assert!(
        OBSERVED_DISMISSED.load(Ordering::Relaxed),
        "expected popup_done to deliver on_close message"
    );

    let trace = xdg_trace.lock().unwrap();
    assert_eq!(
        trace.layer_get_popup_before_commit,
        Some(true),
        "zwlr_layer_surface_v1.get_popup must be sent before popup wl_surface.commit"
    );
    assert_eq!(trace.popup_created_positioners.len(), 1);
    let pos = &trace.popup_created_positioners[0];
    assert_eq!(pos.size, Some((180, 120)));
    assert_eq!(pos.anchor_rect, Some((12, 0, 80, 32)));
    assert_eq!(pos.anchor, Some(xdg_positioner::Anchor::BottomLeft as u32));
    assert_eq!(
        pos.gravity,
        Some(xdg_positioner::Gravity::BottomRight as u32)
    );
    assert_eq!(pos.offset, Some((4, 8)));
    assert_eq!(
        pos.constraint_adjustment,
        Some(
            (xdg_positioner::ConstraintAdjustment::FlipY
                | xdg_positioner::ConstraintAdjustment::SlideX)
                .bits()
        )
    );
    assert!(pos.reactive);
    assert_eq!(trace.popup_reposition_tokens, vec![1]);
    assert_eq!(trace.popup_reposition_offsets, vec![Some((4, 20))]);
    assert!(trace.popup_destroyed, "expected xdg_popup.destroy on close");
    assert!(
        !trace.popup_null_buffer_attached,
        "xdg_popup must not attach a null buffer before destroy"
    );
}

#[test]
fn socketpair_layer_closed_and_output_hotplug_recreates_memo_surfaces_without_quitting() {
    use std::sync::atomic::AtomicUsize;
    use waywi::render::{Color, Element, Rect};

    #[derive(Clone)]
    enum HotplugMsg {
        PopupClosed,
        Tick,
        Quit,
    }

    struct HotplugState {
        popup_open: bool,
        ticks: usize,
    }

    static POPUP_DISMISSED: AtomicBool = AtomicBool::new(false);
    static MEMO_SHELL_BUILDS: AtomicUsize = AtomicUsize::new(0);
    static MEMO_INNER_BUILDS: AtomicUsize = AtomicUsize::new(0);
    static SURVIVED_AFTER_HOTPLUG: AtomicBool = AtomicBool::new(false);

    struct HotplugApp;

    impl Application for HotplugApp {
        type State = HotplugState;
        type Message = HotplugMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            // Multiple ticks after hotplug verify the app stayed alive and frame_pending pacing
            // did not spam redundant commits while frame callbacks are unacknowledged.
            ctx.after(Duration::from_millis(80), HotplugMsg::Tick);
            ctx.after(Duration::from_millis(140), HotplugMsg::Tick);
            ctx.after(Duration::from_millis(220), HotplugMsg::Quit);
            Ok(HotplugState {
                popup_open: true,
                ticks: 0,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                HotplugMsg::PopupClosed => {
                    state.popup_open = false;
                    POPUP_DISMISSED.store(true, Ordering::Relaxed);
                }
                HotplugMsg::Tick => {
                    state.ticks += 1;
                    if state.ticks >= 2 {
                        SURVIVED_AFTER_HOTPLUG.store(true, Ordering::Relaxed);
                    }
                }
                HotplugMsg::Quit => ctx.quit(),
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            // Intentionally no `.on_close(...)` on layer surfaces: `zwlr_layer_surface_v1.closed`
            // must NOT set `quit_requested = true`.
            s.push(
                Surface::panel("bar", Output::Primary)
                    .anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)
                    .size((1920.logical_px(), 32.logical_px()))
                    .child(Rect::new().size((40.logical_px(), 20.logical_px())).fill(
                        if state.ticks % 2 == 0 {
                            Color::RED
                        } else {
                            Color::GREEN
                        },
                    )),
            );
            s.push(Surface::memo("wallpaper", &1u32, |_| {
                MEMO_SHELL_BUILDS.fetch_add(1, Ordering::Relaxed);
                Surface::panel("wallpaper", Output::All)
                    .size((1920.logical_px(), 1080.logical_px()))
                    .child(Element::memo("wp-inner", &1u32, |_| {
                        MEMO_INNER_BUILDS.fetch_add(1, Ordering::Relaxed);
                        Rect::new()
                            .size((100.logical_px(), 100.logical_px()))
                            .fill(Color::BLUE)
                            .into()
                    }))
            }));
            if state.popup_open {
                s.push(
                    Surface::popup("menu", "bar")
                        .size((160.logical_px(), 90.logical_px()))
                        .positioner(PopupPositioner::new(Bounds::from_xywh(
                            10.0, 0.0, 60.0, 32.0,
                        )))
                        .on_close(HotplugMsg::PopupClosed),
                );
            }
            s
        }
    }

    POPUP_DISMISSED.store(false, Ordering::Relaxed);
    MEMO_SHELL_BUILDS.store(0, Ordering::Relaxed);
    MEMO_INNER_BUILDS.store(0, Ordering::Relaxed);
    SURVIVED_AFTER_HOTPLUG.store(false, Ordering::Relaxed);

    let (conn, stop, srv, _, xdg_trace) = spawn_mock_compositor_configured(MockServerConfig {
        enable_xdg_shell: true,
        enable_layer_shell: true,
        simulate_output_hotplug_on_two_layers: true,
        ..MockServerConfig::default()
    });
    let res = App::new().run_on_connection::<HotplugApp>(conn);
    stop.store(true, Ordering::Relaxed);
    let _ = srv.join();
    assert!(res.is_ok(), "HotplugApp failed: {res:?}");

    assert!(
        SURVIVED_AFTER_HOTPLUG.load(Ordering::Relaxed),
        "app must stay alive after zwlr_layer_surface_v1.closed (monitor OSD/hotplug)"
    );
    assert!(
        POPUP_DISMISSED.load(Ordering::Relaxed),
        "child xdg_popup must receive on_close when parent layer surface closes"
    );
    assert!(
        MEMO_SHELL_BUILDS.load(Ordering::Relaxed) >= 2,
        "Surface::memo(Output::All) must rebuild after OutputsChanged"
    );
    assert!(
        MEMO_INNER_BUILDS.load(Ordering::Relaxed) >= 2,
        "nested Element::memo inside Surface::memo must rebuild for recreated output"
    );

    let trace = xdg_trace.lock().unwrap();
    assert!(
        trace.layer_closed_sent >= 2,
        "expected mock compositor to send closed to both layer surfaces, got {}",
        trace.layer_closed_sent
    );
    assert!(
        trace.layer_surfaces_created >= 4,
        "expected both panel and memo wallpaper to be recreated on new wl_output, got {}",
        trace.layer_surfaces_created
    );
    // While MockCompositor never answers `wl_surface.frame`, pacing must prevent spamming
    // extra commits on the recreated `bar` across multiple ticks.
    assert!(
        trace.buffer_commits <= trace.frame_callbacks_created + 1,
        "frame_pending pacing violated: commits={} frame_callbacks={}",
        trace.buffer_commits,
        trace.frame_callbacks_created
    );
}
