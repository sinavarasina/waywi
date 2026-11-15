use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor as SctkAnchor, KeyboardInteractivity as SctkKbInteractivity, Layer as SctkLayer,
    LayerShell, LayerSurface,
};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_output, wl_surface};

use crate::reconcile::SurfaceSpec;
use crate::surface::{Anchor, KeyboardInteractivity, Layer};

/// Converts WayWi [`Layer`] into `wlr_layer` stacking layer.
#[must_use]
pub(crate) fn map_layer(layer: Layer) -> SctkLayer {
    match layer {
        Layer::Background => SctkLayer::Background,
        Layer::Bottom => SctkLayer::Bottom,
        Layer::Top => SctkLayer::Top,
        Layer::Overlay => SctkLayer::Overlay,
    }
}

/// Converts WayWi [`Anchor`] bitflags into `wlr_layer` anchors.
#[must_use]
pub(crate) fn map_anchor(anchor: Anchor) -> SctkAnchor {
    let mut out = SctkAnchor::empty();
    if anchor.contains(Anchor::TOP) {
        out |= SctkAnchor::TOP;
    }
    if anchor.contains(Anchor::BOTTOM) {
        out |= SctkAnchor::BOTTOM;
    }
    if anchor.contains(Anchor::LEFT) {
        out |= SctkAnchor::LEFT;
    }
    if anchor.contains(Anchor::RIGHT) {
        out |= SctkAnchor::RIGHT;
    }
    out
}

/// Converts WayWi [`KeyboardInteractivity`] into `wlr_layer` keyboard focus mode.
#[must_use]
pub(crate) fn map_keyboard_interactivity(kb: KeyboardInteractivity) -> SctkKbInteractivity {
    match kb {
        KeyboardInteractivity::None => SctkKbInteractivity::None,
        KeyboardInteractivity::OnDemand => SctkKbInteractivity::OnDemand,
        KeyboardInteractivity::Exclusive => SctkKbInteractivity::Exclusive,
    }
}

/// Creates a new `zwlr_layer_surface_v1` configured according to [`SurfaceSpec`].
pub(crate) fn create_layer_surface<D>(
    layer_shell: &LayerShell,
    qh: &QueueHandle<D>,
    wl_surface: wl_surface::WlSurface,
    spec: &SurfaceSpec,
    output: Option<&wl_output::WlOutput>,
) -> LayerSurface
where
    D: wayland_client::Dispatch<
            smithay_client_toolkit::reexports::protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
            smithay_client_toolkit::shell::wlr_layer::LayerSurfaceData,
        > + 'static,
{
    let namespace = if spec.namespace.is_empty() {
        "waywi".to_string()
    } else {
        spec.namespace.as_str().to_string()
    };
    let layer_surface = layer_shell.create_layer_surface(
        qh,
        wl_surface,
        map_layer(spec.layer),
        Some(namespace),
        output,
    );
    apply_layer_spec(&layer_surface, spec);
    layer_surface.commit();
    layer_surface
}

/// Applies mutable layer-surface double-buffered state without unnecessarily committing yet.
pub(crate) fn apply_layer_spec(layer_surface: &LayerSurface, spec: &SurfaceSpec) {
    layer_surface.set_layer(map_layer(spec.layer));
    layer_surface.set_anchor(map_anchor(spec.anchor));
    layer_surface.set_margin(
        spec.margin.top.0.round() as i32,
        spec.margin.right.0.round() as i32,
        spec.margin.bottom.0.round() as i32,
        spec.margin.left.0.round() as i32,
    );

    // Note: in `wlr-layer-shell`, passing 0 on an axis anchored to opposite edges allows the
    // compositor to stretch to output width/height while explicit pixels set exact extent.
    let w = if spec.anchor.contains(Anchor::LEFT)
        && spec.anchor.contains(Anchor::RIGHT)
        && spec.logical_size.width.0 <= 1.0
    {
        0
    } else {
        spec.logical_size.width.0.max(1.0).round() as u32
    };
    let h = if spec.anchor.contains(Anchor::TOP)
        && spec.anchor.contains(Anchor::BOTTOM)
        && spec.logical_size.height.0 <= 1.0
    {
        0
    } else {
        spec.logical_size.height.0.max(1.0).round() as u32
    };
    layer_surface.set_size(w, h);
    layer_surface.set_exclusive_zone(spec.exclusive_zone);
    layer_surface
        .set_keyboard_interactivity(map_keyboard_interactivity(spec.keyboard_interactivity));
}
