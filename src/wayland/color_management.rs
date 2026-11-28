//! Wayland `wp_color_management_v1` per-surface color-space & HDR backend.

use std::collections::{HashMap, HashSet};

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_surface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_management_surface_v1::{self, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{
        self, Feature as CmFeature, Primaries as CmPrimaries, RenderIntent as CmRenderIntent,
        TransferFunction as CmTf, WpColorManagerV1,
    },
    wp_image_description_creator_params_v1::{self, WpImageDescriptionCreatorParamsV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};

use crate::surface::ColorSpace;
use crate::wayland::connection::WaylandState;

struct PendingImageDesc {
    wl_surface_id: ObjectId,
    desc: WpImageDescriptionV1,
}

/// Manages `wp_color_management_v1` parametric image descriptions per `wl_surface`.
pub(crate) struct WaylandColorManagementState {
    manager: Option<WpColorManagerV1>,
    supports_parametric: bool,
    supported_primaries: HashSet<u32>,
    supported_tfs: HashSet<u32>,
    surfaces: HashMap<ObjectId, (WpColorManagementSurfaceV1, ColorSpace)>,
    pending_descs: HashMap<u64, PendingImageDesc>,
    next_desc_id: u64,
}

impl Drop for WaylandColorManagementState {
    fn drop(&mut self) {
        self.clear_all();
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandColorManagementState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals.bind::<WpColorManagerV1, _, _>(qh, 1..=1, ()).ok();
        Self {
            manager,
            supports_parametric: false,
            supported_primaries: HashSet::new(),
            supported_tfs: HashSet::new(),
            surfaces: HashMap::new(),
            pending_descs: HashMap::new(),
            next_desc_id: 1,
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    /// Synchronizes surface color space cleanly (idempotent; skips wire traffic when unchanged).
    pub(crate) fn sync_surface(
        &mut self,
        surface: &wl_surface::WlSurface,
        space: ColorSpace,
        qh: &QueueHandle<WaylandState>,
    ) {
        let wl_id = surface.id();
        if space == ColorSpace::Default {
            if let Some((cm_surf, _)) = self.surfaces.remove(&wl_id) {
                cm_surf.unset_image_description();
                cm_surf.destroy();
            }
            self.drop_pending_for_surface(&wl_id);
            return;
        }

        let Some(mgr) = self.manager.clone() else {
            return;
        };
        if !self.supports_parametric {
            return;
        }
        let Some((primaries, tf)) = map_color_space(space) else {
            return;
        };
        if !self.supported_primaries.is_empty()
            && !self.supported_primaries.contains(&(primaries as u32))
        {
            return;
        }
        if !self.supported_tfs.is_empty() && !self.supported_tfs.contains(&(tf as u32)) {
            return;
        }

        if let Some((_, existing_space)) = self.surfaces.get(&wl_id)
            && *existing_space == space
        {
            return;
        }

        if let Some((_, existing_space)) = self.surfaces.get_mut(&wl_id) {
            *existing_space = space;
        } else {
            let cm_surf = mgr.get_surface(surface, qh, ());
            self.surfaces.insert(wl_id.clone(), (cm_surf, space));
        }

        self.drop_pending_for_surface(&wl_id);
        let creator = mgr.create_parametric_creator(qh, ());
        creator.set_primaries_named(primaries);
        creator.set_tf_named(tf);
        let desc_id = self.next_desc_id;
        self.next_desc_id = self.next_desc_id.wrapping_add(1).max(1);
        let desc = creator.create(qh, desc_id);
        self.pending_descs.insert(
            desc_id,
            PendingImageDesc {
                wl_surface_id: wl_id,
                desc,
            },
        );
    }

    fn drop_pending_for_surface(&mut self, wl_id: &ObjectId) {
        let stale: Vec<u64> = self
            .pending_descs
            .iter()
            .filter(|(_, p)| &p.wl_surface_id == wl_id)
            .map(|(k, _)| *k)
            .collect();
        for k in stale {
            if let Some(p) = self.pending_descs.remove(&k) {
                p.desc.destroy();
            }
        }
    }

    /// Destroys surface-attached color management objects BEFORE `wl_surface` is destroyed.
    pub(crate) fn clear_surface(&mut self, wl_id: &ObjectId) {
        self.drop_pending_for_surface(wl_id);
        if let Some((cm_surf, _)) = self.surfaces.remove(wl_id) {
            cm_surf.destroy();
        }
    }

    pub(crate) fn clear_all(&mut self) {
        for (_, p) in self.pending_descs.drain() {
            p.desc.destroy();
        }
        for (_, (cm_surf, _)) in self.surfaces.drain() {
            cm_surf.destroy();
        }
    }
}

const fn map_color_space(space: ColorSpace) -> Option<(CmPrimaries, CmTf)> {
    match space {
        ColorSpace::Default => None,
        ColorSpace::Srgb => Some((CmPrimaries::Srgb, CmTf::Srgb)),
        ColorSpace::DisplayP3 => Some((CmPrimaries::DisplayP3, CmTf::Srgb)),
        ColorSpace::Rec2020Pq => Some((CmPrimaries::Bt2020, CmTf::St2084Pq)),
        ColorSpace::LinearSrgb => Some((CmPrimaries::Srgb, CmTf::ExtLinear)),
    }
}

impl Dispatch<WpColorManagerV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &WpColorManagerV1,
        event: wp_color_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wp_color_manager_v1::Event::SupportedFeature { feature } => {
                if matches!(feature, WEnum::Value(CmFeature::Parametric)) {
                    state.color_management.supports_parametric = true;
                }
            }
            wp_color_manager_v1::Event::SupportedPrimariesNamed {
                primaries: WEnum::Value(p),
            } => {
                state.color_management.supported_primaries.insert(p as u32);
            }
            wp_color_manager_v1::Event::SupportedTfNamed {
                tf: WEnum::Value(t),
            } => {
                state.color_management.supported_tfs.insert(t as u32);
            }
            _ => {}
        }
    }
}

impl Dispatch<WpColorManagementSurfaceV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WpColorManagementSurfaceV1,
        _event: wp_color_management_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpColorManagementOutputV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WpColorManagementOutputV1,
        _event: wp_color_management_output_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpImageDescriptionCreatorParamsV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WpImageDescriptionCreatorParamsV1,
        _event: wp_image_description_creator_params_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpImageDescriptionV1, u64> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        desc_id: &u64,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_v1::Event::Ready { .. }
            | wp_image_description_v1::Event::Ready2 { .. } => {
                if let Some(pending) = state.color_management.pending_descs.remove(desc_id) {
                    if let Some((cm_surf, _)) =
                        state.color_management.surfaces.get(&pending.wl_surface_id)
                    {
                        cm_surf.set_image_description(&pending.desc, CmRenderIntent::Perceptual);
                        if let Some(&rk) = state.surface_by_obj.get(&pending.wl_surface_id)
                            && let Some(live) = state.surfaces.get(&rk)
                        {
                            live.role.wl_surface().commit();
                        }
                    }
                    pending.desc.destroy();
                }
            }
            wp_image_description_v1::Event::Failed { .. } => {
                if let Some(pending) = state.color_management.pending_descs.remove(desc_id) {
                    pending.desc.destroy();
                }
            }
            _ => {}
        }
    }
}
