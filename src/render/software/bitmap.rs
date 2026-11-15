//! Premultiplied bitmap/sprite/canvas blitting for [`SoftwareRenderer`].

use crate::image::ImageFit;
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
) {
    let (_, _, sw, sh) = src.src_rect;
    if sw == 0 || sh == 0 || src.full_w == 0 || src.full_h == 0 {
        return;
    }
    let dst_x = (cmd.local_rect.x.0 + cmd.affine.tx) * scale_f - tile.origin_x as f32;
    let dst_y = (cmd.local_rect.y.0 + cmd.affine.ty) * scale_f - tile.origin_y as f32;
    let dst_w = (cmd.local_rect.width.0 * scale_f).max(0.0);
    let dst_h = (cmd.local_rect.height.0 * scale_f).max(0.0);

    let (draw_x, draw_y, draw_w, draw_h) = fit.resolve_rect(
        dst_x,
        dst_y,
        dst_w,
        dst_h,
        sw as f32 * scale_f,
        sh as f32 * scale_f,
    );

    // Fast path: axis-aligned 1:1 pixel blit with no scaling or rotation.
    if cmd.affine.is_axis_aligned_unscaled()
        && (draw_w - sw as f32).abs() < 0.5
        && (draw_h - sh as f32).abs() < 0.5
    {
        fast_blit_premultiplied_1to1(
            &mut tile.pixels,
            draw_x.round() as i32,
            draw_y.round() as i32,
            src,
            cmd.opacity,
        );
    } else {
        #[cfg(feature = "renderer-software")]
        if let Some(mut pixmap) =
            tiny_skia::PixmapMut::from_bytes(tile.pixels.dst, tile.pixels.width, tile.pixels.height)
        {
            let (sx, sy, _, _) = src.src_rect;
            let mut cell_buf;
            let (cell_pixels, cw, ch): (&[u8], u32, u32) =
                if sx == 0 && sy == 0 && sw == src.full_w && sh == src.full_h {
                    (src.pixels, src.full_w, src.full_h)
                } else {
                    let cw = sw.min(src.full_w.saturating_sub(sx));
                    let ch = sh.min(src.full_h.saturating_sub(sy));
                    cell_buf = vec![0u8; (cw as usize) * (ch as usize) * 4];
                    for row in 0..ch {
                        let s_off = (((sy + row) * src.full_w + sx) as usize) * 4;
                        let d_off = ((row * cw) as usize) * 4;
                        let len = (cw as usize) * 4;
                        if s_off + len <= src.pixels.len() {
                            cell_buf[d_off..d_off + len]
                                .copy_from_slice(&src.pixels[s_off..s_off + len]);
                        }
                    }
                    (&cell_buf, cw, ch)
                };

            if let Some(src_ref) = tiny_skia::PixmapRef::from_bytes(cell_pixels, cw, ch) {
                let sx_scale = draw_w / (cw.max(1) as f32);
                let sy_scale = draw_h / (ch.max(1) as f32);
                let transform = tiny_skia::Transform::from_row(
                    cmd.affine.a * sx_scale,
                    cmd.affine.b * sx_scale,
                    cmd.affine.c * sy_scale,
                    cmd.affine.d * sy_scale,
                    draw_x,
                    draw_y,
                );
                let paint = tiny_skia::PixmapPaint {
                    opacity: cmd.opacity,
                    quality: tiny_skia::FilterQuality::Bilinear,
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                };
                let mask = build_clip_mask(tile.pixels.width, tile.pixels.height, tile.pixels.clip);
                pixmap.draw_pixmap(0, 0, src_ref, &paint, transform, mask.as_ref());
            }
        }
    }
}
