//! Vector path and clip mask conversion helpers for the `tiny-skia` software backend.

#[cfg(feature = "renderer-software")]
use crate::geometry::{Bounds, BufferRect, LogicalPx};
#[cfg(feature = "renderer-software")]
use crate::render::canvas::{PathBuilder, PathOp};
#[cfg(feature = "renderer-software")]
use crate::render::style::{BoxShadow, CornerRadius, Fill, GradientStop};

#[cfg(feature = "renderer-software")]
pub(super) fn build_clip_mask(
    tile_w: u32,
    tile_h: u32,
    clip: BufferRect,
) -> Option<tiny_skia::Mask> {
    if clip.x <= 0 && clip.y <= 0 && clip.width >= tile_w as i32 && clip.height >= tile_h as i32 {
        return None;
    }
    let mut mask = tiny_skia::Mask::new(tile_w, tile_h)?;
    if let Some(cr) = tiny_skia::Rect::from_xywh(
        clip.x as f32,
        clip.y as f32,
        clip.width as f32,
        clip.height as f32,
    ) {
        let cpath = tiny_skia::PathBuilder::from_rect(cr);
        mask.fill_path(
            &cpath,
            tiny_skia::FillRule::Winding,
            false,
            tiny_skia::Transform::identity(),
        );
    }
    Some(mask)
}

#[cfg(feature = "renderer-software")]
pub(crate) fn build_rounded_rect_path(
    rect: Bounds,
    radius: CornerRadius,
    scale: f32,
) -> Option<tiny_skia::Path> {
    let x = rect.x.0 * scale;
    let y = rect.y.0 * scale;
    let w = rect.width.0 * scale;
    let h = rect.height.0 * scale;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    if radius.is_zero() {
        let r = tiny_skia::Rect::from_xywh(x, y, w, h)?;
        return Some(tiny_skia::PathBuilder::from_rect(r));
    }

    let [tl, tr, br, bl] = radius.clamped(w, h, 1.0);

    // Kappa approximation for quarter-circle cubic Beziers.
    const K: f32 = 0.552_284_8;
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x + tl, y);
    pb.line_to(x + w - tr, y);
    if tr > 0.0 {
        pb.cubic_to(
            x + w - tr + tr * K,
            y,
            x + w,
            y + tr - tr * K,
            x + w,
            y + tr,
        );
    }
    pb.line_to(x + w, y + h - br);
    if br > 0.0 {
        pb.cubic_to(
            x + w,
            y + h - br + br * K,
            x + w - br + br * K,
            y + h,
            x + w - br,
            y + h,
        );
    }
    pb.line_to(x + bl, y + h);
    if bl > 0.0 {
        pb.cubic_to(
            x + bl - bl * K,
            y + h,
            x,
            y + h - bl + bl * K,
            x,
            y + h - bl,
        );
    }
    pb.line_to(x, y + tl);
    if tl > 0.0 {
        pb.cubic_to(x, y + tl - tl * K, x + tl - tl * K, y, x + tl, y);
    }
    pb.close();
    pb.finish()
}

#[cfg(feature = "renderer-software")]
pub(crate) fn to_tiny_skia_path(builder: &PathBuilder, scale: f32) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for &op in &builder.ops {
        match op {
            PathOp::MoveTo(x, y) => pb.move_to(x * scale, y * scale),
            PathOp::LineTo(x, y) => pb.line_to(x * scale, y * scale),
            PathOp::QuadTo(cx, cy, x, y) => {
                pb.quad_to(cx * scale, cy * scale, x * scale, y * scale);
            }
            PathOp::CubicTo(cx1, cy1, cx2, cy2, x, y) => {
                pb.cubic_to(
                    cx1 * scale,
                    cy1 * scale,
                    cx2 * scale,
                    cy2 * scale,
                    x * scale,
                    y * scale,
                );
            }
            PathOp::Close => pb.close(),
        }
    }
    pb.finish()
}

#[cfg(feature = "renderer-software")]
fn to_skia_stops(stops: &[GradientStop], opacity: f32) -> Vec<tiny_skia::GradientStop> {
    let mut sorted: Vec<GradientStop> = stops.to_vec();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    sorted
        .into_iter()
        .map(|s| {
            let c = s.color.with_opacity(opacity);
            tiny_skia::GradientStop::new(
                s.offset.clamp(0.0, 1.0),
                tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a),
            )
        })
        .collect()
}

