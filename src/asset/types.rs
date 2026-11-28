use std::path::PathBuf;
use std::sync::Arc;

/// Unique identifier for a decoded asset inside [`App`](crate::app::App)'s shared asset cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(pub u64);

impl AssetId {
    #[inline]
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// `Copy` handle referencing a preloaded or dynamically loaded image asset in `App`'s
/// shared decoded cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageAsset {
    pub id: AssetId,
    pub width: u32,
    pub height: u32,
}

impl From<&ImageAsset> for ImageAsset {
    #[inline]
    fn from(value: &ImageAsset) -> Self {
        *value
    }
}

/// Decoded RGBA8 premultiplied bitmap stored once in the shared cache.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Premultiplied `[R, G, B, A]` pixels (`width * height * 4` bytes).
    pub pixels: Arc<[u8]>,
    /// Monotonically incremented whenever this asset is reloaded.
    pub revision: u64,
}

/// Supported image format detected from magic bytes or extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Svg,
    Gif,
    Webp,
}

/// Memory retention policy for decoded RGBA8 pixel buffers in WayWi's internal asset cache.
///
/// The default [`AssetRetention::KeepDecoded`] keeps decoded premultiplied RGBA8 pixels resident
/// in RAM for immediate zero-decode redraws (ideal for icons, active sprites, and frequently
/// updated UI images).
///
/// [`AssetRetention::ReclaimAfterPresent`] automatically drops the decoded RGBA8 pixel buffer
/// after a frame referencing the asset is presented, while preserving its [`ImageAsset`] handle,
/// dimensions, revision, and backing source (disk path or compressed encoded bytes). If a future
/// redraw is needed (for example, output resize, monitor hotplug, scale change, or a hidden
/// sprite reappearing), WayWi transparently re-decodes the pixels on demand before rasterization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssetRetention {
    /// Keep decoded RGBA8 pixel data resident in RAM until unloaded or explicitly evicted
    /// (default).
    #[default]
    KeepDecoded,
    /// Release decoded RGBA8 pixel data after presenting frames that draw this asset,
    /// automatically re-decoding from its backing path or compressed bytes on any future redraw.
    ReclaimAfterPresent,
}

#[derive(Debug, Clone)]
pub(super) enum AssetSource {
    Path {
        path: PathBuf,
        min_svg_px: u32,
    },
    EncodedBytes {
        bytes: Arc<[u8]>,
        format: ImageFormat,
        min_svg_px: u32,
    },
    StaticBytes {
        bytes: &'static [u8],
        format: ImageFormat,
        min_svg_px: u32,
    },
    RawPixels,
}

impl AssetSource {
    #[inline]
    pub(super) const fn is_reclaimable(&self) -> bool {
        !matches!(self, Self::RawPixels)
    }
}

#[derive(Debug, Clone)]
pub(super) struct CacheEntry {
    pub image: Option<DecodedImage>,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub format: Option<ImageFormat>,
    pub source: AssetSource,
    pub content_hash: u64,
    pub retention: AssetRetention,
}
