use std::hash::{Hash, Hasher};

use bitflags::bitflags;

/// Stable key identifying a declared Wayland surface across `view(state)` updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceKey(pub u64);

impl SurfaceKey {
    #[must_use]
    pub fn from_hashable<T: Hash + 'static>(value: &T) -> Self {
        use std::any::TypeId;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        TypeId::of::<T>().hash(&mut hasher);
        value.hash(&mut hasher);
        Self(hasher.finish())
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

impl Anchor {
    #[allow(non_upper_case_globals)]
    pub const Top: Self = Self::TOP;
    #[allow(non_upper_case_globals)]
    pub const Bottom: Self = Self::BOTTOM;
    #[allow(non_upper_case_globals)]
    pub const Left: Self = Self::LEFT;
    #[allow(non_upper_case_globals)]
    pub const Right: Self = Self::RIGHT;
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
}
