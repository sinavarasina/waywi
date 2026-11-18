//! Wayland `zwp_text_input_manager_v3` / `zwp_text_input_v3` IME protocol integration.

use std::collections::HashMap;

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3},
};

use crate::geometry::Rect as GeoRect;
use crate::input::{ImeConfig, ImeContentPurpose, ImeEvent, ImePreedit};
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;

#[derive(Debug, Clone, Default)]
struct PendingIncomingIme {
    preedit: Option<ImePreedit>,
    commit: Option<String>,
    delete_before: u32,
    delete_after: u32,
}

struct SeatTextInput {
    proxy: ZwpTextInputV3,
    focused_surface: Option<ObjectId>,
    enabled: bool,
    commit_count: u32,
    last_sent: Option<ImeConfig>,
    last_external_seq: u64,
    pending_in: PendingIncomingIme,
}

impl SeatTextInput {
    fn new(proxy: ZwpTextInputV3) -> Self {
        Self {
            proxy,
            focused_surface: None,
            enabled: false,
            commit_count: 0,
            last_sent: None,
            last_external_seq: 0,
            pending_in: PendingIncomingIme::default(),
        }
    }

    fn reset_compositor_state(&mut self) {
        self.enabled = false;
        self.last_sent = None;
        self.pending_in = PendingIncomingIme::default();
    }

    fn send_full_state(&mut self, cfg: &ImeConfig) {
        self.proxy.enable();
        self.proxy
            .set_content_type(to_wire_hint(cfg), to_wire_purpose(cfg.content_purpose));
        if let Some(ref sur) = cfg.surrounding_text {
            self.proxy.set_surrounding_text(
                sur.text.clone(),
                sur.cursor.min(i32::MAX as usize) as i32,
                sur.anchor.min(i32::MAX as usize) as i32,
            );
            if cfg.external_change_seq != self.last_external_seq {
                self.proxy.set_text_change_cause(ChangeCause::Other);
            }
        }
        if let Some(rect) = cfg.cursor_rect {
            let (x, y, w, h) = to_wire_cursor_rect(rect);
            self.proxy.set_cursor_rectangle(x, y, w, h);
        }
        self.commit_with(cfg, true);
    }

    fn send_delta_state(&mut self, cfg: &ImeConfig) -> bool {
        let Some(prev) = self.last_sent.as_ref() else {
            self.send_full_state(cfg);
            return true;
        };
        if prev == cfg {
            return false;
        }
        let mut changed = false;

        if prev.content_hint != cfg.content_hint || prev.content_purpose != cfg.content_purpose {
            self.proxy
                .set_content_type(to_wire_hint(cfg), to_wire_purpose(cfg.content_purpose));
            changed = true;
        }
        if prev.surrounding_text != cfg.surrounding_text
            && let Some(ref sur) = cfg.surrounding_text
        {
            self.proxy.set_surrounding_text(
                sur.text.clone(),
                sur.cursor.min(i32::MAX as usize) as i32,
                sur.anchor.min(i32::MAX as usize) as i32,
            );
            if cfg.external_change_seq != self.last_external_seq {
                self.proxy.set_text_change_cause(ChangeCause::Other);
            }
            changed = true;
        }
        if prev.cursor_rect != cfg.cursor_rect
            && let Some(rect) = cfg.cursor_rect
        {
            let (x, y, w, h) = to_wire_cursor_rect(rect);
            self.proxy.set_cursor_rectangle(x, y, w, h);
            changed = true;
        }

        if changed {
            self.commit_with(cfg, true);
        } else {
            // Only metadata like `external_change_seq` changed cleanly without surrounding text delta.
            self.last_external_seq = cfg.external_change_seq;
            self.last_sent = Some(cfg.clone());
        }
        changed
    }

