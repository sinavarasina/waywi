use std::path::Path;
use std::time::Duration;

use crate::asset::{AssetCache, AssetId, AssetRetention};
use crate::capabilities::{Capabilities, Capability};
use crate::display::OutputInfo;
use crate::error::{Result, WaywiError};
use crate::image::ImageAsset;
use crate::surface::{IntoSurfaceKey, SurfaceKey};
use crate::system::{PrivacySnapshot, SystemProviders};

use super::worker::{
    AsyncAssetWorker, BoxedActivationCallback, BoxedAssetCallback, BoxedMsgFactory,
};

#[cfg(feature = "clipboard")]
pub(crate) type ClipboardSetter<'a> =
    &'a mut dyn FnMut(crate::clipboard::ClipboardSelection, Vec<(String, Vec<u8>)>) -> Result<()>;

#[cfg(feature = "text")]
pub(crate) type TextCursorMeasurer<'a> = &'a mut dyn FnMut(
    &crate::text::SharedStr,
    crate::text::TextMeasureStyle<'_>,
    usize,
) -> crate::text::TextCursorMetrics;

/// Runtime context passed into `Application::init` and `Application::update`.
///
/// Gives controlled, feature- and capability-honest access to app shutdown, output discovery,
/// image/sound asset loading and playback, brightness/privacy helpers, clipboard writes,
/// imperative one-shot delays, system samplers, and desktop IPC backends.
pub struct EffectCtx<'a> {
    pub(crate) quit_requested: &'a mut bool,
    pub(crate) capabilities: &'a Capabilities,
    pub(crate) outputs: &'a [OutputInfo],
    pub(crate) assets: &'a mut AssetCache,
    pub(crate) asset_worker: &'a AsyncAssetWorker,
    pub(crate) system: &'a mut SystemProviders,
    pub(crate) pending_after: &'a mut Vec<(Duration, BoxedMsgFactory)>,
    pub(crate) reloaded_assets: &'a mut Vec<(AssetId, u64)>,
    pub(crate) pending_activation_requests: &'a mut Vec<(Option<String>, BoxedActivationCallback)>,
    pub(crate) pending_surface_activations: &'a mut Vec<(SurfaceKey, String)>,

    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    pub(crate) audio: &'a crate::audio::AudioClient,
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub(crate) player: &'a crate::audio::AudioPlayer,
    #[cfg(feature = "hyprland-ipc")]
    pub(crate) hyprland: Option<&'a crate::hyprland::HyprlandClient>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway: Option<&'a crate::sway::SwayClient>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus: &'a crate::dbus::DbusClient,
    #[cfg(feature = "clipboard")]
    pub(crate) clipboard_writer: Option<ClipboardSetter<'a>>,
    #[cfg(feature = "text")]
    pub(crate) text_cursor_measurer: TextCursorMeasurer<'a>,
}

impl<'a> EffectCtx<'a> {
    /// Requests clean application shutdown at the end of the current update cycle.
    pub fn quit(&mut self) {
        *self.quit_requested = true;
    }

