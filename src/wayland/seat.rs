use smithay_client_toolkit::seat::keyboard::{
    KeyEvent as SctkKeyEvent, Modifiers as SctkModifiers,
};
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, PointerEvent as SctkPointerEvent, PointerEventKind,
};

use crate::input::{Key, Modifiers, MouseButton};

/// Converts SCTK XKB modifiers into WayWi [`Modifiers`].
#[must_use]
pub(crate) fn map_modifiers(mods: SctkModifiers) -> Modifiers {
    let mut out = Modifiers::empty();
    if mods.ctrl {
        out |= Modifiers::CTRL;
    }
    if mods.alt {
        out |= Modifiers::ALT;
    }
    if mods.shift {
        out |= Modifiers::SHIFT;
    }
    if mods.logo {
        out |= Modifiers::SUPER;
    }
    out
}

/// Converts SCTK XKB keysym event into WayWi [`Key`].
#[must_use]
pub(crate) fn map_key_event(ev: &SctkKeyEvent) -> Key {
    Key::from_keysym(ev.keysym.raw())
}

/// Converts Linux input button code from `wl_pointer` into [`MouseButton`].
#[must_use]
pub(crate) fn map_button(button: u32) -> MouseButton {
    MouseButton::from_linux_button(button)
}

/// Normalizes SCTK `AxisScroll` step values (preferring v120 discrete steps or smooth delta).
#[must_use]
pub(crate) fn scroll_delta(axis: &AxisScroll) -> (f32, i32) {
    let delta = if axis.value120 != 0 {
        axis.value120 as f32 / 120.0
    } else if axis.discrete != 0 {
        axis.discrete as f32
    } else {
        axis.absolute as f32 / 10.0
    };
    let discrete = if axis.discrete != 0 {
        axis.discrete
    } else if axis.value120 != 0 {
        axis.value120 / 120
    } else {
        0
    };
    (delta, discrete)
}

#[derive(Debug, Clone)]
pub(crate) enum MappedPointerAction {
    Enter {
        x: f32,
        y: f32,
    },
    Leave,
    Motion {
        x: f32,
        y: f32,
    },
    Button {
        x: f32,
        y: f32,
        button: MouseButton,
        pressed: bool,
    },
    Scroll {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
        discrete_x: i32,
        discrete_y: i32,
    },
}

/// Translates a batch of SCTK pointer events into normalized surface-local actions.
pub(crate) fn map_pointer_event(ev: &SctkPointerEvent) -> MappedPointerAction {
    let x = ev.position.0 as f32;
    let y = ev.position.1 as f32;
    match &ev.kind {
        PointerEventKind::Enter { .. } => MappedPointerAction::Enter { x, y },
        PointerEventKind::Leave { .. } => MappedPointerAction::Leave,
        PointerEventKind::Motion { .. } => MappedPointerAction::Motion { x, y },
        PointerEventKind::Press { button, .. } => MappedPointerAction::Button {
            x,
            y,
            button: map_button(*button),
            pressed: true,
        },
        PointerEventKind::Release { button, .. } => MappedPointerAction::Button {
            x,
            y,
            button: map_button(*button),
            pressed: false,
        },
        PointerEventKind::Axis {
            horizontal,
            vertical,
            ..
        } => {
            let (dx, discrete_x) = scroll_delta(horizontal);
            let (dy, discrete_y) = scroll_delta(vertical);
            MappedPointerAction::Scroll {
                x,
                y,
                dx,
                dy,
                discrete_x,
                discrete_y,
            }
        }
    }
}
