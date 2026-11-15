use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Result, WaywiError};

use super::decode::{
    decode_image_bytes, decode_image_bytes_sized, detect_format, hash_bytes, premultiply_into,
    read_bounded_file, validate_dimensions,
};
use super::types::{AssetId, AssetSource, CacheEntry, DecodedImage, ImageAsset, ImageFormat};

/// Shared decoded asset store owned by `App`.
pub(crate) struct AssetCache {
    next_id: u64,
    next_revision: u64,
    entries: HashMap<AssetId, CacheEntry>,
    path_index: HashMap<PathBuf, AssetId>,
    sized_path_index: HashMap<(PathBuf, u32), AssetId>,
    hash_index: HashMap<u64, AssetId>,
    svg_source_cache: HashMap<AssetId, Arc<[u8]>>,
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
            svg_source_cache: HashMap::new(),
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
                    width: entry.image.width,
                    height: entry.image.height,
                });
            }
        } else {
            if let Some(&existing_id) = self.sized_path_index.get(&(canonical.clone(), min_svg_px))
                && let Some(entry) = self.entries.get(&existing_id)
            {
                return Ok(ImageAsset {
                    id: existing_id,
                    width: entry.image.width,
                    height: entry.image.height,
                });
            }
            if let Some(&existing_id) = self.path_index.get(&canonical)
                && let Some(entry) = self.entries.get(&existing_id)
                && (!self.svg_source_cache.contains_key(&existing_id)
                    || entry.image.width.max(entry.image.height) >= min_svg_px)
            {
                return Ok(ImageAsset {
                    id: existing_id,
                    width: entry.image.width,
                    height: entry.image.height,
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
                width: entry.image.width,
                height: entry.image.height,
            });
        }

        let decoded =
            decode_image_bytes_sized(&bytes, format, effective_svg_px, self.alloc_revision())?;
        let id = self.alloc_id();
        let asset = ImageAsset {
            id,
            width: decoded.width,
            height: decoded.height,
        };
        if format == ImageFormat::Svg {
            self.svg_source_cache.insert(id, Arc::from(bytes));
        }
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
                image: decoded,
                source: AssetSource::Path(canonical),
                content_hash: cache_hash,
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
                width: entry.image.width,
                height: entry.image.height,
            });
        }

        let decoded =
            decode_image_bytes_sized(bytes, format, effective_svg_px, self.alloc_revision())?;
        let id = self.alloc_id();
        let asset = ImageAsset {
            id,
            width: decoded.width,
            height: decoded.height,
        };
        if format == ImageFormat::Svg {
            self.svg_source_cache.insert(id, Arc::from(bytes));
        }
        self.hash_index.insert(cache_hash, id);
        self.entries.insert(
            id,
            CacheEntry {
                image: decoded,
                source: AssetSource::Bytes,
                content_hash: cache_hash,
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
                width: entry.image.width,
                height: entry.image.height,
            });
        }

        let id = self.alloc_id();
        let revision = self.alloc_revision();
        let asset = ImageAsset { id, width, height };
        self.hash_index.insert(content_hash, id);
        self.entries.insert(
            id,
            CacheEntry {
                image: DecodedImage {
                    width,
                    height,
                    pixels: Arc::from(premultiplied_rgba),
                    revision,
                    animated_frames: None,
                },
                source: AssetSource::Bytes,
                content_hash,
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
        let decoded = decode_image_bytes(&bytes, format, self.alloc_revision())?;
        let updated = ImageAsset {
            id: asset.id,
            width: decoded.width,
            height: decoded.height,
        };

        if let Some(old_entry) = self.entries.get(&asset.id) {
            self.hash_index.remove(&old_entry.content_hash);
            if let AssetSource::Path(ref old_path) = old_entry.source {
                self.path_index.remove(old_path);
            }
        }
        if format == ImageFormat::Svg {
            self.svg_source_cache.insert(asset.id, Arc::from(bytes));
        } else {
            self.svg_source_cache.remove(&asset.id);
        }
        self.path_index.insert(canonical.clone(), asset.id);
        self.hash_index.insert(new_hash, asset.id);
        self.entries.insert(
            asset.id,
            CacheEntry {
                image: decoded,
                source: AssetSource::Path(canonical),
                content_hash: new_hash,
            },
        );
        Ok(updated)
    }

    /// Removes an asset from the cache and frees its decoded pixel buffer (once unused).
    pub(crate) fn unload(&mut self, id: AssetId) -> bool {
        if let Some(entry) = self.entries.remove(&id) {
            self.hash_index.remove(&entry.content_hash);
            if let AssetSource::Path(path) = entry.source {
                self.path_index.remove(&path);
            }
            self.sized_path_index.retain(|_, &mut cached| cached != id);
            self.svg_source_cache.remove(&id);
            true
        } else {
            false
        }
    }

    /// Stores a decoded image completed by a background worker thread under a fresh `AssetId`.
    pub(crate) fn commit_worker_decoded(
        &mut self,
        path: Option<PathBuf>,
        content_hash: u64,
        mut decoded: DecodedImage,
    ) -> ImageAsset {
        decoded.revision = self.alloc_revision();
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
        self.entries.insert(
            id,
            CacheEntry {
                image: decoded,
                source: path.map_or(AssetSource::Bytes, AssetSource::Path),
                content_hash,
            },
        );
        asset
    }

    #[must_use]
    pub(crate) fn get(&self, id: AssetId) -> Option<&DecodedImage> {
        self.entries.get(&id).map(|e| &e.image)
    }

    #[must_use]
    pub(crate) fn revision_of(&self, id: AssetId) -> u64 {
        self.entries.get(&id).map_or(0, |e| e.image.revision)
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
