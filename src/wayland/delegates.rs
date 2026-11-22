//! SCTK and `wayland-client` protocol delegate implementations for [`WaylandState`].

mod input;
mod protocols;
mod surface;

use smithay_client_toolkit::{
    delegate_activation, delegate_compositor, delegate_keyboard, delegate_layer, delegate_output,
    delegate_pointer, delegate_registry, delegate_seat, delegate_shm, delegate_touch,
    delegate_xdg_popup, delegate_xdg_shell, delegate_xdg_window,
};

use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::ActivationRequestData;

delegate_compositor!(WaylandState);
delegate_output!(WaylandState);
delegate_shm!(WaylandState);
delegate_seat!(WaylandState);
delegate_keyboard!(WaylandState);
delegate_pointer!(WaylandState);
delegate_touch!(WaylandState);
delegate_layer!(WaylandState);
delegate_xdg_shell!(WaylandState);
delegate_xdg_window!(WaylandState);
delegate_xdg_popup!(WaylandState);
delegate_activation!(WaylandState, ActivationRequestData);
delegate_registry!(WaylandState);
