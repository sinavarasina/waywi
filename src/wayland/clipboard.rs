//! Wayland data-control clipboard backend supporting `ext-data-control-v1` and `zwlr-data-control-v1`.
//!
//! All clipboard pipe reads and writes run directly on WayWi's single `calloop` event loop via
//! non-blocking [`calloop::generic::Generic`] FD sources paired with bounded
//! [`calloop::timer::Timer`] deadlines — zero auxiliary OS threads.

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::{self, ExtDataControlManagerV1},
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::{self, ZwlrDataControlManagerV1},
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use crate::capabilities::Capability;
use crate::clipboard::{
    ClipboardEvent, ClipboardSelection, ClipboardSnapshot, select_preferred_mime,
};
use crate::error::{Result, WaywiError};
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;

const MAX_CLIPBOARD_BYTES: usize = 4 * 1024 * 1024;
const PIPE_IO_TIMEOUT: Duration = Duration::from_secs(2);

enum ManagerProxy {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

enum DeviceProxy {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

impl DeviceProxy {
    fn destroy(self) {
        match self {
            Self::Ext(d) => d.destroy(),
            Self::Wlr(d) => d.destroy(),
        }
    }
}

enum OfferProxy {
    Ext(ExtDataControlOfferV1),
    Wlr(ZwlrDataControlOfferV1),
}

impl OfferProxy {
    fn receive(&self, mime: &str, fd: std::os::fd::BorrowedFd<'_>) {
        let sanitized = sanitize_mime(mime);
        if sanitized.is_empty() {
            return;
        }
        match self {
            Self::Ext(o) => o.receive(sanitized, fd),
            Self::Wlr(o) => o.receive(sanitized, fd),
        }
    }

    fn destroy(self) {
        match self {
            Self::Ext(o) => o.destroy(),
            Self::Wlr(o) => o.destroy(),
        }
    }
}

enum SourceProxy {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

impl SourceProxy {
    fn offer(&self, mime: &str) {
        let sanitized = sanitize_mime(mime);
        if sanitized.is_empty() {
            return;
        }
        match self {
            Self::Ext(s) => s.offer(sanitized),
            Self::Wlr(s) => s.offer(sanitized),
        }
    }

    fn destroy(self) {
        match self {
            Self::Ext(s) => s.destroy(),
            Self::Wlr(s) => s.destroy(),
        }
    }
}

struct ActiveSource {
    proxy: SourceProxy,
    payloads: HashMap<String, Arc<[u8]>>,
}

struct PendingClipRead {
    selection: ClipboardSelection,
    mime_types: Vec<String>,
    primary_mime: String,
    buf: Vec<u8>,
    fd_token: Option<RegistrationToken>,
    timer_token: Option<RegistrationToken>,
}

struct PendingClipWrite {
    payload: Arc<[u8]>,
    offset: usize,
    fd_token: Option<RegistrationToken>,
    timer_token: Option<RegistrationToken>,
}

/// Manages Wayland clipboard / primary-selection watches and outbound data sources on `calloop`.
pub(crate) struct WaylandClipboardState {
    manager: Option<ManagerProxy>,
    device: Option<DeviceProxy>,
    watching: bool,
    pending_offers: HashMap<ObjectId, (OfferProxy, Vec<String>)>,
    active_clipboard_offer: Option<OfferProxy>,
    active_primary_offer: Option<OfferProxy>,
    active_sources: HashMap<ObjectId, ActiveSource>,
    loop_handle: Option<LoopHandle<'static, WaylandState>>,
    next_io_id: u64,
    pending_reads: HashMap<u64, PendingClipRead>,
    pending_writes: HashMap<u64, PendingClipWrite>,
}

impl Default for WaylandClipboardState {
    fn default() -> Self {
        Self {
            manager: None,
            device: None,
            watching: false,
            pending_offers: HashMap::new(),
            active_clipboard_offer: None,
            active_primary_offer: None,
            active_sources: HashMap::new(),
            loop_handle: None,
            next_io_id: 1,
            pending_reads: HashMap::new(),
            pending_writes: HashMap::new(),
        }
    }
}

impl Drop for WaylandClipboardState {
    fn drop(&mut self) {
        self.cleanup();
        for (_, (offer, _)) in self.pending_offers.drain() {
            offer.destroy();
        }
        if let Some(offer) = self.active_clipboard_offer.take() {
            offer.destroy();
        }
        if let Some(offer) = self.active_primary_offer.take() {
            offer.destroy();
        }
        for (_, src) in self.active_sources.drain() {
            src.proxy.destroy();
        }
        if let Some(dev) = self.device.take() {
            dev.destroy();
        }
    }
}

impl WaylandClipboardState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ExtDataControlManagerV1, _, _>(qh, 1..=1, ())
            .ok()
            .map(ManagerProxy::Ext)
            .or_else(|| {
                globals
                    .bind::<ZwlrDataControlManagerV1, _, _>(qh, 1..=2, ())
                    .ok()
                    .map(ManagerProxy::Wlr)
            });

