//! Translates WayWi `DrawCommand` and `CanvasOp` primitives into `femtovg` vector calls.

use femtovg::renderer::WGPURenderer;
use femtovg::{Canvas, Color as FemtoColor, Paint, Path, Transform2D};

use crate::geometry::{Affine2D, Bounds, LogicalPx, LogicalSize};
use crate::image::ImageFit;
use crate::render::backend::FrameScene;
use crate::render::canvas::{CanvasOp, PathOp};
use crate::render::retained::{DrawCommand, PaintPayload};
use crate::render::style::{BoxShadow, Color, CornerRadius, Fill};
#[cfg(feature = "text")]
use crate::text::TextMeasureStyle;
#[cfg(feature = "text")]
use crate::text::engine::{TextDrawParams, TextEngine};

use super::textures::GpuTextureCache;

#[inline]
pub(crate) fn to_femto_color(c: Color, opacity: f32) -> FemtoColor {
    let alpha = ((f32::from(c.a) * opacity.clamp(0.0, 1.0)).round() as u32).min(255) as u8;
    FemtoColor::rgba(c.r, c.g, c.b, alpha)
}

fn build_femto_fill_paint(fill: &Fill, rect: Bounds, opacity: f32) -> Option<Paint> {
    let mut paint = match fill {
        Fill::Solid(c) => {
            if c.a == 0 {
                return None;
            }
            Paint::color(to_femto_color(*c, opacity))
        }
        Fill::Linear(g) => {
            if !g.is_visible() {
                return None;
            }
            let x0 = rect.x.0 + g.start.x.0 * rect.width.0;
            let y0 = rect.y.0 + g.start.y.0 * rect.height.0;
            let mut x1 = rect.x.0 + g.end.x.0 * rect.width.0;
            let y1 = rect.y.0 + g.end.y.0 * rect.height.0;
            if (x1 - x0).abs() < 1e-4 && (y1 - y0).abs() < 1e-4 {
                x1 += 1.0;
            }
            let mut stops: Vec<(f32, FemtoColor)> = g
                .stops
                .iter()
                .map(|s| (s.offset.clamp(0.0, 1.0), to_femto_color(s.color, opacity)))
                .collect();
            stops.sort_by(|a, b| a.0.total_cmp(&b.0));
            Paint::linear_gradient_stops(x0, y0, x1, y1, stops)
        }
        Fill::Radial(g) => {
            if !g.is_visible() {
                return None;
            }
            let cx = rect.x.0 + g.center.x.0 * rect.width.0;
            let cy = rect.y.0 + g.center.y.0 * rect.height.0;
            let radius = (g.radius * rect.width.0.max(rect.height.0)).max(0.5);
            let mut stops: Vec<(f32, FemtoColor)> = g
                .stops
                .iter()
                .map(|s| (s.offset.clamp(0.0, 1.0), to_femto_color(s.color, opacity)))
                .collect();
            stops.sort_by(|a, b| a.0.total_cmp(&b.0));
            Paint::radial_gradient_stops(cx, cy, 0.0, radius, stops)
        }
    };
    paint.set_anti_alias(true);
    Some(paint)
}

fn draw_femto_box_shadow(
    canvas: &mut Canvas<WGPURenderer>,
    rect: Bounds,
    corner_radius: CornerRadius,
    shadow: BoxShadow,
    opacity: f32,
) {
    let inner = to_femto_color(shadow.color, opacity);
    if inner.a <= 0.0 {
        return;
    }
    let spread = shadow.spread.0;
    let blur = shadow.blur_radius.0.max(0.0);
    let sx = rect.x.0 + shadow.offset_x.0 - spread;
    let sy = rect.y.0 + shadow.offset_y.0 - spread;
    let sw = (rect.width.0 + spread * 2.0).max(0.0);
    let sh = (rect.height.0 + spread * 2.0).max(0.0);
    if sw <= 0.0 || sh <= 0.0 {
        return;
    }
    let [tl, tr, br, bl] = corner_radius.clamped(rect.width.0, rect.height.0, 1.0);
    let uniform_r = (tl.max(tr).max(br).max(bl) + spread).max(0.0);
    if blur <= 0.5 {
        let p = build_rounded_rect(
            Bounds::new(LogicalPx(sx), LogicalPx(sy), LogicalPx(sw), LogicalPx(sh)),
            CornerRadius::all(LogicalPx(uniform_r)),
        );
        let mut paint = Paint::color(inner);
        paint.set_anti_alias(true);
        canvas.fill_path(&p, &paint);
        return;
    }
    let feather = (blur * 2.0).max(1.0);
    let outer = FemtoColor::rgba(shadow.color.r, shadow.color.g, shadow.color.b, 0);
    let mut path = Path::new();
    path.rect(sx - blur, sy - blur, sw + blur * 2.0, sh + blur * 2.0);
    let mut paint = Paint::box_gradient(sx, sy, sw, sh, uniform_r, feather, inner, outer);
    paint.set_anti_alias(true);
    canvas.fill_path(&path, &paint);
}

