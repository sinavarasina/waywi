use super::primitives::Point;
use super::units::{BufferPx, LogicalPx, ScaleFactor};

/// Axis-aligned rectangle in logical surface coordinates.
///
/// Alias [`Bounds`] is also exported for callers avoiding name collision with the visual
/// [`Rect`](crate::render::Rect) primitive.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: LogicalPx,
    pub y: LogicalPx,
    pub width: LogicalPx,
    pub height: LogicalPx,
}

/// Convenience alias for [`Rect`] in layout and hit-testing APIs.
pub type Bounds = Rect;
/// Alias for [`Rect`] when avoiding collision with [`Rect`](crate::render::Rect) visual nodes.
pub type GeoRect = Rect;

impl Rect {
    pub const ZERO: Self = Self {
        x: LogicalPx::ZERO,
        y: LogicalPx::ZERO,
        width: LogicalPx::ZERO,
        height: LogicalPx::ZERO,
    };

    #[inline]
    #[must_use]
    pub const fn new(x: LogicalPx, y: LogicalPx, width: LogicalPx, height: LogicalPx) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[inline]
    #[must_use]
    pub const fn from_xywh(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x: LogicalPx(x),
            y: LogicalPx(y),
            width: LogicalPx(width),
            height: LogicalPx(height),
        }
    }

    #[inline]
    #[must_use]
    pub fn right(self) -> LogicalPx {
        self.x + self.width
    }

    #[inline]
    #[must_use]
    pub fn bottom(self) -> LogicalPx {
        self.y + self.height
    }

    #[inline]
    #[must_use]
    pub fn center(self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    #[inline]
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.width.0 <= 0.0 || self.height.0 <= 0.0
    }

    #[inline]
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        point.x.0 >= self.x.0
            && point.x.0 < self.x.0 + self.width.0
            && point.y.0 >= self.y.0
            && point.y.0 < self.y.0 + self.height.0
    }

    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        let w = x1 - x0;
        let h = y1 - y0;
        if w.0 > 0.0 && h.0 > 0.0 {
            Some(Self::new(x0, y0, w, h))
        } else {
            None
        }
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    #[must_use]
    pub fn outset(self, margin: LogicalPx) -> Self {
        Self::new(
            self.x - margin,
            self.y - margin,
            self.width + margin * 2.0,
            self.height + margin * 2.0,
        )
    }

    /// Converts a logical rect into an integer physical buffer damage rect expanded outward.
    #[must_use]
    pub fn to_buffer_rect(
        self,
        scale: ScaleFactor,
        max_w: BufferPx,
        max_h: BufferPx,
    ) -> BufferRect {
        let s = scale.as_f32();
        let x0 = (self.x.0 * s).floor() as i32;
        let y0 = (self.y.0 * s).floor() as i32;
        let x1 = ((self.x.0 + self.width.0) * s).ceil() as i32;
        let y1 = ((self.y.0 + self.height.0) * s).ceil() as i32;
        let cx0 = x0.clamp(0, max_w.0);
        let cy0 = y0.clamp(0, max_h.0);
        let cx1 = x1.clamp(cx0, max_w.0);
        let cy1 = y1.clamp(cy0, max_h.0);
        BufferRect {
            x: cx0,
            y: cy0,
            width: cx1 - cx0,
            height: cy1 - cy0,
        }
    }
}

/// Physical pixel rectangle inside a `wl_shm` buffer (for `wl_surface.damage_buffer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BufferRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl BufferRect {
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    #[inline]
    #[must_use]
    pub const fn right(self) -> i32 {
        self.x + self.width
    }

    #[inline]
    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.y + self.height
    }

    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width).min(other.x + other.width);
        let y1 = (self.y + self.height).min(other.y + other.height);
        if x1 > x0 && y1 > y0 {
            Some(Self {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            })
        } else {
            None
        }
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.width).max(other.x + other.width);
        let y1 = (self.y + self.height).max(other.y + other.height);
        Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }
}
