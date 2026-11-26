use std::collections::BTreeSet;

use crate::error::{Result, WaywiError};

/// Capabilities detected at compile time and probed from the Wayland/desktop session at startup.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Active Wayland compositor connection.
    Wayland,
    /// `zwlr_layer_shell_v1` protocol for panels, compact floating surfaces, and overlays.
    LayerShell,
    /// `xdg_wm_base` protocol for explicit fallback toplevel windows and `xdg_popup` menus.
    XdgShell,
    /// `xdg_activation_v1` protocol for Wayland focus-handoff activation tokens.
    XdgActivation,
    /// `wp_fractional_scale_v1` + `wp_viewporter` fractional scaling support.
    FractionalScale,
    /// `wp_viewporter` destination scaling support.
    Viewporter,
    /// Wayland seat pointer input capability.
    PointerInput,
    /// Wayland seat keyboard input capability.
    KeyboardInput,
    /// Wayland seat touch input capability.
    TouchInput,
    /// CPU software renderer (`renderer-software` feature).
    SoftwareRenderer,
    /// GPU hardware/WGPU vector renderer (`renderer-gpu` feature + available GPU adapter).
    GpuRenderer,
    /// Text shaping and rasterization (`text` feature).
    Text,
    /// PNG decoding (`image-png` feature).
    ImagePng,
    /// SVG parsing and rasterization (`image-svg` feature).
    ImageSvg,
    /// Animated GIF decoding (`image-gif` feature).
    ImageGif,
    /// Keyed sprite/frame animation scheduler (`animation` feature).
    Animation,
    /// Single-instance local socket and POSIX signal triggers (`control-ipc` feature).
    ControlIpc,
    /// Hyprland IPC connectivity (`hyprland-ipc` feature).
    HyprlandIpc,
    /// Sway IPC connectivity (`sway-ipc` feature).
    SwayIpc,
    /// PipeWire audio backend (`pipewire` feature).
    PipeWire,
    /// PulseAudio client backend (`pulseaudio` feature).
    PulseAudio,
    /// ALSA mixer backend (`alsa` feature).
    Alsa,
    /// D-Bus integration (`dbus` feature).
    Dbus,
    /// XDG Desktop Portal global shortcuts (`global-shortcuts` feature + portal support).
    GlobalShortcuts,
    /// AT-SPI2 / AccessKit screen-reader accessibility bridge (`accessibility` feature).
    Accessibility,
    /// WebP image decoding (`image-webp` feature).
    ImageWebp,
    /// Audio playback engine (compressed codecs via `audio-playback` or native backend PCM output).
    AudioPlayback,
    /// Wayland clipboard / primary-selection data-control manager (`clipboard` feature + compositor support).
    Clipboard,
    /// Wayland `ext-idle-notify-v1` user idle monitor (`idle-notify` feature + compositor support).
    IdleNotify,
    /// Wayland `zwp_text_input_manager_v3` IME composition and preedit support (`ime` feature + compositor support).
    TextInput,
    /// Active seat XKB keyboard layout inspection.
    KeyboardLayout,
    /// Wayland `zwp_pointer_gestures_v1` multi-finger touchpad gestures (`pointer-gestures` feature + compositor support).
    PointerGestures,
    /// Wayland `zwp_tablet_manager_v2` graphics tablet / stylus / pad support (`tablet` feature + compositor support).
    TabletInput,
    /// Wayland `wp_cursor_shape_v1` server-side cursor shapes (`cursor-shape` feature + compositor support).
    CursorShape,
    /// Wayland `ext_session_lock_v1` native session lockscreen protocol (`session-lock` feature + compositor support).
    SessionLock,
    /// Wayland screen/output pixel capture (`ext_image_copy_capture_v1` or `zwlr_screencopy_manager_v1`, `screencopy` feature).
    Screencopy,
    /// Wayland foreign toplevel window listing (`zwlr_foreign_toplevel_management_v1` or `ext_foreign_toplevel_list_v1`, `foreign-toplevel` feature).
    ForeignToplevel,
    /// Wayland foreign toplevel window control actions (`zwlr_foreign_toplevel_management_v1`, `foreign-toplevel` feature).
    ForeignToplevelControl,
    /// Wayland `ext_workspace_v1` cross-compositor workspace protocol (`ext-workspace` feature + compositor support).
    ExtWorkspace,
    /// Core Wayland `wl_data_device_manager` Drag & Drop support.
    DragAndDrop,
    /// Wayland `zxdg_decoration_manager_v1` server/client window decoration negotiation.
    XdgDecoration,
    /// Wayland `zwp_idle_inhibit_manager_v1` per-surface idle inhibition (`idle-inhibit` feature + compositor support).
    IdleInhibit,
    /// Wayland `zwp_virtual_keyboard_v1` virtual keyboard input injection (`virtual-keyboard` feature + compositor support).
    VirtualKeyboard,
    /// Wayland `zwp_pointer_constraints_v1` pointer lock and confinement (`pointer-constraints` feature + compositor support).
    PointerConstraints,
    /// Wayland `zwp_relative_pointer_v1` unaccelerated relative pointer deltas (`pointer-constraints` feature + compositor support).
    RelativePointer,
}

