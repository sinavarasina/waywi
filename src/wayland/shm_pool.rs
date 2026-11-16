use std::ffi::CString;
use std::os::fd::{AsFd, OwnedFd};

use memmap2::MmapMut;
use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool};

use crate::error::{Result, WaywiError};
use crate::geometry::BufferRect;
use crate::reconcile::RetainedSurfaceKey;
use crate::wayland::connection::WaylandState;

fn align_64(n: usize) -> usize {
    (n + 63) & !63
}

/// User data attached to every `wl_buffer` created from [`SurfaceShmPool`].
///
/// `generation` identifies a specific buffer incarnation of `slot`: when a mismatched buffer is
/// destroyed and its slot index is reused, in-flight or stale `wl_buffer.release` events carrying
/// the old generation must not free the replacement buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BufferSlotUserData {
    pub(crate) key: RetainedSurfaceKey,
    pub(crate) slot: usize,
    pub(crate) generation: u64,
}

#[derive(Debug)]
struct ShmBufferSlot {
    buffer: wl_buffer::WlBuffer,
    offset: usize,
    width: u32,
    height: u32,
    stride: u32,
    generation: u64,
    busy: bool,
    needs_full_redraw: bool,
    pending_damage: Vec<BufferRect>,
}

/// Result of attempting to acquire an SHM buffer slot for drawing.
pub(crate) enum AcquireSlot<'a> {
    Ready {
        slot: usize,
        buffer: wl_buffer::WlBuffer,
        generation: u64,
        pixels: &'a mut [u8],
        stride: u32,
        damage: Vec<BufferRect>,
    },
    AllBusy,
}

/// Reusable `memfd_create` + `wl_shm_pool` with on-demand single-or-double buffering; the memfd is
/// closed (never truncated) when hidden/destroyed so compositor-side mappings stay valid.
#[derive(Debug, Default)]
pub(crate) struct SurfaceShmPool {
    fd: Option<OwnedFd>,
    mmap: Option<MmapMut>,
    wl_pool: Option<wl_shm_pool::WlShmPool>,
    pool_bytes: usize,
    slots: [Option<ShmBufferSlot>; 2],
    next_generation: u64,
}

impl SurfaceShmPool {
    pub(crate) fn notify_damage(&mut self, damage: &[BufferRect], full_redraw: bool) {
        for slot in self.slots.iter_mut().flatten() {
            if full_redraw {
                slot.needs_full_redraw = true;
                slot.pending_damage.clear();
            } else if !slot.needs_full_redraw {
                slot.pending_damage.extend_from_slice(damage);
            }
        }
    }

    /// Frees a committed slot when the compositor reports `wl_buffer.release` for the exact
    /// buffer incarnation still owned by that slot; stale generations are ignored. Returns
    /// `true` when the slot was actually freed.
    pub(crate) fn mark_released(&mut self, slot: usize, generation: u64) -> bool {
        if let Some(Some(s)) = self.slots.get_mut(slot)
            && s.generation == generation
        {
            s.busy = false;
            return true;
        }
        false
    }

    pub(crate) fn mark_committed(&mut self, slot: usize, generation: u64) {
        if let Some(Some(s)) = self.slots.get_mut(slot)
            && s.generation == generation
        {
            s.busy = true;
            s.needs_full_redraw = false;
            s.pending_damage.clear();
        }
    }

