//! Wayland seat, pointer, keyboard, and touch input delegates.

use smithay_client_toolkit::seat::keyboard::{
    KeyEvent as SctkKeyEvent, KeyboardHandler, Keymap, Keysym, Modifiers as SctkModifiers,
    RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{
    PointerEvent as SctkPointerEvent, PointerEventKind, PointerHandler,
};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability as SeatCapability, SeatHandler, SeatState};
use wayland_client::protocol::{wl_keyboard, wl_pointer, wl_seat, wl_surface, wl_touch};
use wayland_client::{Connection, Proxy, QueueHandle};

use crate::geometry::{LogicalPx, Point};
use crate::input::{TouchEvent, TouchPhase};
use crate::wayland::connection::WaylandState;
use crate::wayland::seat::{MappedPointerAction, map_key_event, map_modifiers, map_pointer_event};
use crate::wayland::surface_state::{ActiveTouch, WaylandEvent};

impl SeatHandler for WaylandState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        #[cfg(feature = "ime")]
        self.ime.add_seat(&seat, qh);
        #[cfg(feature = "tablet")]
        self.tablet.add_seat(&seat, qh);
        #[cfg(not(any(feature = "ime", feature = "tablet")))]
        let _ = (qh, seat);
    }

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: SeatCapability,
    ) {
        match capability {
            SeatCapability::Keyboard => {
                if let Ok(kb) = self.seat_state.get_keyboard::<Self, Self>(qh, &seat, None) {
                    self.keyboards.push(kb);
                    self.has_keyboard = true;
                }
            }
            SeatCapability::Pointer => {
                if let Ok(ptr) = self.seat_state.get_pointer(qh, &seat) {
                    #[cfg(feature = "pointer-gestures")]
                    self.pointer_gestures.add_pointer(&ptr, qh);
                    self.pointers.push(ptr);
                    self.has_pointer = true;
                }
            }
            SeatCapability::Touch => {
                if let Ok(t) = self.seat_state.get_touch(qh, &seat) {
                    self.touches.push(t);
                    self.has_touch = true;
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: SeatCapability,
    ) {
        match capability {
            SeatCapability::Keyboard => {
                for kb in self.keyboards.drain(..) {
                    kb.release();
                }
                self.has_keyboard = false;
            }
            SeatCapability::Pointer => {
                #[cfg(feature = "pointer-gestures")]
                self.pointer_gestures.clear_all_pointers();
                for ptr in self.pointers.drain(..) {
                    ptr.release();
                }
                self.has_pointer = false;
            }
            SeatCapability::Touch => {
                for t in self.touches.drain(..) {
                    t.release();
                }
                self.has_touch = false;
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        #[cfg(feature = "ime")]
        self.ime.remove_seat(&seat);
        #[cfg(feature = "tablet")]
        self.tablet.remove_seat(&seat);
        #[cfg(not(any(feature = "ime", feature = "tablet")))]
        let _ = seat;
    }
}

impl PointerHandler for WaylandState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[SctkPointerEvent],
    ) {
        for ev in events {
            let Some(&key) = self.surface_by_obj.get(&ev.surface.id()) else {
                continue;
            };
            if let PointerEventKind::Press { serial, .. } = ev.kind
                && let Some(seat) = self.seat_state.seats().next()
            {
                self.last_input_serial = Some((seat, serial, ev.surface.clone()));
            }
            match map_pointer_event(ev) {
                MappedPointerAction::Enter { x, y } => {
                    self.last_pointer_pos
                        .insert(key, Point::new(LogicalPx(x), LogicalPx(y)));
                    self.events.push_back(WaylandEvent::PointerEnter {
                        key,
                        x,
                        y,
                        modifiers: self.modifiers,
                    });
                }
                MappedPointerAction::Leave => {
                    self.last_pointer_pos.remove(&key);
                    self.events.push_back(WaylandEvent::PointerLeave { key });
                }
                MappedPointerAction::Motion { x, y } => {
                    self.last_pointer_pos
                        .insert(key, Point::new(LogicalPx(x), LogicalPx(y)));
                    self.events.push_back(WaylandEvent::PointerMotion {
                        key,
                        x,
                        y,
                        modifiers: self.modifiers,
                    });
                }
                MappedPointerAction::Button {
                    x,
                    y,
                    button,
                    pressed,
                } => {
                    self.last_pointer_pos
                        .insert(key, Point::new(LogicalPx(x), LogicalPx(y)));
                    self.events.push_back(WaylandEvent::PointerButton {
                        key,
                        x,
                        y,
                        button,
                        pressed,
                        modifiers: self.modifiers,
                    });
                }
                MappedPointerAction::Scroll {
                    x,
                    y,
                    dx,
                    dy,
                    discrete_x,
                    discrete_y,
                    source,
                    stop_x,
                    stop_y,
                } => {
                    self.last_pointer_pos
                        .insert(key, Point::new(LogicalPx(x), LogicalPx(y)));
                    self.events.push_back(WaylandEvent::PointerScroll {
                        key,
                        x,
                        y,
                        dx,
                        dy,
                        discrete_x,
                        discrete_y,
                        source,
                        stop_x,
                        stop_y,
                        modifiers: self.modifiers,
                    });
                }
            }
        }
    }
}

impl KeyboardHandler for WaylandState {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        self.focused_keyboard_surface = self.surface_by_obj.get(&surface.id()).copied();
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        if self.focused_keyboard_surface == self.surface_by_obj.get(&surface.id()).copied() {
            self.focused_keyboard_surface = None;
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: SctkKeyEvent,
    ) {
        if let Some(key) = self.focused_keyboard_surface {
            if let Some(seat) = self.seat_state.seats().next()
                && let Some(surf) = self.surfaces.get(&key)
            {
                self.last_input_serial = Some((seat, serial, surf.role.wl_surface().clone()));
            }
            self.events.push_back(WaylandEvent::KeyboardKey {
                key,
                pressed: true,
                repeat: false,
                logical_key: map_key_event(&event),
                modifiers: self.modifiers,
                text: event.utf8,
            });
        }
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: SctkKeyEvent,
    ) {
        if let Some(key) = self.focused_keyboard_surface {
            self.events.push_back(WaylandEvent::KeyboardKey {
                key,
                pressed: true,
                repeat: true,
                logical_key: map_key_event(&event),
                modifiers: self.modifiers,
                text: event.utf8,
            });
        }
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: SctkKeyEvent,
    ) {
        if let Some(key) = self.focused_keyboard_surface {
            self.events.push_back(WaylandEvent::KeyboardKey {
                key,
                pressed: false,
                repeat: false,
                logical_key: map_key_event(&event),
                modifiers: self.modifiers,
                text: None,
            });
        }
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: SctkModifiers,
        _raw_modifiers: RawModifiers,
        layout: u32,
    ) {
        self.modifiers = map_modifiers(modifiers);
        if self.keyboard_layout_index != layout {
            self.keyboard_layout_index = layout;
            self.events.push_back(WaylandEvent::KeyboardLayoutChanged(
                self.keyboard_layout_info(),
            ));
        }
    }

    fn update_keymap(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        keymap: Keymap<'_>,
    ) {
        let layouts = crate::input::keyboard_layout::parse_xkb_layouts(&keymap.as_string());
        if !layouts.is_empty() || !self.keyboard_layouts.is_empty() {
            self.keyboard_layouts = layouts;
            self.events.push_back(WaylandEvent::KeyboardLayoutChanged(
                self.keyboard_layout_info(),
            ));
        }
    }
}

impl TouchHandler for WaylandState {
    fn down(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        serial: u32,
        _time: u32,
        surface: wl_surface::WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        if let Some(&key) = self.surface_by_obj.get(&surface.id()) {
            if let Some(seat) = self.seat_state.seats().next() {
                self.last_input_serial = Some((seat, serial, surface));
            }
            let x = position.0 as f32;
            let y = position.1 as f32;
            let pos = Point::new(LogicalPx(x), LogicalPx(y));
            self.active_touches.insert(
                id,
                ActiveTouch {
                    key,
                    x,
                    y,
                    shape: None,
                    orientation: None,
                },
            );
            self.events.push_back(WaylandEvent::Touch {
                key,
                event: TouchEvent {
                    id,
                    phase: TouchPhase::Down,
                    position: pos,
                    local_position: pos,
                    shape: None,
                    orientation: None,
                    modifiers: self.modifiers,
                },
            });
        }
    }

    fn up(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        _serial: u32,
        _time: u32,
        id: i32,
    ) {
        if let Some(active) = self.active_touches.remove(&id) {
            let pos = Point::new(LogicalPx(active.x), LogicalPx(active.y));
            self.events.push_back(WaylandEvent::Touch {
                key: active.key,
                event: TouchEvent {
                    id,
                    phase: TouchPhase::Up,
                    position: pos,
                    local_position: pos,
                    shape: active.shape,
                    orientation: active.orientation,
                    modifiers: self.modifiers,
                },
            });
        }
    }

    fn motion(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        _time: u32,
        id: i32,
        position: (f64, f64),
    ) {
        if let Some(active) = self.active_touches.get_mut(&id) {
            active.x = position.0 as f32;
            active.y = position.1 as f32;
            let pos = Point::new(LogicalPx(active.x), LogicalPx(active.y));
            let key = active.key;
            let shape = active.shape;
            let orientation = active.orientation;
            self.events.push_back(WaylandEvent::Touch {
                key,
                event: TouchEvent {
                    id,
                    phase: TouchPhase::Move,
                    position: pos,
                    local_position: pos,
                    shape,
                    orientation,
                    modifiers: self.modifiers,
                },
            });
        }
    }

    fn shape(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        id: i32,
        major: f64,
        minor: f64,
    ) {
        let shape = Some((major as f32, minor as f32));
        if let Some(active) = self.active_touches.get_mut(&id) {
            active.shape = shape;
        }
        // Enrich any same-frame queued Touch event for this contact atomically.
        for ev in self.events.iter_mut().rev() {
            if let WaylandEvent::Touch { event, .. } = ev
                && event.id == id
            {
                event.shape = shape;
                break;
            }
        }
    }

    fn orientation(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        id: i32,
        orientation: f64,
    ) {
        let orient = Some(orientation as f32);
        if let Some(active) = self.active_touches.get_mut(&id) {
            active.orientation = orient;
        }
        // Enrich any same-frame queued Touch event for this contact atomically.
        for ev in self.events.iter_mut().rev() {
            if let WaylandEvent::Touch { event, .. } = ev
                && event.id == id
            {
                event.orientation = orient;
                break;
            }
        }
    }

    fn cancel(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _touch: &wl_touch::WlTouch) {
        let modifiers = self.modifiers;
        let cancelled: Vec<(i32, ActiveTouch)> = self.active_touches.drain().collect();
        for (id, active) in cancelled {
            let pos = Point::new(LogicalPx(active.x), LogicalPx(active.y));
            self.events.push_back(WaylandEvent::Touch {
                key: active.key,
                event: TouchEvent {
                    id,
                    phase: TouchPhase::Cancel,
                    position: pos,
                    local_position: pos,
                    shape: active.shape,
                    orientation: active.orientation,
                    modifiers,
                },
            });
        }
    }
}
