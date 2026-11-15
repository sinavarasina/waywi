use crate::geometry::LogicalPx;

/// RGBA color in sRGB 8-bit channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    pub const BLACK: Self = Self::rgba(0, 0, 0, 255);
    pub const WHITE: Self = Self::rgba(255, 255, 255, 255);
    pub const GRAY: Self = Self::rgba(128, 128, 128, 255);
    pub const RED: Self = Self::rgba(255, 0, 0, 255);
    pub const GREEN: Self = Self::rgba(0, 255, 0, 255);
    pub const BLUE: Self = Self::rgba(0, 0, 255, 255);

    #[inline]
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    #[inline]
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    #[inline]
    #[must_use]
    pub fn rgba_f32(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self {
            r: (r.clamp(0.0, 1.0) * 255.0).round() as u8,
            g: (g.clamp(0.0, 1.0) * 255.0).round() as u8,
            b: (b.clamp(0.0, 1.0) * 255.0).round() as u8,
            a: (a.clamp(0.0, 1.0) * 255.0).round() as u8,
        }
    }

    /// Parses a `0xRRGGBBAA` packed integer color.
    #[inline]
    #[must_use]
    pub const fn from_hex_rgba(hex: u32) -> Self {
        Self {
            r: ((hex >> 24) & 0xff) as u8,
            g: ((hex >> 16) & 0xff) as u8,
            b: ((hex >> 8) & 0xff) as u8,
            a: (hex & 0xff) as u8,
        }
    }

    /// Parses a `0xRRGGBB` packed integer color with full alpha.
    #[inline]
    #[must_use]
    pub const fn from_hex_rgb(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xff) as u8,
            g: ((hex >> 8) & 0xff) as u8,
            b: (hex & 0xff) as u8,
            a: 255,
        }
    }

    #[inline]
    #[must_use]
    pub fn with_opacity(self, opacity: f32) -> Self {
        let a = (f32::from(self.a) * opacity.clamp(0.0, 1.0)).round() as u8;
        Self { a, ..self }
    }

    /// Returns premultiplied `[R, G, B, A]` bytes.
    #[inline]
    #[must_use]
    pub fn to_premultiplied_rgba(self) -> [u8; 4] {
        if self.a == 255 {
            [self.r, self.g, self.b, 255]
        } else if self.a == 0 {
            [0, 0, 0, 0]
        } else {
            let a = u16::from(self.a);
            [
                ((u16::from(self.r) * a + 127) / 255) as u8,
                ((u16::from(self.g) * a + 127) / 255) as u8,
                ((u16::from(self.b) * a + 127) / 255) as u8,
                self.a,
            ]
        }
    }

    /// Returns premultiplied little-endian `wl_shm::Format::Argb8888` (`[B, G, R, A]`) bytes.
    #[inline]
    #[must_use]
    pub fn to_premultiplied_bgra(self) -> [u8; 4] {
        let [r, g, b, a] = self.to_premultiplied_rgba();
        [b, g, r, a]
    }
}

/// Per-corner radii for rectangles and clip masks.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CornerRadius {
    pub top_left: LogicalPx,
    pub top_right: LogicalPx,
    pub bottom_right: LogicalPx,
    pub bottom_left: LogicalPx,
}

impl CornerRadius {
    pub const ZERO: Self = Self::all(LogicalPx::ZERO);

    #[inline]
    #[must_use]
    pub const fn all(radius: LogicalPx) -> Self {
        Self {
            top_left: radius,
            top_right: radius,
            bottom_right: radius,
            bottom_left: radius,
        }
    }

    #[inline]
    #[must_use]
    pub const fn new(
        top_left: LogicalPx,
        top_right: LogicalPx,
        bottom_right: LogicalPx,
        bottom_left: LogicalPx,
    ) -> Self {
        Self {
            top_left,
            top_right,
            bottom_right,
            bottom_left,
        }
    }

    #[inline]
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.top_left.0 <= 0.0
            && self.top_right.0 <= 0.0
            && self.bottom_right.0 <= 0.0
            && self.bottom_left.0 <= 0.0
    }

    /// Returns `[top_left, top_right, bottom_right, bottom_left]` scaled by `scale` and clamped to
    /// half the minimum box dimension.
    #[cfg(any(feature = "renderer-software", feature = "renderer-gpu"))]
    #[inline]
    #[must_use]
    pub(crate) fn clamped(self, width: f32, height: f32, scale: f32) -> [f32; 4] {
        let max_r = (width.min(height) * scale * 0.5).max(0.0);
        [
            (self.top_left.0 * scale).clamp(0.0, max_r),
            (self.top_right.0 * scale).clamp(0.0, max_r),
            (self.bottom_right.0 * scale).clamp(0.0, max_r),
            (self.bottom_left.0 * scale).clamp(0.0, max_r),
        ]
    }
}

impl From<LogicalPx> for CornerRadius {
    #[inline]
    fn from(value: LogicalPx) -> Self {
        Self::all(value)
    }
}

/// Border stroke style for [`Rect`](super::Rect).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub width: LogicalPx,
    pub color: Color,
}

/// Cross-axis alignment inside [`Row`](super::Row) and [`Column`](super::Column) containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}
