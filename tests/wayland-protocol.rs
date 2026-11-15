use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wayland_client::Connection;
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
use waywi::geometry::{LogicalPxExt, ScaleFactor, Size};
use waywi::surface::{Surface, Surfaces};
use waywi::{App, Application};

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
                data_init.init(id, ());
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

fn spawn_mock_compositor() -> (Connection, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let (server_sock, client_sock) = UnixStream::pair().expect("socketpair");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = Arc::clone(&stop);

    let handle = std::thread::spawn(move || {
        let mut display: Display<MockCompositor> = Display::new().expect("wayland Display");
        let mut dh = display.handle();
        dh.create_global::<MockCompositor, wl_compositor::WlCompositor, _>(6, ());
        dh.create_global::<MockCompositor, wl_shm::WlShm, _>(1, ());
        dh.create_global::<MockCompositor, wl_output::WlOutput, _>(4, ());
        dh.create_global::<MockCompositor, wl_seat::WlSeat, _>(7, ());
        dh.insert_client(server_sock, Arc::new(DummyClientData))
            .expect("insert_client");

        let mut state = MockCompositor {
            wl_surfaces_created: 0,
        };
        while !stop_srv.load(Ordering::Relaxed) {
            let _ = display.dispatch_clients(&mut state);
            let _ = display.flush_clients();
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    let conn = Connection::from_socket(client_sock).expect("Connection::from_socket");
    (conn, stop, handle)
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
