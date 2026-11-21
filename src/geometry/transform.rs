use super::primitives::Point;
use super::rect::Rect;
use super::units::LogicalPx;

/// Element-local 2D transform around a normalized pivot (default `(0.5, 0.5)` = center) plus
/// post-translation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub translate_x: LogicalPx,
    pub translate_y: LogicalPx,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation_rad: f32,
    /// Normalized horizontal pivot within the element's bounds (`0.0` = left, `0.5` = center, `1.0` = right).
    pub pivot_x: f32,
    /// Normalized vertical pivot within the element's bounds (`0.0` = top, `0.5` = center, `1.0` = bottom).
    pub pivot_y: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self::identity()
    }
}

impl Transform {
    #[inline]
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            translate_x: LogicalPx::ZERO,
            translate_y: LogicalPx::ZERO,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_rad: 0.0,
            pivot_x: 0.5,
            pivot_y: 0.5,
        }
    }

    #[inline]
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.translate_x.0 == 0.0
            && self.translate_y.0 == 0.0
            && (self.scale_x - 1.0).abs() < f32::EPSILON
            && (self.scale_y - 1.0).abs() < f32::EPSILON
            && self.rotation_rad.abs() < f32::EPSILON
    }

    #[inline]
    #[must_use]
    pub fn is_pure_translation(&self) -> bool {
        (self.scale_x - 1.0).abs() < f32::EPSILON
            && (self.scale_y - 1.0).abs() < f32::EPSILON
            && self.rotation_rad.abs() < f32::EPSILON
    }

    #[inline]
    #[must_use]
    pub const fn translate(mut self, x: LogicalPx, y: LogicalPx) -> Self {
        self.translate_x = x;
        self.translate_y = y;
        self
    }

    #[inline]
    #[must_use]
    pub const fn scale(mut self, factor: f32) -> Self {
        self.scale_x = factor;
        self.scale_y = factor;
        self
    }

    #[inline]
    #[must_use]
    pub const fn scale_xy(mut self, sx: f32, sy: f32) -> Self {
        self.scale_x = sx;
        self.scale_y = sy;
        self
    }

    #[inline]
    #[must_use]
    pub fn rotate_deg(mut self, degrees: f32) -> Self {
        self.rotation_rad = degrees.to_radians();
        self
    }

    #[inline]
    #[must_use]
    pub const fn rotate_rad(mut self, radians: f32) -> Self {
        self.rotation_rad = radians;
        self
    }

    /// Sets the normalized rotation/scale pivot within the element's bounds
    /// (`(0.5, 0.5)` = center, `(0.5, 1.0)` = bottom-center, `(0.0, 0.5)` = left-center).
    #[inline]
    #[must_use]
    pub const fn pivot(mut self, nx: f32, ny: f32) -> Self {
        self.pivot_x = nx;
        self.pivot_y = ny;
        self
    }

    /// Resolves this transform's logical pivot point inside `bounds`.
    #[inline]
    #[must_use]
    pub fn pivot_point(&self, bounds: Rect) -> Point {
        Point::new(
            LogicalPx(bounds.x.0 + bounds.width.0 * self.pivot_x),
            LogicalPx(bounds.y.0 + bounds.height.0 * self.pivot_y),
        )
    }

    /// Computes a 2D affine matrix `[sx, ky, kx, sy, tx, ty]` in logical space pivoting around `pivot`.
    #[must_use]
    pub fn to_affine_around(&self, pivot: Point) -> Affine2D {
        let (sin, cos) = self.rotation_rad.sin_cos();
        let a = cos * self.scale_x;
        let b = sin * self.scale_x;
        let c = -sin * self.scale_y;
        let d = cos * self.scale_y;
        let px = pivot.x.0;
        let py = pivot.y.0;
        let tx = px + self.translate_x.0 - (a * px + c * py);
        let ty = py + self.translate_y.0 - (b * px + d * py);
        Affine2D { a, b, c, d, tx, ty }
    }
}

/// 2D affine transform `[x', y'] = [a*x + c*y + tx, b*x + d*y + ty]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2D {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Default for Affine2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine2D {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    #[inline]
    #[must_use]
    pub fn is_identity(&self) -> bool {
        (self.a - 1.0).abs() < 1e-6
            && self.b.abs() < 1e-6
            && self.c.abs() < 1e-6
            && (self.d - 1.0).abs() < 1e-6
            && self.tx.abs() < 1e-6
            && self.ty.abs() < 1e-6
    }

    #[inline]
    #[must_use]
    pub fn is_axis_aligned_unscaled(&self) -> bool {
        (self.a - 1.0).abs() < 1e-6
            && self.b.abs() < 1e-6
            && self.c.abs() < 1e-6
            && (self.d - 1.0).abs() < 1e-6
    }

    /// Composes `self * rhs` (apply `rhs` first, then `self`).
    #[must_use]
    pub fn then(&self, outer: &Self) -> Self {
        Self {
            a: outer.a * self.a + outer.c * self.b,
            b: outer.b * self.a + outer.d * self.b,
            c: outer.a * self.c + outer.c * self.d,
            d: outer.b * self.c + outer.d * self.d,
            tx: outer.a * self.tx + outer.c * self.ty + outer.tx,
            ty: outer.b * self.tx + outer.d * self.ty + outer.ty,
        }
    }

    #[inline]
    #[must_use]
    pub fn transform_point(&self, p: Point) -> Point {
        Point::new(
            LogicalPx(self.a * p.x.0 + self.c * p.y.0 + self.tx),
            LogicalPx(self.b * p.x.0 + self.d * p.y.0 + self.ty),
        )
    }

    /// Inverts the affine matrix for hit-testing transformed elements.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-8 {
            return None;
        }
        let inv = 1.0 / det;
        let a = self.d * inv;
        let b = -self.b * inv;
        let c = -self.c * inv;
        let d = self.a * inv;
        let tx = -(a * self.tx + c * self.ty);
        let ty = -(b * self.tx + d * self.ty);
        Some(Self { a, b, c, d, tx, ty })
    }

    /// Computes the axis-aligned bounding box of `rect` transformed by `self`.
    #[must_use]
    pub fn transform_rect(&self, rect: Rect) -> Rect {
        let p0 = self.transform_point(Point::new(rect.x, rect.y));
        let p1 = self.transform_point(Point::new(rect.x + rect.width, rect.y));
        let p2 = self.transform_point(Point::new(rect.x, rect.y + rect.height));
        let p3 = self.transform_point(Point::new(rect.x + rect.width, rect.y + rect.height));
        let min_x = p0.x.min(p1.x).min(p2.x).min(p3.x);
        let max_x = p0.x.max(p1.x).max(p2.x).max(p3.x);
        let min_y = p0.y.min(p1.y).min(p2.y).min(p3.y);
        let max_y = p0.y.max(p1.y).max(p2.y).max(p3.y);
        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}