#[inline]
fn to_femto_transform(affine: Affine2D, scale: f32) -> Transform2D {
    Transform2D([
        affine.a * scale,
        affine.b * scale,
        affine.c * scale,
        affine.d * scale,
        affine.tx * scale,
        affine.ty * scale,
    ])
}

pub(crate) fn encode_commands(
    canvas: &mut Canvas<WGPURenderer>,
    textures: &mut GpuTextureCache,
    #[cfg(feature = "text")] text_engine: &mut TextEngine,
    scene: FrameScene<'_>,
) {
    let scale = scene.scale;
    let scale_f = scale.as_f32();
    let assets = scene.assets;
    let frame_seq = scene.frame_seq;
    for cmd in scene.commands {
        match &cmd.payload {
            PaintPayload::ClipPush { corner_radius } => {
                canvas.save();
                canvas.reset_transform();
                canvas.set_transform(&to_femto_transform(cmd.affine, scale_f));
                let r = &cmd.local_rect;
                let [tl, tr, br, bl] = corner_radius.clamped(r.width.0, r.height.0, 1.0);
                let uniform_r = tl.max(tr).max(br).max(bl);
                canvas.rounded_scissor(r.x.0, r.y.0, r.width.0, r.height.0, uniform_r);
            }
            PaintPayload::ClipPop => {
                canvas.restore();
            }
            PaintPayload::Rect {
                fill,
                corner_radius,
                border,
                shadow,
            } => {
                let r = cmd.local_rect;
                if r.width.0 <= 0.0 || r.height.0 <= 0.0 {
                    continue;
                }
                canvas.save();
                canvas.reset_transform();
                canvas.set_transform(&to_femto_transform(cmd.affine, scale_f));

                if let Some(sh) = shadow
                    && sh.is_visible()
                {
                    draw_femto_box_shadow(canvas, r, *corner_radius, *sh, cmd.opacity);
                }

                if let Some(paint) = build_femto_fill_paint(fill, r, cmd.opacity) {
                    let path = build_rounded_rect(r, *corner_radius);
                    canvas.fill_path(&path, &paint);
                }

                if let Some(b) = border
                    && b.width.0 > 0.0
                    && b.color.a > 0
                {
                    let half = b.width.0 * 0.5;
                    let inset = Bounds::new(
                        LogicalPx(r.x.0 + half),
                        LogicalPx(r.y.0 + half),
                        LogicalPx((r.width.0 - b.width.0).max(0.0)),
                        LogicalPx((r.height.0 - b.width.0).max(0.0)),
                    );
                    let path = build_rounded_rect(inset, *corner_radius);
                    let mut paint = Paint::color(to_femto_color(b.color, cmd.opacity));
                    paint.set_anti_alias(true);
                    paint.set_line_width(b.width.0.max(0.5 / scale_f));
                    canvas.stroke_path(&path, &paint);
                }
                canvas.restore();
            }
            PaintPayload::Image {
                asset,
                asset_revision,
                fit,
                filter,
                source_rect,
            } => {
                let img = match source_rect {
                    Some(sr) => textures.resolve_sprite_cell(
                        canvas,
                        assets,
                        super::textures::SpriteCellSpec {
                            asset_id: asset.id,
                            asset_rev: *asset_revision,
                            src_rect: sr.as_tuple(),
                            filter: *filter,
                        },
                        frame_seq,
                    ),
                    None => textures.resolve_asset_image(
                        canvas,
                        assets,
                        asset,
                        *asset_revision,
                        *filter,
                        frame_seq,
                    ),
                };
                let Some(img) = img else {
                    continue;
                };
                draw_textured_rect(canvas, cmd, scale_f, img.id, img.width, img.height, *fit);
            }
            PaintPayload::Text {
                content,
                spans,
                font_size,
                line_height_multiplier,
                color,
                font,
                align,
                bounds_mode,
                overflow,
                max_lines,
                wrap_to_width,
            } => {
                #[cfg(feature = "text")]
                {
                    let max_width = (cmd.local_rect.width.0 > 0.0).then_some(cmd.local_rect.width);
                    let box_w_px = (cmd.local_rect.width.0 * scale_f).ceil().max(1.0) as u32;
                    let box_h_px = (cmd.local_rect.height.0 * scale_f).ceil().max(1.0) as u32;
                    let params = TextDrawParams {
                        content,
                        style: TextMeasureStyle {
                            font_size: *font_size,
                            line_height_multiplier: *line_height_multiplier,
                            max_width,
                            font,
                            align: *align,
                            bounds_mode: *bounds_mode,
                            overflow: *overflow,
                            max_lines: *max_lines,
                            spans: spans.as_deref(),
                        },
                        wrap_to_width: *wrap_to_width,
                        scale,
                    };
                    let Some(quad) = textures.resolve_text_quad(
                        canvas,
                        text_engine,
                        params,
                        *color,
                        (box_w_px, box_h_px),
                        frame_seq,
                    ) else {
                        continue;
                    };
                    let pad_px = GpuTextureCache::text_quad_padding_px(*font_size, scale);
                    draw_text_quad(
                        canvas,
                        cmd,
                        scale_f,
                        quad.id,
                        quad.width,
                        quad.height,
                        pad_px,
                    );
                }
                #[cfg(not(feature = "text"))]
                {
                    let _ = (
                        content,
                        spans,
                        font_size,
                        line_height_multiplier,
                        color,
                        font,
                        align,
                        bounds_mode,
                        overflow,
                        max_lines,
                        wrap_to_width,
                    );
                }
            }
            PaintPayload::Canvas { cache_key, draw } => {
                canvas.save();
                canvas.reset_transform();
                let mut offset_affine = cmd.affine;
                offset_affine.tx += cmd.local_rect.x.0;
                offset_affine.ty += cmd.local_rect.y.0;
                canvas.set_transform(&to_femto_transform(offset_affine, scale_f));
                let logical_size = LogicalSize::new(cmd.local_rect.width, cmd.local_rect.height);
                if let Some(key) = *cache_key {
                    let ops =
                        textures.resolve_canvas_ops(cmd.node_id, key, logical_size, scale_f, draw);
                    replay_canvas_ops(canvas, ops, cmd.local_rect, cmd.opacity, scale_f);
                } else {
                    let ops = textures.record_uncached_ops(logical_size, scale_f, draw);
                    replay_canvas_ops(canvas, &ops, cmd.local_rect, cmd.opacity, scale_f);
                }
                canvas.restore();
            }
        }
    }
}

