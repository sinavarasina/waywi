use smithay_client_toolkit::seat::keyboard::{
    KeyEvent as SctkKeyEvent, Modifiers as SctkModifiers,
};
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, PointerEvent as SctkPointerEvent, PointerEventKind,
};
use wayland_client::protocol::wl_pointer;

use crate::input::{Key, Modifiers, MouseButton, ScrollSource};

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

/// Maps Wayland `wl_pointer::AxisSource` into [`ScrollSource`].
#[must_use]
pub(crate) fn map_scroll_source(source: Option<wl_pointer::AxisSource>) -> ScrollSource {
    match source {
        Some(wl_pointer::AxisSource::Wheel) => ScrollSource::Wheel,
        Some(wl_pointer::AxisSource::Finger) => ScrollSource::Finger,
        Some(wl_pointer::AxisSource::Continuous) => ScrollSource::Continuous,
        Some(wl_pointer::AxisSource::WheelTilt) => ScrollSource::WheelTilt,
        _ => ScrollSource::Unknown,
    }
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
        source: ScrollSource,
        stop_x: bool,
        stop_y: bool,
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
            source,
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
                source: map_scroll_source(*source),
                stop_x: horizontal.stop,
                stop_y: vertical.stop,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_source_and_axis_delta_mapping() {
        assert_eq!(
            map_scroll_source(Some(wl_pointer::AxisSource::Wheel)),
            ScrollSource::Wheel
        );
        assert_eq!(
            map_scroll_source(Some(wl_pointer::AxisSource::Finger)),
            ScrollSource::Finger
        );
        assert_eq!(
            map_scroll_source(Some(wl_pointer::AxisSource::Continuous)),
            ScrollSource::Continuous
        );
        assert_eq!(
            map_scroll_source(Some(wl_pointer::AxisSource::WheelTilt)),
            ScrollSource::WheelTilt
        );
        assert_eq!(map_scroll_source(None), ScrollSource::Unknown);

        let smooth = AxisScroll {
            absolute: 25.0,
            discrete: 0,
            stop: false,
            value120: 0,
            ..AxisScroll::default()
        };
        assert_eq!(scroll_delta(&smooth), (2.5, 0));

        let stop_only = AxisScroll {
            absolute: 0.0,
            discrete: 0,
            stop: true,
            value120: 0,
            ..AxisScroll::default()
        };
        assert_eq!(scroll_delta(&stop_only), (0.0, 0));
    }
}
