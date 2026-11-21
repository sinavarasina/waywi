use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Result, WaywiError};
use crate::render::retained::{DrawCommand, PaintPayload};

use super::decode::{
    decode_image_bytes, decode_image_bytes_sized, detect_format, hash_bytes, premultiply_into,
    read_bounded_file, validate_dimensions,
};
use super::types::{
    AssetId, AssetRetention, AssetSource, CacheEntry, DecodedImage, ImageAsset, ImageFormat,
};

/// Shared decoded asset store owned by `App`.
pub(crate) struct AssetCache {
    next_id: u64,
    next_revision: u64,
    entries: HashMap<AssetId, CacheEntry>,
    path_index: HashMap<PathBuf, AssetId>,
    sized_path_index: HashMap<(PathBuf, u32), AssetId>,
    hash_index: HashMap<u64, AssetId>,
    evicted_count: usize,
    reclaimable_count: usize,
}

impl Default for AssetCache {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetCache {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            next_revision: 1,
            entries: HashMap::new(),
            path_index: HashMap::new(),
            sized_path_index: HashMap::new(),
            hash_index: HashMap::new(),
            evicted_count: 0,
            reclaimable_count: 0,
        }
    }

    pub(crate) fn load_file(&mut self, path: impl AsRef<Path>) -> Result<ImageAsset> {
        self.load_file_sized(path, 0)
    }

    pub(crate) fn load_file_sized(
        &mut self,
        path: impl AsRef<Path>,
        min_svg_px: u32,
    ) -> Result<ImageAsset> {
        let path_ref = path.as_ref();
        let canonical = std::fs::canonicalize(path_ref).unwrap_or_else(|_| path_ref.to_path_buf());

        if min_svg_px == 0 {
            if let Some(&existing_id) = self.path_index.get(&canonical)
                && let Some(entry) = self.entries.get(&existing_id)
            {
                return Ok(ImageAsset {
                    id: existing_id,
                    width: entry.width,
                    height: entry.height,
                });
            }
        } else {
            if let Some(&existing_id) = self.sized_path_index.get(&(canonical.clone(), min_svg_px))
                && let Some(entry) = self.entries.get(&existing_id)
            {
                return Ok(ImageAsset {
                    id: existing_id,
                    width: entry.width,
                    height: entry.height,
                });
            }
            if let Some(&existing_id) = self.path_index.get(&canonical)
                && let Some(entry) = self.entries.get(&existing_id)
                && (entry.format != Some(ImageFormat::Svg)
                    || entry.width.max(entry.height) >= min_svg_px)
            {
                return Ok(ImageAsset {
                    id: existing_id,
                    width: entry.width,
                    height: entry.height,
                });
            }
        }

        let bytes = read_bounded_file(&canonical)?;
        let format = detect_format(&bytes, Some(&canonical))?;
        let base_hash = hash_bytes(&bytes);
        let (effective_svg_px, cache_hash) = svg_sized_hash(base_hash, format, min_svg_px);

        if let Some(&existing_id) = self.hash_index.get(&cache_hash)
            && let Some(entry) = self.entries.get(&existing_id)
        {
            if effective_svg_px > 0 {
                self.sized_path_index
                    .insert((canonical, effective_svg_px), existing_id);
            } else {
                self.path_index.insert(canonical, existing_id);
            }
            return Ok(ImageAsset {
                id: existing_id,
                width: entry.width,
                height: entry.height,
            });
        }

        let revision = self.alloc_revision();
        let decoded = decode_image_bytes_sized(&bytes, format, effective_svg_px, revision)?;
        let id = self.alloc_id();
        let asset = ImageAsset {
            id,
            width: decoded.width,
            height: decoded.height,
        };
        if effective_svg_px > 0 {
            self.sized_path_index
                .insert((canonical.clone(), effective_svg_px), id);
        } else {
            self.path_index.insert(canonical.clone(), id);
        }
        self.hash_index.insert(cache_hash, id);
        self.entries.insert(
            id,
            CacheEntry {
                width: decoded.width,
                height: decoded.height,
                revision,
                format: Some(format),
                image: Some(decoded),
                source: AssetSource::Path {
                    path: canonical,
                    min_svg_px: effective_svg_px,
                },
                content_hash: cache_hash,
                retention: AssetRetention::KeepDecoded,
            },
        );
        Ok(asset)
    }

    pub(crate) fn load_bytes(&mut self, bytes: &[u8]) -> Result<ImageAsset> {
        self.load_bytes_sized(bytes, 0)
    }

    pub(crate) fn load_bytes_sized(&mut self, bytes: &[u8], min_svg_px: u32) -> Result<ImageAsset> {
        let format = detect_format(bytes, None)?;
        let base_hash = hash_bytes(bytes);
        let (effective_svg_px, cache_hash) = svg_sized_hash(base_hash, format, min_svg_px);

        if let Some(&existing_id) = self.hash_index.get(&cache_hash)
            && let Some(entry) = self.entries.get(&existing_id)
        {
            return Ok(ImageAsset {
                id: existing_id,
                width: entry.width,
                height: entry.height,
            });
        }

        let revision = self.alloc_revision();
        let decoded = decode_image_bytes_sized(bytes, format, effective_svg_px, revision)?;
        let id = self.alloc_id();
        let asset = ImageAsset {
            id,
            width: decoded.width,
            height: decoded.height,
        };
        self.hash_index.insert(cache_hash, id);
        self.entries.insert(
            id,
            CacheEntry {
                width: decoded.width,
                height: decoded.height,
                revision,
                format: Some(format),
                image: Some(decoded),
                source: AssetSource::EncodedBytes {
                    bytes: Arc::from(bytes),
                    format,
                    min_svg_px: effective_svg_px,
                },
                content_hash: cache_hash,
                retention: AssetRetention::KeepDecoded,
            },
        );
        Ok(asset)
    }

    /// Inserts a pre-decoded image directly (useful for procedural test assets or worker completions).
    pub(crate) fn insert_decoded_rgba(
        &mut self,
        width: u32,
        height: u32,
        premultiplied_rgba: Vec<u8>,
    ) -> Result<ImageAsset> {
        validate_dimensions(width, height)?;
        let expected_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| WaywiError::AssetDecode("image byte size overflow".to_owned()))?;
        if premultiplied_rgba.len() != expected_len {
            return Err(WaywiError::AssetDecode(format!(
                "expected {expected_len} RGBA bytes, got {}",
                premultiplied_rgba.len()
            )));
        }

        let content_hash =
            hash_bytes(&premultiplied_rgba) ^ ((u64::from(width) << 32) | u64::from(height));
        if let Some(&existing_id) = self.hash_index.get(&content_hash)
            && let Some(entry) = self.entries.get(&existing_id)
        {
            return Ok(ImageAsset {
                id: existing_id,
                width: entry.width,
                height: entry.height,
            });
        }

        let id = self.alloc_id();
        let revision = self.alloc_revision();
        let asset = ImageAsset { id, width, height };
        self.hash_index.insert(content_hash, id);
        self.entries.insert(
            id,
            CacheEntry {
                width,
                height,
                revision,
                format: None,
                image: Some(DecodedImage {
                    width,
                    height,
                    pixels: Arc::from(premultiplied_rgba),
                    revision,
                    animated_frames: None,
                }),
                source: AssetSource::RawPixels,
                content_hash,
                retention: AssetRetention::KeepDecoded,
            },
        );
        Ok(asset)
    }

    /// Converts raw straight RGB/RGBA pixel data (such as XDG notification `image-data` hints)
    /// into cached premultiplied RGBA asset memory.
    pub(crate) fn insert_raw_pixels(
        &mut self,
        width: u32,
        height: u32,
        rowstride: usize,
        has_alpha: bool,
        data: &[u8],
    ) -> Result<ImageAsset> {
        validate_dimensions(width, height)?;
        let w = width as usize;
        let h = height as usize;
        let src_channels = if has_alpha { 4 } else { 3 };
        let min_row = w
            .checked_mul(src_channels)
            .ok_or_else(|| WaywiError::AssetDecode("raw pixel row size overflow".to_owned()))?;
        let stride = rowstride.max(min_row);
        let needed = h
            .checked_sub(1)
            .and_then(|rows| rows.checked_mul(stride))
            .and_then(|off| off.checked_add(min_row))
            .ok_or_else(|| WaywiError::AssetDecode("raw pixel buffer overflow".to_owned()))?;
        if data.len() < needed {
            return Err(WaywiError::AssetDecode(format!(
                "raw pixel buffer too short: need {needed} bytes, got {}",
                data.len()
            )));
        }

        let mut premul = vec![0u8; w * h * 4];
        for y in 0..h {
            let row_src = &data[y * stride..];
            let row_dst = &mut premul[y * w * 4..(y + 1) * w * 4];
            for x in 0..w {
                let s = x * src_channels;
                let d = x * 4;
                let a = if has_alpha { row_src[s + 3] } else { 255 };
                premultiply_into(
                    &mut row_dst[d..d + 4],
                    row_src[s],
                    row_src[s + 1],
                    row_src[s + 2],
                    a,
                );
            }
        }
        self.insert_decoded_rgba(width, height, premul)
    }

    /// Reloads an existing [`ImageAsset`] from `path`, updating its revision so dependent surfaces
    /// automatically redraw.
    pub(crate) fn reload(
        &mut self,
        asset: ImageAsset,
        path: impl AsRef<Path>,
    ) -> Result<ImageAsset> {
        if !self.entries.contains_key(&asset.id) {
            return Err(WaywiError::AssetDecode(format!(
                "cannot reload unknown or unloaded asset {:?}",
                asset.id
            )));
        }
        let path_ref = path.as_ref();
        let canonical = std::fs::canonicalize(path_ref).unwrap_or_else(|_| path_ref.to_path_buf());
        let bytes = read_bounded_file(&canonical)?;
        let format = detect_format(&bytes, Some(&canonical))?;
        let new_hash = hash_bytes(&bytes);
        let revision = self.alloc_revision();
        let decoded = decode_image_bytes(&bytes, format, revision)?;
        let updated = ImageAsset {
            id: asset.id,
            width: decoded.width,
            height: decoded.height,
        };

        let mut preserved_retention = AssetRetention::KeepDecoded;
        if let Some(old_entry) = self.entries.get(&asset.id) {
            preserved_retention = old_entry.retention;
            if old_entry.image.is_none() {
                self.evicted_count = self.evicted_count.saturating_sub(1);
            }
            self.hash_index.remove(&old_entry.content_hash);
            if let AssetSource::Path {
                path: ref old_path, ..
            } = old_entry.source
            {
                self.path_index.remove(old_path);
            }
        }
        self.sized_path_index
            .retain(|_, &mut cached| cached != asset.id);
        self.path_index.insert(canonical.clone(), asset.id);
        self.hash_index.insert(new_hash, asset.id);
        self.entries.insert(
            asset.id,
            CacheEntry {
                width: decoded.width,
                height: decoded.height,
                revision,
                format: Some(format),
                image: Some(decoded),
                source: AssetSource::Path {
                    path: canonical,
                    min_svg_px: 0,
                },
                content_hash: new_hash,
                retention: preserved_retention,
            },
        );
        Ok(updated)
    }

    /// Removes an asset from the cache and frees its decoded pixel buffer (once unused).
    pub(crate) fn unload(&mut self, id: AssetId) -> bool {
        if let Some(entry) = self.entries.remove(&id) {
            if entry.image.is_none() {
                self.evicted_count = self.evicted_count.saturating_sub(1);
            }
            if entry.retention == AssetRetention::ReclaimAfterPresent {
                self.reclaimable_count = self.reclaimable_count.saturating_sub(1);
            }
            self.hash_index.remove(&entry.content_hash);
            if let AssetSource::Path { path, .. } = entry.source {
                self.path_index.remove(&path);
            }
            self.sized_path_index.retain(|_, &mut cached| cached != id);
            true
        } else {
            false
        }
    }

    /// Stores a decoded image completed by a background worker thread under a fresh `AssetId`.
    pub(crate) fn commit_worker_decoded(
        &mut self,
        path: Option<PathBuf>,
        format: Option<ImageFormat>,
        content_hash: u64,
        mut decoded: DecodedImage,
    ) -> ImageAsset {
        let revision = self.alloc_revision();
        decoded.revision = revision;
        let id = self.alloc_id();
        let asset = ImageAsset {
            id,
            width: decoded.width,
            height: decoded.height,
        };
        if let Some(ref p) = path {
            self.path_index.insert(p.clone(), id);
        }
        self.hash_index.insert(content_hash, id);
        let source = path.map_or(AssetSource::RawPixels, |p| AssetSource::Path {
            path: p,
            min_svg_px: 0,
        });
        self.entries.insert(
            id,
            CacheEntry {
                width: decoded.width,
                height: decoded.height,
                revision,
                format,
                image: Some(decoded),
                source,
                content_hash,
                retention: AssetRetention::KeepDecoded,
            },
        );
        asset
    }

    /// Updates the memory retention policy for `id`.
    ///
    /// Returns `false` if `id` is unknown or if [`AssetRetention::ReclaimAfterPresent`] is
    /// requested for a raw-pixel asset (`load_rgba_image`) that cannot be re-decoded.
    pub(crate) fn set_retention(&mut self, id: AssetId, retention: AssetRetention) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };
        if retention == AssetRetention::ReclaimAfterPresent && !entry.source.is_reclaimable() {
            return false;
        }
        if entry.retention != retention {
            match retention {
                AssetRetention::ReclaimAfterPresent => {
                    self.reclaimable_count += 1;
                }
                AssetRetention::KeepDecoded => {
                    self.reclaimable_count = self.reclaimable_count.saturating_sub(1);
                }
            }
            entry.retention = retention;
        }
        true
    }

    #[must_use]
    pub(crate) fn retention(&self, id: AssetId) -> Option<AssetRetention> {
        self.entries.get(&id).map(|e| e.retention)
    }

    /// Releases the decoded RGBA8 pixel buffer for `id` while preserving its handle, dimensions,
    /// revision, and backing source so future redraws transparently re-decode on demand.
    ///
    /// Returns `true` if decoded pixels were resident and freed; returns `false` if `id` is
    /// unknown, already evicted, or backed by raw pixels with no encoded/file source.
    pub(crate) fn evict_decoded_pixels(&mut self, id: AssetId) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };
        if !entry.source.is_reclaimable() || entry.image.is_none() {
            return false;
        }
        entry.image = None;
        self.evicted_count += 1;
        true
    }

    /// Re-decodes `id` from its backing disk path or compressed bytes if its decoded pixels were
    /// previously evicted, preserving its original `revision`.
    pub(crate) fn ensure_decoded(&mut self, id: AssetId) -> Result<()> {
        let Some(entry) = self.entries.get_mut(&id) else {
            return Err(WaywiError::AssetDecode(format!(
                "cannot decode unknown asset {id:?}"
            )));
        };
        if entry.image.is_some() {
            return Ok(());
        }
        let decoded = match &entry.source {
            AssetSource::Path { path, min_svg_px } => {
                let bytes = read_bounded_file(path)?;
                let format = detect_format(&bytes, Some(path))?;
                let effective_svg_px = if format == ImageFormat::Svg {
                    *min_svg_px
                } else {
                    0
                };
                decode_image_bytes_sized(&bytes, format, effective_svg_px, entry.revision)?
            }
            AssetSource::EncodedBytes {
                bytes,
                format,
                min_svg_px,
            } => {
                let effective_svg_px = if *format == ImageFormat::Svg {
                    *min_svg_px
                } else {
                    0
                };
                decode_image_bytes_sized(bytes, *format, effective_svg_px, entry.revision)?
            }
            AssetSource::RawPixels => {
                return Err(WaywiError::AssetDecode(format!(
                    "raw pixel asset {id:?} has no re-decodable backing source"
                )));
            }
        };
        entry.width = decoded.width;
        entry.height = decoded.height;
        entry.image = Some(decoded);
        self.evicted_count = self.evicted_count.saturating_sub(1);
        Ok(())
    }

    /// Ensures every evicted `Image` or `Sprite` asset referenced by `commands` has its decoded
    /// RGBA8 pixels resident before constructing a `FrameScene`.
    ///
    /// Fast path: single branch (`O(1)`) when no cached assets are currently evicted.
    pub(crate) fn ensure_decoded_for_commands(&mut self, commands: &[DrawCommand]) {
        if self.evicted_count == 0 {
            return;
        }
        for cmd in commands {
            let id = match &cmd.payload {
                PaintPayload::Image { asset, .. } => asset.id,
                PaintPayload::Sprite { sheet, .. } => sheet.asset.id,
                _ => continue,
            };
            if self.entries.get(&id).is_some_and(|e| e.image.is_none()) {
                let _ = self.ensure_decoded(id);
                if self.evicted_count == 0 {
                    break;
                }
            }
        }
    }

    /// Collects resident [`AssetRetention::ReclaimAfterPresent`] asset IDs referenced by
    /// `commands` so callers can reclaim them once all dirty surfaces in the pass have committed.
    ///
    /// Fast path: single branch (`O(1)`) when no assets use `ReclaimAfterPresent`.
    pub(crate) fn collect_reclaimable_from_commands(
        &self,
        commands: &[DrawCommand],
        out: &mut Vec<AssetId>,
    ) {
        if self.reclaimable_count == 0 {
            return;
        }
        for cmd in commands {
            let id = match &cmd.payload {
                PaintPayload::Image { asset, .. } => asset.id,
                PaintPayload::Sprite { sheet, .. } => sheet.asset.id,
                _ => continue,
            };
            if self.entries.get(&id).is_some_and(|e| {
                e.retention == AssetRetention::ReclaimAfterPresent && e.image.is_some()
            }) {
                out.push(id);
            }
        }
    }

    /// Evicts decoded pixel buffers for every `ReclaimAfterPresent` asset listed in `ids`.
    pub(crate) fn reclaim_assets(&mut self, ids: &[AssetId]) {
        for &id in ids {
            if self
                .entries
                .get(&id)
                .is_some_and(|e| e.retention == AssetRetention::ReclaimAfterPresent)
            {
                self.evict_decoded_pixels(id);
            }
        }
    }

    #[must_use]
    pub(crate) fn get(&self, id: AssetId) -> Option<&DecodedImage> {
        self.entries.get(&id).and_then(|e| e.image.as_ref())
    }

    #[must_use]
    pub(crate) fn dimensions_of(&self, id: AssetId) -> Option<(u32, u32)> {
        self.entries.get(&id).map(|e| (e.width, e.height))
    }

    #[must_use]
    pub(crate) fn revision_of(&self, id: AssetId) -> u64 {
        self.entries.get(&id).map_or(0, |e| e.revision)
    }

    #[must_use]
    pub(crate) fn is_decoded(&self, id: AssetId) -> bool {
        self.entries.get(&id).is_some_and(|e| e.image.is_some())
    }

    #[must_use]
    pub(crate) fn decoded_pixel_bytes(&self) -> usize {
        self.entries
            .values()
            .filter_map(|e| e.image.as_ref())
            .map(|img| {
                let extra: usize = img
                    .animated_frames
                    .as_ref()
                    .map_or(0, |frames| frames.iter().map(|f| f.pixels.len()).sum());
                img.pixels.len() + extra
            })
            .sum()
    }

    fn alloc_id(&mut self) -> AssetId {
        let id = AssetId(self.next_id);
        self.next_id += 1;
        id
    }

    fn alloc_revision(&mut self) -> u64 {
        let rev = self.next_revision;
        self.next_revision += 1;
        rev
    }
}