fn draw_textured_rect(
    canvas: &mut Canvas<WGPURenderer>,
    cmd: &DrawCommand,
    scale_f: f32,
    image_id: femtovg::ImageId,
    img_w: u32,
    img_h: u32,
    fit: ImageFit,
) {
    let r = cmd.local_rect;
    if r.width.0 <= 0.0 || r.height.0 <= 0.0 || img_w == 0 || img_h == 0 {
        return;
    }
    let (dx, dy, dw, dh) = fit.resolve_rect(
        r.x.0,
        r.y.0,
        r.width.0,
        r.height.0,
        img_w as f32,
        img_h as f32,
    );

    canvas.save();
    canvas.reset_transform();
    canvas.set_transform(&to_femto_transform(cmd.affine, scale_f));
    let mut path = Path::new();
    path.rect(dx, dy, dw, dh);
    let paint = Paint::image(image_id, dx, dy, dw, dh, 0.0, cmd.opacity.clamp(0.0, 1.0));
    canvas.fill_path(&path, &paint);
    canvas.restore();
}

#[cfg(feature = "text")]
fn draw_text_quad(
    canvas: &mut Canvas<WGPURenderer>,
    cmd: &DrawCommand,
    scale_f: f32,
    image_id: femtovg::ImageId,
    tex_w: u32,
    tex_h: u32,
    pad_px: u32,
) {
    if tex_w == 0 || tex_h == 0 {
        return;
    }
    let r = cmd.local_rect;
    let opacity = cmd.opacity.clamp(0.0, 1.0);
    canvas.save();
    canvas.reset_transform();
    if cmd.affine.is_axis_aligned_unscaled() {
        let pad_f = pad_px as f32;
        let snap_x = ((r.x.0 + cmd.affine.tx) * scale_f).round() - pad_f;
        let snap_y = ((r.y.0 + cmd.affine.ty) * scale_f).round() - pad_f;
        let w = tex_w as f32;
        let h = tex_h as f32;
        let mut path = Path::new();
        path.rect(snap_x, snap_y, w, h);
        let paint = Paint::image(image_id, snap_x, snap_y, w, h, 0.0, opacity);
        canvas.fill_path(&path, &paint);
    } else {
        let pad_logical = pad_px as f32 / scale_f;
        let dw = tex_w as f32 / scale_f;
        let dh = tex_h as f32 / scale_f;
        let dx = r.x.0 - pad_logical;
        let dy = r.y.0 - pad_logical;
        canvas.set_transform(&to_femto_transform(cmd.affine, scale_f));
        let mut path = Path::new();
        path.rect(dx, dy, dw, dh);
        let paint = Paint::image(image_id, dx, dy, dw, dh, 0.0, opacity);
        canvas.fill_path(&path, &paint);
    }
    canvas.restore();
}

