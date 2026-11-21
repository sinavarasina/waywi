//! GPU texture and recorded canvas caches backed by `femtovg::ImageId`s.

use std::collections::HashMap;
#[cfg(feature = "text")]
use std::hash::{Hash, Hasher};

use femtovg::renderer::WGPURenderer;
use femtovg::rgb::RGBA8;
use femtovg::{Canvas, ImageFlags, ImageId, ImageSource};

use crate::asset::{AssetCache, AssetId};
use crate::geometry::LogicalSize;
#[cfg(feature = "text")]
use crate::geometry::{LogicalPx, ScaleFactor};
use crate::image::ImageFilter;
use crate::input::NodeId;
use crate::render::CanvasDrawHandle;
use crate::render::canvas::{CanvasOp, Painter};
#[cfg(feature = "text")]
use crate::render::style::Color;
#[cfg(feature = "text")]
use crate::text::engine::{TextDrawParams, TextEngine, TextRasterTarget};

/// Maximum cumulative RGBA GPU upload budget tracked across cached assets, sprites, and text.
const MAX_IMAGE_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ASSET_IMAGES: usize = 256;
const MAX_SPRITE_IMAGES: usize = 256;
#[cfg(feature = "text")]
const MAX_TEXT_IMAGES: usize = 512;
const STALE_IMAGE_AGE_FRAMES: u64 = 240;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CachedImageMeta<I = ImageId> {
    pub(crate) id: I,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) last_used_frame: u64,
}

impl<I> CachedImageMeta<I> {
    fn byte_size(&self) -> usize {
        (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4)
    }
}

pub(crate) type CachedGpuImage = CachedImageMeta<ImageId>;

pub(crate) struct CachedCanvasOps {
    pub(crate) render_key: u64,
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
    pub(crate) ops: Vec<CanvasOp>,
}

#[derive(Clone, Copy)]
enum ImageBucket {
    Asset,
    Sprite,
    #[cfg(feature = "text")]
    Text,
}

type AssetCacheKey = (AssetId, u64, ImageFilter);
type SpriteCacheKey = (AssetId, u64, u32, u32, u32, u32, ImageFilter);

struct GpuImageStore<I: Copy> {
    assets: HashMap<AssetCacheKey, CachedImageMeta<I>>,
    sprites: HashMap<SpriteCacheKey, CachedImageMeta<I>>,
    #[cfg(feature = "text")]
    texts: HashMap<u64, CachedImageMeta<I>>,
    total_bytes: usize,
    max_bytes: usize,
}

impl<I: Copy> Default for GpuImageStore<I> {
    fn default() -> Self {
        Self::with_max_bytes(MAX_IMAGE_CACHE_BYTES)
    }
}