fn svg_sized_hash(base_hash: u64, format: ImageFormat, min_svg_px: u32) -> (u32, u64) {
    let effective = if format == ImageFormat::Svg {
        min_svg_px
    } else {
        0
    };
    let hash = if effective > 0 {
        base_hash ^ u64::from(effective).wrapping_mul(0x9E37_79B9_7F4A_7C15)
    } else {
        base_hash
    };
    (effective, hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_counter_invariants(cache: &AssetCache) {
        let actual_evicted = cache.entries.values().filter(|e| e.image.is_none()).count();
        let actual_reclaimable = cache
            .entries
            .values()
            .filter(|e| e.retention == AssetRetention::ReclaimAfterPresent)
            .count();
        assert_eq!(cache.evicted_count, actual_evicted);
        assert_eq!(cache.reclaimable_count, actual_reclaimable);
    }

    #[test]
    fn raw_pixel_assets_cannot_be_evicted_or_marked_reclaimable() {
        let mut cache = AssetCache::new();
        let raw = cache
            .insert_decoded_rgba(2, 2, vec![255; 16])
            .expect("raw rgba insert");
        assert_eq!(cache.retention(raw.id), Some(AssetRetention::KeepDecoded));
        assert!(!cache.set_retention(raw.id, AssetRetention::ReclaimAfterPresent));
        assert!(!cache.evict_decoded_pixels(raw.id));
        assert!(cache.is_decoded(raw.id));
        assert_eq!(cache.decoded_pixel_bytes(), 16);
        assert_counter_invariants(&cache);

        assert!(cache.unload(raw.id));
        assert_eq!(cache.decoded_pixel_bytes(), 0);
        assert_counter_invariants(&cache);
    }

    #[cfg(feature = "image-svg")]
    #[test]
    fn eviction_redecode_and_unload_keep_revision_and_counters_consistent() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6"><rect width="8" height="6" fill="red"/></svg>"#;
        let mut cache = AssetCache::new();
        let asset = cache.load_bytes_sized(svg, 24).expect("svg decode");
        let rev_before = cache.revision_of(asset.id);
        let dims_before = cache.dimensions_of(asset.id);
        assert_eq!(dims_before, Some((24, 18)));
        assert_eq!(cache.decoded_pixel_bytes(), 24 * 18 * 4);

        assert!(cache.set_retention(asset.id, AssetRetention::ReclaimAfterPresent));
        assert_counter_invariants(&cache);
        assert!(cache.evict_decoded_pixels(asset.id));
        assert!(!cache.evict_decoded_pixels(asset.id));
        assert!(!cache.is_decoded(asset.id));
        assert_eq!(cache.decoded_pixel_bytes(), 0);
        assert_eq!(cache.revision_of(asset.id), rev_before);
        assert_eq!(cache.dimensions_of(asset.id), dims_before);
        assert_counter_invariants(&cache);

        // Deduplication hit while evicted still returns accurate dimensions without re-decoding.
        let dedup = cache.load_bytes_sized(svg, 24).expect("dedup");
        assert_eq!(dedup, asset);
        assert!(!cache.is_decoded(asset.id));

        // On-demand re-decode restores exact pixels and keeps revision identical.
        cache.ensure_decoded(asset.id).expect("redecode");
        assert!(cache.is_decoded(asset.id));
        assert_eq!(cache.revision_of(asset.id), rev_before);
        assert_eq!(cache.decoded_pixel_bytes(), 24 * 18 * 4);
        assert_counter_invariants(&cache);

        // Switching retention back to KeepDecoded and unloading an evicted asset keeps counters clean.
        assert!(cache.evict_decoded_pixels(asset.id));
        assert!(cache.unload(asset.id));
        assert_counter_invariants(&cache);
    }
}