    fn commit_with(&mut self, cfg: &ImeConfig, enabled: bool) {
        self.proxy.commit();
        self.commit_count = self.commit_count.wrapping_add(1);
        self.enabled = enabled;
        self.last_external_seq = cfg.external_change_seq;
        self.last_sent = enabled.then(|| cfg.clone());
    }
}

/// Manages optional `zwp_text_input_manager_v3` global and per-seat `zwp_text_input_v3` objects.
#[derive(Default)]
pub(crate) struct WaylandImeState {
    manager: Option<ZwpTextInputManagerV3>,
    seats: HashMap<ObjectId, SeatTextInput>,
}

impl Drop for WaylandImeState {
    fn drop(&mut self) {
        for (_, seat) in self.seats.drain() {
            seat.proxy.destroy();
        }
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandImeState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ZwpTextInputManagerV3, _, _>(qh, 1..=2, ())
            .ok();
        Self {
            manager,
            seats: HashMap::new(),
        }
    }

    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn add_seat(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<WaylandState>) {
        let Some(ref mgr) = self.manager else {
            return;
        };
        let seat_id = seat.id();
        if self.seats.contains_key(&seat_id) {
            return;
        }
        let text_input = mgr.get_text_input(seat, qh, seat_id.clone());
        self.seats.insert(seat_id, SeatTextInput::new(text_input));
    }

    pub(crate) fn remove_seat(&mut self, seat: &wl_seat::WlSeat) {
        if let Some(removed) = self.seats.remove(&seat.id()) {
            removed.proxy.destroy();
        }
    }

    pub(crate) fn clear_surface(&mut self, surface_obj_id: &ObjectId) {
        for seat in self.seats.values_mut() {
            if seat.focused_surface.as_ref() == Some(surface_obj_id) {
                if seat.enabled {
                    seat.proxy.disable();
                    seat.proxy.commit();
                    seat.commit_count = seat.commit_count.wrapping_add(1);
                }
                seat.focused_surface = None;
                seat.reset_compositor_state();
            }
        }
    }

    /// Synchronizes `cfg` to any seat whose text-input focus currently targets `surface_obj_id`.
    /// Returns `true` when at least one `zwp_text_input_v3.commit` was issued.
    pub(crate) fn sync_surface(&mut self, surface_obj_id: &ObjectId, cfg: &ImeConfig) -> bool {
        let mut committed = false;
        for seat in self.seats.values_mut() {
            if seat.focused_surface.as_ref() != Some(surface_obj_id) {
                continue;
            }
            if !cfg.enabled {
                if seat.enabled {
                    seat.proxy.disable();
                    seat.commit_with(cfg, false);
                    committed = true;
                }
            } else if !seat.enabled {
                seat.send_full_state(cfg);
                committed = true;
            } else if seat.send_delta_state(cfg) {
                committed = true;
            }
        }
        committed
    }
}

fn to_wire_hint(cfg: &ImeConfig) -> ContentHint {
    ContentHint::from_bits_truncate(cfg.content_hint.bits())
}

fn to_wire_purpose(purpose: ImeContentPurpose) -> ContentPurpose {
    match purpose {
        ImeContentPurpose::Normal => ContentPurpose::Normal,
        ImeContentPurpose::Alpha => ContentPurpose::Alpha,
        ImeContentPurpose::Digits => ContentPurpose::Digits,
        ImeContentPurpose::Number => ContentPurpose::Number,
        ImeContentPurpose::Phone => ContentPurpose::Phone,
        ImeContentPurpose::Url => ContentPurpose::Url,
        ImeContentPurpose::Email => ContentPurpose::Email,
        ImeContentPurpose::Name => ContentPurpose::Name,
        ImeContentPurpose::Password => ContentPurpose::Password,
        ImeContentPurpose::Pin => ContentPurpose::Pin,
        ImeContentPurpose::Date => ContentPurpose::Date,
        ImeContentPurpose::Time => ContentPurpose::Time,
        ImeContentPurpose::Datetime => ContentPurpose::Datetime,
        ImeContentPurpose::Terminal => ContentPurpose::Terminal,
    }
}

