//! Core Wayland `wl_data_device_manager` Drag & Drop backend (incoming & outgoing).
//!
//! All pipe I/O runs directly on WayWi's single `calloop` event loop via non-blocking
//! [`calloop::generic::Generic`] FD sources paired with bounded [`calloop::timer::Timer`]
//! deadlines — zero auxiliary OS threads and full Wayland v3 protocol compliance
//! (`wl_data_offer.finish()` is sent strictly after a clean bounded EOF, never before).

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{self, DndAction as ProtoDndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_seat, wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};

use crate::geometry::{LogicalPx, Point};
use crate::input::{DndAction, DndEvent};
use crate::reconcile::RetainedSurfaceKey;
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;

const MAX_DND_BYTES: usize = 16 * 1024 * 1024;
const PIPE_TIMEOUT: Duration = Duration::from_secs(3);

const DEFAULT_MIME_PREF: [&str; 4] = [
    "text/uri-list",
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
];

struct OfferState {
    proxy: WlDataOffer,
    mime_types: Vec<String>,
    source_actions: DndAction,
    selected_action: DndAction,
}

struct ActiveDropTarget {
    key: RetainedSurfaceKey,
    offer_id: ObjectId,
    position: Point,
    chosen_mime: String,
}

struct OutgoingDrag {
    proxy: WlDataSource,
    payloads: HashMap<String, Arc<[u8]>>,
}

struct PendingDndRead {
    key: RetainedSurfaceKey,
    position: Point,
    mime_type: String,
    action: DndAction,
    offer: Option<WlDataOffer>,
    buf: Vec<u8>,
    fd_token: Option<RegistrationToken>,
    timer_token: Option<RegistrationToken>,
}

struct PendingDndWrite {
    payload: Arc<[u8]>,
    offset: usize,
    fd_token: Option<RegistrationToken>,
    timer_token: Option<RegistrationToken>,
}

/// Manages `wl_data_device_manager` / `wl_data_device` Drag & Drop state on the main `calloop`.
pub(crate) struct WaylandDndState {
    manager: Option<WlDataDeviceManager>,
    devices: HashMap<ObjectId, WlDataDevice>,
    offers: HashMap<ObjectId, OfferState>,
    active_target: Option<ActiveDropTarget>,
    /// Offer rejected during `enter` (either surface has no `on_dnd` or no MIME matched), kept so
    /// it can be destroyed cleanly on `leave` without leaking protocol IDs.
    rejected_enter_offer: Option<ObjectId>,
    outgoing: HashMap<ObjectId, OutgoingDrag>,
    loop_handle: Option<LoopHandle<'static, WaylandState>>,
    next_io_id: u64,
    pending_reads: HashMap<u64, PendingDndRead>,
    pending_writes: HashMap<u64, PendingDndWrite>,
}

impl Drop for WaylandDndState {
    fn drop(&mut self) {
        self.cleanup();
        for (_, o) in self.offers.drain() {
            o.proxy.destroy();
        }
        for (_, src) in self.outgoing.drain() {
            src.proxy.destroy();
        }
        for (_, dev) in self.devices.drain() {
            if dev.version() >= 2 {
                dev.release();
            }
        }
    }
}