        let mut st = Self::default();
        st.manager = manager;
        st
    }

    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn set_loop_handle(&mut self, handle: LoopHandle<'static, WaylandState>) {
        self.loop_handle = Some(handle);
    }

    fn alloc_io_id(&mut self) -> u64 {
        let id = self.next_io_id;
        self.next_io_id = self.next_io_id.wrapping_add(1).max(1);
        id
    }

    fn cancel_all_pending_reads(&mut self) {
        for (_, mut read) in self.pending_reads.drain() {
            if let Some(ref h) = self.loop_handle {
                if let Some(tok) = read.fd_token.take() {
                    h.remove(tok);
                }
                if let Some(tok) = read.timer_token.take() {
                    h.remove(tok);
                }
            }
        }
    }

    /// Unregisters all in-flight non-blocking clipboard pipe sources and timers cleanly.
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

    pub(crate) fn set_watching(
        &mut self,
        watching: bool,
        seat: Option<&wl_seat::WlSeat>,
        qh: &QueueHandle<WaylandState>,
    ) {
        self.watching = watching;
        if watching {
            self.ensure_device(seat, qh);
        } else if self.active_sources.is_empty()
            && let Some(dev) = self.device.take()
        {
            dev.destroy();
            self.cancel_all_pending_reads();
            for (_, (offer, _)) in self.pending_offers.drain() {
                offer.destroy();
            }
            if let Some(offer) = self.active_clipboard_offer.take() {
                offer.destroy();
            }
            if let Some(offer) = self.active_primary_offer.take() {
                offer.destroy();
            }
        }
    }

    fn ensure_device(
        &mut self,
        seat: Option<&wl_seat::WlSeat>,
        qh: &QueueHandle<WaylandState>,
    ) -> bool {
        if self.device.is_some() {
            return true;
        }
        let (Some(manager), Some(seat)) = (&self.manager, seat) else {
            return false;
        };
        let dev = match manager {
            ManagerProxy::Ext(mgr) => DeviceProxy::Ext(mgr.get_data_device(seat, qh, ())),
            ManagerProxy::Wlr(mgr) => DeviceProxy::Wlr(mgr.get_data_device(seat, qh, ())),
        };
        self.device = Some(dev);
        true
    }

