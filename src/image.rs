pub use crate::asset::ImageAsset;
use crate::input::CommonProps;

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
}

impl<M: 'static> Image<M> {
    pub fn new(asset: impl Into<ImageAsset>) -> Self {
        Self {
            common: CommonProps::default(),
            asset: asset.into(),
            fit: ImageFit::Fill,
        }
    }

    pub fn fit(mut self, fit: ImageFit) -> Self {
        self.fit = fit;
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
        }
    }
}