/// Builds a `tiny_skia::Paint` for solid colors, linear gradients, or radial gradients in the
/// scaled coordinate space of `rect`.
#[cfg(feature = "renderer-software")]
pub(crate) fn build_skia_fill_paint<'a>(
    fill: &Fill,
    rect: Bounds,
    scale: f32,
    opacity: f32,
) -> Option<tiny_skia::Paint<'a>> {
    let mut paint = tiny_skia::Paint {
        anti_alias: true,
        ..Default::default()
    };
    let rx = rect.x.0 * scale;
    let ry = rect.y.0 * scale;
    let rw = (rect.width.0 * scale).max(1.0);
    let rh = (rect.height.0 * scale).max(1.0);

    match fill {
        Fill::Solid(c) => {
            let eff = c.with_opacity(opacity);
            if eff.a == 0 {
                return None;
            }
            paint.set_color_rgba8(eff.r, eff.g, eff.b, eff.a);
        }
        Fill::Linear(g) => {
            let stops = to_skia_stops(&g.stops, opacity);
            let start = tiny_skia::Point::from_xy(rx + g.start.x.0 * rw, ry + g.start.y.0 * rh);
            let mut end = tiny_skia::Point::from_xy(rx + g.end.x.0 * rw, ry + g.end.y.0 * rh);
            if (end.x - start.x).abs() < 1e-4 && (end.y - start.y).abs() < 1e-4 {
                end.x += 1.0;
            }
            paint.shader = tiny_skia::LinearGradient::new(
                start,
                end,
                stops,
                tiny_skia::SpreadMode::Pad,
                tiny_skia::Transform::identity(),
            )?;
        }
        Fill::Radial(g) => {
            let stops = to_skia_stops(&g.stops, opacity);
            let center = tiny_skia::Point::from_xy(rx + g.center.x.0 * rw, ry + g.center.y.0 * rh);
            let radius = (g.radius * rw.max(rh)).max(0.5);
            paint.shader = tiny_skia::RadialGradient::new(
                center,
                0.0,
                center,
                radius,
                stops,
                tiny_skia::SpreadMode::Pad,
                tiny_skia::Transform::identity(),
            )?;
        }
    }
    Some(paint)
}

/// Renders a soft layered drop shadow beneath `rect` on `tiny_skia::PixmapMut` without allocating
/// an intermediate blur pixmap.
#[cfg(feature = "renderer-software")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_skia_box_shadow(
    pixmap: &mut tiny_skia::PixmapMut<'_>,
    rect: Bounds,
    corner_radius: CornerRadius,
    shadow: BoxShadow,
    scale: f32,
    opacity: f32,
    transform: tiny_skia::Transform,
    mask: Option<&tiny_skia::Mask>,
) {
    let base_color = shadow.color.with_opacity(opacity);
    if base_color.a == 0 {
        return;
    }
    let spread = shadow.spread.0;
    let blur = shadow.blur_radius.0.max(0.0);
    let base_rect = Bounds::new(
        LogicalPx(rect.x.0 + shadow.offset_x.0 - spread),
        LogicalPx(rect.y.0 + shadow.offset_y.0 - spread),
        LogicalPx((rect.width.0 + spread * 2.0).max(0.0)),
        LogicalPx((rect.height.0 + spread * 2.0).max(0.0)),
    );
    if base_rect.width.0 <= 0.0 || base_rect.height.0 <= 0.0 {
        return;
    }

    if blur <= 0.5 {
        let rad = CornerRadius::new(
            LogicalPx((corner_radius.top_left.0 + spread).max(0.0)),
            LogicalPx((corner_radius.top_right.0 + spread).max(0.0)),
            LogicalPx((corner_radius.bottom_right.0 + spread).max(0.0)),
            LogicalPx((corner_radius.bottom_left.0 + spread).max(0.0)),
        );
        if let Some(path) = build_rounded_rect_path(base_rect, rad, scale) {
            let mut paint = tiny_skia::Paint {
                anti_alias: true,
                ..Default::default()
            };
            paint.set_color_rgba8(base_color.r, base_color.g, base_color.b, base_color.a);
            pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, mask);
        }
        return;
    }

    let steps: usize = ((blur * 0.5).ceil() as usize).clamp(3, 8);
    let base_alpha = f32::from(base_color.a) / 255.0;
    // Concentric layered Gaussian-falloff approximation: cumulative inner opacity approaches
    // `base_alpha` while outer rings feather smoothly over `blur`.
    let per_step_alpha = 1.0 - (1.0 - base_alpha).powf(1.0 / steps as f32);
    for i in (0..steps).rev() {
        let t = (i as f32 + 0.5) / steps as f32;
        let expand = blur * t;
        let weight = (-2.2 * t * t).exp();
        let layer_a = (per_step_alpha * weight * 255.0).round().clamp(1.0, 255.0) as u8;
        let layer_rect = Bounds::new(
            LogicalPx(base_rect.x.0 - expand),
            LogicalPx(base_rect.y.0 - expand),
            LogicalPx(base_rect.width.0 + expand * 2.0),
            LogicalPx(base_rect.height.0 + expand * 2.0),
        );
        let rad = CornerRadius::new(
            LogicalPx((corner_radius.top_left.0 + spread + expand).max(0.0)),
            LogicalPx((corner_radius.top_right.0 + spread + expand).max(0.0)),
            LogicalPx((corner_radius.bottom_right.0 + spread + expand).max(0.0)),
            LogicalPx((corner_radius.bottom_left.0 + spread + expand).max(0.0)),
        );
        if let Some(path) = build_rounded_rect_path(layer_rect, rad, scale) {
            let mut paint = tiny_skia::Paint {
                anti_alias: true,
                ..Default::default()
            };
            paint.set_color_rgba8(base_color.r, base_color.g, base_color.b, layer_a);
            pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, mask);
        }
    }
}