    /// Sets or clears the Wayland selection (`ClipboardSelection::Clipboard` or `Primary`).
    pub(crate) fn set_selection(
        &mut self,
        seat: Option<&wl_seat::WlSeat>,
        selection: ClipboardSelection,
        entries: Vec<(String, Vec<u8>)>,
        qh: &QueueHandle<WaylandState>,
    ) -> Result<()> {
        if self.manager.is_none() {
            return Err(WaywiError::UnsupportedCapability(Capability::Clipboard));
        }
        if !self.ensure_device(seat, qh) {
            return Err(WaywiError::WaylandConnection(
                "no active wl_seat available for clipboard selection".to_owned(),
            ));
        }

        if entries.is_empty() {
            match self.device.as_ref().expect("device ensured above") {
                DeviceProxy::Ext(dev) => match selection {
                    ClipboardSelection::Clipboard => dev.set_selection(None),
                    ClipboardSelection::Primary => dev.set_primary_selection(None),
                },
                DeviceProxy::Wlr(dev) => match selection {
                    ClipboardSelection::Clipboard => dev.set_selection(None),
                    ClipboardSelection::Primary => {
                        if dev.version() < 2 {
                            return Err(WaywiError::UnsupportedCapability(Capability::Clipboard));
                        }
                        dev.set_primary_selection(None);
                    }
                },
            }
            return Ok(());
        }

        let Some(manager) = self.manager.as_ref() else {
            return Err(WaywiError::UnsupportedCapability(Capability::Clipboard));
        };

        let mut payloads = HashMap::new();
        let mut offered_mimes = Vec::new();
        for (mime, bytes) in entries {
            let clean = sanitize_mime(&mime);
            if clean.is_empty() {
                continue;
            }
            if !payloads.contains_key(&clean) {
                offered_mimes.push(clean.clone());
            }
            payloads.insert(clean, Arc::from(bytes));
        }
        if offered_mimes.is_empty() {
            return Err(WaywiError::InvalidState(
                "clipboard selection requires at least one valid MIME type".to_owned(),
            ));
        }

        let (source_id, source_proxy) = match manager {
            ManagerProxy::Ext(mgr) => {
                let src = mgr.create_data_source(qh, ());
                let id = src.id();
                (id, SourceProxy::Ext(src))
            }
            ManagerProxy::Wlr(mgr) => {
                let src = mgr.create_data_source(qh, ());
                let id = src.id();
                (id, SourceProxy::Wlr(src))
            }
        };

        for mime in &offered_mimes {
            source_proxy.offer(mime);
        }

        match (
            self.device.as_ref().expect("device ensured"),
            &source_proxy,
            selection,
        ) {
            (DeviceProxy::Ext(dev), SourceProxy::Ext(src), ClipboardSelection::Clipboard) => {
                dev.set_selection(Some(src));
            }
            (DeviceProxy::Ext(dev), SourceProxy::Ext(src), ClipboardSelection::Primary) => {
                dev.set_primary_selection(Some(src));
            }
            (DeviceProxy::Wlr(dev), SourceProxy::Wlr(src), ClipboardSelection::Clipboard) => {
                dev.set_selection(Some(src));
            }
            (DeviceProxy::Wlr(dev), SourceProxy::Wlr(src), ClipboardSelection::Primary) => {
                if dev.version() < 2 {
                    source_proxy.destroy();
                    return Err(WaywiError::UnsupportedCapability(Capability::Clipboard));
                }
                dev.set_primary_selection(Some(src));
            }
            _ => unreachable!("manager and device backend always match"),
        }

        self.active_sources.insert(
            source_id,
            ActiveSource {
                proxy: source_proxy,
                payloads,
            },
        );
        Ok(())
    }

    fn on_data_offer(&mut self, offer_id: ObjectId, offer: OfferProxy) {
        self.pending_offers.insert(offer_id, (offer, Vec::new()));
    }

    fn on_offer_mime(&mut self, offer_id: &ObjectId, mime: String) {
        if let Some((_, mimes)) = self.pending_offers.get_mut(offer_id)
            && !mime.is_empty()
            && !mimes.iter().any(|existing| existing == &mime)
        {
            mimes.push(mime);
        }
    }

    fn on_device_finished(&mut self) {
        self.device = None;
        self.cancel_all_pending_reads();
        for (_, (offer, _)) in self.pending_offers.drain() {
            offer.destroy();
        }
    }

    fn on_source_cancelled(&mut self, source_id: &ObjectId) {
        if let Some(entry) = self.active_sources.remove(source_id) {
            entry.proxy.destroy();
        }
    }
}

fn handle_selection(
    state: &mut WaylandState,
    conn: &Connection,
    selection: ClipboardSelection,
    offer_id: Option<ObjectId>,
) {
    let prev = match selection {
        ClipboardSelection::Clipboard => state.clipboard.active_clipboard_offer.take(),
        ClipboardSelection::Primary => state.clipboard.active_primary_offer.take(),
    };
    if let Some(p) = prev {
        p.destroy();
    }

    let Some(id) = offer_id else {
        state
            .events
            .push_back(WaylandEvent::Clipboard(ClipboardEvent::Changed(
                ClipboardSnapshot {
                    selection,
                    mime_types: Vec::new(),
                    text: None,
                    bytes: Vec::new(),
                    primary_mime: None,
                    cleared: true,
                },
            )));
        return;
    };

    let Some((offer, mime_types)) = state.clipboard.pending_offers.remove(&id) else {
        return;
    };
    let preferred = select_preferred_mime(&mime_types).map(str::to_owned);
    let Some(target_mime) = preferred else {
        match selection {
            ClipboardSelection::Clipboard => state.clipboard.active_clipboard_offer = Some(offer),
            ClipboardSelection::Primary => state.clipboard.active_primary_offer = Some(offer),
        }
        state
            .events
            .push_back(WaylandEvent::Clipboard(ClipboardEvent::Changed(
                ClipboardSnapshot {
                    selection,
                    mime_types,
                    text: None,
                    bytes: Vec::new(),
                    primary_mime: None,
                    cleared: false,
                },
            )));
        return;
    };

    let Ok((read_fd, write_fd)) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC) else {
        match selection {
            ClipboardSelection::Clipboard => state.clipboard.active_clipboard_offer = Some(offer),
            ClipboardSelection::Primary => state.clipboard.active_primary_offer = Some(offer),
        }
        state
            .events
            .push_back(WaylandEvent::Clipboard(ClipboardEvent::Error(
                "failed to create pipe for clipboard transfer".to_owned(),
            )));
        return;
    };

