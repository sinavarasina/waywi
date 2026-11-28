//! Tile-level command rasterizer for [`SoftwareRenderer`].

use crate::asset::AssetCache;
use crate::geometry::{LogicalSize, ScaleFactor};
use crate::image::{ImageFilter, ImageFit};
use crate::render::canvas::Painter;
use crate::render::retained::{DrawCommand, PaintPayload};
use crate::render::software::bitmap::draw_bitmap_region;
use crate::render::software::fast_path::{BlitSource, PixelTarget, fast_fill_rect_rgba};
#[cfg(feature = "renderer-software")]
use crate::render::software::path::{
    build_clip_mask, build_rounded_rect_path, build_skia_fill_paint, draw_skia_box_shadow,
};
use crate::render::software::{CachedCanvasPixmap, SoftwareRenderer};
use crate::render::style::Fill;

#[cfg(feature = "text")]
use crate::text::TextEngine;

pub(super) struct TileTarget<'a> {
    pub(super) pixels: PixelTarget<'a>,
    pub(super) origin_x: i32,
    pub(super) origin_y: i32,
}

impl SoftwareRenderer {
    pub(super) fn render_command_into_tile(
        &mut self,
        tile: &mut TileTarget<'_>,
        #[cfg(feature = "text")] text_engine: &mut TextEngine,
        scale: ScaleFactor,
        cmd: &DrawCommand,
        assets: &AssetCache,
        frame_seq: u64,
    ) {
        let scale_f = scale.as_f32();
        let axis_aligned = cmd.affine.is_axis_aligned_unscaled();

        match &cmd.payload {
            PaintPayload::Rect {
                fill,
                corner_radius,
                border,
                shadow,
            } => {
                if let Fill::Solid(solid) = fill
                    && axis_aligned
                    && corner_radius.is_zero()
                    && border.is_none()
                    && shadow.is_none()
                {
                    let eff_fill = solid.with_opacity(cmd.opacity);
                    let x = (cmd.local_rect.x.0 + cmd.affine.tx) * scale_f - tile.origin_x as f32;
                    let y = (cmd.local_rect.y.0 + cmd.affine.ty) * scale_f - tile.origin_y as f32;
                    let w = cmd.local_rect.width.0 * scale_f;
                    let h = cmd.local_rect.height.0 * scale_f;
                    fast_fill_rect_rgba(&mut tile.pixels, x, y, w, h, eff_fill);
                } else {
                    #[cfg(feature = "renderer-software")]
                    if let Some(mut pixmap) = tiny_skia::PixmapMut::from_bytes(
                        tile.pixels.dst,
                        tile.pixels.width,
                        tile.pixels.height,
                    ) {
                        let skia_transform = tiny_skia::Transform::from_row(
                            cmd.affine.a,
                            cmd.affine.b,
                            cmd.affine.c,
                            cmd.affine.d,
                            cmd.affine.tx * scale_f - tile.origin_x as f32,
                            cmd.affine.ty * scale_f - tile.origin_y as f32,
                        );
                        let mask = build_clip_mask(
                            tile.pixels.width,
                            tile.pixels.height,
                            tile.pixels.clip,
                        );
                        let mask_ref = mask.as_ref();

                        if let Some(sh) = shadow
                            && sh.is_visible()
                        {
                            draw_skia_box_shadow(
                                &mut pixmap,
                                cmd.local_rect,
                                *corner_radius,
                                *sh,
                                scale_f,
                                cmd.opacity,
                                skia_transform,
                                mask_ref,
                            );
                        }

                        let stroke_border = border
                            .filter(|b| b.width.0 > 0.0)
                            .map(|b| (b.width.0, b.color.with_opacity(cmd.opacity)))
                            .filter(|(_, c)| c.a > 0);
                        let fill_paint = if fill.is_visible() {
                            build_skia_fill_paint(fill, cmd.local_rect, scale_f, cmd.opacity)
                        } else {
                            None
                        };
                        if (fill_paint.is_some() || stroke_border.is_some())
                            && let Some(path) =
                                build_rounded_rect_path(cmd.local_rect, *corner_radius, scale_f)
                        {
                            if let Some(paint) = fill_paint {
                                pixmap.fill_path(
                                    &path,
                                    &paint,
                                    tiny_skia::FillRule::Winding,
                                    skia_transform,
                                    mask_ref,
                                );
                            }
                            if let Some((bw, eff_b)) = stroke_border {
                                let mut paint = tiny_skia::Paint::default();
                                paint.set_color_rgba8(eff_b.r, eff_b.g, eff_b.b, eff_b.a);
                                paint.anti_alias = true;
                                let stroke = tiny_skia::Stroke {
                                    width: bw * scale_f,
                                    ..Default::default()
                                };
                                pixmap.stroke_path(
                                    &path,
                                    &paint,
                                    &stroke,
                                    skia_transform,
                                    mask_ref,
                                );
                            }
                        }
                    }
                }
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
                    let ox = cmd.local_rect.x.0 + cmd.affine.tx - (tile.origin_x as f32 / scale_f);
                    let oy = cmd.local_rect.y.0 + cmd.affine.ty - (tile.origin_y as f32 / scale_f);
                    let max_width = (cmd.local_rect.width.0 > 0.0).then_some(cmd.local_rect.width);
                    let mut text_target = crate::text::engine::TextRasterTarget {
                        dst: tile.pixels.dst,
                        width: tile.pixels.width,
                        height: tile.pixels.height,
                        clip: tile.pixels.clip,
                    };
                    let params = crate::text::engine::TextDrawParams {
                        content,
                        style: crate::text::TextMeasureStyle {
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
                    text_engine.draw_text_premultiplied_rgba(
                        &mut text_target,
                        (ox, oy),
                        params,
                        *color,
                        cmd.opacity,
                        frame_seq,
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
                        frame_seq,
                    );
                }
            }
            PaintPayload::Image {
                asset,
                fit,
                filter,
                source_rect,
                ..
            } => {
                let Some(decoded) = assets.get(asset.id) else {
                    return;
                };
                let src_rect = source_rect.map_or(
                    (0, 0, decoded.width, decoded.height),
                    crate::image::SourceRect::as_tuple,
                );
                draw_bitmap_region(
                    tile,
                    scale_f,
                    cmd,
                    &BlitSource {
                        pixels: &decoded.pixels,
                        full_w: decoded.width,
                        full_h: decoded.height,
                        src_rect,
                    },
                    *fit,
                    *filter,
                    &mut self.scratch_sprite_cell,
                );
            }
            PaintPayload::Canvas { cache_key, draw } => {
                let px_w = cmd.local_rect.width.to_buffer_ceil(scale).as_u32().max(1);
                let px_h = cmd.local_rect.height.to_buffer_ceil(scale).as_u32().max(1);
                let needed = (px_w as usize) * (px_h as usize) * 4;

                let Some(cache_key) = *cache_key else {
                    let mut scratch = std::mem::take(&mut self.scratch_canvas);
                    scratch.resize(needed, 0);
                    scratch[..needed].fill(0);
                    {
                        let mut painter = Painter::new_pixels(
                            LogicalSize::new(cmd.local_rect.width, cmd.local_rect.height),
                            scale_f,
                            &mut scratch[..needed],
                            px_w,
                            px_h,
                        );
                        let ptr: *mut Painter<'_> = &mut painter;
                        // SAFETY: `painter` lives exclusively on this stack frame for `draw.func`.
                        draw.func.call(ptr.cast::<Painter<'static>>());
                    }
                    draw_bitmap_region(
                        tile,
                        scale_f,
                        cmd,
                        &BlitSource {
                            pixels: &scratch[..needed],
                            full_w: px_w,
                            full_h: px_h,
                            src_rect: (0, 0, px_w, px_h),
                        },
                        ImageFit::Fill,
                        ImageFilter::Bilinear,
                        &mut self.scratch_sprite_cell,
                    );
                    self.scratch_canvas = scratch;
                    return;
                };

                let cache_hit = self.canvas_cache.get(&cmd.node_id).is_some_and(|c| {
                    c.cache_key == cache_key && c.width_px == px_w && c.height_px == px_h
                });
                if !cache_hit {
                    if let Some(prev) = self.canvas_cache.remove(&cmd.node_id) {
                        self.canvas_bytes =
                            self.canvas_bytes.saturating_sub(prev.rgba_pixels.len());
                    }
                    while self.canvas_bytes.saturating_add(needed) > self.canvas_max_bytes {
                        let Some(oldest) = self
                            .canvas_cache
                            .iter()
                            .min_by_key(|(_, v)| v.last_used_frame)
                            .map(|(&k, _)| k)
                        else {
                            break;
                        };
                        if let Some(old) = self.canvas_cache.remove(&oldest) {
                            self.canvas_bytes =
                                self.canvas_bytes.saturating_sub(old.rgba_pixels.len());
                        }
                    }

                    let mut buf = vec![0u8; needed];
                    let mut painter = Painter::new_pixels(
                        LogicalSize::new(cmd.local_rect.width, cmd.local_rect.height),
                        scale_f,
                        &mut buf,
                        px_w,
                        px_h,
                    );
                    let ptr: *mut Painter<'_> = &mut painter;
                    // SAFETY: Cast lifetime to `'static` strictly for the synchronous invocation of
                    // `draw.func` while `painter` lives on this stack frame.
                    let ptr_static: *mut Painter<'static> = ptr.cast();
                    draw.func.call(ptr_static);

                    self.canvas_bytes = self.canvas_bytes.saturating_add(buf.len());
                    self.canvas_cache.insert(
                        cmd.node_id,
                        CachedCanvasPixmap {
                            cache_key,
                            width_px: px_w,
                            height_px: px_h,
                            last_used_frame: frame_seq,
                            rgba_pixels: buf,
                        },
                    );
                }

                if let Some(cached) = self.canvas_cache.get_mut(&cmd.node_id) {
                    cached.last_used_frame = frame_seq;
                    draw_bitmap_region(
                        tile,
                        scale_f,
                        cmd,
                        &BlitSource {
                            pixels: &cached.rgba_pixels,
                            full_w: px_w,
                            full_h: px_h,
                            src_rect: (0, 0, px_w, px_h),
                        },
                        ImageFit::Fill,
                        ImageFilter::Bilinear,
                        &mut self.scratch_sprite_cell,
                    );
                }
            }
            PaintPayload::ClipPush { .. } | PaintPayload::ClipPop => {}
        }
    }
}