impl WaylandDndState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<WlDataDeviceManager, _, _>(qh, 1..=3, ())
            .ok();
        Self {
            manager,
            devices: HashMap::new(),
            offers: HashMap::new(),
            active_target: None,
            rejected_enter_offer: None,
            outgoing: HashMap::new(),
            loop_handle: None,
            next_io_id: 1,
            pending_reads: HashMap::new(),
            pending_writes: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn set_loop_handle(&mut self, handle: LoopHandle<'static, WaylandState>) {
        self.loop_handle = Some(handle);
    }

    /// Unregisters all in-flight non-blocking DnD pipe sources & timers and destroys incomplete
    /// offers (without sending `finish()`).
    pub(crate) fn cleanup(&mut self) {
        let handle = self.loop_handle.take();
        for (_, mut read) in self.pending_reads.drain() {
            if let Some(ref h) = handle {
                if let Some(tok) = read.fd_token.take() {
                    h.remove(tok);
                }
                if let Some(tok) = read.timer_token.take() {
                    h.remove(tok);
                }
            }
            if let Some(offer) = read.offer.take() {
                offer.destroy();
            }
        }
        for (_, mut write) in self.pending_writes.drain() {
            if let Some(ref h) = handle {
                if let Some(tok) = write.fd_token.take() {
                    h.remove(tok);
                }
                if let Some(tok) = write.timer_token.take() {
                    h.remove(tok);
                }
            }
        }
    }

    fn alloc_io_id(&mut self) -> u64 {
        let id = self.next_io_id;
        self.next_io_id = self.next_io_id.wrapping_add(1).max(1);
        id
    }

    pub(crate) fn add_seat(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<WaylandState>) {
        let Some(ref mgr) = self.manager else {
            return;
        };
        let sid = seat.id();
        self.devices
            .entry(sid)
            .or_insert_with(|| mgr.get_data_device(seat, qh, ()));
    }

    pub(crate) fn remove_seat(&mut self, seat: &wl_seat::WlSeat) {
        if let Some(dev) = self.devices.remove(&seat.id())
            && dev.version() >= 2
        {
            dev.release();
        }
    }

    pub(crate) fn clear_surface(&mut self, key: RetainedSurfaceKey) {
        if self.active_target.as_ref().is_some_and(|t| t.key == key)
            && let Some(target) = self.active_target.take()
            && let Some(offer) = self.offers.remove(&target.offer_id)
        {
            offer.proxy.destroy();
        }
        let matching_reads: Vec<u64> = self
            .pending_reads
            .iter()
            .filter(|(_, r)| r.key == key)
            .map(|(&id, _)| id)
            .collect();
        for id in matching_reads {
            if let Some(mut read) = self.pending_reads.remove(&id) {
                if let Some(ref h) = self.loop_handle {
                    if let Some(tok) = read.fd_token.take() {
                        h.remove(tok);
                    }
                    if let Some(tok) = read.timer_token.take() {
                        h.remove(tok);
                    }
                }
                if let Some(offer) = read.offer.take() {
                    offer.destroy();
                }
            }
        }
    }

    pub(crate) fn start_drag(
        &mut self,
        surface: &wl_surface::WlSurface,
        seat: &wl_seat::WlSeat,
        serial: u32,
        entries: Vec<(String, Vec<u8>)>,
        actions: DndAction,
        qh: &QueueHandle<WaylandState>,
    ) -> bool {
        let Some(ref mgr) = self.manager else {
            return false;
        };
        let Some(dev) = self.devices.get(&seat.id()) else {
            return false;
        };
        let source = mgr.create_data_source(qh, ());
        let mut payloads = HashMap::with_capacity(entries.len());
        for (mime, data) in entries {
            if mime.is_empty() || mime.contains('\0') {
                continue;
            }
            source.offer(mime.clone());
            payloads.insert(mime, Arc::from(data));
        }
        if payloads.is_empty() {
            source.destroy();
            return false;
        }
        if source.version() >= 3 {
            source.set_actions(to_proto_action(actions));
        }
        dev.start_drag(Some(&source), surface, None, serial);
        let oid = source.id();
        self.outgoing.insert(
            oid,
            OutgoingDrag {
                proxy: source,
                payloads,
            },
        );
        true
    }
}

fn from_proto_action(a: ProtoDndAction) -> DndAction {
    let mut out = DndAction::empty();
    if a.contains(ProtoDndAction::Copy) {
        out |= DndAction::COPY;
    }
    if a.contains(ProtoDndAction::Move) {
        out |= DndAction::MOVE;
    }
    if a.contains(ProtoDndAction::Ask) {
        out |= DndAction::ASK;
    }
    out
}

