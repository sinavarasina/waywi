//! Wayland `ext_background_effect_v1` per-surface background blur backend.

use std::collections::HashMap;

use smithay_client_toolkit::compositor::{CompositorState, Region};
use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_surface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, Capability as BgCap, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};

use crate::geometry::LogicalSize;
use crate::surface::BackgroundBlur;
use crate::wayland::connection::WaylandState;

/// Manages declarative `ext_background_effect_surface_v1` blur regions tied to live `wl_surface`s.
pub(crate) struct WaylandBackgroundEffectState {
    manager: Option<ExtBackgroundEffectManagerV1>,
    blur_supported: bool,
    surfaces: HashMap<ObjectId, ExtBackgroundEffectSurfaceV1>,
}

impl Drop for WaylandBackgroundEffectState {
    fn drop(&mut self) {
        self.clear_all();
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandBackgroundEffectState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ExtBackgroundEffectManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        Self {
            manager,
            blur_supported: false,
            surfaces: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some() && self.blur_supported
    }

    /// Synchronizes background blur state for `surface` cleanly (idempotent on `None`).
    pub(crate) fn sync_surface(
        &mut self,
        compositor: &CompositorState,
        surface: &wl_surface::WlSurface,
        blur: BackgroundBlur,
        logical_size: LogicalSize,
        qh: &QueueHandle<WaylandState>,
    ) {
        let id = surface.id();
        match blur {
            BackgroundBlur::None => {
                if let Some(effect) = self.surfaces.remove(&id) {
                    effect.set_blur_region(None);
                    effect.destroy();
                }
            }
            BackgroundBlur::Full | BackgroundBlur::Region(_) => {
                if !self.is_supported() {
                    return;
                }
                let effect = if let Some(existing) = self.surfaces.get(&id) {
                    existing.clone()
                } else {
                    let Some(ref mgr) = self.manager else {
                        return;
                    };
                    let created = mgr.get_background_effect(surface, qh, ());
                    self.surfaces.insert(id, created.clone());
                    created
                };
                if let Ok(region) = Region::new(compositor) {
                    match blur {
                        BackgroundBlur::Full => {
                            let w = logical_size.width.0.ceil().max(1.0) as i32;
                            let h = logical_size.height.0.ceil().max(1.0) as i32;
                            region.add(0, 0, w, h);
                        }
                        BackgroundBlur::Region(b) => {
                            let x = b.x.0.floor() as i32;
                            let y = b.y.0.floor() as i32;
                            let w = b.width.0.ceil().max(1.0) as i32;
                            let h = b.height.0.ceil().max(1.0) as i32;
                            region.add(x, y, w, h);
                        }
                        BackgroundBlur::None => {}
                    }
                    effect.set_blur_region(Some(region.wl_region()));
                }
            }
        }
    }

    /// Must be called BEFORE `wl_surface` is destroyed (`surface_destroyed` protocol error otherwise).
    pub(crate) fn clear_surface(&mut self, wl_id: &ObjectId) {
        if let Some(effect) = self.surfaces.remove(wl_id) {
            effect.destroy();
        }
    }

    pub(crate) fn clear_all(&mut self) {
        for (_, effect) in self.surfaces.drain() {
            effect.destroy();
        }
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            state.background_effect.blur_supported = match flags {
                WEnum::Value(caps) => caps.contains(BgCap::Blur),
                WEnum::Unknown(raw) => (raw & 1) != 0,
            };
        }
    }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtBackgroundEffectSurfaceV1,
        _event: ext_background_effect_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
