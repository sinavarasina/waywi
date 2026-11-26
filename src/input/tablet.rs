//! Pen tablet and stylus input event types (`zwp_tablet_manager_v2` / `tablet-v2`).

use crate::geometry::Point;

use super::keys::Modifiers;

/// Session-unique identifier for a graphics tablet device (`zwp_tablet_v2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabletId(pub u64);

/// Session-unique identifier for a physical stylus/tool (`zwp_tablet_tool_v2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabletToolId(pub u64);

/// Session-unique identifier for a graphics tablet express-key pad (`zwp_tablet_pad_v2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabletPadId(pub u64);

/// Physical type of a tablet stylus or tool (`zwp_tablet_tool_v2::Type`).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TabletToolType {
    #[default]
    Pen,
    Eraser,
    Brush,
    Pencil,
    Airbrush,
    Finger,
    Mouse,
    Lens,
}

/// Snapshot of physical axes reported for an atomic tablet tool frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TabletToolAxes {
    /// Normalized tip pressure in `0.0..=1.0` (mapped from protocol `0..=65535`).
    pub pressure: f32,
    /// Normalized hover distance in `0.0..=1.0` (mapped from protocol `0..=65535`).
    pub distance: f32,
    /// Pen tilt in degrees relative to the tablet Z axis (`tilt_x`, `tilt_y`).
    pub tilt: (f32, f32),
    /// Z-axis barrel rotation in degrees clockwise from neutral.
    pub rotation: f32,
    /// Normalized slider position in `-1.0..=1.0` (e.g. Airbrush finger wheel).
    pub slider: f32,
    /// Wheel rotation delta in degrees for this frame.
    pub wheel_degrees: f32,
    /// Wheel discrete logical click delta for this frame.
    pub wheel_clicks: i32,
}

/// Phase/action of an atomic `zwp_tablet_tool_v2` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabletToolEventKind {
    /// Stylus entered proximity over this surface.
    ProximityIn,
    /// Stylus left proximity or focus on this surface.
    ProximityOut,
    /// Stylus tip made logical contact with the surface.
    Down,
    /// Stylus tip lifted off the surface.
    Up,
    /// Stylus moved and/or updated physical axes (`pressure`, `tilt`, `distance`, etc.).
    Motion,
    /// Stylus barrel or side button pressed or released (`button` is Linux input code, e.g. `BTN_STYLUS = 0x14b`).
    Button { button: u32, pressed: bool },
}

/// Atomic stylus/tool input event delivered to element and surface `on_tablet` handlers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabletToolEvent {
    pub tool_id: TabletToolId,
    pub tablet_id: Option<TabletId>,
    pub tool_type: TabletToolType,
    pub hardware_serial: u64,
    pub hardware_id_wacom: u64,
    pub kind: TabletToolEventKind,
    pub position: Point,
    pub local_position: Point,
    pub axes: TabletToolAxes,
    pub tip_down: bool,
    pub modifiers: Modifiers,
}

/// Physical source of a tablet pad ring or strip interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TabletPadAxisSource {
    #[default]
    Unknown,
    Finger,
}

/// Event kind emitted by a graphics tablet pad (`zwp_tablet_pad_v2`, rings, strips, dials, modes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TabletPadEventKind {
    /// Pad gained focus on this surface.
    Enter,
    /// Pad lost focus on this surface.
    Leave,
    /// Express key pressed or released (`button` is 0-based pad button index).
    Button { button: u32, pressed: bool },
    /// Active pad group mode switched (`group` and `mode` are 0-based indices).
    ModeSwitch { group: u32, mode: u32 },
    /// Touch ring update (`ring` is 0-based ring index; `angle` in degrees clockwise from north).
    Ring {
        ring: u32,
        angle: Option<f32>,
        source: TabletPadAxisSource,
        stopped: bool,
    },
    /// Touch strip update (`strip` is 0-based strip index; `position` normalized in `0.0..=1.0`).
    Strip {
        strip: u32,
        position: Option<f32>,
        source: TabletPadAxisSource,
        stopped: bool,
    },
    /// Rotary dial update (`dial` is 0-based dial index; `delta` in logical wheel steps `value120 / 120.0`).
    Dial {
        dial: u32,
        delta: f32,
        value120: i32,
    },
}

/// Express-key / ring / strip / dial event delivered to surface `on_tablet` handlers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabletPadEvent {
    pub pad_id: TabletPadId,
    pub tablet_id: Option<TabletId>,
    pub kind: TabletPadEventKind,
    pub modifiers: Modifiers,
}

/// Unified pen tablet input event covering both stylus tools and express-key pads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TabletEvent {
    Tool(TabletToolEvent),
    Pad(TabletPadEvent),
}

/// Maps a Linux tablet stylus button code onto a fallback [`MouseButton`](super::MouseButton)
/// when a stylus interacts with standard pointer widgets without an `on_tablet` handler.
#[must_use]
pub(crate) const fn map_stylus_button_to_mouse(button: u32) -> super::MouseButton {
    match button {
        // BTN_STYLUS (primary barrel button) -> right-click equivalent
        0x14b => super::MouseButton::Right,
        // BTN_STYLUS2 (secondary barrel button) -> middle-click equivalent
        0x14c => super::MouseButton::Middle,
        // BTN_STYLUS3 -> side button
        0x149 => super::MouseButton::Side,
        other => super::MouseButton::from_linux_button(other),
    }
}

#[cfg(test)]
mod tests {
    use super::map_stylus_button_to_mouse;
    use crate::input::MouseButton;

    #[test]
    fn stylus_button_maps_to_expected_mouse_buttons() {
        assert_eq!(map_stylus_button_to_mouse(0x14b), MouseButton::Right);
        assert_eq!(map_stylus_button_to_mouse(0x14c), MouseButton::Middle);
        assert_eq!(map_stylus_button_to_mouse(0x149), MouseButton::Side);
        assert_eq!(map_stylus_button_to_mouse(0x110), MouseButton::Left);
        assert_eq!(map_stylus_button_to_mouse(0x1ff), MouseButton::Other(0x1ff));
    }
}
