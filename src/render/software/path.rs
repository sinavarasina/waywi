//! Vector path and clip mask conversion helpers for the `tiny-skia` software backend.

#[cfg(feature = "renderer-software")]
use crate::geometry::{Bounds, BufferRect};
#[cfg(feature = "renderer-software")]
use crate::render::canvas::{PathBuilder, PathOp};
#[cfg(feature = "renderer-software")]
use crate::render::style::CornerRadius;

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