/// Snapshot of runtime and compile-time capabilities exposed by [`App`](crate::app::App) and
/// [`EffectCtx`](crate::effect::EffectCtx).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    supported: BTreeSet<Capability>,
}

impl Capabilities {
    /// Creates an empty capability registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a capability set populated with features compiled into this build.
    #[must_use]
    pub fn from_compile_features() -> Self {
        let mut caps = Self::new();
        caps.set(
            Capability::SoftwareRenderer,
            cfg!(feature = "renderer-software"),
        );
        caps.set(Capability::GpuRenderer, cfg!(feature = "renderer-gpu"));
        caps.set(Capability::Text, cfg!(feature = "text"));
        caps.set(Capability::ImagePng, cfg!(feature = "image-png"));
        caps.set(Capability::ImageSvg, cfg!(feature = "image-svg"));
        caps.set(Capability::ImageGif, cfg!(feature = "image-gif"));
        caps.set(Capability::ImageWebp, cfg!(feature = "image-webp"));
        caps.set(Capability::Animation, cfg!(feature = "animation"));
        caps.set(Capability::ControlIpc, cfg!(feature = "control-ipc"));
        caps.set(Capability::PipeWire, cfg!(feature = "pipewire"));
        caps.set(Capability::PulseAudio, cfg!(feature = "pulseaudio"));
        caps.set(Capability::Alsa, cfg!(feature = "alsa"));
        caps.set(
            Capability::AudioPlayback,
            cfg!(any(
                feature = "audio-playback",
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa"
            )),
        );
        caps.set(Capability::Dbus, cfg!(feature = "dbus"));
        caps.set(Capability::Accessibility, cfg!(feature = "accessibility"));
        caps
    }

    /// Returns `true` if `cap` is supported and available in the current session.
    #[must_use]
    pub fn has(&self, cap: Capability) -> bool {
        self.supported.contains(&cap)
    }

    /// Enables or disables a capability flag.
    pub fn set(&mut self, cap: Capability, enabled: bool) {
        if enabled {
            self.supported.insert(cap);
        } else {
            self.supported.remove(&cap);
        }
    }

    /// Returns `Ok(())` if `cap` is available, or `Err(WaywiError::UnsupportedCapability(cap))`.
    pub fn require(&self, cap: Capability) -> Result<()> {
        if self.has(cap) {
            Ok(())
        } else {
            Err(WaywiError::UnsupportedCapability(cap))
        }
    }

    /// Updates Wayland protocol and seat capabilities from a live [`Capabilities`] snapshot
    /// without clobbering runtime-probed IPC, GPU, audio, or accessibility flags.
    pub(crate) fn refresh_wayland_from(&mut self, wl_caps: &Self) {
        const WAYLAND_CAPS: [Capability; 27] = [
            Capability::Wayland,
            Capability::LayerShell,
            Capability::XdgShell,
            Capability::XdgActivation,
            Capability::FractionalScale,
            Capability::Viewporter,
            Capability::PointerInput,
            Capability::KeyboardInput,
            Capability::KeyboardLayout,
            Capability::TouchInput,
            Capability::PointerGestures,
            Capability::TabletInput,
            Capability::IdleNotify,
            Capability::Clipboard,
            Capability::TextInput,
            Capability::CursorShape,
            Capability::SessionLock,
            Capability::Screencopy,
            Capability::ForeignToplevel,
            Capability::ForeignToplevelControl,
            Capability::ExtWorkspace,
            Capability::DragAndDrop,
            Capability::XdgDecoration,
            Capability::IdleInhibit,
            Capability::VirtualKeyboard,
            Capability::PointerConstraints,
            Capability::RelativePointer,
        ];
        for cap in WAYLAND_CAPS {
            self.set(cap, wl_caps.has(cap));
        }
    }

    /// Iterates over all currently supported capabilities.
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.supported.iter().copied()
    }
}