fn to_proto_action(a: DndAction) -> ProtoDndAction {
    let mut out = ProtoDndAction::None;
    if a.contains(DndAction::COPY) {
        out |= ProtoDndAction::Copy;
    }
    if a.contains(DndAction::MOVE) {
        out |= ProtoDndAction::Move;
    }
    if a.contains(DndAction::ASK) {
        out |= ProtoDndAction::Ask;
    }
    out
}

/// Selects the best accepted MIME type for an incoming drag offer.
///
/// - When `preferred` is non-empty (set via `Surface::dnd_mime_types`), ONLY those MIME types are
///   accepted, in declared priority order; returns `None` if none match.
/// - When `preferred` is empty, matches standard text/URI defaults (`text/uri-list`,
///   `text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`) and returns `None` for unrecognized
///   binary types.
fn choose_dnd_mime(offered: &[String], preferred: &[String]) -> Option<String> {
    if offered.is_empty() {
        return None;
    }
    if !preferred.is_empty() {
        for want in preferred {
            if let Some(m) = offered.iter().find(|o| o.as_str() == want.as_str()) {
                return Some(m.clone());
            }
        }
        return None;
    }
    for want in DEFAULT_MIME_PREF {
        if let Some(m) = offered.iter().find(|o| o.as_str() == want) {
            return Some(m.clone());
        }
    }
    None
}

fn finish_dnd_read(state: &mut WaylandState, io_id: u64, result: Result<(), String>) {
    let Some(mut entry) = state.dnd.pending_reads.remove(&io_id) else {
        return;
    };
    if let Some(ref handle) = state.dnd.loop_handle
        && let Some(tok) = entry.timer_token.take()
    {
        handle.remove(tok);
    }
    let event = match result {
        Ok(()) => {
            if let Some(offer) = entry.offer.take() {
                if offer.version() >= 3 {
                    offer.finish();
                }
                offer.destroy();
            }
            DndEvent::Drop {
                position: entry.position,
                mime_type: entry.mime_type,
                data: entry.buf,
                action: entry.action,
            }
        }
        Err(message) => {
            // Protocol rule: NEVER call `finish()` on an incomplete or failed transfer.
            if let Some(offer) = entry.offer.take() {
                offer.destroy();
            }
            DndEvent::DropError {
                position: entry.position,
                message,
            }
        }
    };
    state.events.push_back(WaylandEvent::Dnd {
        key: entry.key,
        event,
    });
}

fn register_dnd_pipe_reader(
    state: &mut WaylandState,
    read_fd: OwnedFd,
    key: RetainedSurfaceKey,
    position: Point,
    mime_type: String,
    action: DndAction,
    offer: WlDataOffer,
) {
    let _ = rustix::fs::fcntl_setfl(&read_fd, rustix::fs::OFlags::NONBLOCK);
    let Some(handle) = state.dnd.loop_handle.clone() else {
        offer.destroy();
        state.events.push_back(WaylandEvent::Dnd {
            key,
            event: DndEvent::DropError {
                position,
                message: "drag-and-drop event loop handle unavailable".to_owned(),
            },
        });
        return;
    };

    let io_id = state.dnd.alloc_io_id();
    let file = std::fs::File::from(read_fd);
    let fd_source = Generic::new(file, Interest::READ, Mode::Level);
    let fd_tok = match handle.insert_source(fd_source, move |_, file_ref, wl| {
        let mut chunk = [0u8; 8192];
        loop {
            match rustix::io::read(file_ref.as_fd(), &mut chunk) {
                Ok(0) => {
                    finish_dnd_read(wl, io_id, Ok(()));
                    return Ok(PostAction::Remove);
                }
                Ok(n) => {
                    let Some(entry) = wl.dnd.pending_reads.get_mut(&io_id) else {
                        return Ok(PostAction::Remove);
                    };
                    if entry.buf.len().saturating_add(n) > MAX_DND_BYTES {
                        finish_dnd_read(
                            wl,
                            io_id,
                            Err(format!(
                                "drag-and-drop payload exceeds {MAX_DND_BYTES} byte safety limit"
                            )),
                        );
                        return Ok(PostAction::Remove);
                    }
                    entry.buf.extend_from_slice(&chunk[..n]);
                }
                Err(rustix::io::Errno::AGAIN) => return Ok(PostAction::Continue),
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => {
                    finish_dnd_read(wl, io_id, Err(format!("drag-and-drop read failed: {e}")));
                    return Ok(PostAction::Remove);
                }
            }
        }
    }) {
        Ok(tok) => tok,
        Err(e) => {
            offer.destroy();
            state.events.push_back(WaylandEvent::Dnd {
                key,
                event: DndEvent::DropError {
                    position,
                    message: format!("drag-and-drop source registration failed: {e}"),
                },
            });
            return;
        }
    };

    let timer_tok = handle
        .insert_source(Timer::from_duration(PIPE_TIMEOUT), move |_, _, wl| {
            if let Some(mut entry) = wl.dnd.pending_reads.remove(&io_id) {
                if let Some(ref h) = wl.dnd.loop_handle
                    && let Some(ftok) = entry.fd_token.take()
                {
                    h.remove(ftok);
                }
                // Protocol rule: NEVER call `finish()` on timeout.
                if let Some(o) = entry.offer.take() {
                    o.destroy();
                }
                wl.events.push_back(WaylandEvent::Dnd {
                    key: entry.key,
                    event: DndEvent::DropError {
                        position: entry.position,
                        message: "drag-and-drop pipe read timed out".to_owned(),
                    },
                });
            }
            TimeoutAction::Drop
        })
        .ok();

    state.dnd.pending_reads.insert(
        io_id,
        PendingDndRead {
            key,
            position,
            mime_type,
            action,
            offer: Some(offer),
            buf: Vec::new(),
            fd_token: Some(fd_tok),
            timer_token: timer_tok,
        },
    );
}

