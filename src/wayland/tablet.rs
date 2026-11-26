//! Wayland `zwp_tablet_manager_v2` (`tablet-v2`) graphics tablet, stylus tool, and express-key
//! pad protocol integration.

use std::collections::HashMap;

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::{self, ZwpTabletManagerV2},
    zwp_tablet_pad_dial_v2::{self, ZwpTabletPadDialV2},
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::{self, ZwpTabletPadStripV2},
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::{self, ZwpTabletV2},
};

use crate::geometry::{LogicalPx, Point};
use crate::input::{
    TabletEvent, TabletId, TabletPadAxisSource, TabletPadEvent, TabletPadEventKind, TabletPadId,
    TabletToolAxes, TabletToolEvent, TabletToolEventKind, TabletToolId, TabletToolType,
};
use crate::wayland::connection::WaylandState;
use crate::wayland::surface_state::WaylandEvent;

struct TabletRecord {
    id: TabletId,
    proxy: ZwpTabletV2,
}

#[derive(Default)]
struct PendingToolFrame {
    proximity_in: bool,
    proximity_out: bool,
    down: bool,
    up: bool,
    motion: bool,
    wheel_degrees: f32,
    wheel_clicks: i32,
    buttons: Vec<(u32, bool)>,
}

struct ToolRecord {
    id: TabletToolId,
    proxy: ZwpTabletToolV2,
    tool_type: TabletToolType,
    hardware_serial: u64,
    hardware_id_wacom: u64,
    focused_surface: Option<ObjectId>,
    tablet_id: Option<TabletId>,
    position: Point,
    axes: TabletToolAxes,
    tip_down: bool,
    pending: PendingToolFrame,
}

struct PadRecord {
    id: TabletPadId,
    proxy: ZwpTabletPadV2,
    focused_surface: Option<ObjectId>,
    tablet_id: Option<TabletId>,
    next_group_index: u32,
    next_ring_index: u32,
    next_strip_index: u32,
    next_dial_index: u32,
}

struct PadGroupRecord {
    pad_obj: ObjectId,
    group_index: u32,
    proxy: ZwpTabletPadGroupV2,
}

struct PadRingRecord {
    pad_obj: ObjectId,
    ring_index: u32,
    proxy: ZwpTabletPadRingV2,
    pending_angle: Option<f32>,
    pending_source: TabletPadAxisSource,
    pending_stop: bool,
    dirty: bool,
}

struct PadStripRecord {
    pad_obj: ObjectId,
    strip_index: u32,
    proxy: ZwpTabletPadStripV2,
    pending_pos: Option<f32>,
    pending_source: TabletPadAxisSource,
    pending_stop: bool,
    dirty: bool,
}

struct PadDialRecord {
    pad_obj: ObjectId,
    dial_index: u32,
    proxy: ZwpTabletPadDialV2,
    pending_value120: i32,
    dirty: bool,
}

/// Manages optional `zwp_tablet_manager_v2` global, per-seat tablet seats, tools, and pads.
#[derive(Default)]
pub(crate) struct WaylandTabletState {
    manager: Option<ZwpTabletManagerV2>,
    seats: HashMap<ObjectId, ZwpTabletSeatV2>,
    next_id: u64,
    tablets: HashMap<ObjectId, TabletRecord>,
    tools: HashMap<ObjectId, ToolRecord>,
    pads: HashMap<ObjectId, PadRecord>,
    groups: HashMap<ObjectId, PadGroupRecord>,
    rings: HashMap<ObjectId, PadRingRecord>,
    strips: HashMap<ObjectId, PadStripRecord>,
    dials: HashMap<ObjectId, PadDialRecord>,
}