fn to_wire_cursor_rect(rect: GeoRect) -> (i32, i32, i32, i32) {
    (
        rect.x.0.round() as i32,
        rect.y.0.round() as i32,
        rect.width.0.round().max(1.0) as i32,
        rect.height.0.round().max(1.0) as i32,
    )
}

fn clamp_preedit_bounds(text: &str, cursor_begin: i32, cursor_end: i32) -> Option<ImePreedit> {
    if text.is_empty() {
        return None;
    }
    let hidden = cursor_begin < 0 && cursor_end < 0;
    let (begin, end) = if hidden {
        (None, None)
    } else {
        let clamp = |v: i32| -> usize {
            let mut idx = (v.max(0) as usize).min(text.len());
            while idx > 0 && !text.is_char_boundary(idx) {
                idx -= 1;
            }
            idx
        };
        (Some(clamp(cursor_begin)), Some(clamp(cursor_end)))
    };
    Some(ImePreedit {
        text: text.to_owned(),
        cursor_begin: begin,
        cursor_end: end,
    })
}

impl Dispatch<ZwpTextInputManagerV3, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpTextInputManagerV3,
        _event: zwp_text_input_manager_v3::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpTextInputV3, ObjectId> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        seat_id: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                let surface_id = surface.id();
                if let Some(seat) = state.ime.seats.get_mut(seat_id) {
                    seat.focused_surface = Some(surface_id.clone());
                    seat.reset_compositor_state();
                }
                if let Some(&key) = state.surface_by_obj.get(&surface_id) {
                    state.events.push_back(WaylandEvent::Ime {
                        key,
                        event: ImeEvent::Enter,
                    });
                }
            }
            zwp_text_input_v3::Event::Leave { surface } => {
                let surface_id = surface.id();
                let mut left_focused = false;
                if let Some(seat) = state.ime.seats.get_mut(seat_id)
                    && seat.focused_surface.as_ref() == Some(&surface_id)
                {
                    seat.focused_surface = None;
                    seat.reset_compositor_state();
                    left_focused = true;
                }
                if left_focused && let Some(&key) = state.surface_by_obj.get(&surface_id) {
                    state.events.push_back(WaylandEvent::Ime {
                        key,
                        event: ImeEvent::Leave,
                    });
                }
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                if let Some(seat) = state.ime.seats.get_mut(seat_id) {
                    let raw = text.unwrap_or_default();
                    seat.pending_in.preedit = clamp_preedit_bounds(&raw, cursor_begin, cursor_end);
                }
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                if let Some(seat) = state.ime.seats.get_mut(seat_id) {
                    seat.pending_in.commit = Some(text.unwrap_or_default());
                }
            }
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                if let Some(seat) = state.ime.seats.get_mut(seat_id) {
                    seat.pending_in.delete_before = before_length;
                    seat.pending_in.delete_after = after_length;
                }
            }
            zwp_text_input_v3::Event::Done { serial: _serial } => {
                let Some(seat) = state.ime.seats.get_mut(seat_id) else {
                    return;
                };
                let batch = std::mem::take(&mut seat.pending_in);
                let Some(ref surface_id) = seat.focused_surface else {
                    return;
                };
                if let Some(&key) = state.surface_by_obj.get(surface_id) {
                    state.events.push_back(WaylandEvent::Ime {
                        key,
                        event: ImeEvent::Update {
                            delete_before: batch.delete_before,
                            delete_after: batch.delete_after,
                            commit: batch.commit,
                            preedit: batch.preedit,
                        },
                    });
                }
            }
            zwp_text_input_v3::Event::Action {
                action: WEnum::Value(_),
                serial: _,
            }
            | zwp_text_input_v3::Event::Language { .. }
            | zwp_text_input_v3::Event::PreeditHint { .. }
            | _ => {}
        }
    }
}
