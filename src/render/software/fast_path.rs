use crate::geometry::BufferRect;
use crate::render::style::Color;

/// Mutable RGBA8888 pixel buffer slice paired with bounds and clip rect.
pub(crate) struct PixelTarget<'a> {
    pub(crate) dst: &'a mut [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) clip: BufferRect,
}

/// Borrowed premultiplied RGBA8888 source sub-rectangle.
#[derive(Clone, Copy)]
pub(crate) struct BlitSource<'a> {
    pub(crate) pixels: &'a [u8],
    pub(crate) full_w: u32,
    pub(crate) full_h: u32,
    pub(crate) src_rect: (u32, u32, u32, u32),
}

/// Direct axis-aligned solid rectangle fill clipped to `target.clip`.
pub(crate) fn fast_fill_rect_rgba(
    target: &mut PixelTarget<'_>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Color,
) {
    if color.a == 0 || w <= 0.0 || h <= 0.0 {
        return;
    }
    let clip = target.clip;
    let dst_w = target.width;
    let dst_h = target.height;
    let x0 = (x.round() as i32).max(clip.x).max(0);
    let y0 = (y.round() as i32).max(clip.y).max(0);
    let x1 = ((x + w).round() as i32)
        .min(clip.x + clip.width)
        .min(dst_w as i32);
    let y1 = ((y + h).round() as i32)
        .min(clip.y + clip.height)
        .min(dst_h as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let dst = &mut *target.dst;
    let pm = color.to_premultiplied_rgba();
    if pm[3] == 255 {
        let row_stride = (dst_w as usize) * 4;
        let x_start = (x0 as usize) * 4;
        let x_end = (x1 as usize) * 4;
        let first_off = (y0 as usize) * row_stride + x_start;
        let first_len = x_end - x_start;
        for px in dst[first_off..first_off + first_len].as_chunks_mut::<4>().0 {
            px.copy_from_slice(&pm);
        }
        for yy in (y0 + 1)..y1 {
            let row_off = (yy as usize) * row_stride + x_start;
            dst.copy_within(first_off..first_off + first_len, row_off);
        }
    } else {
        let sa = u32::from(pm[3]);
        let inv = 255 - sa;
        let sr = u32::from(pm[0]);
        let sg = u32::from(pm[1]);
        let sb = u32::from(pm[2]);
        for yy in y0..y1 {
            let row = (yy as usize) * (dst_w as usize) * 4;
            for xx in x0..x1 {
                let idx = row + (xx as usize) * 4;
                dst[idx] = (sr + (u32::from(dst[idx]) * inv + 127) / 255).min(255) as u8;
                dst[idx + 1] = (sg + (u32::from(dst[idx + 1]) * inv + 127) / 255).min(255) as u8;
                dst[idx + 2] = (sb + (u32::from(dst[idx + 2]) * inv + 127) / 255).min(255) as u8;
                dst[idx + 3] = (sa + (u32::from(dst[idx + 3]) * inv + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// Direct 1:1 premultiplied pixel blit clipped to `target.clip`.
pub(crate) fn fast_blit_premultiplied_1to1(
    target: &mut PixelTarget<'_>,
    dx: i32,
    dy: i32,
    source: &BlitSource<'_>,
    opacity: f32,
) {
    let (sx, sy, sw, sh) = source.src_rect;
    let src_w = source.full_w;
    let src_h = source.full_h;
    let src = source.pixels;
    let clip = target.clip;
    let dst_w = target.width;
    let dst_h = target.height;

    let valid_sw = sw.min(src_w.saturating_sub(sx)) as i32;
    let valid_sh = sh.min(src_h.saturating_sub(sy)) as i32;
    let x0 = dx.max(clip.x).max(0);
    let y0 = dy.max(clip.y).max(0);
    let x1 = (dx + valid_sw).min(clip.x + clip.width).min(dst_w as i32);
    let y1 = (dy + valid_sh).min(clip.y + clip.height).min(dst_h as i32);
    if x1 <= x0 || y1 <= y0 || opacity <= 0.0 {
        return;
    }

    let dst = &mut *target.dst;
    let op_255 = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    for yy in y0..y1 {
        let src_y = (sy as i32 + (yy - dy)) as usize;
        let dst_row = (yy as usize) * (dst_w as usize) * 4;
        let src_row = src_y * (src_w as usize) * 4;
        for xx in x0..x1 {
            let src_x = (sx as i32 + (xx - dx)) as usize;
            let s_idx = src_row + src_x * 4;
            let d_idx = dst_row + (xx as usize) * 4;

            let mut sr = u32::from(src[s_idx]);
            let mut sg = u32::from(src[s_idx + 1]);
            let mut sb = u32::from(src[s_idx + 2]);
            let mut sa = u32::from(src[s_idx + 3]);

            if op_255 < 255 {
                sr = (sr * op_255 + 127) / 255;
                sg = (sg * op_255 + 127) / 255;
                sb = (sb * op_255 + 127) / 255;
                sa = (sa * op_255 + 127) / 255;
            }
            if sa == 0 {
                continue;
            }
            if sa == 255 {
                dst[d_idx] = sr as u8;
                dst[d_idx + 1] = sg as u8;
                dst[d_idx + 2] = sb as u8;
                dst[d_idx + 3] = 255;
            } else {
                let inv = 255 - sa;
                dst[d_idx] = (sr + (u32::from(dst[d_idx]) * inv + 127) / 255).min(255) as u8;
                dst[d_idx + 1] =
                    (sg + (u32::from(dst[d_idx + 1]) * inv + 127) / 255).min(255) as u8;
                dst[d_idx + 2] =
                    (sb + (u32::from(dst[d_idx + 2]) * inv + 127) / 255).min(255) as u8;
                dst[d_idx + 3] =
                    (sa + (u32::from(dst[d_idx + 3]) * inv + 127) / 255).min(255) as u8;
            }
        }
    }
}