impl Drop for WaylandTabletState {
    fn drop(&mut self) {
        for (_, r) in self.rings.drain() {
            r.proxy.destroy();
        }
        for (_, s) in self.strips.drain() {
            s.proxy.destroy();
        }
        for (_, d) in self.dials.drain() {
            d.proxy.destroy();
        }
        for (_, g) in self.groups.drain() {
            g.proxy.destroy();
        }
        for (_, p) in self.pads.drain() {
            p.proxy.destroy();
        }
        for (_, t) in self.tools.drain() {
            t.proxy.destroy();
        }
        for (_, t) in self.tablets.drain() {
            t.proxy.destroy();
        }
        for (_, s) in self.seats.drain() {
            s.destroy();
        }
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandTabletState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let mut state = Self::default();
        state.manager = globals.bind::<ZwpTabletManagerV2, _, _>(qh, 1..=2, ()).ok();
        state
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn add_seat(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<WaylandState>) {
        let Some(ref mgr) = self.manager else {
            return;
        };
        let sid = seat.id();
        if self.seats.contains_key(&sid) {
            return;
        }
        let tablet_seat = mgr.get_tablet_seat(seat, qh, ());
        self.seats.insert(sid, tablet_seat);
    }

    pub(crate) fn remove_seat(&mut self, seat: &wl_seat::WlSeat) {
        if let Some(ts) = self.seats.remove(&seat.id()) {
            ts.destroy();
        }
    }

    pub(crate) fn clear_surface(&mut self, wl_surface_id: &ObjectId) {
        for tool in self.tools.values_mut() {
            if tool.focused_surface.as_ref() == Some(wl_surface_id) {
                tool.focused_surface = None;
                tool.tip_down = false;
                tool.pending = PendingToolFrame::default();
            }
        }
        for pad in self.pads.values_mut() {
            if pad.focused_surface.as_ref() == Some(wl_surface_id) {
                pad.focused_surface = None;
            }
        }
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn destroy_pad_children(&mut self, pad_obj: &ObjectId) {
        let ring_ids: Vec<ObjectId> = self
            .rings
            .iter()
            .filter(|(_, r)| &r.pad_obj == pad_obj)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ring_ids {
            if let Some(r) = self.rings.remove(&id) {
                r.proxy.destroy();
            }
        }
        let strip_ids: Vec<ObjectId> = self
            .strips
            .iter()
            .filter(|(_, s)| &s.pad_obj == pad_obj)
            .map(|(k, _)| k.clone())
            .collect();
        for id in strip_ids {
            if let Some(s) = self.strips.remove(&id) {
                s.proxy.destroy();
            }
        }
        let dial_ids: Vec<ObjectId> = self
            .dials
            .iter()
            .filter(|(_, d)| &d.pad_obj == pad_obj)
            .map(|(k, _)| k.clone())
            .collect();
        for id in dial_ids {
            if let Some(d) = self.dials.remove(&id) {
                d.proxy.destroy();
            }
        }
        let group_ids: Vec<ObjectId> = self
            .groups
            .iter()
            .filter(|(_, g)| &g.pad_obj == pad_obj)
            .map(|(k, _)| k.clone())
            .collect();
        for id in group_ids {
            if let Some(g) = self.groups.remove(&id) {
                g.proxy.destroy();
            }
        }
    }
}

fn map_tool_type(raw: WEnum<zwp_tablet_tool_v2::Type>) -> TabletToolType {
    match raw {
        WEnum::Value(zwp_tablet_tool_v2::Type::Pen) => TabletToolType::Pen,
        WEnum::Value(zwp_tablet_tool_v2::Type::Eraser) => TabletToolType::Eraser,
        WEnum::Value(zwp_tablet_tool_v2::Type::Brush) => TabletToolType::Brush,
        WEnum::Value(zwp_tablet_tool_v2::Type::Pencil) => TabletToolType::Pencil,
        WEnum::Value(zwp_tablet_tool_v2::Type::Airbrush) => TabletToolType::Airbrush,
        WEnum::Value(zwp_tablet_tool_v2::Type::Finger) => TabletToolType::Finger,
        WEnum::Value(zwp_tablet_tool_v2::Type::Mouse) => TabletToolType::Mouse,
        WEnum::Value(zwp_tablet_tool_v2::Type::Lens) => TabletToolType::Lens,
        _ => TabletToolType::Pen,
    }
}

fn emit_pad_event(state: &mut WaylandState, pad_obj: &ObjectId, kind: TabletPadEventKind) {
    let Some(pad) = state.tablet.pads.get(pad_obj) else {
        return;
    };
    let Some(surf_id) = pad.focused_surface.as_ref() else {
        return;
    };
    let Some(&key) = state.surface_by_obj.get(surf_id) else {
        return;
    };
    let pad_id = pad.id;
    let tablet_id = pad.tablet_id;
    state.events.push_back(WaylandEvent::Tablet {
        key,
        event: TabletEvent::Pad(TabletPadEvent {
            pad_id,
            tablet_id,
            kind,
            modifiers: state.modifiers,
        }),
    });
}

impl Dispatch<ZwpTabletManagerV2, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpTabletManagerV2,
        _event: zwp_tablet_manager_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpTabletSeatV2, ()> for WaylandState {
    event_created_child!(WaylandState, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);

    fn event(
        state: &mut Self,
        _proxy: &ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_seat_v2::Event::TabletAdded { id } => {
                let tid = TabletId(state.tablet.alloc_id());
                state
                    .tablet
                    .tablets
                    .insert(id.id(), TabletRecord { id: tid, proxy: id });
            }
            zwp_tablet_seat_v2::Event::ToolAdded { id } => {
                let tid = TabletToolId(state.tablet.alloc_id());
                state.tablet.tools.insert(
                    id.id(),
                    ToolRecord {
                        id: tid,
                        proxy: id,
                        tool_type: TabletToolType::Pen,
                        hardware_serial: 0,
                        hardware_id_wacom: 0,
                        focused_surface: None,
                        tablet_id: None,
                        position: Point::ZERO,
                        axes: TabletToolAxes::default(),
                        tip_down: false,
                        pending: PendingToolFrame::default(),
                    },
                );
            }
            zwp_tablet_seat_v2::Event::PadAdded { id } => {
                let pid = TabletPadId(state.tablet.alloc_id());
                state.tablet.pads.insert(
                    id.id(),
                    PadRecord {
                        id: pid,
                        proxy: id,
                        focused_surface: None,
                        tablet_id: None,
                        next_group_index: 0,
                        next_ring_index: 0,
                        next_strip_index: 0,
                        next_dial_index: 0,
                    },
                );
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletV2, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ZwpTabletV2,
        event: zwp_tablet_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwp_tablet_v2::Event::Removed = event {
            let obj_id = proxy.id();
            if let Some(rec) = state.tablet.tablets.remove(&obj_id) {
                rec.proxy.destroy();
            } else {
                proxy.destroy();
            }
        }
    }
}

impl Dispatch<ZwpTabletToolV2, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let obj_id = proxy.id();
        match event {
            zwp_tablet_tool_v2::Event::Type { tool_type } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.tool_type = map_tool_type(tool_type);
                }
            }
            zwp_tablet_tool_v2::Event::HardwareSerial {
                hardware_serial_hi,
                hardware_serial_lo,
            } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.hardware_serial =
                        (u64::from(hardware_serial_hi) << 32) | u64::from(hardware_serial_lo);
                }
            }
            zwp_tablet_tool_v2::Event::HardwareIdWacom {
                hardware_id_hi,
                hardware_id_lo,
            } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.hardware_id_wacom =
                        (u64::from(hardware_id_hi) << 32) | u64::from(hardware_id_lo);
                }
            }
            zwp_tablet_tool_v2::Event::ProximityIn {
                serial,
                tablet,
                surface,
            } => {
                if let Some(seat) = state.seat_state.seats().next() {
                    state.last_input_serial = Some((seat, serial, surface.clone()));
                }
                let tablet_id = state.tablet.tablets.get(&tablet.id()).map(|t| t.id);
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.focused_surface = Some(surface.id());
                    tool.tablet_id = tablet_id;
                    tool.pending.proximity_in = true;
                }
            }
            zwp_tablet_tool_v2::Event::ProximityOut => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.pending.proximity_out = true;
                }
            }
            zwp_tablet_tool_v2::Event::Down { serial } => {
                let focused_wl = state
                    .tablet
                    .tools
                    .get(&obj_id)
                    .and_then(|t| t.focused_surface.as_ref())
                    .and_then(|sid| state.surface_by_obj.get(sid))
                    .and_then(|rk| state.surfaces.get(rk))
                    .map(|s| s.role.wl_surface().clone());
                if let (Some(seat), Some(wl_surf)) = (state.seat_state.seats().next(), focused_wl) {
                    state.last_input_serial = Some((seat, serial, wl_surf));
                }
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.tip_down = true;
                    tool.pending.down = true;
                }
            }
            zwp_tablet_tool_v2::Event::Up => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.tip_down = false;
                    tool.pending.up = true;
                }
            }
            zwp_tablet_tool_v2::Event::Motion { x, y } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.position = Point::new(LogicalPx(x as f32), LogicalPx(y as f32));
                    tool.pending.motion = true;
                }
            }
            zwp_tablet_tool_v2::Event::Pressure { pressure } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.axes.pressure = (pressure as f32 / 65535.0).clamp(0.0, 1.0);
                    tool.pending.motion = true;
                }
            }
            zwp_tablet_tool_v2::Event::Distance { distance } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.axes.distance = (distance as f32 / 65535.0).clamp(0.0, 1.0);
                    tool.pending.motion = true;
                }
            }
            zwp_tablet_tool_v2::Event::Tilt { tilt_x, tilt_y } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.axes.tilt = (tilt_x as f32, tilt_y as f32);
                    tool.pending.motion = true;
                }
            }
            zwp_tablet_tool_v2::Event::Rotation { degrees } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.axes.rotation = degrees as f32;
                    tool.pending.motion = true;
                }
            }
            zwp_tablet_tool_v2::Event::Slider { position } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.axes.slider = (position as f32 / 65535.0).clamp(-1.0, 1.0);
                    tool.pending.motion = true;
                }
            }
            zwp_tablet_tool_v2::Event::Wheel { degrees, clicks } => {
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.pending.wheel_degrees += degrees as f32;
                    tool.pending.wheel_clicks = tool.pending.wheel_clicks.saturating_add(clicks);
                    tool.pending.motion = true;
                }
            }
            zwp_tablet_tool_v2::Event::Button {
                serial,
                button,
                state: btn_state,
            } => {
                let pressed = matches!(
                    btn_state,
                    WEnum::Value(zwp_tablet_tool_v2::ButtonState::Pressed)
                );
                if pressed {
                    let focused_wl = state
                        .tablet
                        .tools
                        .get(&obj_id)
                        .and_then(|t| t.focused_surface.as_ref())
                        .and_then(|sid| state.surface_by_obj.get(sid))
                        .and_then(|rk| state.surfaces.get(rk))
                        .map(|s| s.role.wl_surface().clone());
                    if let (Some(seat), Some(wl_surf)) =
                        (state.seat_state.seats().next(), focused_wl)
                    {
                        state.last_input_serial = Some((seat, serial, wl_surf));
                    }
                }
                if let Some(tool) = state.tablet.tools.get_mut(&obj_id) {
                    tool.pending.buttons.push((button, pressed));
                }
            }
            zwp_tablet_tool_v2::Event::Frame { .. } => {
                let modifiers = state.modifiers;
                let (key, emitted) = {
                    let Some(tool) = state.tablet.tools.get_mut(&obj_id) else {
                        return;
                    };
                    let pending = std::mem::take(&mut tool.pending);
                    let mut axes = tool.axes;
                    axes.wheel_degrees = pending.wheel_degrees;
                    axes.wheel_clicks = pending.wheel_clicks;

                    let target_key = tool
                        .focused_surface
                        .as_ref()
                        .and_then(|sid| state.surface_by_obj.get(sid))
                        .copied();

                    let make_ev = |kind: TabletToolEventKind| TabletToolEvent {
                        tool_id: tool.id,
                        tablet_id: tool.tablet_id,
                        tool_type: tool.tool_type,
                        hardware_serial: tool.hardware_serial,
                        hardware_id_wacom: tool.hardware_id_wacom,
                        kind,
                        position: tool.position,
                        local_position: tool.position,
                        axes,
                        tip_down: tool.tip_down,
                        modifiers,
                    };

                    let mut batch = Vec::new();
                    if pending.proximity_in {
                        batch.push(make_ev(TabletToolEventKind::ProximityIn));
                    }
                    if pending.down {
                        batch.push(make_ev(TabletToolEventKind::Down));
                    }
                    if pending.motion
                        && (!pending.proximity_in && !pending.down && !pending.up
                            || pending.wheel_clicks != 0
                            || pending.wheel_degrees != 0.0)
                    {
                        batch.push(make_ev(TabletToolEventKind::Motion));
                    }
                    for (button, pressed) in pending.buttons {
                        batch.push(make_ev(TabletToolEventKind::Button { button, pressed }));
                    }
                    if pending.up {
                        batch.push(make_ev(TabletToolEventKind::Up));
                    }
                    if pending.proximity_out {
                        batch.push(make_ev(TabletToolEventKind::ProximityOut));
                        tool.focused_surface = None;
                        tool.tip_down = false;
                        tool.axes = TabletToolAxes::default();
                    }
                    (target_key, batch)
                };

                if let Some(key) = key {
                    for ev in emitted {
                        state.events.push_back(WaylandEvent::Tablet {
                            key,
                            event: TabletEvent::Tool(ev),
                        });
                    }
                }
            }
            zwp_tablet_tool_v2::Event::Removed => {
                if let Some(rec) = state.tablet.tools.remove(&obj_id) {
                    rec.proxy.destroy();
                } else {
                    proxy.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletPadV2, ()> for WaylandState {
    event_created_child!(WaylandState, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);

    fn event(
        state: &mut Self,
        proxy: &ZwpTabletPadV2,
        event: zwp_tablet_pad_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let pad_obj = proxy.id();
        match event {
            zwp_tablet_pad_v2::Event::Group { pad_group } => {
                let group_index = state
                    .tablet
                    .pads
                    .get_mut(&pad_obj)
                    .map(|p| {
                        let idx = p.next_group_index;
                        p.next_group_index += 1;
                        idx
                    })
                    .unwrap_or(0);
                state.tablet.groups.insert(
                    pad_group.id(),
                    PadGroupRecord {
                        pad_obj,
                        group_index,
                        proxy: pad_group,
                    },
                );
            }
            zwp_tablet_pad_v2::Event::Enter {
                tablet, surface, ..
            } => {
                let tablet_id = state.tablet.tablets.get(&tablet.id()).map(|t| t.id);
                if let Some(pad) = state.tablet.pads.get_mut(&pad_obj) {
                    pad.focused_surface = Some(surface.id());
                    pad.tablet_id = tablet_id;
                }
                emit_pad_event(state, &pad_obj, TabletPadEventKind::Enter);
            }
            zwp_tablet_pad_v2::Event::Leave { .. } => {
                emit_pad_event(state, &pad_obj, TabletPadEventKind::Leave);
                if let Some(pad) = state.tablet.pads.get_mut(&pad_obj) {
                    pad.focused_surface = None;
                }
            }
            zwp_tablet_pad_v2::Event::Button {
                button,
                state: btn_state,
                ..
            } => {
                let pressed = matches!(
                    btn_state,
                    WEnum::Value(zwp_tablet_pad_v2::ButtonState::Pressed)
                );
                emit_pad_event(
                    state,
                    &pad_obj,
                    TabletPadEventKind::Button { button, pressed },
                );
            }
            zwp_tablet_pad_v2::Event::Removed => {
                state.tablet.destroy_pad_children(&pad_obj);
                if let Some(rec) = state.tablet.pads.remove(&pad_obj) {
                    rec.proxy.destroy();
                } else {
                    proxy.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletPadGroupV2, ()> for WaylandState {
    event_created_child!(WaylandState, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
        zwp_tablet_pad_group_v2::EVT_DIAL_OPCODE => (ZwpTabletPadDialV2, ()),
    ]);

    fn event(
        state: &mut Self,
        proxy: &ZwpTabletPadGroupV2,
        event: zwp_tablet_pad_group_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let group_obj = proxy.id();
        let Some((pad_obj, group_index)) = state
            .tablet
            .groups
            .get(&group_obj)
            .map(|g| (g.pad_obj.clone(), g.group_index))
        else {
            return;
        };
        match event {
            zwp_tablet_pad_group_v2::Event::Ring { ring } => {
                let ring_index = state
                    .tablet
                    .pads
                    .get_mut(&pad_obj)
                    .map(|p| {
                        let idx = p.next_ring_index;
                        p.next_ring_index += 1;
                        idx
                    })
                    .unwrap_or(0);
                state.tablet.rings.insert(
                    ring.id(),
                    PadRingRecord {
                        pad_obj,
                        ring_index,
                        proxy: ring,
                        pending_angle: None,
                        pending_source: TabletPadAxisSource::Unknown,
                        pending_stop: false,
                        dirty: false,
                    },
                );
            }
            zwp_tablet_pad_group_v2::Event::Strip { strip } => {
                let strip_index = state
                    .tablet
                    .pads
                    .get_mut(&pad_obj)
                    .map(|p| {
                        let idx = p.next_strip_index;
                        p.next_strip_index += 1;
                        idx
                    })
                    .unwrap_or(0);
                state.tablet.strips.insert(
                    strip.id(),
                    PadStripRecord {
                        pad_obj,
                        strip_index,
                        proxy: strip,
                        pending_pos: None,
                        pending_source: TabletPadAxisSource::Unknown,
                        pending_stop: false,
                        dirty: false,
                    },
                );
            }
            zwp_tablet_pad_group_v2::Event::Dial { dial } => {
                let dial_index = state
                    .tablet
                    .pads
                    .get_mut(&pad_obj)
                    .map(|p| {
                        let idx = p.next_dial_index;
                        p.next_dial_index += 1;
                        idx
                    })
                    .unwrap_or(0);
                state.tablet.dials.insert(
                    dial.id(),
                    PadDialRecord {
                        pad_obj,
                        dial_index,
                        proxy: dial,
                        pending_value120: 0,
                        dirty: false,
                    },
                );
            }
            zwp_tablet_pad_group_v2::Event::ModeSwitch { mode, .. } => {
                emit_pad_event(
                    state,
                    &pad_obj,
                    TabletPadEventKind::ModeSwitch {
                        group: group_index,
                        mode,
                    },
                );
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletPadRingV2, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ZwpTabletPadRingV2,
        event: zwp_tablet_pad_ring_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let obj_id = proxy.id();
        match event {
            zwp_tablet_pad_ring_v2::Event::Source { source } => {
                if let Some(ring) = state.tablet.rings.get_mut(&obj_id) {
                    ring.pending_source = match source {
                        WEnum::Value(zwp_tablet_pad_ring_v2::Source::Finger) => {
                            TabletPadAxisSource::Finger
                        }
                        _ => TabletPadAxisSource::Unknown,
                    };
                    ring.dirty = true;
                }
            }
            zwp_tablet_pad_ring_v2::Event::Angle { degrees } => {
                if let Some(ring) = state.tablet.rings.get_mut(&obj_id) {
                    ring.pending_angle = Some(degrees as f32);
                    ring.dirty = true;
                }
            }
            zwp_tablet_pad_ring_v2::Event::Stop => {
                if let Some(ring) = state.tablet.rings.get_mut(&obj_id) {
                    ring.pending_stop = true;
                    ring.dirty = true;
                }
            }
            zwp_tablet_pad_ring_v2::Event::Frame { .. } => {
                let emit = state.tablet.rings.get_mut(&obj_id).and_then(|ring| {
                    if !ring.dirty {
                        return None;
                    }
                    let res = (
                        ring.pad_obj.clone(),
                        TabletPadEventKind::Ring {
                            ring: ring.ring_index,
                            angle: ring.pending_angle.take(),
                            source: ring.pending_source,
                            stopped: ring.pending_stop,
                        },
                    );
                    ring.pending_source = TabletPadAxisSource::Unknown;
                    ring.pending_stop = false;
                    ring.dirty = false;
                    Some(res)
                });
                if let Some((pad_obj, kind)) = emit {
                    emit_pad_event(state, &pad_obj, kind);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletPadStripV2, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ZwpTabletPadStripV2,
        event: zwp_tablet_pad_strip_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let obj_id = proxy.id();
        match event {
            zwp_tablet_pad_strip_v2::Event::Source { source } => {
                if let Some(strip) = state.tablet.strips.get_mut(&obj_id) {
                    strip.pending_source = match source {
                        WEnum::Value(zwp_tablet_pad_strip_v2::Source::Finger) => {
                            TabletPadAxisSource::Finger
                        }
                        _ => TabletPadAxisSource::Unknown,
                    };
                    strip.dirty = true;
                }
            }
            zwp_tablet_pad_strip_v2::Event::Position { position } => {
                if let Some(strip) = state.tablet.strips.get_mut(&obj_id) {
                    strip.pending_pos = Some((position as f32 / 65535.0).clamp(0.0, 1.0));
                    strip.dirty = true;
                }
            }
            zwp_tablet_pad_strip_v2::Event::Stop => {
                if let Some(strip) = state.tablet.strips.get_mut(&obj_id) {
                    strip.pending_stop = true;
                    strip.dirty = true;
                }
            }
            zwp_tablet_pad_strip_v2::Event::Frame { .. } => {
                let emit = state.tablet.strips.get_mut(&obj_id).and_then(|strip| {
                    if !strip.dirty {
                        return None;
                    }
                    let res = (
                        strip.pad_obj.clone(),
                        TabletPadEventKind::Strip {
                            strip: strip.strip_index,
                            position: strip.pending_pos.take(),
                            source: strip.pending_source,
                            stopped: strip.pending_stop,
                        },
                    );
                    strip.pending_source = TabletPadAxisSource::Unknown;
                    strip.pending_stop = false;
                    strip.dirty = false;
                    Some(res)
                });
                if let Some((pad_obj, kind)) = emit {
                    emit_pad_event(state, &pad_obj, kind);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletPadDialV2, ()> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &ZwpTabletPadDialV2,
        event: zwp_tablet_pad_dial_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let obj_id = proxy.id();
        match event {
            zwp_tablet_pad_dial_v2::Event::Delta { value120 } => {
                if let Some(dial) = state.tablet.dials.get_mut(&obj_id) {
                    dial.pending_value120 = dial.pending_value120.saturating_add(value120);
                    dial.dirty = true;
                }
            }
            zwp_tablet_pad_dial_v2::Event::Frame { .. } => {
                let emit = state.tablet.dials.get_mut(&obj_id).and_then(|dial| {
                    if !dial.dirty {
                        return None;
                    }
                    let v120 = dial.pending_value120;
                    dial.pending_value120 = 0;
                    dial.dirty = false;
                    Some((
                        dial.pad_obj.clone(),
                        TabletPadEventKind::Dial {
                            dial: dial.dial_index,
                            delta: v120 as f32 / 120.0,
                            value120: v120,
                        },
                    ))
                });
                if let Some((pad_obj, kind)) = emit {
                    emit_pad_event(state, &pad_obj, kind);
                }
            }
            _ => {}
        }
    }
}
