use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
    /// Animated GIF frames if loaded from an animated GIF (`image-gif` feature).
    #[allow(dead_code)]
    pub animated_frames: Option<Arc<Vec<DecodedGifFrame>>>,
}

/// Single frame of an animated GIF (distinct from [`SpriteSheet`](crate::sprite::SpriteSheet)).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DecodedGifFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
    pub delay: Duration,
}

/// Supported image format detected from magic bytes or extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Svg,
    Gif,
    Webp,
}

#[derive(Debug, Clone)]
pub(super) enum AssetSource {
    Path(PathBuf),
    Bytes,
}

#[derive(Debug, Clone)]
pub(super) struct CacheEntry {
    pub image: DecodedImage,
    pub source: AssetSource,
    pub content_hash: u64,
}
