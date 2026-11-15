use std::path::PathBuf;

use wayland_client::Connection;

use crate::effect::EffectCtx;
use crate::error::{Result, WaywiError};
use crate::subscription::Subscriptions;
use crate::surface::Surfaces;

use super::event_loop::run_event_loop;

/// Model-View-Update trait defining a WayWi application.
pub trait Application: Sized {
    /// Immutable-per-view application state updated cleanly inside [`Application::update`].
    type State;
    /// Discrete event/message type produced by input handlers, timers, and IPC subscriptions.
    type Message: Clone + 'static;

    /// Initializes application state once when the runtime connects.
    fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State>;

    /// Processes a single message, mutating state and optionally triggering controlled side effects.
    fn update(state: &mut Self::State, msg: Self::Message, ctx: &mut EffectCtx<'_>) -> Result<()>;

    /// Declares desired Wayland surfaces and their UI trees for the current state.
    fn view(state: &Self::State) -> Surfaces<Self::Message>;

    /// Declares active timers, system samplers, audio/IPC streams, and control triggers.
    fn subscriptions(_state: &Self::State) -> Subscriptions<Self::Message> {
        Subscriptions::none()
    }
}

/// Entry-point builder for running a [`Application`] on a Wayland session.
#[derive(Debug, Default, Clone)]
pub struct App {
    pub(crate) name: Option<String>,
    pub(crate) control_socket_override: Option<PathBuf>,
    pub(crate) renderer_preference: crate::render::RendererPreference,
}

impl App {
    /// Creates a new WayWi application runner with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects whether to prefer GPU hardware rendering
    /// ([`RendererPreference::Auto`](crate::render::RendererPreference::Auto)), force CPU `wl_shm`
    /// ([`RendererPreference::Software`](crate::render::RendererPreference::Software)), or require GPU
    /// hardware rendering ([`RendererPreference::Gpu`](crate::render::RendererPreference::Gpu)).
    #[must_use]
    pub fn renderer(mut self, preference: crate::render::RendererPreference) -> Self {
        self.renderer_preference = preference;
        self
    }

    /// Sets the logical application identifier (used for default control socket and surface
    /// identification when omitted).
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Overrides the default single-instance Unix control socket path.
    #[must_use]
    pub fn control_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.control_socket_override = Some(path.into());
        self
    }

    /// Connects to `$WAYLAND_DISPLAY` and runs `A` until quit or surface close.
    pub fn run<A: Application>(self) -> Result<()> {
        let conn = Connection::connect_to_env().map_err(|e| {
            WaywiError::WaylandConnection(format!("failed to connect to Wayland display: {e}"))
        })?;
        self.run_on_connection::<A>(conn)
    }

    /// Runs `A` directly on a pre-established `wayland_client::Connection` (for embedded
    /// compositors and in-process `wayland-server` protocol testing).
    pub fn run_on_connection<A: Application>(self, conn: Connection) -> Result<()> {
        run_event_loop::<A>(self, conn)
    }

    /// Convenience shortcut equivalent to `App::new().run::<A>()`.
    pub fn start<A: Application>() -> Result<()> {
        Self::new().run::<A>()
    }
}
