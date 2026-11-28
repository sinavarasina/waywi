use crate::geometry::{Bounds, LogicalPx, Point};

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

/// Main-axis distribution inside [`Row`](super::Row) and [`Column`](super::Column) containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Justify {
    /// Pack items at the start of the main axis (default).
    #[default]
    Start,
    /// Center items along the main axis.
    Center,
    /// Pack items at the end of the main axis.
    End,
    /// Distribute remaining space evenly between adjacent items.
    SpaceBetween,
    /// Distribute remaining space around each item (half-space on outer edges).
    SpaceAround,
    /// Distribute remaining space equally between all items and outer edges.
    SpaceEvenly,
}

/// Color stop along a [`LinearGradient`] or [`RadialGradient`] (`offset` in `0.0..=1.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub offset: f32,
    pub color: Color,
}

impl GradientStop {
    #[inline]
    #[must_use]
    pub fn new(offset: f32, color: Color) -> Self {
        Self {
            offset: offset.clamp(0.0, 1.0),
            color,
        }
    }
}

/// Linear color gradient using normalized `[0.0, 1.0]` coordinates relative to the target bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    /// Start point in normalized `[0.0, 1.0]` box coordinates (`(0, 0)` = top-left).
    pub start: Point,
    /// End point in normalized `[0.0, 1.0]` box coordinates (`(1, 1)` = bottom-right).
    pub end: Point,
    /// Ordered color stops (`offset` in `0.0..=1.0`).
    pub stops: Vec<GradientStop>,
}

impl LinearGradient {
    /// Creates a linear gradient between normalized `start` and `end` coordinates.
    #[must_use]
    pub fn new(start: Point, end: Point, stops: impl IntoIterator<Item = GradientStop>) -> Self {
        Self {
            start,
            end,
            stops: stops.into_iter().collect(),
        }
    }

    /// Creates a left-to-right linear gradient from `from` to `to`.
    #[must_use]
    pub fn horizontal(from: Color, to: Color) -> Self {
        Self::new(
            Point::new(LogicalPx(0.0), LogicalPx(0.5)),
            Point::new(LogicalPx(1.0), LogicalPx(0.5)),
            [GradientStop::new(0.0, from), GradientStop::new(1.0, to)],
        )
    }

    /// Creates a top-to-bottom linear gradient from `from` to `to`.
    #[must_use]
    pub fn vertical(from: Color, to: Color) -> Self {
        Self::new(
            Point::new(LogicalPx(0.5), LogicalPx(0.0)),
            Point::new(LogicalPx(0.5), LogicalPx(1.0)),
            [GradientStop::new(0.0, from), GradientStop::new(1.0, to)],
        )
    }

    /// Creates a linear gradient at `angle_deg` degrees (`0°` = left-to-right, `90°` = top-to-bottom).
    #[must_use]
    pub fn angle_deg(angle_deg: f32, stops: impl IntoIterator<Item = GradientStop>) -> Self {
        let rad = angle_deg.to_radians();
        let dx = rad.cos() * 0.5;
        let dy = rad.sin() * 0.5;
        Self::new(
            Point::new(LogicalPx(0.5 - dx), LogicalPx(0.5 - dy)),
            Point::new(LogicalPx(0.5 + dx), LogicalPx(0.5 + dy)),
            stops,
        )
    }

    #[must_use]
    pub fn is_visible(&self) -> bool {
        !self.stops.is_empty() && self.stops.iter().any(|s| s.color.a > 0)
    }
}

/// Radial color gradient using normalized `[0.0, 1.0]` center coordinates and radius relative to
/// the target box's longest edge.
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    /// Center point in normalized `[0.0, 1.0]` box coordinates (`(0.5, 0.5)` = center).
    pub center: Point,
    /// Radius normalized to `max(width, height)` (`0.5` reaches the edge of a square box).
    pub radius: f32,
    /// Ordered color stops (`offset` in `0.0..=1.0`).
    pub stops: Vec<GradientStop>,
}

impl RadialGradient {
    /// Creates a radial gradient centered at `center` with normalized `radius`.
    #[must_use]
    pub fn new(center: Point, radius: f32, stops: impl IntoIterator<Item = GradientStop>) -> Self {
        Self {
            center,
            radius: radius.max(0.001),
            stops: stops.into_iter().collect(),
        }
    }

