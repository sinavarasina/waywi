use std::any::TypeId;
use std::hash::{Hash, Hasher};

use crate::arena::FrameSharedFn;
use crate::geometry::{Bounds, LogicalPx, LogicalSize, Size};
use crate::input::CommonProps;
#[cfg(feature = "renderer-software")]
use crate::render::software::path::{
    build_rounded_rect_path, build_skia_fill_paint, to_tiny_skia_path,
};
use crate::render::style::{Color, CornerRadius, Fill};

/// Vector path command recorded by [`PathBuilder`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathOp {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Backend-agnostic 2D path builder for [`Painter`].
#[derive(Debug, Clone, Default)]
pub struct PathBuilder {
    pub(crate) ops: Vec<PathOp>,
}

impl PathBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, x: LogicalPx, y: LogicalPx) -> &mut Self {
        self.ops.push(PathOp::MoveTo(x.0, y.0));
        self
    }

    pub fn line_to(&mut self, x: LogicalPx, y: LogicalPx) -> &mut Self {
        self.ops.push(PathOp::LineTo(x.0, y.0));
        self
    }

    pub fn quad_to(
        &mut self,
        cx: LogicalPx,
        cy: LogicalPx,
        x: LogicalPx,
        y: LogicalPx,
    ) -> &mut Self {
        self.ops.push(PathOp::QuadTo(cx.0, cy.0, x.0, y.0));
        self
    }

    pub fn cubic_to(
        &mut self,
        cx1: LogicalPx,
        cy1: LogicalPx,
        cx2: LogicalPx,
        cy2: LogicalPx,
        x: LogicalPx,
        y: LogicalPx,
    ) -> &mut Self {
        self.ops
            .push(PathOp::CubicTo(cx1.0, cy1.0, cx2.0, cy2.0, x.0, y.0));
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.ops.push(PathOp::Close);
        self
    }
}

/// Backend-agnostic recorded vector command captured by [`Painter`] for GPU vector backends.
#[cfg(feature = "renderer-gpu")]
#[derive(Debug, Clone)]
pub(crate) enum CanvasOp {
    Clear(Color),
    FillRect(Bounds, Fill),
    FillRoundedRect(Bounds, CornerRadius, Fill),
    StrokeRect(Bounds, LogicalPx, Color),
    FillCircle(LogicalPx, LogicalPx, LogicalPx, Fill),
    StrokeLine(LogicalPx, LogicalPx, LogicalPx, LogicalPx, LogicalPx, Color),
    FillPath(Vec<PathOp>, Fill),
    StrokePath(Vec<PathOp>, LogicalPx, Color),
}

/// Procedural 2D drawing context passed to [`Canvas`] draw closures.
pub struct Painter<'a> {
    pub(crate) logical_size: LogicalSize,
    #[cfg_attr(not(feature = "renderer-software"), allow(dead_code))]
    pub(crate) scale: f32,
    pub(crate) rgba_pixels: Option<&'a mut [u8]>,
    #[cfg(feature = "renderer-gpu")]
    pub(crate) recorded_ops: Option<Vec<CanvasOp>>,
    #[cfg_attr(not(feature = "renderer-software"), allow(dead_code))]
    pub(crate) width_px: u32,
    #[cfg_attr(not(feature = "renderer-software"), allow(dead_code))]
    pub(crate) height_px: u32,
}

impl<'a> Painter<'a> {
    pub(crate) fn new_pixels(
        logical_size: LogicalSize,
        scale: f32,
        rgba_pixels: &'a mut [u8],
        width_px: u32,
        height_px: u32,
    ) -> Self {
        Self {
            logical_size,
            scale,
            rgba_pixels: Some(rgba_pixels),
            #[cfg(feature = "renderer-gpu")]
            recorded_ops: None,
            width_px,
            height_px,
        }
    }

    #[cfg(feature = "renderer-gpu")]
    pub(crate) fn new_recorded(logical_size: LogicalSize, scale: f32) -> Self {
        Self {
            logical_size,
            scale,
            rgba_pixels: None,
            recorded_ops: Some(Vec::new()),
            width_px: 0,
            height_px: 0,
        }
    }

    #[cfg(feature = "renderer-gpu")]
    pub(crate) fn take_recorded_ops(&mut self) -> Vec<CanvasOp> {
        self.recorded_ops.take().unwrap_or_default()
    }

    #[must_use]
    pub fn size(&self) -> LogicalSize {
        self.logical_size
    }

    /// Clears the canvas buffer to `color`.
    pub fn clear(&mut self, color: Color) {
        #[cfg(feature = "renderer-gpu")]
        if let Some(ops) = &mut self.recorded_ops {
            ops.push(CanvasOp::Clear(color));
            return;
        }
        if let Some(pixels) = &mut self.rgba_pixels {
            let pm = color.to_premultiplied_rgba();
            for chunk in pixels.as_chunks_mut::<4>().0 {
                chunk.copy_from_slice(&pm);
            }
        }
    }

