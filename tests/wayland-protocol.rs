use std::os::unix::net::UnixStream;
use std::sync::Arc;
#[cfg(feature = "ime")]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wayland_client::Connection;
#[cfg(feature = "ime")]
use wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
#[cfg(feature = "ime")]
use wayland_protocols::xdg::shell::server::{xdg_surface, xdg_toplevel, xdg_wm_base};
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
#[cfg(feature = "ime")]
use waywi::geometry::GeoRect;
use waywi::geometry::{LogicalPxExt, ScaleFactor, Size};
#[cfg(feature = "ime")]
use waywi::input::{ImeConfig, ImeContentPurpose, ImeEvent, TextInputState};
use waywi::surface::{Surface, Surfaces};
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
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_surface::WlSurface,
        request: wl_surface::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_surface::Request::Frame { callback } = request {
            data_init.init(callback, ());
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

#[cfg(feature = "ime")]
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

#[cfg(feature = "ime")]
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for MockCompositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &xdg_wm_base::XdgWmBase,
        request: xdg_wm_base::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_wm_base::Request::GetXdgSurface { id, surface: _ } => {
                data_init.init(id, ());
            }
            xdg_wm_base::Request::Pong { .. } => {}
            _ => {}
        }
    }
}

#[cfg(feature = "ime")]
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
            xdg_surface::Request::AckConfigure { .. } => {
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

#[cfg(feature = "ime")]
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
    let (conn, stop, handle, _) = spawn_mock_compositor_with(false);
    (conn, stop, handle)
}

#[cfg(feature = "ime")]
type OptTrace = Option<Arc<Mutex<MockImeTrace>>>;
#[cfg(not(feature = "ime"))]
type OptTrace = Option<()>;

fn spawn_mock_compositor_with(
    enable_shell_and_ime: bool,
) -> (
    Connection,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
    OptTrace,
) {
    let (server_sock, client_sock) = UnixStream::pair().expect("socketpair");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = Arc::clone(&stop);
    #[cfg(feature = "ime")]
    let ime_trace = enable_shell_and_ime.then(|| Arc::new(Mutex::new(MockImeTrace::default())));
    #[cfg(not(feature = "ime"))]
    let ime_trace: OptTrace = None;
    #[cfg(feature = "ime")]
    let ime_trace_srv = ime_trace.as_ref().map(Arc::clone);

    let handle = std::thread::spawn(move || {
        let mut display: Display<MockCompositor> = Display::new().expect("wayland Display");
        let mut dh = display.handle();
        dh.create_global::<MockCompositor, wl_compositor::WlCompositor, _>(6, ());
        dh.create_global::<MockCompositor, wl_shm::WlShm, _>(1, ());
        dh.create_global::<MockCompositor, wl_output::WlOutput, _>(4, ());
        dh.create_global::<MockCompositor, wl_seat::WlSeat, _>(7, ());
        #[cfg(feature = "ime")]
        if enable_shell_and_ime {
            dh.create_global::<MockCompositor, xdg_wm_base::XdgWmBase, _>(2, ());
            dh.create_global::<MockCompositor, zwp_text_input_manager_v3::ZwpTextInputManagerV3, _>(
                1,
                (),
            );
        }
        #[cfg(not(feature = "ime"))]
        let _ = enable_shell_and_ime;
        dh.insert_client(server_sock, Arc::new(DummyClientData))
            .expect("insert_client");

        let mut state = MockCompositor {
            wl_surfaces_created: 0,
            last_wl_surface: None,
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
            let _ = display.flush_clients();
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    let conn = Connection::from_socket(client_sock).expect("Connection::from_socket");
    (conn, stop, handle, ime_trace)
}

#[test]
fn socketpair_probes_outputs_capabilities_and_exits_cleanly() {
    struct ProbeApp;
    impl Application for ProbeApp {
        type State = ();
        type Message = ();
        fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
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
        fn update(_: &mut (), _: (), _: &mut EffectCtx<'_>) -> Result<()> {
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
        fn init(_: &mut EffectCtx<'_>) -> Result<()> {
            Ok(())
        }
        fn update(_: &mut (), _: (), _: &mut EffectCtx<'_>) -> Result<()> {
            Ok(())
        }
        fn view(_: &()) -> Surfaces<()> {
            let mut s = Surfaces::new();
            s.push(
                Surface::floating("osd", Output::Primary)
                    .size(Size::px(120.logical_px(), 40.logical_px())),
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
        fn init(_: &mut EffectCtx<'_>) -> Result<()> {
            Ok(())
        }
        fn update(_: &mut (), _: (), _: &mut EffectCtx<'_>) -> Result<()> {
            Ok(())
        }
        fn view(_: &()) -> Surfaces<()> {
            let mut s = Surfaces::new();
            s.push(Surface::window("win").size(Size::px(200.logical_px(), 100.logical_px())));
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

        fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
            assert!(ctx.capabilities().has(Capability::Wayland));
            assert!(ctx.capabilities().has(Capability::XdgShell));
            assert!(ctx.capabilities().has(Capability::TextInput));
            Ok(ImeProtoState::default())
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_>,
        ) -> Result<()> {
            match msg {
                ImeProtoMsg::Ime(ev) => {
                    state.events.push(ev.clone());
                    state.input.handle_ime(&ev);
                    if state.input.text() == "日本" {
                        state.final_text = Some(state.input.text().to_owned());
                        // Give the view -> Wayland update + socket flush one tick before exit.
                        ctx.after(Duration::from_millis(40), || ImeProtoMsg::Quit);
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
                    .size(Size::px(320.logical_px(), 64.logical_px()))
                    .ime(
                        ImeConfig::enabled()
                            .from_text_input(&state.input)
                            .content_purpose(ImeContentPurpose::Email)
                            .cursor_rect(GeoRect::new(
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
