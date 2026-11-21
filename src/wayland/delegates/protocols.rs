//! Low-level `wayland-client::Dispatch` implementations for SHM pools/buffers and wp protocols.

use wayland_client::protocol::{wl_buffer, wl_shm_pool};
use wayland_client::{Connection, Dispatch, QueueHandle};

#[cfg(feature = "fractional-scale")]
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
#[cfg(feature = "fractional-scale")]
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};

#[cfg(feature = "fractional-scale")]
use crate::geometry::ScaleFactor;
#[cfg(feature = "fractional-scale")]
use crate::reconcile::RetainedSurfaceKey;
use crate::wayland::connection::WaylandState;
use crate::wayland::shm_pool::BufferSlotUserData;
use crate::wayland::surface_state::WaylandEvent;

macro_rules! noop_dispatch {
    ($proxy:ty, $event:ty) => {
        impl Dispatch<$proxy, ()> for WaylandState {
            fn event(
                _state: &mut Self,
                _proxy: &$proxy,
                _event: $event,
                _data: &(),
                _conn: &Connection,
                _qh: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

noop_dispatch!(wl_shm_pool::WlShmPool, wl_shm_pool::Event);

impl Dispatch<wl_buffer::WlBuffer, BufferSlotUserData> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        data: &BufferSlotUserData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event
            && let Some(surf) = state.surfaces.get_mut(&data.key)
            && surf.pool.mark_released(data.slot, data.generation)
        {
            if surf.pending_redraw {
                state
                    .events
                    .push_back(WaylandEvent::BufferReleased { key: data.key });
            } else if surf.spec.buffer_retention
                == crate::surface::BufferRetention::ReleaseAfterCommit
                && surf.pool.all_idle()
            {
                surf.pool.shrink_and_unmap();
            }
        }
    }
}

#[cfg(feature = "fractional-scale")]
noop_dispatch!(wp_viewporter::WpViewporter, wp_viewporter::Event);
#[cfg(feature = "fractional-scale")]
noop_dispatch!(wp_viewport::WpViewport, wp_viewport::Event);
#[cfg(feature = "fractional-scale")]
noop_dispatch!(
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_manager_v1::Event
);

#[cfg(feature = "fractional-scale")]
impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, RetainedSurfaceKey> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        key: &RetainedSurfaceKey,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(surf) = state.surfaces.get_mut(key)
        {
            let new_scale = ScaleFactor::from_120ths(scale);
            if surf.scale != new_scale {
                surf.scale = new_scale;
                surf.pending_redraw = true;
                state.events.push_back(WaylandEvent::SurfaceScaleChanged {
                    key: *key,
                    scale: new_scale,
                });
            }
        }
    }
}