fn replay_canvas_ops(
    canvas: &mut Canvas<WGPURenderer>,
    ops: &[CanvasOp],
    bounds: Bounds,
    opacity: f32,
    scale_f: f32,
) {
    for op in ops {
        match op {
            CanvasOp::Clear(c) => {
                let mut p = Path::new();
                p.rect(0.0, 0.0, bounds.width.0, bounds.height.0);
                let paint = Paint::color(to_femto_color(*c, opacity));
                canvas.fill_path(&p, &paint);
            }
            CanvasOp::FillRect(r, fill) => {
                if let Some(paint) = build_femto_fill_paint(fill, *r, opacity) {
                    let mut p = Path::new();
                    p.rect(r.x.0, r.y.0, r.width.0, r.height.0);
                    canvas.fill_path(&p, &paint);
                }
            }
            CanvasOp::FillRoundedRect(r, rad, fill) => {
                if let Some(paint) = build_femto_fill_paint(fill, *r, opacity) {
                    let p = build_rounded_rect(*r, *rad);
                    canvas.fill_path(&p, &paint);
                }
            }
            CanvasOp::StrokeRect(r, width, c) => {
                let mut p = Path::new();
                p.rect(r.x.0, r.y.0, r.width.0, r.height.0);
                let mut paint = Paint::color(to_femto_color(*c, opacity));
                paint.set_anti_alias(true);
                paint.set_line_width(width.0.max(0.5 / scale_f));
                canvas.stroke_path(&p, &paint);
            }
            CanvasOp::FillCircle(cx, cy, rad, fill) => {
                let r = rad.0.max(0.1);
                let circle_bounds = Bounds::new(
                    LogicalPx(cx.0 - r),
                    LogicalPx(cy.0 - r),
                    LogicalPx(r * 2.0),
                    LogicalPx(r * 2.0),
                );
                if let Some(paint) = build_femto_fill_paint(fill, circle_bounds, opacity) {
                    let mut p = Path::new();
                    p.circle(cx.0, cy.0, r);
                    canvas.fill_path(&p, &paint);
                }
            }
            CanvasOp::StrokeLine(x1, y1, x2, y2, width, c) => {
                let mut p = Path::new();
                p.move_to(x1.0, y1.0);
                p.line_to(x2.0, y2.0);
                let mut paint = Paint::color(to_femto_color(*c, opacity));
                paint.set_anti_alias(true);
                paint.set_line_width(width.0.max(0.5 / scale_f));
                canvas.stroke_path(&p, &paint);
            }
            CanvasOp::FillPath(path_ops, fill) => {
                let canvas_bounds = Bounds::new(
                    LogicalPx::ZERO,
                    LogicalPx::ZERO,
                    bounds.width,
                    bounds.height,
                );
                if let Some(paint) = build_femto_fill_paint(fill, canvas_bounds, opacity) {
                    let p = build_path_ops(path_ops);
                    canvas.fill_path(&p, &paint);
                }
            }
            CanvasOp::StrokePath(path_ops, width, c) => {
                let p = build_path_ops(path_ops);
                let mut paint = Paint::color(to_femto_color(*c, opacity));
                paint.set_anti_alias(true);
                paint.set_line_width(width.0.max(0.5 / scale_f));
                canvas.stroke_path(&p, &paint);
            }
        }
    }
}

fn build_rounded_rect(r: Bounds, rad: CornerRadius) -> Path {
    let mut path = Path::new();
    let [tl, tr, br, bl] = rad.clamped(r.width.0, r.height.0, 1.0);
    if tl <= 0.0 && tr <= 0.0 && br <= 0.0 && bl <= 0.0 {
        path.rect(r.x.0, r.y.0, r.width.0, r.height.0);
    } else {
        path.rounded_rect_varying(r.x.0, r.y.0, r.width.0, r.height.0, tl, tr, br, bl);
    }
    path
}

fn build_path_ops(ops: &[PathOp]) -> Path {
    let mut path = Path::new();
    for op in ops {
        match *op {
            PathOp::MoveTo(x, y) => path.move_to(x, y),
            PathOp::LineTo(x, y) => path.line_to(x, y),
            PathOp::QuadTo(cx, cy, x, y) => path.quad_to(cx, cy, x, y),
            PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => path.bezier_to(c1x, c1y, c2x, c2y, x, y),
            PathOp::Close => path.close(),
        }
    }
    path
}