impl<I: Copy> GpuImageStore<I> {
    fn with_max_bytes(max_bytes: usize) -> Self {
        Self {
            assets: HashMap::new(),
            sprites: HashMap::new(),
            #[cfg(feature = "text")]
            texts: HashMap::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    fn prune_map_stale<K: Eq + std::hash::Hash + Copy>(
        map: &mut HashMap<K, CachedImageMeta<I>>,
        total_bytes: &mut usize,
        frame_seq: u64,
        evicted: &mut Vec<I>,
    ) {
        let stale: Vec<_> = map
            .iter()
            .filter(|(_, v)| frame_seq.saturating_sub(v.last_used_frame) >= STALE_IMAGE_AGE_FRAMES)
            .map(|(k, _)| *k)
            .collect();
        for k in stale {
            if let Some(old) = map.remove(&k) {
                *total_bytes = total_bytes.saturating_sub(old.byte_size());
                evicted.push(old.id);
            }
        }
    }

    fn oldest_key_in<K: Eq + std::hash::Hash + Copy>(
        map: &HashMap<K, CachedImageMeta<I>>,
    ) -> Option<(K, u64)> {
        map.iter()
            .min_by_key(|(_, v)| v.last_used_frame)
            .map(|(k, v)| (*k, v.last_used_frame))
    }

    fn evict_map_for_cap<K: Eq + std::hash::Hash + Copy>(
        map: &mut HashMap<K, CachedImageMeta<I>>,
        total_bytes: &mut usize,
        cap: usize,
        frame_seq: u64,
        evicted: &mut Vec<I>,
    ) {
        if map.len() < cap {
            return;
        }
        Self::prune_map_stale(map, total_bytes, frame_seq, evicted);
        while map.len() >= cap {
            let Some((oldest_key, _)) = Self::oldest_key_in(map) else {
                break;
            };
            if let Some(old) = map.remove(&oldest_key) {
                *total_bytes = total_bytes.saturating_sub(old.byte_size());
                evicted.push(old.id);
            }
        }
    }

    fn evict_oldest_global(&mut self) -> Option<I> {
        let mut best: Option<(ImageBucket, u64)> = None;
        if let Some((_, f)) = Self::oldest_key_in(&self.assets) {
            best = Some((ImageBucket::Asset, f));
        }
        if let Some((_, f)) = Self::oldest_key_in(&self.sprites)
            && best.is_none_or(|(_, bf)| f < bf)
        {
            best = Some((ImageBucket::Sprite, f));
        }
        #[cfg(feature = "text")]
        if let Some((_, f)) = Self::oldest_key_in(&self.texts)
            && best.is_none_or(|(_, bf)| f < bf)
        {
            best = Some((ImageBucket::Text, f));
        }
        match best? {
            (ImageBucket::Asset, _) => {
                let (k, _) = Self::oldest_key_in(&self.assets)?;
                let old = self.assets.remove(&k)?;
                self.total_bytes = self.total_bytes.saturating_sub(old.byte_size());
                Some(old.id)
            }
            (ImageBucket::Sprite, _) => {
                let (k, _) = Self::oldest_key_in(&self.sprites)?;
                let old = self.sprites.remove(&k)?;
                self.total_bytes = self.total_bytes.saturating_sub(old.byte_size());
                Some(old.id)
            }
            #[cfg(feature = "text")]
            (ImageBucket::Text, _) => {
                let (k, _) = Self::oldest_key_in(&self.texts)?;
                let old = self.texts.remove(&k)?;
                self.total_bytes = self.total_bytes.saturating_sub(old.byte_size());
                Some(old.id)
            }
        }
    }

    fn prepare_budget(&mut self, incoming_bytes: usize, frame_seq: u64, evicted: &mut Vec<I>) {
        if self.total_bytes.saturating_add(incoming_bytes) <= self.max_bytes {
            return;
        }
        Self::prune_map_stale(&mut self.assets, &mut self.total_bytes, frame_seq, evicted);
        Self::prune_map_stale(&mut self.sprites, &mut self.total_bytes, frame_seq, evicted);
        #[cfg(feature = "text")]
        Self::prune_map_stale(&mut self.texts, &mut self.total_bytes, frame_seq, evicted);
        while self.total_bytes.saturating_add(incoming_bytes) > self.max_bytes {
            let Some(id) = self.evict_oldest_global() else {
                break;
            };
            evicted.push(id);
        }
    }

    fn insert_asset(
        &mut self,
        key: AssetCacheKey,
        entry: CachedImageMeta<I>,
        frame_seq: u64,
    ) -> Vec<I> {
        let mut evicted = Vec::new();
        Self::evict_map_for_cap(
            &mut self.assets,
            &mut self.total_bytes,
            MAX_ASSET_IMAGES,
            frame_seq,
            &mut evicted,
        );
        self.prepare_budget(entry.byte_size(), frame_seq, &mut evicted);
        self.total_bytes = self.total_bytes.saturating_add(entry.byte_size());
        if let Some(prev) = self.assets.insert(key, entry) {
            self.total_bytes = self.total_bytes.saturating_sub(prev.byte_size());
            evicted.push(prev.id);
        }
        evicted
    }

    fn insert_sprite(
        &mut self,
        key: SpriteCacheKey,
        entry: CachedImageMeta<I>,
        frame_seq: u64,
    ) -> Vec<I> {
        let mut evicted = Vec::new();
        Self::evict_map_for_cap(
            &mut self.sprites,
            &mut self.total_bytes,
            MAX_SPRITE_IMAGES,
            frame_seq,
            &mut evicted,
        );
        self.prepare_budget(entry.byte_size(), frame_seq, &mut evicted);
        self.total_bytes = self.total_bytes.saturating_add(entry.byte_size());
        if let Some(prev) = self.sprites.insert(key, entry) {
            self.total_bytes = self.total_bytes.saturating_sub(prev.byte_size());
            evicted.push(prev.id);
        }
        evicted
    }

    #[cfg(feature = "text")]
    fn insert_text(&mut self, key: u64, entry: CachedImageMeta<I>, frame_seq: u64) -> Vec<I> {
        let mut evicted = Vec::new();
        Self::evict_map_for_cap(
            &mut self.texts,
            &mut self.total_bytes,
            MAX_TEXT_IMAGES,
            frame_seq,
            &mut evicted,
        );
        self.prepare_budget(entry.byte_size(), frame_seq, &mut evicted);
        self.total_bytes = self.total_bytes.saturating_add(entry.byte_size());
        if let Some(prev) = self.texts.insert(key, entry) {
            self.total_bytes = self.total_bytes.saturating_sub(prev.byte_size());
            evicted.push(prev.id);
        }
        evicted
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SpriteCellSpec {
    pub(crate) asset_id: AssetId,
    pub(crate) asset_rev: u64,
    pub(crate) src_rect: (u32, u32, u32, u32),
    pub(crate) filter: ImageFilter,
}

fn gpu_cache_filter_and_flags(filter: ImageFilter) -> (ImageFilter, ImageFlags) {
    match filter {
        ImageFilter::Nearest => (
            ImageFilter::Nearest,
            ImageFlags::PREMULTIPLIED | ImageFlags::NEAREST,
        ),
        // GPU hardware sampling maps both Bilinear and Bicubic to linear filtering + mipmaps,
        // normalizing the cache key so identical textures are not uploaded twice.
        ImageFilter::Bilinear | ImageFilter::Bicubic => (
            ImageFilter::Bilinear,
            ImageFlags::PREMULTIPLIED | ImageFlags::GENERATE_MIPMAPS,
        ),
    }
}

#[derive(Default)]
pub(crate) struct GpuTextureCache {
    images: GpuImageStore<ImageId>,
    canvases: HashMap<NodeId, CachedCanvasOps>,
}

impl GpuTextureCache {
    pub(crate) fn drop_nodes(&mut self, node_ids: &[NodeId]) {
        for id in node_ids {
            self.canvases.remove(id);
        }
    }

    pub(crate) fn resolve_asset_image(
        &mut self,
        canvas: &mut Canvas<WGPURenderer>,
        assets: &AssetCache,
        asset: &crate::asset::ImageAsset,
        asset_rev: u64,
        filter: ImageFilter,
        frame_seq: u64,
    ) -> Option<CachedGpuImage> {
        let (cache_filter, flags) = gpu_cache_filter_and_flags(filter);
        let key = (asset.id, asset_rev, cache_filter);
        if let Some(entry) = self.images.assets.get_mut(&key) {
            entry.last_used_frame = frame_seq;
            return Some(*entry);
        }

        let decoded = assets.get(asset.id)?;
        let (w, h, pixels) = (decoded.width, decoded.height, decoded.pixels.as_ref());
        if w == 0 || h == 0 {
            return None;
        }
        let id = upload_premultiplied_rgba_with_flags(canvas, pixels, w, h, flags)?;
        let entry = CachedGpuImage {
            id,
            width: w,
            height: h,
            last_used_frame: frame_seq,
        };
        for old_id in self.images.insert_asset(key, entry, frame_seq) {
            canvas.delete_image(old_id);
        }
        Some(entry)
    }

    pub(crate) fn resolve_sprite_cell(
        &mut self,
        canvas: &mut Canvas<WGPURenderer>,
        assets: &AssetCache,
        spec: SpriteCellSpec,
        frame_seq: u64,
    ) -> Option<CachedGpuImage> {
        let (sx, sy, sw, sh) = spec.src_rect;
        let (cache_filter, flags) = gpu_cache_filter_and_flags(spec.filter);
        let key = (spec.asset_id, spec.asset_rev, sx, sy, sw, sh, cache_filter);
        if let Some(entry) = self.images.sprites.get_mut(&key) {
            entry.last_used_frame = frame_seq;
            return Some(*entry);
        }

        let decoded = assets.get(spec.asset_id)?;
        let full_w = decoded.width;
        let full_h = decoded.height;
        let cw = sw.min(full_w.saturating_sub(sx));
        let ch = sh.min(full_h.saturating_sub(sy));
        if cw == 0 || ch == 0 {
            return None;
        }

        let id = if sx == 0 && sy == 0 && cw == full_w && ch == full_h {
            upload_premultiplied_rgba_with_flags(canvas, &decoded.pixels, cw, ch, flags)?
        } else {
            let mut sub = Vec::with_capacity((cw as usize) * (ch as usize) * 4);
            let src = decoded.pixels.as_ref();
            for row in 0..ch {
                let s_off = (((sy + row) * full_w + sx) as usize) * 4;
                let len = (cw as usize) * 4;
                if s_off + len <= src.len() {
                    sub.extend_from_slice(&src[s_off..s_off + len]);
                } else {
                    sub.resize(sub.len() + len, 0);
                }
            }
            upload_premultiplied_rgba_with_flags(canvas, &sub, cw, ch, flags)?
        };

        let entry = CachedGpuImage {
            id,
            width: cw,
            height: ch,
            last_used_frame: frame_seq,
        };
        for old_id in self.images.insert_sprite(key, entry, frame_seq) {
            canvas.delete_image(old_id);
        }
        Some(entry)
    }

    #[cfg(feature = "text")]
    #[must_use]
    pub(crate) fn text_quad_padding_px(font_size: LogicalPx, scale: ScaleFactor) -> u32 {
        ((font_size.0 * scale.as_f32()) * 0.45)
            .ceil()
            .clamp(4.0, 32.0) as u32
    }

    #[cfg(feature = "text")]
    pub(crate) fn resolve_text_quad(
        &mut self,
        canvas: &mut Canvas<WGPURenderer>,
        text_engine: &mut TextEngine,
        params: TextDrawParams<'_>,
        color: Color,
        box_px: (u32, u32),
        frame_seq: u64,
    ) -> Option<CachedGpuImage> {
        let (box_w_px, box_h_px) = box_px;
        if params.content.is_empty() || box_w_px == 0 || box_h_px == 0 {
            return None;
        }
        let s = params.style;
        let pad_px = Self::text_quad_padding_px(s.font_size, params.scale);
        let tex_w = box_w_px.saturating_add(pad_px.saturating_mul(2));
        let tex_h = box_h_px.saturating_add(pad_px.saturating_mul(2));

        let mut h = std::collections::hash_map::DefaultHasher::new();
        params.content.as_str().hash(&mut h);
        ((s.font_size.0 * 100.0).round() as u32).hash(&mut h);
        ((s.line_height_multiplier * 100.0).round() as u32).hash(&mut h);
        s.max_width
            .map(|w| (w.0 * 100.0).round() as u32)
            .hash(&mut h);
        params.wrap_to_width.hash(&mut h);
        color.r.hash(&mut h);
        color.g.hash(&mut h);
        color.b.hash(&mut h);
        color.a.hash(&mut h);
        s.font.hash(&mut h);
        s.align.hash(&mut h);
        params.scale.as_120ths().hash(&mut h);
        tex_w.hash(&mut h);
        tex_h.hash(&mut h);
        pad_px.hash(&mut h);
        let key = h.finish();

        if let Some(entry) = self.images.texts.get_mut(&key) {
            entry.last_used_frame = frame_seq;
            return Some(*entry);
        }

        let mut rgba = vec![0u8; (tex_w as usize) * (tex_h as usize) * 4];
        let mut target = TextRasterTarget {
            dst: &mut rgba,
            width: tex_w,
            height: tex_h,
            clip: crate::geometry::BufferRect {
                x: 0,
                y: 0,
                width: tex_w as i32,
                height: tex_h as i32,
            },
        };
        let pad_logical = pad_px as f32 / params.scale.as_f32();
        text_engine.draw_text_premultiplied_rgba(
            &mut target,
            (pad_logical, pad_logical),
            params,
            color,
            1.0,
            frame_seq,
        );
        let id = upload_premultiplied_rgba_with_flags(
            canvas,
            &rgba,
            tex_w,
            tex_h,
            ImageFlags::PREMULTIPLIED | ImageFlags::NEAREST,
        )?;
        let entry = CachedGpuImage {
            id,
            width: tex_w,
            height: tex_h,
            last_used_frame: frame_seq,
        };
        for old_id in self.images.insert_text(key, entry, frame_seq) {
            canvas.delete_image(old_id);
        }
        Some(entry)
    }

    pub(crate) fn resolve_canvas_ops(
        &mut self,
        node_id: NodeId,
        render_key: u64,
        logical_size: LogicalSize,
        scale_f: f32,
        draw: &CanvasDrawHandle,
    ) -> &[CanvasOp] {
        let px_w = (logical_size.width.0 * scale_f).ceil().max(1.0) as u32;
        let px_h = (logical_size.height.0 * scale_f).ceil().max(1.0) as u32;
        let hit = self.canvases.get(&node_id).is_some_and(|c| {
            c.render_key == render_key && c.width_px == px_w && c.height_px == px_h
        });
        if !hit {
            let mut painter = Painter::new_recorded(logical_size, scale_f);
            let painter_ptr = (&mut painter as *mut Painter<'_>).cast::<Painter<'static>>();
            draw.func.call(painter_ptr);
            let ops = painter.take_recorded_ops();
            self.canvases.insert(
                node_id,
                CachedCanvasOps {
                    render_key,
                    width_px: px_w,
                    height_px: px_h,
                    ops,
                },
            );
        }
        &self.canvases[&node_id].ops
    }
}

fn upload_premultiplied_rgba_with_flags(
    canvas: &mut Canvas<WGPURenderer>,
    rgba_bytes: &[u8],
    w: u32,
    h: u32,
    flags: ImageFlags,
) -> Option<ImageId> {
    let mut pixels = Vec::with_capacity((w as usize) * (h as usize));
    for &[r, g, b, a] in rgba_bytes.as_chunks::<4>().0 {
        pixels.push(RGBA8::new(r, g, b, a));
    }
    let img = femtovg::imgref::Img::new(pixels.as_slice(), w as usize, h as usize);
    canvas.create_image(ImageSource::Rgba(img), flags).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_image_store_enforces_global_byte_budget_lru() {
        // 16x16 RGBA = 1024 bytes each; cap at 2048 bytes (2 images).
        let mut store = GpuImageStore::<u32>::with_max_bytes(2048);
        let mk = |id, frame_seq| CachedImageMeta {
            id,
            width: 16,
            height: 16,
            last_used_frame: frame_seq,
        };

        assert!(
            store
                .insert_asset((AssetId::new(1), 1, ImageFilter::Bilinear), mk(1, 1), 1)
                .is_empty()
        );
        assert!(
            store
                .insert_sprite(
                    (AssetId::new(2), 1, 0, 0, 16, 16, ImageFilter::Bilinear),
                    mk(2, 2),
                    2,
                )
                .is_empty()
        );
        assert_eq!(store.total_bytes, 2048);

        // Touch asset 1 at frame 5 so sprite 2 (frame 2) becomes globally oldest.
        if let Some(a) = store
            .assets
            .get_mut(&(AssetId::new(1), 1, ImageFilter::Bilinear))
        {
            a.last_used_frame = 5;
        }

        let evicted = store.insert_asset((AssetId::new(3), 1, ImageFilter::Bilinear), mk(3, 6), 6);
        assert_eq!(evicted, vec![2]);
        assert_eq!(store.total_bytes, 2048);
        assert!(
            store
                .assets
                .contains_key(&(AssetId::new(1), 1, ImageFilter::Bilinear))
        );
        assert!(
            store
                .assets
                .contains_key(&(AssetId::new(3), 1, ImageFilter::Bilinear))
        );
        assert!(store.sprites.is_empty());
    }

    #[test]
    fn gpu_image_store_prunes_stale_entries_before_recent_ones() {
        let mut store = GpuImageStore::<u32>::with_max_bytes(2048);
        let mk = |id, frame_seq| CachedImageMeta {
            id,
            width: 16,
            height: 16,
            last_used_frame: frame_seq,
        };

        assert!(
            store
                .insert_asset((AssetId::new(1), 1, ImageFilter::Bilinear), mk(10, 1), 1)
                .is_empty()
        );
        assert!(
            store
                .insert_sprite(
                    (AssetId::new(2), 1, 0, 0, 16, 16, ImageFilter::Bilinear),
                    mk(20, 250),
                    250,
                )
                .is_empty()
        );

        let evicted = store.insert_asset(
            (AssetId::new(3), 1, ImageFilter::Bilinear),
            mk(30, 260),
            260,
        );
        assert_eq!(evicted, vec![10]);
        assert_eq!(store.total_bytes, 2048);
    }

    #[cfg(feature = "text")]
    #[test]
    fn text_quad_padding_gives_margin_for_glyph_overhang() {
        assert_eq!(
            GpuTextureCache::text_quad_padding_px(LogicalPx(12.0), ScaleFactor::ONE),
            6
        );
        assert_eq!(
            GpuTextureCache::text_quad_padding_px(LogicalPx(6.0), ScaleFactor::ONE),
            4
        );
        assert_eq!(
            GpuTextureCache::text_quad_padding_px(LogicalPx(64.0), ScaleFactor::from_integer(2)),
            32
        );
    }
}
