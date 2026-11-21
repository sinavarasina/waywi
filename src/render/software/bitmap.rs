//! Premultiplied bitmap/sprite/canvas blitting for [`SoftwareRenderer`].

use crate::image::{ImageFilter, ImageFit};
use crate::render::retained::DrawCommand;
use crate::render::software::fast_path::{BlitSource, fast_blit_premultiplied_1to1};
#[cfg(feature = "renderer-software")]
use crate::render::software::path::build_clip_mask;
use crate::render::software::raster::TileTarget;

pub(super) fn draw_bitmap_region(
    tile: &mut TileTarget<'_>,
    scale_f: f32,
    cmd: &DrawCommand,
    src: &BlitSource<'_>,
    fit: ImageFit,
    filter: ImageFilter,
    scratch_cell: &mut Vec<u8>,
) {
    #[cfg(not(feature = "renderer-software"))]
    let _ = (filter, scratch_cell);

    let (_, _, sw, sh) = src.src_rect;
    if sw == 0 || sh == 0 || src.full_w == 0 || src.full_h == 0 {
        return;
    }

    // Resolve destination rect in element-local logical coordinates before applying `cmd.affine`
    // so arbitrary scaling, horizontal/vertical flips, and rotations map accurately around pivot.
    let (dx, dy, dw, dh) = fit.resolve_rect(
        cmd.local_rect.x.0,
        cmd.local_rect.y.0,
        cmd.local_rect.width.0.max(0.0),
        cmd.local_rect.height.0.max(0.0),
        sw as f32,
        sh as f32,
    );
    let draw_w_px = dw * scale_f;
    let draw_h_px = dh * scale_f;

    // Fast path: axis-aligned 1:1 pixel blit with no scaling, flipping, or rotation.
    if cmd.affine.is_axis_aligned_unscaled()
        && (draw_w_px - sw as f32).abs() < 0.5
        && (draw_h_px - sh as f32).abs() < 0.5
    {
        let draw_x_px = (dx + cmd.affine.tx) * scale_f - tile.origin_x as f32;
        let draw_y_px = (dy + cmd.affine.ty) * scale_f - tile.origin_y as f32;
        fast_blit_premultiplied_1to1(
            &mut tile.pixels,
            draw_x_px.round() as i32,
            draw_y_px.round() as i32,
            src,
            cmd.opacity,
        );
    } else {
        #[cfg(feature = "renderer-software")]
        if let Some(mut pixmap) =
            tiny_skia::PixmapMut::from_bytes(tile.pixels.dst, tile.pixels.width, tile.pixels.height)
        {
            let (sx, sy, _, _) = src.src_rect;
            let (cell_pixels, cw, ch): (&[u8], u32, u32) =
                if sx == 0 && sy == 0 && sw == src.full_w && sh == src.full_h {
                    (src.pixels, src.full_w, src.full_h)
                } else {
                    let cw = sw.min(src.full_w.saturating_sub(sx));
                    let ch = sh.min(src.full_h.saturating_sub(sy));
                    let needed = (cw as usize) * (ch as usize) * 4;
                    if scratch_cell.len() < needed {
                        scratch_cell.resize(needed, 0);
                    }
                    let cell_slice = &mut scratch_cell[..needed];
                    let row_bytes = (cw as usize) * 4;
                    for row in 0..ch {
                        let s_off = (((sy + row) * src.full_w + sx) as usize) * 4;
                        let d_off = (row as usize) * row_bytes;
                        if s_off + row_bytes <= src.pixels.len() {
                            cell_slice[d_off..d_off + row_bytes]
                                .copy_from_slice(&src.pixels[s_off..s_off + row_bytes]);
                        } else {
                            cell_slice[d_off..d_off + row_bytes].fill(0);
                        }
                    }
                    (&scratch_cell[..needed], cw, ch)
                };

            if cw > 0
                && ch > 0
                && let Some(src_ref) = tiny_skia::PixmapRef::from_bytes(cell_pixels, cw, ch)
            {
                let sx_px = draw_w_px / (cw as f32);
                let sy_px = draw_h_px / (ch as f32);
                let tx = (cmd.affine.a * dx + cmd.affine.c * dy + cmd.affine.tx) * scale_f
                    - tile.origin_x as f32;
                let ty = (cmd.affine.b * dx + cmd.affine.d * dy + cmd.affine.ty) * scale_f
                    - tile.origin_y as f32;
                let transform = tiny_skia::Transform::from_row(
                    cmd.affine.a * sx_px,
                    cmd.affine.b * sx_px,
                    cmd.affine.c * sy_px,
                    cmd.affine.d * sy_px,
                    tx,
                    ty,
                );
                let quality = match filter {
                    ImageFilter::Bilinear => tiny_skia::FilterQuality::Bilinear,
                    ImageFilter::Bicubic => tiny_skia::FilterQuality::Bicubic,
                    ImageFilter::Nearest => tiny_skia::FilterQuality::Nearest,
                };
                let paint = tiny_skia::PixmapPaint {
                    opacity: cmd.opacity,
                    quality,
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                };
                let mask = build_clip_mask(tile.pixels.width, tile.pixels.height, tile.pixels.clip);
                pixmap.draw_pixmap(0, 0, src_ref, &paint, transform, mask.as_ref());
            }
        }
    }
}