    /// Creates a centered radial gradient from `inner` (`offset 0.0`) to `outer` (`offset 1.0`).
    #[must_use]
    pub fn centered(inner: Color, outer: Color) -> Self {
        Self::new(
            Point::new(LogicalPx(0.5), LogicalPx(0.5)),
            0.5,
            [GradientStop::new(0.0, inner), GradientStop::new(1.0, outer)],
        )
    }

    #[must_use]
    pub fn is_visible(&self) -> bool {
        !self.stops.is_empty() && self.stops.iter().any(|s| s.color.a > 0)
    }
}

/// Fill brush for [`Rect`](super::Rect) and [`Painter`](super::Painter) primitives.
#[derive(Debug, Clone, PartialEq)]
pub enum Fill {
    Solid(Color),
    Linear(LinearGradient),
    Radial(RadialGradient),
}

impl Default for Fill {
    fn default() -> Self {
        Self::Solid(Color::TRANSPARENT)
    }
}

impl Fill {
    /// Returns `true` when the fill has at least one non-transparent color/stop.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        match self {
            Self::Solid(c) => c.a > 0,
            Self::Linear(g) => g.is_visible(),
            Self::Radial(g) => g.is_visible(),
        }
    }
}

impl From<Color> for Fill {
    #[inline]
    fn from(color: Color) -> Self {
        Self::Solid(color)
    }
}

impl From<LinearGradient> for Fill {
    #[inline]
    fn from(gradient: LinearGradient) -> Self {
        Self::Linear(gradient)
    }
}

impl From<RadialGradient> for Fill {
    #[inline]
    fn from(gradient: RadialGradient) -> Self {
        Self::Radial(gradient)
    }
}

/// Drop shadow attached to a [`Rect`](super::Rect).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadow {
    pub offset_x: LogicalPx,
    pub offset_y: LogicalPx,
    pub blur_radius: LogicalPx,
    pub spread: LogicalPx,
    pub color: Color,
}

impl BoxShadow {
    /// Creates a drop shadow with `(offset_x, offset_y)`, `blur_radius`, and `color` (zero spread).
    #[inline]
    #[must_use]
    pub const fn new(
        offset_x: LogicalPx,
        offset_y: LogicalPx,
        blur_radius: LogicalPx,
        color: Color,
    ) -> Self {
        Self {
            offset_x,
            offset_y,
            blur_radius,
            spread: LogicalPx::ZERO,
            color,
        }
    }

    /// Sets the shadow spread expansion in logical pixels.
    #[inline]
    #[must_use]
    pub const fn spread(mut self, spread: LogicalPx) -> Self {
        self.spread = spread;
        self
    }

    #[inline]
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.color.a > 0
            && (self.blur_radius.0 > 0.0
                || self.spread.0 != 0.0
                || self.offset_x.0 != 0.0
                || self.offset_y.0 != 0.0)
    }

    /// Expands `bounds` to conservatively enclose the rendered shadow region (for dirty rects).
    #[must_use]
    pub fn expand_bounds(&self, bounds: Bounds) -> Bounds {
        let pad = self.blur_radius.0.max(0.0) + self.spread.0.max(0.0) + 1.0;
        let sx = bounds.x.0 + self.offset_x.0 - pad;
        let sy = bounds.y.0 + self.offset_y.0 - pad;
        let sx2 = bounds.x.0 + bounds.width.0 + self.offset_x.0 + pad;
        let sy2 = bounds.y.0 + bounds.height.0 + self.offset_y.0 + pad;
        let min_x = bounds.x.0.min(sx);
        let min_y = bounds.y.0.min(sy);
        let max_x = (bounds.x.0 + bounds.width.0).max(sx2);
        let max_y = (bounds.y.0 + bounds.height.0).max(sy2);
        Bounds::new(
            LogicalPx(min_x),
            LogicalPx(min_y),
            LogicalPx((max_x - min_x).max(0.0)),
            LogicalPx((max_y - min_y).max(0.0)),
        )
    }
}
