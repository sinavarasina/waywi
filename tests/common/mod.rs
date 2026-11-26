//! Shared mock Wayland compositor utilities used across socketpair wire-protocol integration tests.

#![allow(dead_code)]

use std::collections::HashMap;
use std::os::fd::OwnedFd;

use wayland_server::backend::{ClientData, ClientId, DisconnectReason, ObjectId};
use wayland_server::protocol::wl_output;
use wayland_server::{DataInit, New, Resource};

pub struct DummyClientData;

impl ClientData for DummyClientData {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

pub struct PoolInfo {
    pub fd: OwnedFd,
}

pub struct BufferInfo {
    pub pool_id: ObjectId,
    pub offset: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

/// Initializes a standard 1920x1080@60Hz `eDP-1` mock `wl_output` resource.
pub fn bind_mock_output<D: wayland_server::Dispatch<wl_output::WlOutput, ()> + 'static>(
    resource: New<wl_output::WlOutput>,
    data_init: &mut DataInit<'_, D>,
) -> wl_output::WlOutput {
    let out = data_init.init(resource, ());
    out.geometry(
        0,
        0,
        340,
        190,
        wl_output::Subpixel::Unknown,
        "Mock".into(),
        "Panel".into(),
        wl_output::Transform::Normal,
    );
    out.mode(
        wl_output::Mode::Current | wl_output::Mode::Preferred,
        1920,
        1080,
        60_000,
    );
    if out.version() >= 2 {
        out.scale(1);
    }
    if out.version() >= 4 {
        out.name("eDP-1".into());
    }
    if out.version() >= 2 {
        out.done();
    }
    out
}

/// Writes synthetic 4-byte-per-pixel data into a client-supplied `wl_shm` buffer's backing `memfd`.
pub fn write_shm_buffer(
    pools: &HashMap<ObjectId, PoolInfo>,
    buffers: &HashMap<ObjectId, BufferInfo>,
    buf_id: &ObjectId,
    pixel_fn: impl Fn(u32, u32) -> [u8; 4],
) {
    let Some(b) = buffers.get(buf_id) else {
        return;
    };
    let Some(p) = pools.get(&b.pool_id) else {
        return;
    };
    let total = b.offset + (b.stride as usize) * (b.height as usize);
    // SAFETY: `p.fd` is a valid client-provided SHM memfd of at least `total` bytes.
    if let Ok(mut mmap) = unsafe { memmap2::MmapOptions::new().len(total).map_mut(&p.fd) } {
        for y in 0..b.height {
            for x in 0..b.width {
                let off = b.offset + (y as usize) * (b.stride as usize) + (x as usize) * 4;
                if off + 4 <= mmap.len() {
                    mmap[off..off + 4].copy_from_slice(&pixel_fn(x, y));
                }
            }
        }
    }
}

/// Declares a no-op `GlobalDispatch<$iface, ()>` binding straight into `di.init(r, ())`.
#[macro_export]
macro_rules! impl_simple_global {
    ($state:ty, $iface:ty) => {
        impl wayland_server::GlobalDispatch<$iface, ()> for $state {
            fn bind(
                _: &mut Self,
                _: &wayland_server::DisplayHandle,
                _: &wayland_server::Client,
                r: wayland_server::New<$iface>,
                _: &(),
                di: &mut wayland_server::DataInit<'_, Self>,
            ) {
                di.init(r, ());
            }
        }
    };
}

/// Declares a no-op `Dispatch<$iface, ()>` ignoring incoming client requests.
#[macro_export]
macro_rules! impl_noop_dispatch {
    ($state:ty, $iface:ty) => {
        impl wayland_server::Dispatch<$iface, ()> for $state {
            fn request(
                _: &mut Self,
                _: &wayland_server::Client,
                _: &$iface,
                _: <$iface as wayland_server::Resource>::Request,
                _: &(),
                _: &wayland_server::DisplayHandle,
                _: &mut wayland_server::DataInit<'_, Self>,
            ) {
            }
        }
    };
}