    pub fn fill_rect(&mut self, rect: Bounds, fill: impl Into<Fill>) {
        let fill = fill.into();
        #[cfg(feature = "renderer-gpu")]
        if let Some(ops) = &mut self.recorded_ops {
            ops.push(CanvasOp::FillRect(rect, fill));
            return;
        }
        #[cfg(feature = "renderer-software")]
        {
            if let Some(pixels) = &mut self.rgba_pixels
                && let Some(mut pixmap) =
                    tiny_skia::PixmapMut::from_bytes(pixels, self.width_px, self.height_px)
                && let Some(r) = tiny_skia::Rect::from_xywh(
                    rect.x.0 * self.scale,
                    rect.y.0 * self.scale,
                    rect.width.0 * self.scale,
                    rect.height.0 * self.scale,
                )
                && let Some(paint) = build_skia_fill_paint(&fill, rect, self.scale, 1.0)
            {
                pixmap.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
            }
        }
        #[cfg(not(feature = "renderer-software"))]
        {
            let _ = (rect, fill);
        }
    }

    pub fn fill_rounded_rect(&mut self, rect: Bounds, radius: CornerRadius, fill: impl Into<Fill>) {
        let fill = fill.into();
        #[cfg(feature = "renderer-gpu")]
        if let Some(ops) = &mut self.recorded_ops {
            ops.push(CanvasOp::FillRoundedRect(rect, radius, fill));
            return;
        }
        #[cfg(feature = "renderer-software")]
        {
            if let Some(pixels) = &mut self.rgba_pixels
                && let Some(mut pixmap) =
                    tiny_skia::PixmapMut::from_bytes(pixels, self.width_px, self.height_px)
                && let Some(path) = build_rounded_rect_path(rect, radius, self.scale)
                && let Some(paint) = build_skia_fill_paint(&fill, rect, self.scale, 1.0)
            {
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
        #[cfg(not(feature = "renderer-software"))]
        {
            let _ = (rect, radius, fill);
        }
    }

    pub fn stroke_rect(&mut self, rect: Bounds, width: LogicalPx, color: Color) {
        #[cfg(feature = "renderer-gpu")]
        if let Some(ops) = &mut self.recorded_ops {
            ops.push(CanvasOp::StrokeRect(rect, width, color));
            return;
        }
        #[cfg(feature = "renderer-software")]
        {
            if let Some(pixels) = &mut self.rgba_pixels
                && let Some(mut pixmap) =
                    tiny_skia::PixmapMut::from_bytes(pixels, self.width_px, self.height_px)
                && let Some(r) = tiny_skia::Rect::from_xywh(
                    rect.x.0 * self.scale,
                    rect.y.0 * self.scale,
                    rect.width.0 * self.scale,
                    rect.height.0 * self.scale,
                )
            {
                let path = tiny_skia::PathBuilder::from_rect(r);
                let mut paint = tiny_skia::Paint::default();
                paint.set_color_rgba8(color.r, color.g, color.b, color.a);
                paint.anti_alias = true;
                let stroke = tiny_skia::Stroke {
                    width: (width.0 * self.scale).max(0.5),
                    ..Default::default()
                };
                pixmap.stroke_path(
                    &path,
                    &paint,
                    &stroke,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
        #[cfg(not(feature = "renderer-software"))]
        {
            let _ = (rect, width, color);
        }
    }

    pub fn fill_circle(
        &mut self,
        cx: LogicalPx,
        cy: LogicalPx,
        radius: LogicalPx,
        fill: impl Into<Fill>,
    ) {
        let fill = fill.into();
        #[cfg(feature = "renderer-gpu")]
        if let Some(ops) = &mut self.recorded_ops {
            ops.push(CanvasOp::FillCircle(cx, cy, radius, fill));
            return;
        }
        #[cfg(feature = "renderer-software")]
        {
            let r = radius.0.max(0.1);
            let circle_bounds = Bounds::new(
                LogicalPx(cx.0 - r),
                LogicalPx(cy.0 - r),
                LogicalPx(r * 2.0),
                LogicalPx(r * 2.0),
            );
            if let Some(pixels) = &mut self.rgba_pixels
                && let Some(mut pixmap) =
                    tiny_skia::PixmapMut::from_bytes(pixels, self.width_px, self.height_px)
                && let Some(path) = tiny_skia::PathBuilder::from_circle(
                    cx.0 * self.scale,
                    cy.0 * self.scale,
                    (radius.0 * self.scale).max(0.1),
                )
                && let Some(paint) = build_skia_fill_paint(&fill, circle_bounds, self.scale, 1.0)
            {
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
        #[cfg(not(feature = "renderer-software"))]
        {
            let _ = (cx, cy, radius, fill);
        }
    }

    pub fn stroke_line(
        &mut self,
        x1: LogicalPx,
        y1: LogicalPx,
        x2: LogicalPx,
        y2: LogicalPx,
        width: LogicalPx,
        color: Color,
    ) {
        #[cfg(feature = "renderer-gpu")]
        if let Some(ops) = &mut self.recorded_ops {
            ops.push(CanvasOp::StrokeLine(x1, y1, x2, y2, width, color));
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(x1, y1).line_to(x2, y2);
        self.stroke_path(&pb, width, color);
    }

    pub fn fill_path(&mut self, builder: &PathBuilder, fill: impl Into<Fill>) {
        let fill = fill.into();
        #[cfg(feature = "renderer-gpu")]
        if let Some(ops) = &mut self.recorded_ops {
            ops.push(CanvasOp::FillPath(builder.ops.clone(), fill));
            return;
        }
        #[cfg(feature = "renderer-software")]
        {
            let canvas_bounds = Bounds::new(
                LogicalPx::ZERO,
                LogicalPx::ZERO,
                self.logical_size.width,
                self.logical_size.height,
            );
            if let Some(pixels) = &mut self.rgba_pixels
                && let Some(mut pixmap) =
                    tiny_skia::PixmapMut::from_bytes(pixels, self.width_px, self.height_px)
                && let Some(path) = to_tiny_skia_path(builder, self.scale)
                && let Some(paint) = build_skia_fill_paint(&fill, canvas_bounds, self.scale, 1.0)
            {
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
        #[cfg(not(feature = "renderer-software"))]
        {
            let _ = (builder, fill);
        }
    }

    pub fn stroke_path(&mut self, builder: &PathBuilder, width: LogicalPx, color: Color) {
        #[cfg(feature = "renderer-gpu")]
        if let Some(ops) = &mut self.recorded_ops {
            ops.push(CanvasOp::StrokePath(builder.ops.clone(), width, color));
            return;
        }
        #[cfg(feature = "renderer-software")]
        {
            if let Some(pixels) = &mut self.rgba_pixels
                && let Some(mut pixmap) =
                    tiny_skia::PixmapMut::from_bytes(pixels, self.width_px, self.height_px)
                && let Some(path) = to_tiny_skia_path(builder, self.scale)
            {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color_rgba8(color.r, color.g, color.b, color.a);
                paint.anti_alias = true;
                let stroke = tiny_skia::Stroke {
                    width: (width.0 * self.scale).max(0.5),
                    ..Default::default()
                };
                pixmap.stroke_path(
                    &path,
                    &paint,
                    &stroke,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
        #[cfg(not(feature = "renderer-software"))]
        {
            let _ = (builder, width, color);
        }
    }
}

/// Procedural drawing boundary isolating imperative 2D commands from `view`.
#[must_use]
#[derive(Clone)]
pub struct Canvas<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) cache_key: Option<u64>,
    pub(crate) draw: FrameSharedFn<*mut Painter<'static>, ()>,
}

impl<M: 'static> Canvas<M> {
    /// Creates an uncached procedural `Canvas` element with explicit `size` and draw closure.
    ///
    /// Use [`Self::cache_key`] for static or dependency-keyed drawings whose rasterized pixmap /
    /// vector commands should be cached across frames.
    pub fn new<F>(size: impl Into<Size>, draw: F) -> Self
    where
        F: Fn(&mut Painter<'_>) + 'static,
    {
        let common = CommonProps {
            size: size.into(),
            ..Default::default()
        };

        let erased = FrameSharedFn::new(move |painter_ptr: *mut Painter<'static>| {
            // SAFETY: Caller guarantees `painter_ptr` points to a valid `Painter` exclusively
            // for the synchronous duration of this call.
            let painter = unsafe { &mut *painter_ptr };
            draw(painter);
        });

        Self {
            common,
            cache_key: None,
            draw: erased,
        }
    }

    /// Enables intermediate caching keyed by `key` so `draw` only re-executes when `key`, size,
    /// or scale changes.
    pub fn cache_key<K: Hash + 'static>(mut self, key: K) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        TypeId::of::<K>().hash(&mut hasher);
        key.hash(&mut hasher);
        self.cache_key = Some(hasher.finish());
        self
    }

    crate::input::impl_common_builders!();

    pub fn map<N: 'static, F>(self, f: F) -> Canvas<N>
    where
        F: Fn(M) -> N + 'static,
    {
        Canvas {
            common: self.common.map(f),
            cache_key: self.cache_key,
            draw: self.draw,
        }
    }
}
