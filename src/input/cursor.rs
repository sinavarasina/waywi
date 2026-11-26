//! Cursor shape, pointer constraint, relative pointer, and virtual keyboard input types.

/// Compositor cursor shape requested via `wp_cursor_shape_v1` (or hidden via `wl_pointer.set_cursor(None)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorShape {
    /// Standard default arrow cursor.
    #[default]
    Default,
    ContextMenu,
    Help,
    /// Clickable link/button hand pointer.
    Pointer,
    Progress,
    Wait,
    Cell,
    /// Precision crosshair (useful for region selection, color pickers, and drawing canvases).
    Crosshair,
    /// Text selection I-beam.
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    /// Open hand indicating grabbable content.
    Grab,
    /// Closed hand indicating active drag/pan.
    Grabbing,
    EResize,
    NResize,
    NeResize,
    NwResize,
    SResize,
    SeResize,
    SwResize,
    WResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ColResize,
    RowResize,
    AllScroll,
    ZoomIn,
    ZoomOut,
    /// Hides the hardware cursor completely on this surface/element (`wl_pointer.set_cursor(serial, None, 0, 0)`).
    Hidden,
}

/// Pointer constraint policy requested via `zwp_pointer_constraints_v1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PointerConstraintMode {
    /// Normal unconstrained pointer movement.
    #[default]
    None,
    /// Locks the pointer in place while focused (`zwp_pointer_constraints_v1.lock_pointer`),
    /// ideal for infinite rotary dials/knobs or precision canvas panning paired with
    /// [`RelativePointerEvent`].
    Locked,
    /// Confines the pointer within the surface bounds (`zwp_pointer_constraints_v1.confine_pointer`).
    Confined,
}

/// Unaccelerated relative pointer motion delta delivered via `zwp_relative_pointer_v1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelativePointerEvent {
    /// Monotonic compositor timestamp in microseconds.
    pub time_us: u64,
    /// Accelerated horizontal delta in surface-local logical units.
    pub dx: f64,
    /// Accelerated vertical delta in surface-local logical units.
    pub dy: f64,
    /// Raw unaccelerated horizontal delta.
    pub dx_unaccel: f64,
    /// Raw unaccelerated vertical delta.
    pub dy_unaccel: f64,
}

/// Key state passed to [`EffectCtx::virtual_key`](crate::effect::EffectCtx::virtual_key)
/// (`zwp_virtual_keyboard_v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VirtualKeyState {
    Released,
    Pressed,
}
