//! Cross-compositor Wayland desktop extension types and subscription factories:
//! - `ext_session_lock_v1` ([`SessionLock`], [`SessionLockEvent`])
//! - `ext_image_copy_capture_v1` / `zwlr_screencopy_manager_v1` ([`ScreencopyTarget`], [`ScreencopyFrame`])
//! - `zwlr_foreign_toplevel_management_v1` / `ext_foreign_toplevel_list_v1` ([`ForeignToplevel`], [`ForeignToplevelSnapshot`])
//! - `ext_workspace_v1` ([`ExtWorkspace`], [`ExtWorkspaceSnapshot`])
//! - `zwlr_gamma_control_manager_v1` ([`GammaRamp`])
//! - `wp_color_management_v1` ([`OutputColorInfo`])

use bitflags::bitflags;

use crate::arena::FrameSharedFn;
use crate::display::{Output, OutputId};
use crate::error::{Result, WaywiError};
use crate::geometry::Bounds;
use crate::render::Color;
use crate::subscription::{Subscription, SubscriptionSpec};
use crate::surface::ColorSpace;

/// Lifecycle events emitted by the Wayland `ext_session_lock_v1` session lock protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionLockEvent {
    /// The compositor has locked all outputs and is displaying WayWi's lock surfaces.
    Locked,
    /// The compositor rejected or terminated the lock session (e.g., another client already holds
    /// the lock). Applications should remove their `Surface::session_lock` surfaces or exit.
    Finished,
}

/// Subscription factory for `ext_session_lock_v1` lock lifecycle events.
pub struct SessionLock;

impl SessionLock {
    /// Subscribes to [`SessionLockEvent`] notifications (`Locked` / `Finished`).
    ///
    /// Declaring one or more [`Surface::session_lock`](crate::surface::Surface::session_lock)
    /// surfaces automatically acquires the compositor lock; removing all lock surfaces triggers
    /// `unlock_and_destroy()`.
    pub fn subscribe<M: 'static>(map: impl Fn(SessionLockEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::SessionLock {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Target area requested via [`EffectCtx::capture_screen`](crate::effect::EffectCtx::capture_screen).
#[derive(Debug, Clone, PartialEq)]
pub enum ScreencopyTarget {
    /// Capture an entire Wayland output.
    Output(Output),
    /// Capture a logical sub-region of a Wayland output.
    Region { output: Output, bounds: Bounds },
}

/// Decoded RGBA8 screen capture frame returned asynchronously by
/// [`EffectCtx::capture_screen`](crate::effect::EffectCtx::capture_screen).
///
/// Pixels are always normalized to top-to-bottom straight RGBA8 (`4` bytes per pixel,
/// `stride == width * 4`) regardless of compositor SHM format (`Xrgb8888`, `Argb8888`,
/// `Xbgr8888`, `Abgr8888`) or `y_invert` flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreencopyFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub rgba: Vec<u8>,
}

impl ScreencopyFrame {
    /// Returns `[r, g, b, a]` at pixel coordinate `(x, y)`, or `None` if out of bounds.
    #[must_use]
    pub fn pixel_rgba(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let off = (y as usize)
            .checked_mul(self.stride as usize)?
            .checked_add((x as usize).checked_mul(4)?)?;
        let slice = self.rgba.get(off..off + 4)?;
        Some([slice[0], slice[1], slice[2], slice[3]])
    }

    /// Returns a [`Color`] at pixel coordinate `(x, y)`, or `None` if out of bounds.
    #[must_use]
    pub fn pixel_color(&self, x: u32, y: u32) -> Option<Color> {
        let [r, g, b, a] = self.pixel_rgba(x, y)?;
        Some(Color::rgba(r, g, b, a))
    }

    /// Borrows the raw top-to-bottom RGBA8 pixel slice.
    #[must_use]
    pub fn as_rgba_bytes(&self) -> &[u8] {
        &self.rgba
    }
}

/// Stable runtime handle ID identifying a foreign toplevel window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ForeignToplevelId(pub u64);

bitflags! {
    /// State flags reported for a foreign toplevel window (`zwlr_foreign_toplevel_management_v1`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct ForeignToplevelState: u8 {
        const ACTIVATED  = 1 << 0;
        const MAXIMIZED  = 1 << 1;
        const MINIMIZED  = 1 << 2;
        const FULLSCREEN = 1 << 3;
    }
}

/// Snapshot metadata for a single foreign toplevel window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignToplevelInfo {
    pub id: ForeignToplevelId,
    /// Compositor-provided unique string identifier (`ext_foreign_toplevel_list_v1`, when present).
    pub identifier: Option<String>,
    pub title: String,
    pub app_id: String,
    pub state: ForeignToplevelState,
    pub outputs: Vec<OutputId>,
    pub parent: Option<ForeignToplevelId>,
    /// `true` when this handle supports control actions (`activate`, `close`, `minimize`, etc.).
    pub can_control: bool,
}