fn finish_dnd_write(state: &mut WaylandState, io_id: u64) {
    if let Some(mut entry) = state.dnd.pending_writes.remove(&io_id)
        && let Some(ref handle) = state.dnd.loop_handle
        && let Some(tok) = entry.timer_token.take()
    {
        handle.remove(tok);
    }
}

fn start_dnd_pipe_writer(state: &mut WaylandState, fd: OwnedFd, payload: Arc<[u8]>) {
    let _ = rustix::fs::fcntl_setfl(&fd, rustix::fs::OFlags::NONBLOCK);
    let mut offset = 0usize;

    // Fast path: most DnD payloads (URIs, text) fit inside the kernel pipe buffer and complete
    // immediately without even registering a calloop source.
    while offset < payload.len() {
        match rustix::io::write(fd.as_fd(), &payload[offset..]) {
            Ok(0) => return,
            Ok(n) => offset += n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::AGAIN) => break,
            Err(_) => return,
        }
    }
    if offset >= payload.len() {
        return;
    }

    let Some(handle) = state.dnd.loop_handle.clone() else {
        return;
    };

    let io_id = state.dnd.alloc_io_id();
    let file = std::fs::File::from(fd);
    let fd_source = Generic::new(file, Interest::WRITE, Mode::Level);
    let Ok(fd_tok) = handle.insert_source(fd_source, move |_, file_ref, wl| {
        loop {
            let Some(entry) = wl.dnd.pending_writes.get_mut(&io_id) else {
                return Ok(PostAction::Remove);
            };
            if entry.offset >= entry.payload.len() {
                finish_dnd_write(wl, io_id);
                return Ok(PostAction::Remove);
            }
            match rustix::io::write(file_ref.as_fd(), &entry.payload[entry.offset..]) {
                Ok(0) => {
                    finish_dnd_write(wl, io_id);
                    return Ok(PostAction::Remove);
                }
                Ok(n) => {
                    entry.offset += n;
                    if entry.offset >= entry.payload.len() {
                        finish_dnd_write(wl, io_id);
                        return Ok(PostAction::Remove);
                    }
                }
                Err(rustix::io::Errno::AGAIN) => return Ok(PostAction::Continue),
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => {
                    finish_dnd_write(wl, io_id);
                    return Ok(PostAction::Remove);
                }
            }
        }
    }) else {
        return;
    };

    let timer_tok = handle
        .insert_source(Timer::from_duration(PIPE_TIMEOUT), move |_, _, wl| {
            if let Some(mut entry) = wl.dnd.pending_writes.remove(&io_id)
                && let Some(ref h) = wl.dnd.loop_handle
                && let Some(ftok) = entry.fd_token.take()
            {
                h.remove(ftok);
            }
            TimeoutAction::Drop
        })
        .ok();

    state.dnd.pending_writes.insert(
        io_id,
        PendingDndWrite {
            payload,
            offset,
            fd_token: Some(fd_tok),
            timer_token: timer_tok,
        },
    );
}