    offer.receive(&target_mime, write_fd.as_fd());
    let _ = conn.flush();
    drop(write_fd);
    match selection {
        ClipboardSelection::Clipboard => state.clipboard.active_clipboard_offer = Some(offer),
        ClipboardSelection::Primary => state.clipboard.active_primary_offer = Some(offer),
    }

    register_clipboard_pipe_reader(state, read_fd, selection, mime_types, target_mime);
}

fn finish_clipboard_read(
    state: &mut WaylandState,
    io_id: u64,
    result: std::result::Result<(), String>,
) {
    let Some(mut entry) = state.clipboard.pending_reads.remove(&io_id) else {
        return;
    };
    if let Some(ref handle) = state.clipboard.loop_handle
        && let Some(tok) = entry.timer_token.take()
    {
        handle.remove(tok);
    }
    let event = match result {
        Ok(()) => {
            let text = decode_text_for_mime(&entry.primary_mime, &entry.buf);
            ClipboardEvent::Changed(ClipboardSnapshot {
                selection: entry.selection,
                mime_types: entry.mime_types,
                text,
                bytes: entry.buf,
                primary_mime: Some(entry.primary_mime),
                cleared: false,
            })
        }
        Err(msg) => ClipboardEvent::Error(msg),
    };
    state.events.push_back(WaylandEvent::Clipboard(event));
}

fn register_clipboard_pipe_reader(
    state: &mut WaylandState,
    read_fd: OwnedFd,
    selection: ClipboardSelection,
    mime_types: Vec<String>,
    primary_mime: String,
) {
    let _ = rustix::fs::fcntl_setfl(&read_fd, rustix::fs::OFlags::NONBLOCK);
    let Some(handle) = state.clipboard.loop_handle.clone() else {
        state
            .events
            .push_back(WaylandEvent::Clipboard(ClipboardEvent::Error(
                "clipboard event loop handle unavailable".to_owned(),
            )));
        return;
    };

    let io_id = state.clipboard.alloc_io_id();
    let file = std::fs::File::from(read_fd);
    let fd_source = Generic::new(file, Interest::READ, Mode::Level);
    let fd_tok = match handle.insert_source(fd_source, move |_, file_ref, wl| {
        let mut chunk = [0u8; 8192];
        loop {
            match rustix::io::read(file_ref.as_fd(), &mut chunk) {
                Ok(0) => {
                    finish_clipboard_read(wl, io_id, Ok(()));
                    return Ok(PostAction::Remove);
                }
                Ok(n) => {
                    let Some(entry) = wl.clipboard.pending_reads.get_mut(&io_id) else {
                        return Ok(PostAction::Remove);
                    };
                    let remaining = MAX_CLIPBOARD_BYTES.saturating_sub(entry.buf.len());
                    if remaining == 0 {
                        finish_clipboard_read(wl, io_id, Ok(()));
                        return Ok(PostAction::Remove);
                    }
                    let take = n.min(remaining);
                    entry.buf.extend_from_slice(&chunk[..take]);
                    if take < n {
                        finish_clipboard_read(wl, io_id, Ok(()));
                        return Ok(PostAction::Remove);
                    }
                }
                Err(rustix::io::Errno::AGAIN) => return Ok(PostAction::Continue),
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => {
                    finish_clipboard_read(wl, io_id, Err(format!("clipboard read failed: {e}")));
                    return Ok(PostAction::Remove);
                }
            }
        }
    }) {
        Ok(tok) => tok,
        Err(e) => {
            state
                .events
                .push_back(WaylandEvent::Clipboard(ClipboardEvent::Error(format!(
                    "clipboard reader registration failed: {e}"
                ))));
            return;
        }
    };

    let timer_tok = handle
        .insert_source(Timer::from_duration(PIPE_IO_TIMEOUT), move |_, _, wl| {
            if let Some(mut entry) = wl.clipboard.pending_reads.remove(&io_id) {
                if let Some(ref h) = wl.clipboard.loop_handle
                    && let Some(ftok) = entry.fd_token.take()
                {
                    h.remove(ftok);
                }
                wl.events
                    .push_back(WaylandEvent::Clipboard(ClipboardEvent::Error(
                        "clipboard read timed out waiting for source application".to_owned(),
                    )));
            }
            TimeoutAction::Drop
        })
        .ok();

    state.clipboard.pending_reads.insert(
        io_id,
        PendingClipRead {
            selection,
            mime_types,
            primary_mime,
            buf: Vec::new(),
            fd_token: Some(fd_tok),
            timer_token: timer_tok,
        },
    );
}