    /// Destroys pool/buffers, drops mappings, and closes the memfd when hidden/removed. The file is
    /// never truncated: a compositor may still map/use released buffers, and shrinking it under
    /// that mapping causes SIGBUS in the compositor.
    pub(crate) fn shrink_and_unmap(&mut self) {
        for slot in &mut self.slots {
            if let Some(s) = slot.take() {
                s.buffer.destroy();
            }
        }
        self.mmap = None;
        if let Some(pool) = self.wl_pool.take() {
            pool.destroy();
        }
        drop(self.fd.take());
        self.pool_bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn current_pool_bytes(&self) -> usize {
        self.pool_bytes
    }

    #[cfg(test)]
    pub(crate) fn allocated_slot_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    pub(crate) fn acquire_for_draw(
        &mut self,
        width: u32,
        height: u32,
        surface_key: RetainedSurfaceKey,
        wl_shm: &wl_shm::WlShm,
        qh: &QueueHandle<WaylandState>,
    ) -> Result<AcquireSlot<'_>> {
        let width = width.max(1);
        let height = height.max(1);
        let stride = width.checked_mul(4).ok_or_else(|| {
            WaywiError::WaylandConnection("surface buffer stride overflow".into())
        })?;
        let slot_bytes = align_64((stride as usize).checked_mul(height as usize).ok_or_else(
            || WaywiError::WaylandConnection("surface buffer byte size overflow".into()),
        )?);

        let slot0_busy = self.slots[0].as_ref().is_some_and(|s| s.busy);
        let slot1_busy = self.slots[1].as_ref().is_some_and(|s| s.busy);

        let target_idx = if !slot0_busy && !slot1_busy {
            // Both idle: if slot 0 mismatches dimensions, compact cleanly back to single buffer.
            let mismatch = self.slots[0]
                .as_ref()
                .is_some_and(|s| s.width != width || s.height != height);
            if mismatch {
                if let Some(s) = self.slots[0].take() {
                    s.buffer.destroy();
                }
                if let Some(s) = self.slots[1].take() {
                    s.buffer.destroy();
                }
            }
            0
        } else if !slot0_busy {
            let mismatch = self.slots[0]
                .as_ref()
                .is_some_and(|s| s.width != width || s.height != height);
            if !mismatch {
                0
            } else {
                // slot1 is busy at some offset; let's check where slot0 can safely fit or wait
                return Ok(AcquireSlot::AllBusy);
            }
        } else if !slot1_busy {
            let mismatch = self.slots[1]
                .as_ref()
                .is_some_and(|s| s.width != width || s.height != height);
            if mismatch && let Some(s) = self.slots[1].take() {
                s.buffer.destroy();
            }
            1
        } else {
            return Ok(AcquireSlot::AllBusy);
        };

        let offset = if target_idx == 0 {
            0
        } else {
            self.slots[0]
                .as_ref()
                .map(|s| align_64(s.offset + (s.stride as usize) * (s.height as usize)))
                .unwrap_or(slot_bytes)
        };
        let required_bytes = offset + slot_bytes;

        self.ensure_capacity(required_bytes, wl_shm, qh)?;

        if self.slots[target_idx].is_none() {
            let pool = self.wl_pool.as_ref().ok_or_else(|| {
                WaywiError::WaylandConnection("wl_shm_pool missing after resize".into())
            })?;
            self.next_generation += 1;
            let generation = self.next_generation;
            let buffer = pool.create_buffer(
                offset as i32,
                width as i32,
                height as i32,
                stride as i32,
                wl_shm::Format::Argb8888,
                qh,
                BufferSlotUserData {
                    key: surface_key,
                    slot: target_idx,
                    generation,
                },
            );
            self.slots[target_idx] = Some(ShmBufferSlot {
                buffer,
                offset,
                width,
                height,
                stride,
                generation,
                busy: false,
                needs_full_redraw: true,
                pending_damage: Vec::new(),
            });
        }

        let slot = self.slots[target_idx].as_mut().unwrap();
        let buffer = slot.buffer.clone();
        let damage = if slot.needs_full_redraw {
            vec![BufferRect {
                x: 0,
                y: 0,
                width: width as i32,
                height: height as i32,
            }]
        } else {
            slot.pending_damage.clone()
        };
        let byte_len = (slot.stride as usize) * (slot.height as usize);
        let slice_start = slot.offset;
        let slice_end = slice_start + byte_len;
        let mmap = self.mmap.as_mut().ok_or_else(|| {
            WaywiError::WaylandConnection("shm mmap missing after acquire".into())
        })?;

        Ok(AcquireSlot::Ready {
            slot: target_idx,
            buffer,
            generation: slot.generation,
            pixels: &mut mmap[slice_start..slice_end],
            stride,
            damage,
        })
    }