impl Dispatch<WlDataDeviceManager, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WlDataDeviceManager,
        _event: wl_data_device_manager::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &WlDataDevice,
        event: wl_data_device::Event,
        _data: &(),
        conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                let oid = id.id();
                state.dnd.offers.insert(
                    oid,
                    OfferState {
                        proxy: id,
                        mime_types: Vec::new(),
                        source_actions: DndAction::COPY,
                        selected_action: DndAction::COPY,
                    },
                );
            }
            wl_data_device::Event::Enter {
                serial,
                surface,
                x,
                y,
                id,
            } => {
                // Clean up any stale rejected offer from a prior enter without leave.
                if let Some(stale) = state.dnd.rejected_enter_offer.take()
                    && let Some(old) = state.dnd.offers.remove(&stale)
                {
                    old.proxy.destroy();
                }
                let Some(offer_proxy) = id else {
                    return;
                };
                let offer_id = offer_proxy.id();
                let Some(&key) = state.surface_by_obj.get(&surface.id()) else {
                    offer_proxy.accept(serial, None);
                    state.dnd.rejected_enter_offer = Some(offer_id);
                    return;
                };
                let (accepts_dnd, preferred) = state
                    .surfaces
                    .get(&key)
                    .map(|s| (s.spec.accepts_dnd, s.spec.dnd_mime_types.clone()))
                    .unwrap_or((false, Vec::new()));

                if !accepts_dnd {
                    offer_proxy.accept(serial, None);
                    if offer_proxy.version() >= 3 {
                        offer_proxy.set_actions(ProtoDndAction::None, ProtoDndAction::None);
                    }
                    state.dnd.rejected_enter_offer = Some(offer_id);
                    return;
                }

                let position = Point::new(LogicalPx(x as f32), LogicalPx(y as f32));
                let Some(offer) = state.dnd.offers.get(&offer_id) else {
                    offer_proxy.accept(serial, None);
                    return;
                };
                let chosen = choose_dnd_mime(&offer.mime_types, &preferred);
                offer.proxy.accept(serial, chosen.clone());
                if offer.proxy.version() >= 3 {
                    if chosen.is_some() {
                        offer.proxy.set_actions(
                            ProtoDndAction::Copy | ProtoDndAction::Move,
                            ProtoDndAction::Copy,
                        );
                    } else {
                        offer
                            .proxy
                            .set_actions(ProtoDndAction::None, ProtoDndAction::None);
                    }
                }

                let Some(chosen_mime) = chosen else {
                    state.dnd.rejected_enter_offer = Some(offer_id);
                    return;
                };

                let mimes = offer.mime_types.clone();
                let actions = offer.source_actions;
                state.dnd.active_target = Some(ActiveDropTarget {
                    key,
                    offer_id,
                    position,
                    chosen_mime,
                });
                state.events.push_back(WaylandEvent::Dnd {
                    key,
                    event: DndEvent::Enter {
                        position,
                        mime_types: mimes,
                        actions,
                    },
                });
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                if let Some(ref mut target) = state.dnd.active_target {
                    let position = Point::new(LogicalPx(x as f32), LogicalPx(y as f32));
                    target.position = position;
                    let key = target.key;
                    state.events.push_back(WaylandEvent::Dnd {
                        key,
                        event: DndEvent::Motion { position },
                    });
                }
            }
            wl_data_device::Event::Leave => {
                if let Some(rejected) = state.dnd.rejected_enter_offer.take()
                    && let Some(offer) = state.dnd.offers.remove(&rejected)
                {
                    offer.proxy.destroy();
                }
                if let Some(target) = state.dnd.active_target.take() {
                    if let Some(offer) = state.dnd.offers.remove(&target.offer_id) {
                        offer.proxy.destroy();
                    }
                    state.events.push_back(WaylandEvent::Dnd {
                        key: target.key,
                        event: DndEvent::Leave,
                    });
                }
            }
            wl_data_device::Event::Drop => {
                let Some(target) = state.dnd.active_target.take() else {
                    return;
                };
                let Some(offer) = state.dnd.offers.remove(&target.offer_id) else {
                    return;
                };
                match rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC) {
                    Ok((read_fd, write_fd)) => {
                        offer
                            .proxy
                            .receive(target.chosen_mime.clone(), write_fd.as_fd());
                        let _ = conn.flush();
                        drop(write_fd);
                        register_dnd_pipe_reader(
                            state,
                            read_fd,
                            target.key,
                            target.position,
                            target.chosen_mime,
                            offer.selected_action,
                            offer.proxy,
                        );
                    }
                    Err(e) => {
                        offer.proxy.destroy();
                        state.events.push_back(WaylandEvent::Dnd {
                            key: target.key,
                            event: DndEvent::DropError {
                                position: target.position,
                                message: format!("pipe creation failed: {e}"),
                            },
                        });
                    }
                }
            }
            wl_data_device::Event::Selection { id } => {
                // Core `wl_data_device` selection offers are not used by WayWi (clipboard uses
                // data-control-v1); destroy unsolicited selection offers to avoid leaking IDs.
                if let Some(offer_proxy) = id
                    && let Some(offer) = state.dnd.offers.remove(&offer_proxy.id())
                {
                    offer.proxy.destroy();
                }
            }
            _ => {}
        }
    }

    event_created_child!(WaylandState, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &WlDataOffer,
        event: wl_data_offer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let Some(offer) = state.dnd.offers.get_mut(&proxy.id()) else {
            return;
        };
        match event {
            wl_data_offer::Event::Offer { mime_type } => {
                if !offer.mime_types.contains(&mime_type) {
                    offer.mime_types.push(mime_type);
                }
            }
            wl_data_offer::Event::SourceActions {
                source_actions: WEnum::Value(a),
            } => {
                offer.source_actions = from_proto_action(a);
            }
            wl_data_offer::Event::Action {
                dnd_action: WEnum::Value(a),
            } => {
                offer.selected_action = from_proto_action(a);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &WlDataSource,
        event: wl_data_source::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let oid = proxy.id();
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let payload = state
                    .dnd
                    .outgoing
                    .get(&oid)
                    .and_then(|src| src.payloads.get(&mime_type))
                    .cloned();
                if let Some(payload) = payload {
                    start_dnd_pipe_writer(state, fd, payload);
                }
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                if let Some(src) = state.dnd.outgoing.remove(&oid) {
                    src.proxy.destroy();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::choose_dnd_mime;

    #[test]
    fn choose_dnd_mime_respects_explicit_preferences_strictly() {
        let offered = vec![
            "application/octet-stream".to_owned(),
            "text/plain".to_owned(),
        ];
        // Explicit preference not offered -> must reject (None), never fall back to random MIME.
        assert_eq!(choose_dnd_mime(&offered, &["image/png".to_owned()]), None);
        // Explicit preference offered -> picks it.
        assert_eq!(
            choose_dnd_mime(&offered, &["image/png".to_owned(), "text/plain".to_owned()]),
            Some("text/plain".to_owned())
        );
        // Empty preferences -> matches DEFAULT_MIME_PREF, rejects unknown binary-only offers.
        assert_eq!(
            choose_dnd_mime(&offered, &[]),
            Some("text/plain".to_owned())
        );
        assert_eq!(
            choose_dnd_mime(&["application/x-custom-binary".to_owned()], &[]),
            None
        );
    }
}