fn finish_clipboard_write(state: &mut WaylandState, io_id: u64) {
    if let Some(mut entry) = state.clipboard.pending_writes.remove(&io_id)
        && let Some(ref handle) = state.clipboard.loop_handle
        && let Some(tok) = entry.timer_token.take()
    {
        handle.remove(tok);
    }
}

fn handle_source_send(
    state: &mut WaylandState,
    source_id: &ObjectId,
    mime_type: &str,
    fd: OwnedFd,
) {
    let Some(payload) = state
        .clipboard
        .active_sources
        .get(source_id)
        .and_then(|entry| entry.payloads.get(mime_type))
        .cloned()
    else {
        return;
    };

    let _ = rustix::fs::fcntl_setfl(&fd, rustix::fs::OFlags::NONBLOCK);
    let mut offset = 0usize;

    // Fast path: small clipboard payloads fit inside the kernel pipe buffer and complete
    // synchronously without registering a calloop source.
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

    let Some(handle) = state.clipboard.loop_handle.clone() else {
        return;
    };

    let io_id = state.clipboard.alloc_io_id();
    let file = std::fs::File::from(fd);
    let fd_source = Generic::new(file, Interest::WRITE, Mode::Level);
    let Ok(fd_tok) = handle.insert_source(fd_source, move |_, file_ref, wl| {
        loop {
            let Some(entry) = wl.clipboard.pending_writes.get_mut(&io_id) else {
                return Ok(PostAction::Remove);
            };
            if entry.offset >= entry.payload.len() {
                finish_clipboard_write(wl, io_id);
                return Ok(PostAction::Remove);
            }
            match rustix::io::write(file_ref.as_fd(), &entry.payload[entry.offset..]) {
                Ok(0) => {
                    finish_clipboard_write(wl, io_id);
                    return Ok(PostAction::Remove);
                }
                Ok(n) => {
                    entry.offset += n;
                    if entry.offset >= entry.payload.len() {
                        finish_clipboard_write(wl, io_id);
                        return Ok(PostAction::Remove);
                    }
                }
                Err(rustix::io::Errno::AGAIN) => return Ok(PostAction::Continue),
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => {
                    finish_clipboard_write(wl, io_id);
                    return Ok(PostAction::Remove);
                }
            }
        }
    }) else {
        return;
    };

    let timer_tok = handle
        .insert_source(Timer::from_duration(PIPE_IO_TIMEOUT), move |_, _, wl| {
            if let Some(mut entry) = wl.clipboard.pending_writes.remove(&io_id)
                && let Some(ref h) = wl.clipboard.loop_handle
                && let Some(ftok) = entry.fd_token.take()
            {
                h.remove(ftok);
            }
            TimeoutAction::Drop
        })
        .ok();

    state.clipboard.pending_writes.insert(
        io_id,
        PendingClipWrite {
            payload,
            offset,
            fd_token: Some(fd_tok),
            timer_token: timer_tok,
        },
    );
}

