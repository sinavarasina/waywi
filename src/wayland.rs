#[cfg(feature = "clipboard")]
pub(crate) mod clipboard;
pub(crate) mod connection;
pub(crate) mod delegates;
#[cfg(feature = "idle-notify")]
pub(crate) mod idle;
#[cfg(feature = "ime")]
pub(crate) mod ime;
pub(crate) mod layer_shell;
pub(crate) mod output;
pub(crate) mod seat;
pub(crate) mod shm_pool;
pub(crate) mod surface_state;
pub(crate) mod xdg_shell;

pub(crate) use connection::WaylandState;
pub(crate) use surface_state::WaylandEvent;
