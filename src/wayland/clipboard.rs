//! Wayland data-control clipboard backend supporting `ext-data-control-v1` and `zwlr-data-control-v1`.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};

use calloop::ping::Ping;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
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
const PIPE_READ_TIMEOUT_SECS: i64 = 2;

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

/// Manages Wayland clipboard / primary-selection watches and outbound data sources.
pub(crate) struct WaylandClipboardState {
    manager: Option<ManagerProxy>,
    device: Option<DeviceProxy>,
    watching: bool,
    pending_offers: HashMap<ObjectId, (OfferProxy, Vec<String>)>,
    active_clipboard_offer: Option<OfferProxy>,
    active_primary_offer: Option<OfferProxy>,
    active_sources: HashMap<ObjectId, ActiveSource>,
    async_events: Arc<Mutex<VecDeque<ClipboardEvent>>>,
    wake_ping: Option<Ping>,
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
            async_events: Arc::new(Mutex::new(VecDeque::new())),
            wake_ping: None,
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

        Self {
            manager,
            ..Self::default()
        }
    }

    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn set_wake_ping(&mut self, ping: Ping) {
        self.wake_ping = Some(ping);
    }

    pub(crate) fn drain_async_events(&self) -> Vec<ClipboardEvent> {
        let mut out = Vec::new();
        if let Ok(mut guard) = self.async_events.lock() {
            while let Some(ev) = guard.pop_front() {
                out.push(ev);
            }
        }
        out
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
            self.pending_offers.clear();
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

    fn on_selection(
        &mut self,
        events: &mut VecDeque<WaylandEvent>,
        selection: ClipboardSelection,
        offer_id: Option<ObjectId>,
    ) {
        let slot = match selection {
            ClipboardSelection::Clipboard => &mut self.active_clipboard_offer,
            ClipboardSelection::Primary => &mut self.active_primary_offer,
        };
        if let Some(prev) = slot.take() {
            prev.destroy();
        }

        let Some(id) = offer_id else {
            events.push_back(WaylandEvent::Clipboard(ClipboardEvent::Changed(
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

        let Some((offer, mime_types)) = self.pending_offers.remove(&id) else {
            return;
        };
        let preferred = select_preferred_mime(&mime_types).map(str::to_owned);
        let Some(target_mime) = preferred else {
            *slot = Some(offer);
            events.push_back(WaylandEvent::Clipboard(ClipboardEvent::Changed(
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

        let Ok((rx, tx)) = std::io::pipe() else {
            *slot = Some(offer);
            events.push_back(WaylandEvent::Clipboard(ClipboardEvent::Error(
                "failed to create pipe for clipboard transfer".to_owned(),
            )));
            return;
        };

        offer.receive(&target_mime, tx.as_fd());
        drop(tx);
        *slot = Some(offer);

        spawn_pipe_reader(
            rx,
            selection,
            mime_types,
            target_mime,
            Arc::clone(&self.async_events),
            self.wake_ping.clone(),
        );
    }

    fn on_device_finished(&mut self) {
        self.device = None;
        self.pending_offers.clear();
    }

    fn on_source_send(&self, source_id: &ObjectId, mime_type: &str, fd: OwnedFd) {
        let Some(entry) = self.active_sources.get(source_id) else {
            return;
        };
        let Some(payload) = entry.payloads.get(mime_type).map(Arc::clone) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("waywi-clip-tx".to_owned())
            .spawn(move || {
                let mut file = std::fs::File::from(fd);
                let _ = file.write_all(&payload);
                let _ = file.flush();
            });
    }

    fn on_source_cancelled(&mut self, source_id: &ObjectId) {
        if let Some(entry) = self.active_sources.remove(source_id) {
            entry.proxy.destroy();
        }
    }
}

fn spawn_pipe_reader(
    mut rx: std::io::PipeReader,
    selection: ClipboardSelection,
    mime_types: Vec<String>,
    primary_mime: String,
    async_events: Arc<Mutex<VecDeque<ClipboardEvent>>>,
    wake_ping: Option<Ping>,
) {
    let async_events_tx = Arc::clone(&async_events);
    let spawn_res = std::thread::Builder::new()
        .name("waywi-clip-rx".to_owned())
        .spawn(move || {
            let event = match read_pipe_bounded(&mut rx) {
                Ok(bytes) => {
                    let text = decode_text_for_mime(&primary_mime, &bytes);
                    ClipboardEvent::Changed(ClipboardSnapshot {
                        selection,
                        mime_types,
                        text,
                        bytes,
                        primary_mime: Some(primary_mime),
                        cleared: false,
                    })
                }
                Err(err) => ClipboardEvent::Error(err),
            };
            if let Ok(mut guard) = async_events_tx.lock() {
                guard.push_back(event);
            }
            if let Some(ping) = wake_ping {
                ping.ping();
            }
        });
    if let Err(err) = spawn_res
        && let Ok(mut guard) = async_events.lock()
    {
        guard.push_back(ClipboardEvent::Error(format!(
            "failed to spawn clipboard reader thread: {err}"
        )));
    }
}

fn read_pipe_bounded(rx: &mut std::io::PipeReader) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    let timeout = Timespec {
        tv_sec: PIPE_READ_TIMEOUT_SECS,
        tv_nsec: 0,
    };
    loop {
        let mut pfd = [PollFd::new(&*rx, PollFlags::IN | PollFlags::HUP)];
        match poll(&mut pfd, Some(&timeout)) {
            Ok(0) => {
                return Err("clipboard read timed out waiting for source application".to_owned());
            }
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(format!("clipboard pipe poll failed: {e}")),
        }
        match rx.read(&mut chunk) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                let remaining = MAX_CLIPBOARD_BYTES.saturating_sub(out.len());
                if remaining == 0 {
                    return Ok(out);
                }
                let take = n.min(remaining);
                out.extend_from_slice(&chunk[..take]);
                if take < n {
                    return Ok(out);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("clipboard read failed: {e}")),
        }
    }
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
                _conn: &Connection,
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
                        state.clipboard.on_selection(
                            &mut state.events,
                            ClipboardSelection::Clipboard,
                            id.map(|o| o.id()),
                        );
                    }
                    $dev_mod::Event::PrimarySelection { id } => {
                        state.clipboard.on_selection(
                            &mut state.events,
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
                        state.clipboard.on_source_send(&proxy.id(), &mime_type, fd);
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