pub(crate) fn decode_text_for_mime(mime: &str, bytes: &[u8]) -> Option<String> {
    let lower = mime.to_ascii_lowercase();
    let is_text = lower.starts_with("text/") || lower == "utf8_string" || lower == "text";
    if !is_text {
        return None;
    }
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

fn sanitize_mime(mime: &str) -> String {
    mime.chars()
        .filter(|&c| c != '\0' && c != '\n' && c != '\r')
        .collect()
}

macro_rules! impl_data_control_dispatch {
    (
        $mgr:ty, $mgr_ev:ty,
        $dev:ty, $dev_mod:ident, $offer_variant:ident,
        $offer:ty, $offer_mod:ident,
        $src:ty, $src_mod:ident
    ) => {
        impl Dispatch<$mgr, ()> for WaylandState {
            fn event(
                _state: &mut Self,
                _proxy: &$mgr,
                _event: $mgr_ev,
                _data: &(),
                _conn: &Connection,
                _qh: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<$dev, ()> for WaylandState {
            event_created_child!(WaylandState, $dev, [
                $dev_mod::EVT_DATA_OFFER_OPCODE => ($offer, ())
            ]);

            fn event(
                state: &mut Self,
                _proxy: &$dev,
                event: $dev_mod::Event,
                _data: &(),
                conn: &Connection,
                _qh: &QueueHandle<Self>,
            ) {
                match event {
                    $dev_mod::Event::DataOffer { id } => {
                        let obj_id = id.id();
                        state
                            .clipboard
                            .on_data_offer(obj_id, OfferProxy::$offer_variant(id));
                    }
                    $dev_mod::Event::Selection { id } => {
                        handle_selection(
                            state,
                            conn,
                            ClipboardSelection::Clipboard,
                            id.map(|o| o.id()),
                        );
                    }
                    $dev_mod::Event::PrimarySelection { id } => {
                        handle_selection(
                            state,
                            conn,
                            ClipboardSelection::Primary,
                            id.map(|o| o.id()),
                        );
                    }
                    $dev_mod::Event::Finished => {
                        state.clipboard.on_device_finished();
                    }
                    _ => {}
                }
            }
        }

        impl Dispatch<$offer, ()> for WaylandState {
            fn event(
                state: &mut Self,
                proxy: &$offer,
                event: $offer_mod::Event,
                _data: &(),
                _conn: &Connection,
                _qh: &QueueHandle<Self>,
            ) {
                if let $offer_mod::Event::Offer { mime_type } = event {
                    state.clipboard.on_offer_mime(&proxy.id(), mime_type);
                }
            }
        }

        impl Dispatch<$src, ()> for WaylandState {
            fn event(
                state: &mut Self,
                proxy: &$src,
                event: $src_mod::Event,
                _data: &(),
                _conn: &Connection,
                _qh: &QueueHandle<Self>,
            ) {
                match event {
                    $src_mod::Event::Send { mime_type, fd } => {
                        handle_source_send(state, &proxy.id(), &mime_type, fd);
                    }
                    $src_mod::Event::Cancelled => {
                        state.clipboard.on_source_cancelled(&proxy.id());
                    }
                    _ => {}
                }
            }
        }
    };
}

impl_data_control_dispatch!(
    ExtDataControlManagerV1,
    ext_data_control_manager_v1::Event,
    ExtDataControlDeviceV1,
    ext_data_control_device_v1,
    Ext,
    ExtDataControlOfferV1,
    ext_data_control_offer_v1,
    ExtDataControlSourceV1,
    ext_data_control_source_v1
);

impl_data_control_dispatch!(
    ZwlrDataControlManagerV1,
    zwlr_data_control_manager_v1::Event,
    ZwlrDataControlDeviceV1,
    zwlr_data_control_device_v1,
    Wlr,
    ZwlrDataControlOfferV1,
    zwlr_data_control_offer_v1,
    ZwlrDataControlSourceV1,
    zwlr_data_control_source_v1
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_decoding_respects_mime_types_and_utf8() {
        assert_eq!(
            decode_text_for_mime("text/plain;charset=utf-8", b"hello"),
            Some("hello".to_owned())
        );
        assert_eq!(
            decode_text_for_mime("UTF8_STRING", b"world"),
            Some("world".to_owned())
        );
        assert_eq!(decode_text_for_mime("image/png", b"\x89PNG"), None);
        assert_eq!(decode_text_for_mime("text/plain", &[0xff, 0xfe]), None);
    }

    #[test]
    fn mime_sanitization_strips_nul_and_newlines() {
        assert_eq!(sanitize_mime("text/plain\0\r\n"), "text/plain");
    }
}