    fn ensure_capacity(
        &mut self,
        required_bytes: usize,
        wl_shm: &wl_shm::WlShm,
        qh: &QueueHandle<WaylandState>,
    ) -> Result<()> {
        if self.fd.is_none() {
            let name = CString::new("waywi-shm").unwrap();
            let fd = memfd_create(&name, MemfdFlags::CLOEXEC)
                .map_err(|e| WaywiError::WaylandConnection(format!("memfd_create failed: {e}")))?;
            self.fd = Some(fd);
        }
        let fd = self.fd.as_ref().unwrap();

        if required_bytes > self.pool_bytes || self.mmap.is_none() {
            let new_bytes = required_bytes.max(self.pool_bytes).max(4096);
            ftruncate(fd, new_bytes as u64).map_err(|e| {
                WaywiError::WaylandConnection(format!("memfd ftruncate failed: {e}"))
            })?;
            // Safety: file descriptor stays valid as OwnedFd owned by `self.fd` and is mapped
            // exclusively into this process and the compositor's shared SHM mapping.
            let mmap = unsafe { MmapMut::map_mut(fd) }
                .map_err(|e| WaywiError::WaylandConnection(format!("mmap failed: {e}")))?;
            if let Some(pool) = &self.wl_pool {
                if new_bytes > self.pool_bytes {
                    pool.resize(new_bytes as i32);
                }
            } else {
                let pool = wl_shm.create_pool(fd.as_fd(), new_bytes as i32, qh, ());
                self.wl_pool = Some(pool);
            }
            self.mmap = Some(mmap);
            self.pool_bytes = new_bytes;
        } else if self.wl_pool.is_none() {
            let pool = wl_shm.create_pool(fd.as_fd(), self.pool_bytes as i32, qh, ());
            self.wl_pool = Some(pool);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use smithay_client_toolkit::globals::ProvidesBoundGlobal;
    use wayland_client::Connection;
    use wayland_server::backend::ClientData;
    use wayland_server::protocol::{wl_buffer, wl_compositor, wl_shm, wl_shm_pool};
    use wayland_server::{Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New};

    use super::*;
    use crate::surface::SurfaceKey;

    struct DummyClient;
    impl ClientData for DummyClient {
        fn initialized(&self, _: wayland_server::backend::ClientId) {}
        fn disconnected(
            &self,
            _: wayland_server::backend::ClientId,
            _: wayland_server::backend::DisconnectReason,
        ) {
        }
    }

    struct ShmServer;
    impl GlobalDispatch<wl_compositor::WlCompositor, ()> for ShmServer {
        fn bind(
            _: &mut Self,
            _: &DisplayHandle,
            _: &Client,
            r: New<wl_compositor::WlCompositor>,
            _: &(),
            di: &mut DataInit<'_, Self>,
        ) {
            di.init(r, ());
        }
    }
    impl Dispatch<wl_compositor::WlCompositor, ()> for ShmServer {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_compositor::WlCompositor,
            _: wl_compositor::Request,
            _: &(),
            _: &DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
        }
    }
    impl GlobalDispatch<wl_shm::WlShm, ()> for ShmServer {
        fn bind(
            _: &mut Self,
            _: &DisplayHandle,
            _: &Client,
            r: New<wl_shm::WlShm>,
            _: &(),
            di: &mut DataInit<'_, Self>,
        ) {
            let shm = di.init(r, ());
            shm.format(wl_shm::Format::Argb8888);
        }
    }
    impl Dispatch<wl_shm::WlShm, ()> for ShmServer {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_shm::WlShm,
            req: wl_shm::Request,
            _: &(),
            _: &DisplayHandle,
            di: &mut DataInit<'_, Self>,
        ) {
            if let wl_shm::Request::CreatePool { id, .. } = req {
                di.init(id, ());
            }
        }
    }
    impl Dispatch<wl_shm_pool::WlShmPool, ()> for ShmServer {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_shm_pool::WlShmPool,
            req: wl_shm_pool::Request,
            _: &(),
            _: &DisplayHandle,
            di: &mut DataInit<'_, Self>,
        ) {
            if let wl_shm_pool::Request::CreateBuffer { id, .. } = req {
                di.init(id, ());
            }
        }
    }
    impl Dispatch<wl_buffer::WlBuffer, ()> for ShmServer {
        fn request(
            _: &mut Self,
            _: &Client,
            _: &wl_buffer::WlBuffer,
            _: wl_buffer::Request,
            _: &(),
            _: &DisplayHandle,
            _: &mut DataInit<'_, Self>,
        ) {
        }
    }

    #[test]
    fn shm_pool_single_to_double_buffer_on_demand_and_shrink_on_hide() {
        let (srv_sock, cli_sock) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);

        let srv_thread = std::thread::spawn(move || {
            let mut display: Display<ShmServer> = Display::new().unwrap();
            let mut dh = display.handle();
            dh.create_global::<ShmServer, wl_compositor::WlCompositor, _>(4, ());
            dh.create_global::<ShmServer, wl_shm::WlShm, _>(1, ());
            dh.insert_client(srv_sock, Arc::new(DummyClient)).unwrap();
            let mut s = ShmServer;
            while !stop2.load(Ordering::Relaxed) {
                let _ = display.dispatch_clients(&mut s);
                let _ = display.flush_clients();
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let conn = Connection::from_socket(cli_sock).unwrap();
        let (state, _queue, qh) = WaylandState::init(&conn).unwrap();
        let wl_shm = state.shm.bound_global().unwrap();

        let key = RetainedSurfaceKey {
            surface_key: SurfaceKey::from_hashable(&"test"),
            output_id: None,
        };
        let mut pool = SurfaceShmPool::default();
        assert_eq!(pool.allocated_slot_count(), 0);

        let AcquireSlot::Ready {
            slot: slot0,
            generation: gen0,
            ..
        } = pool.acquire_for_draw(64, 32, key, &wl_shm, &qh).unwrap()
        else {
            panic!("expected slot 0 ready");
        };
        assert_eq!(slot0, 0);
        assert_eq!(pool.allocated_slot_count(), 1);
        assert!(pool.current_pool_bytes() >= 64 * 32 * 4);
        pool.mark_committed(slot0, gen0);

        // Slot 0 is still busy -> allocates slot 1 on-demand.
        let AcquireSlot::Ready {
            slot: slot1,
            generation: gen1,
            ..
        } = pool.acquire_for_draw(64, 32, key, &wl_shm, &qh).unwrap()
        else {
            panic!("expected slot 1 ready");
        };
        assert_eq!(slot1, 1);
        assert_eq!(pool.allocated_slot_count(), 2);
        pool.mark_committed(slot1, gen1);

        assert!(matches!(
            pool.acquire_for_draw(64, 32, key, &wl_shm, &qh).unwrap(),
            AcquireSlot::AllBusy
        ));

        pool.mark_released(slot0, gen0);
        assert!(matches!(
            pool.acquire_for_draw(64, 32, key, &wl_shm, &qh).unwrap(),
            AcquireSlot::Ready { slot: 0, .. }
        ));

        // Re-acquiring a released (still alive) slot reuses the same buffer generation.
        let AcquireSlot::Ready {
            slot: slot0b,
            generation: gen0b,
            ..
        } = pool.acquire_for_draw(64, 32, key, &wl_shm, &qh).unwrap()
        else {
            panic!("expected slot 0 ready");
        };
        assert_eq!(slot0b, 0);
        assert_eq!(gen0b, gen0);
        pool.mark_committed(slot0, gen0b);
        pool.mark_released(slot0, gen0b);
        pool.mark_released(slot1, gen1);

        // A dimension mismatch destroys both buffers and recreates slot 0 with a new generation.
        let AcquireSlot::Ready {
            slot: slot2,
            generation: gen2,
            ..
        } = pool.acquire_for_draw(96, 24, key, &wl_shm, &qh).unwrap()
        else {
            panic!("expected resized slot 0 ready");
        };
        assert_ne!(gen2, gen0);
        pool.mark_committed(slot2, gen2);

        let AcquireSlot::Ready {
            slot: slot3,
            generation: gen3,
            ..
        } = pool.acquire_for_draw(96, 24, key, &wl_shm, &qh).unwrap()
        else {
            panic!("expected slot 1 ready");
        };
        assert_eq!(slot3, 1);
        pool.mark_committed(slot3, gen3);

        assert!(matches!(
            pool.acquire_for_draw(96, 24, key, &wl_shm, &qh).unwrap(),
            AcquireSlot::AllBusy
        ));
        // A stale release for a destroyed buffer incarnation must not free the recycled slot.
        pool.mark_released(slot2, gen0);
        assert!(matches!(
            pool.acquire_for_draw(96, 24, key, &wl_shm, &qh).unwrap(),
            AcquireSlot::AllBusy
        ));
        pool.mark_released(slot2, gen2);
        assert!(matches!(
            pool.acquire_for_draw(96, 24, key, &wl_shm, &qh).unwrap(),
            AcquireSlot::Ready { slot: 0, .. }
        ));

        pool.shrink_and_unmap();
        assert_eq!(pool.current_pool_bytes(), 0);
        assert_eq!(pool.allocated_slot_count(), 0);

        stop.store(true, Ordering::Relaxed);
        let _ = srv_thread.join();
    }
}
