use std::hash::Hash;

use bitflags::bitflags;

use crate::geometry::{Bounds, LogicalPx};
use crate::image::ImageAsset;
use crate::text::SharedStr;

/// Stable key identifying a declared Wayland surface across `view(state)` updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceKey(pub u64);

impl SurfaceKey {
    #[must_use]
    pub fn from_hashable<T: Hash + 'static>(value: &T) -> Self {
        Self(crate::arena::hash_typed_key::<T, Self>(value, |k| k.0))
    }
}

/// Conversion trait allowing `&str`, integers, tuples, and domain keys to identify surfaces.
pub trait IntoSurfaceKey {
    fn into_surface_key(self) -> SurfaceKey;
}

impl<T: Hash + 'static> IntoSurfaceKey for T {
    fn into_surface_key(self) -> SurfaceKey {
        SurfaceKey::from_hashable(&self)
    }
}

/// Layer-shell stacking layer (`zwlr_layer_shell_v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Layer {
    Background,
    Bottom,
    #[default]
    Top,
    Overlay,
}

bitflags! {
    /// Output edge anchor flags for layer-shell surfaces (`zwlr_layer_surface_v1`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Anchor: u8 {
        const TOP    = 1 << 0;
        const BOTTOM = 1 << 1;
        const LEFT   = 1 << 2;
        const RIGHT  = 1 << 3;
    }
}

/// Keyboard focus policy requested from the Wayland compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeyboardInteractivity {
    /// Surface never steals keyboard focus (ideal for passive bars and OSDs).
    #[default]
    None,
    /// Surface receives keyboard focus when explicitly clicked or focused (`OnDemand`).
    OnDemand,
    /// Surface grabs exclusive keyboard focus while mapped (ideal for launchers/lockscreens).
    Exclusive,
}

/// Pointer/touch input region policy for a WayWi surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InputMode {
    /// Entire surface bounds receive input (`wl_surface.set_input_region(None)`).
    #[default]
    Full,
    /// Only interactive element bounds receive input; transparent space passes clicks through.
    Selective,
    /// Entire surface passes all pointer/touch input through (`empty wl_region`).
    PassThrough,
}

/// Client-side SHM buffer memory retention policy for a [`Surface`](super::Surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BufferRetention {
    /// Keep allocated `wl_shm` buffers mapped across frames for low-latency redraws (default).
    #[default]
    Retain,
    /// Unmap and destroy client-side `wl_shm` buffers once the compositor sends
    /// `wl_buffer.release` and no redraw is pending (ideal for static wallpapers and large
    /// rarely-updated surfaces to minimize steady-state RSS).
    ReleaseAfterCommit,
}

/// High-level Wayland surface intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceKind {
    /// Edge-anchored panel surface with optional exclusive zone (`zwlr_layer_shell_v1`).
    Panel,
    /// Compact bounded layer surface sized to its content/explicit bounds (`zwlr_layer_shell_v1`).
    Floating,
    /// Full-output transparent overlay surface (`zwlr_layer_shell_v1`).
    Overlay,
    /// Explicit fallback XDG toplevel window (`xdg_wm_base`) when layer-shell is unavailable.
    Window,
    /// Compositor-positioned XDG popup (`xdg_popup` + `xdg_positioner`) attached to a parent
    /// layer-shell (`zwlr_layer_surface_v1.get_popup`), toplevel window, or parent popup.
    Popup,
    /// Native Wayland session lockscreen surface (`ext_session_lock_v1`) covering an output.
    SessionLock,
}

/// Window decoration policy negotiated via `zxdg_decoration_manager_v1` for [`Surface::window`](super::Surface::window).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WindowDecoration {
    /// Request server-side window decorations (titlebar & borders drawn by compositor).
    #[default]
    Server,
    /// Let the compositor choose its default decoration mode (`ServerDefault`).
    Auto,
    /// Request client-side window decorations (application draws its own titlebar/chrome).
    Client,
    /// Do not attach a `zxdg_toplevel_decoration_v1` object; render borderless/client-only.
    None,
}

/// Declarative background blur region requested via `ext_background_effect_v1`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BackgroundBlur {
    /// No compositor background blur.
    #[default]
    None,
    /// Blur the entire surface bounds.
    Full,
    /// Blur a specific surface-local logical rectangle.
    Region(Bounds),
}

/// Declarative window icon requested via `xdg_toplevel_icon_v1` on [`Surface::window`](super::Surface::window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowIcon {
    /// Icon theme name (e.g. `"org.waywi.demo"`, `"audio-volume-high"`).
    Name(SharedStr),
    /// Loaded WayWi image asset uploaded as a Wayland SHM icon buffer.
    Asset(ImageAsset),
}