    /// Returns detected compile-time and live session capabilities.
    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        self.capabilities
    }

    /// Returns metadata for all currently connected Wayland outputs.
    #[must_use]
    pub fn outputs(&self) -> &[OutputInfo] {
        self.outputs
    }

    /// Returns a mutable handle to shared Linux system metrics/hardware providers.
    pub fn system(&mut self) -> &mut SystemProviders {
        self.system
    }

    /// Loads and caches an image asset (`PNG` / `SVG` / `GIF` per enabled Cargo features) from
    /// disk synchronously, deduplicating by canonical path and content hash.
    pub fn load_image(&mut self, path: impl AsRef<Path>) -> Result<ImageAsset> {
        self.assets.load_file(path)
    }

    /// Loads and caches an image asset from disk, ensuring vector (`SVG`) sources are rasterized
    /// so their longest edge is at least `min_svg_px` pixels for crisp downscaling.
    pub fn load_image_sized(
        &mut self,
        path: impl AsRef<Path>,
        min_svg_px: u32,
    ) -> Result<ImageAsset> {
        self.assets.load_file_sized(path, min_svg_px)
    }

    /// Loads and caches an image asset from encoded bytes (for `include_bytes!`, embedded icons,
    /// etc.).
    pub fn load_image_bytes(&mut self, bytes: &[u8]) -> Result<ImageAsset> {
        self.assets.load_bytes(bytes)
    }

    /// Loads and caches an image asset from encoded bytes, rasterizing vector (`SVG`) sources to
    /// at least `min_svg_px` pixels on the longest edge.
    pub fn load_image_bytes_sized(&mut self, bytes: &[u8], min_svg_px: u32) -> Result<ImageAsset> {
        self.assets.load_bytes_sized(bytes, min_svg_px)
    }

    /// Loads and caches a raw straight RGB/RGBA pixel buffer (such as XDG desktop notification
    /// `image-data` hints) without requiring encoded PNG/SVG headers.
    pub fn load_rgba_image(
        &mut self,
        width: u32,
        height: u32,
        rowstride: usize,
        has_alpha: bool,
        data: &[u8],
    ) -> Result<ImageAsset> {
        self.assets
            .insert_raw_pixels(width, height, rowstride, has_alpha, data)
    }

    /// Dispatches image reading and decoding to WayWi's background asset worker thread and delivers
    /// the constructed message when ready.
    pub fn load_image_async<M: Send + 'static>(
        &mut self,
        path: impl AsRef<Path>,
        map: impl FnOnce(Result<ImageAsset>) -> M + Send + 'static,
    ) {
        let path = path.as_ref().to_path_buf();
        let boxed: BoxedAssetCallback = Box::new(move |res| Box::new(map(res)));
        self.asset_worker.submit(path, boxed);
    }

    /// Reloads `asset` from `path`, updating its revision counter so dependent surfaces invalidate
    /// and redraw cleanly.
    pub fn reload_asset(&mut self, asset: ImageAsset, path: impl AsRef<Path>) -> Result<()> {
        let updated = self.assets.reload(asset, path)?;
        let rev = self.assets.revision_of(updated.id);
        self.reloaded_assets.push((updated.id, rev));
        Ok(())
    }

    /// Configures the decoded pixel memory retention policy for `asset`.
    ///
    /// - [`AssetRetention::KeepDecoded`] (default): keeps decoded premultiplied RGBA8 pixels
    ///   resident in RAM for immediate redraws.
    /// - [`AssetRetention::ReclaimAfterPresent`]: releases the decoded RGBA8 pixel buffer after
    ///   presenting frames that reference `asset` (e.g., static 4K wallpapers), while retaining
    ///   the [`ImageAsset`] handle, dimensions, revision, and backing disk path or compressed
    ///   bytes so future redraws (output resize, hotplug, scale change) transparently re-decode
    ///   on demand.
    ///
    /// Returns `false` if `asset` is unknown/unloaded, or if
    /// [`AssetRetention::ReclaimAfterPresent`] is requested for a raw pixel asset created via
    /// [`Self::load_rgba_image`] (which has no compressed or disk backing source).
    pub fn set_asset_retention(&mut self, asset: ImageAsset, retention: AssetRetention) -> bool {
        self.assets.set_retention(asset.id, retention)
    }

    /// Returns the active [`AssetRetention`] policy for `asset`, or `None` if unloaded.
    #[must_use]
    pub fn asset_retention(&self, asset: ImageAsset) -> Option<AssetRetention> {
        self.assets.retention(asset.id)
    }

    /// Immediately frees the decoded RGBA8 pixel buffer for `asset` while keeping its
    /// [`ImageAsset`] handle, dimensions, revision, and backing disk path or compressed bytes
    /// intact.
    ///
    /// Unlike [`Self::unload_asset`], any future redraw referencing `asset` (such as a sprite
    /// sheet or popup becoming visible again, or a surface resizing) transparently re-decodes the
    /// pixels on demand.
    ///
    /// Returns `true` if decoded pixels were resident and freed; returns `false` if `asset` is
    /// unknown, already evicted, or backed only by raw pixels ([`Self::load_rgba_image`]).
    pub fn evict_asset_pixels(&mut self, asset: ImageAsset) -> bool {
        self.assets.evict_decoded_pixels(asset.id)
    }

    /// Removes `asset` permanently from the internal asset cache to reclaim all associated memory.
    ///
    /// Once unloaded, the handle can no longer be rendered unless reloaded into a fresh asset.
    /// Prefer [`Self::set_asset_retention`] or [`Self::evict_asset_pixels`] when the asset may
    /// still need to redraw later.
    pub fn unload_asset(&mut self, asset: ImageAsset) -> bool {
        self.assets.unload(asset.id)
    }

    /// Schedules an imperative one-shot message to be delivered after `delay`.
    ///
    /// Prefer [`Timer::once`](crate::timer::Timer::once) in `subscriptions` when cancellation or
    /// reset-on-rekey semantics are needed.
    pub fn after<M: Send + 'static>(
        &mut self,
        delay: Duration,
        make_msg: impl FnOnce() -> M + Send + 'static,
    ) {
        self.pending_after
            .push((delay, Box::new(move || Box::new(make_msg()))));
    }

    /// Requests a Wayland `xdg_activation_v1` focus-handoff token asynchronously (non-blocking).
    ///
    /// When the compositor issues the token, `map(Ok(token))` is delivered to
    /// [`Application::update`](crate::app::Application::update), where it can be passed to
    /// [`DesktopEntry::launch_with_token`](crate::desktop::DesktopEntry::launch_with_token) or
    /// [`Self::activate_surface`]. If the compositor does not advertise `xdg_activation_v1`,
    /// `map(Err(WaywiError::UnsupportedCapability(Capability::XdgActivation)))` is delivered.
    pub fn request_activation_token<M: Send + 'static>(
        &mut self,
        app_id: Option<&str>,
        map: impl FnOnce(Result<String>) -> M + Send + 'static,
    ) {
        let app_id = app_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        self.pending_activation_requests
            .push((app_id, Box::new(move |res| Box::new(map(res)))));
    }

    /// Requests compositor activation/focus for a live WayWi surface using a Wayland
    /// `xdg_activation_v1` token (e.g. received from another client or
    /// [`Self::request_activation_token`]).
    pub fn activate_surface(
        &mut self,
        surface_key: impl IntoSurfaceKey,
        token: impl Into<String>,
    ) -> Result<()> {
        self.capabilities.require(Capability::XdgActivation)?;
        let token = token.into();
        if token.trim().is_empty() {
            return Err(WaywiError::InvalidState(
                "xdg_activation_v1 token must not be empty".to_owned(),
            ));
        }
        self.pending_surface_activations
            .push((surface_key.into_surface_key(), token));
        Ok(())
    }

    /// Accesses the active audio control handle (`set_volume_linear`, `toggle_mute`, etc.).
    #[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
    pub fn audio(&self) -> Result<&crate::audio::AudioClient> {
        if self.capabilities.has(Capability::PipeWire)
            || self.capabilities.has(Capability::PulseAudio)
            || self.capabilities.has(Capability::Alsa)
        {
            return Ok(self.audio);
        }
        #[cfg(feature = "pipewire")]
        {
            Err(WaywiError::UnsupportedCapability(Capability::PipeWire))
        }
        #[cfg(all(not(feature = "pipewire"), feature = "pulseaudio"))]
        {
            Err(WaywiError::UnsupportedCapability(Capability::PulseAudio))
        }
        #[cfg(all(
            not(feature = "pipewire"),
            not(feature = "pulseaudio"),
            feature = "alsa"
        ))]
        {
            Err(WaywiError::UnsupportedCapability(Capability::Alsa))
        }
    }

    /// Accesses the Hyprland IPC client when running under Hyprland.
    #[cfg(feature = "hyprland-ipc")]
    pub fn hyprland(&self) -> Result<&crate::hyprland::HyprlandClient> {
        self.capabilities.require(Capability::HyprlandIpc)?;
        self.hyprland
            .ok_or(WaywiError::UnsupportedCapability(Capability::HyprlandIpc))
    }

    /// Accesses the Sway IPC client when running under Sway / i3-compatible socket.
    #[cfg(feature = "sway-ipc")]
    pub fn sway(&self) -> Result<&crate::sway::SwayClient> {
        self.capabilities.require(Capability::SwayIpc)?;
        self.sway
            .ok_or(WaywiError::UnsupportedCapability(Capability::SwayIpc))
    }

    /// Accesses the optional neutral [`CompositorClient`](crate::compositor::CompositorClient)
    /// over whichever compositor IPC backend is active.
    #[cfg(any(feature = "hyprland-ipc", feature = "sway-ipc"))]
    pub fn compositor(&self) -> Result<crate::compositor::CompositorClient<'_>> {
        #[cfg(feature = "hyprland-ipc")]
        if let Some(h) = self.hyprland
            && self.capabilities.has(Capability::HyprlandIpc)
        {
            return Ok(crate::compositor::CompositorClient {
                hyprland: Some(h),
                #[cfg(feature = "sway-ipc")]
                sway: self.sway,
                _marker: std::marker::PhantomData,
            });
        }
        #[cfg(feature = "sway-ipc")]
        if let Some(s) = self.sway
            && self.capabilities.has(Capability::SwayIpc)
        {
            return Ok(crate::compositor::CompositorClient {
                #[cfg(feature = "hyprland-ipc")]
                hyprland: None,
                sway: Some(s),
                _marker: std::marker::PhantomData,
            });
        }
        #[cfg(feature = "hyprland-ipc")]
        {
            Err(WaywiError::UnsupportedCapability(Capability::HyprlandIpc))
        }
        #[cfg(all(not(feature = "hyprland-ipc"), feature = "sway-ipc"))]
        {
            Err(WaywiError::UnsupportedCapability(Capability::SwayIpc))
        }
    }

    /// Accesses the D-Bus session/system query and desktop service client (MPRIS, notifications,
    /// system tray, NetworkManager, PowerProfiles, UPower, logind, BlueZ, ModemManager, and raw
    /// queries/calls).
    #[cfg(feature = "dbus")]
    pub fn dbus(&self) -> Result<&crate::dbus::DbusClient> {
        self.capabilities.require(Capability::Dbus)?;
        Ok(self.dbus)
    }

    /// Accesses the audio player when playback support is compiled in and enabled.
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn player(&self) -> Result<&crate::audio::AudioPlayer> {
        self.capabilities.require(Capability::AudioPlayback)?;
        Ok(self.player)
    }

    /// Loads and caches a sound asset from disk (WAV always; OGG/MP3/FLAC with `audio-playback`).
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn load_sound(&self, path: impl AsRef<Path>) -> Result<crate::audio::SoundAsset> {
        self.player.load_file(path)
    }

    /// Loads and caches an encoded sound buffer from memory.
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn load_sound_bytes(&self, bytes: &[u8]) -> Result<crate::audio::SoundAsset> {
        self.player.load_bytes(bytes)
    }

    /// Loads and caches a procedural UI sound preset without external files.
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    #[must_use]
    pub fn load_sound_preset(&self, preset: crate::audio::SoundPreset) -> crate::audio::SoundAsset {
        self.player.load_preset(preset)
    }

    /// Resolves and caches a freedesktop XDG Sound Theme event sound (e.g. `"message-new-instant"`).
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn load_system_sound(&self, name: &str) -> Result<crate::audio::SoundAsset> {
        self.player.load_system_sound(name)
    }

    /// Plays a cached [`SoundAsset`](crate::audio::SoundAsset) asynchronously on the SFX worker.
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn play_sound(&self, sound: crate::audio::SoundAsset) {
        self.player.play(sound);
    }

    /// Plays a cached [`SoundAsset`](crate::audio::SoundAsset) scaled by `volume` (`0.0..=2.0`).
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn play_sound_volume(&self, sound: crate::audio::SoundAsset, volume: f32) {
        self.player.play_with_volume(sound, volume);
    }

    /// Plays a built-in synthesized UI sound effect preset at `volume` (`0.0..=2.0`).
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn play_sound_preset(&self, preset: crate::audio::SoundPreset, volume: f32) {
        self.player.play_preset(preset, volume);
    }

    /// Resolves and plays an XDG Sound Theme event sound at `volume`.
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn play_system_sound(&self, name: &str, volume: f32) -> Result<()> {
        self.player.play_system_sound(name, volume)
    }

    /// Stops all currently active SFX voices immediately.
    #[cfg(any(
        feature = "pipewire",
        feature = "pulseaudio",
        feature = "alsa",
        feature = "audio-playback"
    ))]
    pub fn stop_sounds(&self) {
        self.player.stop_all();
    }

    /// Sets laptop backlight brightness (`0.0..=100.0` %) via the cached `systemd-logind`
    /// D-Bus client (if available) or `/sys/class/backlight`.
    pub fn set_backlight_percent(&self, device_name: &str, percent: f32) -> Result<()> {
        let raw = crate::system::hardware::backlight_percent_to_raw(device_name, percent)?;
        #[cfg(feature = "dbus")]
        if let Ok(val32) = u32::try_from(raw)
            && self
                .dbus
                .set_backlight_brightness("backlight", device_name, val32)
                .is_ok()
        {
            return Ok(());
        }
        crate::system::hardware::write_backlight_sysfs_raw(device_name, raw)
    }

    /// Schedules a non-blocking DDC/CI VCP `0x10` Luminance write (`0.0..=100.0` %) for an external
    /// monitor I2C bus.
    pub fn set_ddc_brightness(&mut self, i2c_bus: u32, percent: f32) {
        self.system.set_ddc_brightness(i2c_bus, percent);
    }

    /// Samples currently active camera, microphone, audio-capture, screen-share, and location
    /// hardware access consumers.
    pub fn privacy_snapshot(&mut self) -> PrivacySnapshot {
        self.system.privacy_snapshot()
    }

    /// Publishes UTF-8 text to the Wayland standard clipboard (`ClipboardSelection::Clipboard`).
    pub fn set_clipboard_text(&mut self, text: impl Into<String>) -> Result<()> {
        let s = text.into();
        let bytes = s.into_bytes();
        self.write_clipboard_entries(
            true,
            vec![
                ("text/plain;charset=utf-8".to_owned(), bytes.clone()),
                ("UTF8_STRING".to_owned(), bytes.clone()),
                ("text/plain".to_owned(), bytes),
            ],
        )
    }

    /// Publishes UTF-8 text to the Wayland primary selection (`ClipboardSelection::Primary`).
    pub fn set_clipboard_primary_text(&mut self, text: impl Into<String>) -> Result<()> {
        let s = text.into();
        let bytes = s.into_bytes();
        self.write_clipboard_entries(
            false,
            vec![
                ("text/plain;charset=utf-8".to_owned(), bytes.clone()),
                ("UTF8_STRING".to_owned(), bytes.clone()),
                ("text/plain".to_owned(), bytes),
            ],
        )
    }

    /// Publishes arbitrary `(mime_type, payload)` entries (or clears with an empty vector) to the
    /// requested Wayland selection buffer.
    #[cfg(feature = "clipboard")]
    pub fn set_clipboard(
        &mut self,
        selection: crate::clipboard::ClipboardSelection,
        entries: Vec<(String, Vec<u8>)>,
    ) -> Result<()> {
        self.capabilities.require(Capability::Clipboard)?;
        let Some(writer) = self.clipboard_writer.as_deref_mut() else {
            return Err(WaywiError::UnsupportedCapability(Capability::Clipboard));
        };
        writer(selection, entries)
    }

    #[cfg(feature = "text")]
    /// Measures shaped text bounds and exact cursor `(x, y)` coordinates at `cursor_byte`
    /// using the default typography line height (`1.25`) and left alignment matching [`crate::text::Text::new`].
    pub fn measure_text_cursor(
        &mut self,
        text: &str,
        font: &crate::text::Font,
        font_size: crate::geometry::LogicalPx,
        cursor_byte: usize,
    ) -> crate::text::TextCursorMetrics {
        let style = crate::text::TextMeasureStyle {
            font_size,
            line_height_multiplier: 1.25,
            max_width: None,
            font,
            align: crate::text::TextAlign::Left,
            bounds_mode: crate::text::TextBoundsMode::Typographic,
        };
        let shared = crate::text::SharedStr::from_str_inline_or_arc(text);
        (self.text_cursor_measurer)(&shared, style, cursor_byte)
    }

    /// Measures shaped text bounds and exact cursor `(x, y)` coordinates with full [`crate::text::TextLayoutOptions`].
    #[cfg(feature = "text")]
    pub fn measure_text_cursor_with(
        &mut self,
        text: &str,
        options: &crate::text::TextLayoutOptions,
        cursor_byte: usize,
    ) -> crate::text::TextCursorMetrics {
        let shared = crate::text::SharedStr::from_str_inline_or_arc(text);
        let style = crate::text::TextMeasureStyle::from_options(options);
        (self.text_cursor_measurer)(&shared, style, cursor_byte)
    }

    fn write_clipboard_entries(
        &mut self,
        primary_clipboard: bool,
        entries: Vec<(String, Vec<u8>)>,
    ) -> Result<()> {
        #[cfg(feature = "clipboard")]
        {
            let selection = if primary_clipboard {
                crate::clipboard::ClipboardSelection::Clipboard
            } else {
                crate::clipboard::ClipboardSelection::Primary
            };
            self.set_clipboard(selection, entries)
        }
        #[cfg(not(feature = "clipboard"))]
        {
            let _ = (primary_clipboard, entries);
            Err(WaywiError::UnsupportedCapability(Capability::Clipboard))
        }
    }
}
