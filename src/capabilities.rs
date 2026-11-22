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

    /// Iterates over all currently supported capabilities.
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.supported.iter().copied()
    }
}