/// Target surface color space requested via `wp_color_management_v1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorSpace {
    /// Use the compositor's default color management (typically sRGB; unsets explicit description).
    #[default]
    Default,
    /// Standard sRGB primaries + sRGB transfer function.
    Srgb,
    /// Wide-gamut Display P3 primaries + sRGB transfer function.
    DisplayP3,
    /// HDR BT.2020 primaries + SMPTE ST 2084 (PQ) transfer function.
    Rec2020Pq,
    /// Extended linear sRGB primaries + linear transfer function.
    LinearSrgb,
}

/// Anchor point on the parent surface's anchor rectangle (`xdg_positioner.set_anchor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PopupAnchor {
    None,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    #[default]
    BottomLeft,
    TopRight,
    BottomRight,
}

/// Direction in which the popup grows away from the anchor point (`xdg_positioner.set_gravity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PopupGravity {
    None,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    BottomLeft,
    TopRight,
    #[default]
    BottomRight,
}

bitflags! {
    /// Compositor repositioning flags applied when an `xdg_popup` would otherwise be clipped by
    /// an output edge (`xdg_positioner.set_constraint_adjustment`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PopupConstraintAdjustment: u32 {
        const SLIDE_X  = 1 << 0;
        const SLIDE_Y  = 1 << 1;
        const FLIP_X   = 1 << 2;
        const FLIP_Y   = 1 << 3;
        const RESIZE_X = 1 << 4;
        const RESIZE_Y = 1 << 5;
    }
}

impl Default for PopupConstraintAdjustment {
    #[inline]
    fn default() -> Self {
        Self::FLIP_X | Self::FLIP_Y | Self::SLIDE_X | Self::SLIDE_Y
    }
}

/// Positioning rules passed to `xdg_positioner` for [`Surface::popup`](super::Surface::popup).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopupPositioner {
    /// Rectangle in the parent surface's logical coordinates that the popup anchors to.
    pub anchor_rect: Bounds,
    /// Point on `anchor_rect` where the popup attaches.
    pub anchor: PopupAnchor,
    /// Direction in which the popup extends from the anchor point.
    pub gravity: PopupGravity,
    /// Logical `(dx, dy)` offset applied after anchor/gravity placement.
    pub offset_x: LogicalPx,
    pub offset_y: LogicalPx,
    /// Constraint-adjustment flags when near monitor edges (default: flip + slide on X and Y).
    pub constraint_adjustment: PopupConstraintAdjustment,
    /// Request reactive repositioning (`xdg_positioner.set_reactive`, `xdg_wm_base >= 3`).
    pub reactive: bool,
}

impl Default for PopupPositioner {
    fn default() -> Self {
        Self {
            anchor_rect: Bounds::from_xywh(0.0, 0.0, 1.0, 1.0),
            anchor: PopupAnchor::default(),
            gravity: PopupGravity::default(),
            offset_x: LogicalPx::ZERO,
            offset_y: LogicalPx::ZERO,
            constraint_adjustment: PopupConstraintAdjustment::default(),
            reactive: true,
        }
    }
}

impl PopupPositioner {
    /// Creates a positioner anchored to `anchor_rect` in the parent surface's logical coordinates.
    #[must_use]
    pub fn new(anchor_rect: Bounds) -> Self {
        Self {
            anchor_rect,
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn anchor(mut self, anchor: PopupAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    #[must_use]
    pub const fn gravity(mut self, gravity: PopupGravity) -> Self {
        self.gravity = gravity;
        self
    }

    #[must_use]
    pub const fn offset(mut self, dx: LogicalPx, dy: LogicalPx) -> Self {
        self.offset_x = dx;
        self.offset_y = dy;
        self
    }

    #[must_use]
    pub const fn constraint_adjustment(mut self, flags: PopupConstraintAdjustment) -> Self {
        self.constraint_adjustment = flags;
        self
    }

    #[must_use]
    pub const fn reactive(mut self, reactive: bool) -> Self {
        self.reactive = reactive;
        self
    }
}

/// Internal popup attachment specification stored on `Surface<M>` and `SurfaceSpec`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PopupSpec {
    pub(crate) parent: SurfaceKey,
    pub(crate) positioner: PopupPositioner,
    pub(crate) grab: bool,
    /// Declared logical size resolved during reconciliation so dynamic popup resizing triggers
    /// `xdg_popup.reposition` without conflicting with the compositor's configured render size.
    pub(crate) desired_size: Option<crate::geometry::LogicalSize>,
}