/// Atomic snapshot of all foreign toplevel windows reported by the compositor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForeignToplevelSnapshot {
    pub toplevels: Vec<ForeignToplevelInfo>,
    /// `true` when the compositor backs this list with `zwlr_foreign_toplevel_management_v1`.
    pub can_control: bool,
}

/// Subscription factory for cross-compositor foreign toplevel window tracking
/// (`zwlr_foreign_toplevel_management_v1` preferred, fallback to `ext_foreign_toplevel_list_v1`).
pub struct ForeignToplevel;

impl ForeignToplevel {
    /// Subscribes to atomic [`ForeignToplevelSnapshot`] updates whenever windows open, change
    /// title/app_id/state, or close.
    pub fn subscribe<M: 'static>(
        map: impl Fn(ForeignToplevelSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::ForeignToplevel {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Stable runtime handle ID identifying an `ext_workspace_v1` workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtWorkspaceId(pub u64);

/// Stable runtime handle ID identifying an `ext_workspace_v1` workspace group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtWorkspaceGroupId(pub u64);

bitflags! {
    /// State flags for an `ext_workspace_v1` workspace.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct ExtWorkspaceState: u8 {
        const ACTIVE = 1 << 0;
        const URGENT = 1 << 1;
        const HIDDEN = 1 << 2;
    }
}

bitflags! {
    /// Allowed mutations advertised by the compositor for an `ext_workspace_v1` workspace.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct ExtWorkspaceCapabilities: u8 {
        const ACTIVATE   = 1 << 0;
        const DEACTIVATE = 1 << 1;
        const REMOVE     = 1 << 2;
        const ASSIGN     = 1 << 3;
    }
}

/// Snapshot metadata for one workspace in `ext_workspace_v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtWorkspaceInfo {
    pub id: ExtWorkspaceId,
    /// Optional stable compositor-assigned string ID.
    pub protocol_id: Option<String>,
    pub name: String,
    pub coordinates: Vec<u32>,
    pub state: ExtWorkspaceState,
    pub capabilities: ExtWorkspaceCapabilities,
    pub group: Option<ExtWorkspaceGroupId>,
}

/// Snapshot metadata for one workspace group in `ext_workspace_v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtWorkspaceGroupInfo {
    pub id: ExtWorkspaceGroupId,
    pub can_create_workspace: bool,
    pub outputs: Vec<OutputId>,
    pub workspaces: Vec<ExtWorkspaceId>,
}

/// Atomic snapshot of all groups and workspaces committed on `ext_workspace_manager_v1.done`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtWorkspaceSnapshot {
    pub groups: Vec<ExtWorkspaceGroupInfo>,
    pub workspaces: Vec<ExtWorkspaceInfo>,
}

/// Subscription factory for the cross-compositor `ext_workspace_v1` protocol.
pub struct ExtWorkspace;

