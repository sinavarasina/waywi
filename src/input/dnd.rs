//! Core Wayland `wl_data_device` Drag & Drop types.

use bitflags::bitflags;

use crate::geometry::Point;

bitflags! {
    /// Wayland `wl_data_device_manager` Drag & Drop action bitmask.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct DndAction: u32 {
        const COPY = 1 << 0;
        const MOVE = 1 << 1;
        const ASK  = 1 << 2;
    }
}

/// Drag & Drop lifecycle events delivered to [`Surface::on_dnd`](crate::surface::Surface::on_dnd).
#[derive(Debug, Clone, PartialEq)]
pub enum DndEvent {
    /// A drag operation entered this surface advertising `mime_types` and `actions`.
    Enter {
        position: Point,
        mime_types: Vec<String>,
        actions: DndAction,
    },
    /// The drag cursor moved across this surface.
    Motion { position: Point },
    /// The drag cursor left this surface without dropping.
    Leave,
    /// Payload was dropped and asynchronously read from the source pipe.
    Drop {
        position: Point,
        mime_type: String,
        data: Vec<u8>,
        action: DndAction,
    },
    /// A drop occurred, but reading the offered pipe failed (e.g. timeout or exceeded 16 MiB bound).
    DropError { position: Point, message: String },
}
