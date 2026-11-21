pub use crate::asset::ImageAsset;
use crate::input::CommonProps;

/// Pixel-space sub-rectangle `(x, y, width, height)` inside an [`ImageAsset`] texture atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl SourceRect {
    #[inline]
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[inline]
    #[must_use]
    pub const fn as_tuple(self) -> (u32, u32, u32, u32) {
        (self.x, self.y, self.width, self.height)
    }
}

/// Resampling filter used when scaling or transforming an [`Image`] or [`Sprite`](crate::sprite::Sprite).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageFilter {
    /// Smooth bilinear filtering (default for UI icons and photographs; enables mipmaps on GPU).
    #[default]
    Bilinear,
    /// High-quality 16-tap bicubic resampling for scaled illustrations and artwork in the software
    /// renderer (falls back to hardware bilinear + mipmaps on the GPU backend).
    Bicubic,
    /// Crisp nearest-neighbor sampling (ideal for pixel art and retro sprites).
    Nearest,
}

/// How an [`Image`] fits inside its declared bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageFit {
    /// Stretch/scale image to fill element bounds.
    #[default]
    Fill,
    /// Scale proportionally so the entire image fits inside bounds.
    Contain,
    /// Scale proportionally to cover bounds completely (excess clipped).
    Cover,
    /// Draw at original intrinsic asset size without scaling.
    Original,
}

impl ImageFit {
    /// Computes `(dx, dy, dw, dh)` for fitting `(src_w, src_h)` into `(dst_x, dst_y, dst_w, dst_h)`.
    #[inline]
    #[must_use]
    pub(crate) fn resolve_rect(
        self,
        dst_x: f32,
        dst_y: f32,
        dst_w: f32,
        dst_h: f32,
        src_w: f32,
        src_h: f32,
    ) -> (f32, f32, f32, f32) {
        match self {
            Self::Fill => (dst_x, dst_y, dst_w, dst_h),
            Self::Original => (dst_x, dst_y, src_w, src_h),
            Self::Contain => {
                let s = (dst_w / src_w).min(dst_h / src_h);
                let w = src_w * s;
                let h = src_h * s;
                (dst_x + (dst_w - w) * 0.5, dst_y + (dst_h - h) * 0.5, w, h)
            }
            Self::Cover => {
                let s = (dst_w / src_w).max(dst_h / src_h);
                let w = src_w * s;
                let h = src_h * s;
                (dst_x + (dst_w - w) * 0.5, dst_y + (dst_h - h) * 0.5, w, h)
            }
        }
    }
}

/// Raster or vector image element backed by an [`ImageAsset`].
#[must_use]
#[derive(Clone)]
pub struct Image<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) asset: ImageAsset,
    pub(crate) fit: ImageFit,
    pub(crate) filter: ImageFilter,
    pub(crate) source_rect: Option<SourceRect>,
    pub(crate) flip_x: bool,
    pub(crate) flip_y: bool,
}

impl<M: 'static> Image<M> {
    pub fn new(asset: impl Into<ImageAsset>) -> Self {
        Self {
            common: CommonProps::default(),
            asset: asset.into(),
            fit: ImageFit::Fill,
            filter: ImageFilter::Bilinear,
            source_rect: None,
            flip_x: false,
            flip_y: false,
        }
    }

    pub fn fit(mut self, fit: ImageFit) -> Self {
        self.fit = fit;
        self
    }

    /// Sets the resampling filter used when scaling or transforming this image.
    pub fn filter(mut self, filter: ImageFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Crops this image to a pixel-space sub-rectangle `(x, y, width, height)` inside the
    /// underlying [`ImageAsset`] texture atlas.
    pub fn source_rect(mut self, x: u32, y: u32, width: u32, height: u32) -> Self {
        self.source_rect = Some(SourceRect::new(x, y, width, height));
        self
    }

    /// Crops this image to an explicit [`SourceRect`] region inside the underlying [`ImageAsset`].
    pub fn region(mut self, rect: SourceRect) -> Self {
        self.source_rect = Some(rect);
        self
    }

    /// Mirrors this image horizontally around its transform pivot (composes with `.transform(...)`).
    pub fn flip_x(mut self, flip: bool) -> Self {
        self.flip_x = flip;
        self
    }

    /// Mirrors this image vertically around its transform pivot (composes with `.transform(...)`).
    pub fn flip_y(mut self, flip: bool) -> Self {
        self.flip_y = flip;
        self
    }

    crate::input::impl_common_builders!();

    /// Maps emitted messages into a parent message type `N`.
    pub fn map<N: 'static, F>(self, f: F) -> Image<N>
    where
        F: Fn(M) -> N + 'static,
    {
        Image {
            common: self.common.map(f),
            asset: self.asset,
            fit: self.fit,
            filter: self.filter,
            source_rect: self.source_rect,
            flip_x: self.flip_x,
            flip_y: self.flip_y,
        }
    }
}
