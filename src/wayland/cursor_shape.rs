//! Wayland `wp_cursor_shape_v1` server-side cursor shape management (plus `CursorShape::Hidden`).

use std::collections::HashMap;

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_pointer;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{self, Shape as ProtoShape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::{self, WpCursorShapeManagerV1},
};

use crate::input::CursorShape;
use crate::wayland::connection::WaylandState;

/// Manages `wp_cursor_shape_manager_v1` and per-pointer `wp_cursor_shape_device_v1` objects.
pub(crate) struct WaylandCursorShapeState {
    manager: Option<WpCursorShapeManagerV1>,
    devices: HashMap<ObjectId, WpCursorShapeDeviceV1>,
}

impl Drop for WaylandCursorShapeState {
    fn drop(&mut self) {
        self.clear_all_pointers();
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandCursorShapeState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<WpCursorShapeManagerV1, _, _>(qh, 1..=2, ())
            .ok();
        Self {
            manager,
            devices: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn add_pointer(
        &mut self,
        pointer: &wl_pointer::WlPointer,
        qh: &QueueHandle<WaylandState>,
    ) {
        let Some(ref mgr) = self.manager else {
            return;
        };
        let id = pointer.id();
        self.devices
            .entry(id)
            .or_insert_with(|| mgr.get_pointer(pointer, qh, ()));
    }

    pub(crate) fn clear_all_pointers(&mut self) {
        for (_, dev) in self.devices.drain() {
            dev.destroy();
        }
    }

    /// Sends `wp_cursor_shape_device_v1.set_shape` for `pointer` and `enter_serial`.
    pub(crate) fn set_shape(
        &self,
        pointer: &wl_pointer::WlPointer,
        enter_serial: u32,
        shape: CursorShape,
    ) -> bool {
        let Some(dev) = self.devices.get(&pointer.id()) else {
            return false;
        };
        dev.set_shape(enter_serial, map_cursor_shape(shape));
        true
    }
}

fn map_cursor_shape(shape: CursorShape) -> ProtoShape {
    match shape {
        CursorShape::Default | CursorShape::Hidden => ProtoShape::Default,
        CursorShape::ContextMenu => ProtoShape::ContextMenu,
        CursorShape::Help => ProtoShape::Help,
        CursorShape::Pointer => ProtoShape::Pointer,
        CursorShape::Progress => ProtoShape::Progress,
        CursorShape::Wait => ProtoShape::Wait,
        CursorShape::Cell => ProtoShape::Cell,
        CursorShape::Crosshair => ProtoShape::Crosshair,
        CursorShape::Text => ProtoShape::Text,
        CursorShape::VerticalText => ProtoShape::VerticalText,
        CursorShape::Alias => ProtoShape::Alias,
        CursorShape::Copy => ProtoShape::Copy,
        CursorShape::Move => ProtoShape::Move,
        CursorShape::NoDrop => ProtoShape::NoDrop,
        CursorShape::NotAllowed => ProtoShape::NotAllowed,
        CursorShape::Grab => ProtoShape::Grab,
        CursorShape::Grabbing => ProtoShape::Grabbing,
        CursorShape::EResize => ProtoShape::EResize,
        CursorShape::NResize => ProtoShape::NResize,
        CursorShape::NeResize => ProtoShape::NeResize,
        CursorShape::NwResize => ProtoShape::NwResize,
        CursorShape::SResize => ProtoShape::SResize,
        CursorShape::SeResize => ProtoShape::SeResize,
        CursorShape::SwResize => ProtoShape::SwResize,
        CursorShape::WResize => ProtoShape::WResize,
        CursorShape::EwResize => ProtoShape::EwResize,
        CursorShape::NsResize => ProtoShape::NsResize,
        CursorShape::NeswResize => ProtoShape::NeswResize,
        CursorShape::NwseResize => ProtoShape::NwseResize,
        CursorShape::ColResize => ProtoShape::ColResize,
        CursorShape::RowResize => ProtoShape::RowResize,
        CursorShape::AllScroll => ProtoShape::AllScroll,
        CursorShape::ZoomIn => ProtoShape::ZoomIn,
        CursorShape::ZoomOut => ProtoShape::ZoomOut,
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WpCursorShapeManagerV1,
        _event: wp_cursor_shape_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpCursorShapeDeviceV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WpCursorShapeDeviceV1,
        _event: wp_cursor_shape_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