impl ExtWorkspace {
    /// Subscribes to atomic [`ExtWorkspaceSnapshot`] updates emitted on `done`.
    pub fn subscribe<M: 'static>(
        map: impl Fn(ExtWorkspaceSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::ExtWorkspace {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Hardware RGB gamma lookup table uploaded via `zwlr_gamma_control_manager_v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GammaRamp {
    pub red: Vec<u16>,
    pub green: Vec<u16>,
    pub blue: Vec<u16>,
}

impl GammaRamp {
    /// Builds a neutral linear identity gamma ramp of `size` entries (`0..=65535`).
    #[must_use]
    pub fn identity(size: usize) -> Self {
        Self::from_rgb_scales(size, 1.0, 1.0, 1.0)
    }

    /// Builds a color-temperature + brightness gamma ramp using a smooth Planckian/Tanner
    /// whitepoint approximation (`kelvin` clamped to `1000..=25000`, `brightness` clamped to
    /// `0.05..=1.0`).
    ///
    /// Typical values: `6500 K` (neutral daylight), `4000 K` (warm evening), `3000 K` (night light).
    #[must_use]
    pub fn from_temperature(size: usize, kelvin: u32, brightness: f32) -> Self {
        let (r, g, b) = kelvin_to_rgb_multipliers(kelvin);
        let br = if brightness.is_finite() {
            brightness.clamp(0.05, 1.0)
        } else {
            1.0
        };
        Self::from_rgb_scales(size, r * br, g * br, b * br)
    }

    /// Builds a linear gamma ramp scaled by per-channel multipliers (`0.0..=1.0`).
    #[must_use]
    pub fn from_rgb_scales(size: usize, r_scale: f32, g_scale: f32, b_scale: f32) -> Self {
        let len = size.max(2);
        let denom = (len - 1) as f32;
        let rs = r_scale.clamp(0.0, 1.0);
        let gs = g_scale.clamp(0.0, 1.0);
        let bs = b_scale.clamp(0.0, 1.0);

        let mut red = Vec::with_capacity(len);
        let mut green = Vec::with_capacity(len);
        let mut blue = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / denom;
            red.push((t * rs * 65535.0).round().clamp(0.0, 65535.0) as u16);
            green.push((t * gs * 65535.0).round().clamp(0.0, 65535.0) as u16);
            blue.push((t * bs * 65535.0).round().clamp(0.0, 65535.0) as u16);
        }
        Self { red, green, blue }
    }

    /// Validates that all three channels are non-empty and have equal length.
    pub fn validate(&self) -> Result<()> {
        if self.red.is_empty()
            || self.red.len() != self.green.len()
            || self.red.len() != self.blue.len()
        {
            return Err(WaywiError::InvalidState(
                "gamma ramp red/green/blue tables must be non-empty and equal length".to_owned(),
            ));
        }
        Ok(())
    }

    /// Resamples this ramp to `target_size` entries using linear interpolation when the
    /// compositor's hardware LUT size differs from the caller's table length.
    #[must_use]
    pub fn resample(&self, target_size: usize) -> Self {
        let target = target_size.max(2);
        if self.red.len() == target && self.green.len() == target && self.blue.len() == target {
            return self.clone();
        }
        Self {
            red: resample_channel(&self.red, target),
            green: resample_channel(&self.green, target),
            blue: resample_channel(&self.blue, target),
        }
    }
}

fn resample_channel(src: &[u16], target: usize) -> Vec<u16> {
    if src.is_empty() {
        return vec![0; target];
    }
    if src.len() == 1 {
        return vec![src[0]; target];
    }
    let max_src = (src.len() - 1) as f32;
    let denom = (target - 1) as f32;
    (0..target)
        .map(|i| {
            let pos = (i as f32 / denom) * max_src;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(src.len() - 1);
            let frac = pos - lo as f32;
            let v = f32::from(src[lo]) * (1.0 - frac) + f32::from(src[hi]) * frac;
            v.round().clamp(0.0, 65535.0) as u16
        })
        .collect()
}

/// Converts a correlated color temperature in Kelvin (`1000..=25000`) into normalized RGB channel
/// multipliers (`0.0..=1.0`), calibrated so `6500 K` is neutral `(1.0, 1.0, 1.0)`.
#[must_use]
pub fn kelvin_to_rgb_multipliers(kelvin: u32) -> (f32, f32, f32) {
    let k = kelvin.clamp(1000, 25000);
    if (6450..=6550).contains(&k) {
        return (1.0, 1.0, 1.0);
    }
    let temp = (k as f64) / 100.0;
    let r = if temp <= 66.0 {
        255.0
    } else {
        (329.698727446 * (temp - 60.0).powf(-0.1332047592)).clamp(0.0, 255.0)
    };
    let g = if temp <= 66.0 {
        (99.4708025861 * temp.ln() - 161.1195681661).clamp(0.0, 255.0)
    } else {
        (288.1221695283 * (temp - 60.0).powf(-0.0755148492)).clamp(0.0, 255.0)
    };
    let b = if temp >= 66.0 {
        255.0
    } else if temp <= 19.0 {
        0.0
    } else {
        (138.5177312231 * (temp - 10.0).ln() - 305.0447927307).clamp(0.0, 255.0)
    };
    let max_c = r.max(g).max(b).max(1.0);
    ((r / max_c) as f32, (g / max_c) as f32, (b / max_c) as f32)
}

/// Color-management metadata reported for a Wayland output (`wp_color_management_v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputColorInfo {
    pub output_id: OutputId,
    pub hdr_supported: bool,
    pub preferred_color_space: ColorSpace,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_ramp_temperature_and_resampling_behave_monotonically() {
        let neutral = GammaRamp::from_temperature(256, 6500, 1.0);
        assert_eq!(neutral.red.len(), 256);
        assert_eq!(neutral.red[0], 0);
        assert_eq!(neutral.red[255], 65535);
        assert_eq!(neutral.red, neutral.green);
        assert_eq!(neutral.red, neutral.blue);

        let warm = GammaRamp::from_temperature(256, 3200, 0.8);
        assert!(warm.red[255] > warm.green[255]);
        assert!(warm.green[255] > warm.blue[255]);
        assert!(warm.red[255] <= (0.81 * 65535.0) as u16);

        let resampled = warm.resample(1024);
        assert_eq!(resampled.red.len(), 1024);
        assert_eq!(resampled.red[0], warm.red[0]);
        assert_eq!(resampled.red[1023], warm.red[255]);
    }
}
