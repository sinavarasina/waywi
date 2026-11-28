use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::app::MemoryConfig;
use crate::asset::{AssetCache, AssetId, AssetRetention};
use crate::capabilities::{Capabilities, Capability};
use crate::display::{Output, OutputId, OutputInfo};
use crate::error::{Result, WaywiError};
use crate::image::ImageAsset;
use crate::input::{DndAction, VirtualKeyState};
use crate::surface::{IntoSurfaceKey, SurfaceKey};
use crate::system::SystemProviders;
use crate::wayland_ext::{
    ExtWorkspaceGroupId, ExtWorkspaceId, ForeignToplevelId, GammaRamp, ScreencopyFrame,
    ScreencopyTarget,
};

use super::worker::{
    ActivationCallback, AssetCallback, BlockingTaskCallback, BlockingTaskJob, DragStartRequest,
    ExtWorkspaceAction, ForeignToplevelAction, GammaAction, ScreencopyCallback,
    VirtualKeyboardAction,
};

#[cfg(feature = "clipboard")]
pub(crate) type ClipboardSetter<'a> =
    &'a mut dyn FnMut(crate::clipboard::ClipboardSelection, Vec<(String, Vec<u8>)>) -> Result<()>;

/// Runtime context passed into `Application::init` and `Application::update`.
///
/// Gives controlled, feature- and capability-honest access to app shutdown, memory trimming,
/// output discovery, image asset lifecycle, brightness control, clipboard writes, imperative
/// one-shot delays, system samplers, audio/player clients, and desktop IPC backends.
pub struct EffectCtx<'a, M> {
    pub(crate) quit_requested: &'a mut bool,
    pub(crate) trim_requested: &'a mut bool,
    pub(crate) capabilities: &'a Capabilities,
    pub(crate) outputs: &'a [OutputInfo],
    pub(crate) memory_config: &'a MemoryConfig,
    pub(crate) assets: &'a mut AssetCache,
    pub(crate) pending_asset_jobs: &'a mut Vec<(PathBuf, AssetCallback<M>)>,
    pub(crate) pending_blocking_tasks: &'a mut Vec<(BlockingTaskJob, BlockingTaskCallback<M>)>,
    pub(crate) system: &'a mut SystemProviders,
    pub(crate) pending_after: &'a mut Vec<(Duration, M)>,
    pub(crate) reloaded_assets: &'a mut Vec<(AssetId, u64)>,
    pub(crate) evicted_assets: &'a mut Vec<AssetId>,
    pub(crate) pending_activation_requests: &'a mut Vec<(Option<String>, ActivationCallback<M>)>,
    pub(crate) pending_surface_activations: &'a mut Vec<(SurfaceKey, String)>,
    pub(crate) pending_screencopy_requests:
        &'a mut Vec<(ScreencopyTarget, bool, ScreencopyCallback<M>)>,
    pub(crate) pending_toplevel_actions: &'a mut Vec<ForeignToplevelAction>,
    pub(crate) pending_ext_workspace_actions: &'a mut Vec<ExtWorkspaceAction>,
    pub(crate) pending_virtual_keyboard_actions: &'a mut Vec<VirtualKeyboardAction>,
    pub(crate) pending_drag_starts: &'a mut Vec<DragStartRequest>,
    pub(crate) pending_gamma_actions: &'a mut Vec<GammaAction>,

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
    pub(crate) hyprland: Option<&'a crate::compositor::hyprland::HyprlandClient>,
    #[cfg(feature = "sway-ipc")]
    pub(crate) sway: Option<&'a crate::compositor::sway::SwayClient>,
    #[cfg(feature = "dbus")]
    pub(crate) dbus: &'a crate::dbus::DbusClient,
    #[cfg(feature = "clipboard")]
    pub(crate) clipboard_writer: Option<ClipboardSetter<'a>>,
    #[cfg(feature = "text")]
    pub(crate) text_engine: &'a mut crate::text::TextEngine,
    #[cfg(feature = "text")]
    pub(crate) frame_seq: u64,
}

impl<'a, M: 'static> EffectCtx<'a, M> {
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

    /// Returns the active [`MemoryConfig`] governing cache budgets and retention defaults.
    #[must_use]
    pub fn memory_config(&self) -> &MemoryConfig {
        self.memory_config
    }

    /// Requests an immediate best-effort trim of reclaimable caches and idle buffers at the end
    /// of this effect cycle (reclaimable image pixels, text/canvas/GPU/scratch renderer caches,
    /// `FrameArenaPool`, `MemoStore`, system/D-Bus caches, sound cache, and idle `wl_shm` pools).
    pub fn trim_memory(&mut self) {
        *self.trim_requested = true;
    }

    /// Returns a mutable handle to shared Linux system metrics and hardware providers.
    pub fn system(&mut self) -> &mut SystemProviders {
        self.system
    }

    /// Loads and caches an image asset (`PNG` / `SVG` / `GIF` / `WebP` per enabled Cargo features)
    /// from disk synchronously, deduplicating by canonical path and content hash.
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

    /// Loads and caches an image asset from encoded bytes.
    pub fn load_image_bytes(&mut self, bytes: &[u8]) -> Result<ImageAsset> {
        self.assets.load_bytes(bytes)
    }

    /// Loads and caches an image asset from encoded bytes, rasterizing vector (`SVG`) sources to
    /// at least `min_svg_px` pixels on the longest edge.
    pub fn load_image_bytes_sized(&mut self, bytes: &[u8], min_svg_px: u32) -> Result<ImageAsset> {
        self.assets.load_bytes_sized(bytes, min_svg_px)
    }

    /// Loads and caches an image asset from `'static` encoded bytes (such as `include_bytes!`),
    /// avoiding a heap copy of the compressed bytes when [`AssetRetention::ReclaimAfterPresent`]
    /// is used.
    pub fn load_image_static(&mut self, bytes: &'static [u8]) -> Result<ImageAsset> {
        self.assets.load_static_bytes(bytes)
    }

    /// Loads and caches an image asset from `'static` encoded bytes, rasterizing vector (`SVG`)
    /// sources to at least `min_svg_px` pixels on the longest edge.
    pub fn load_image_static_sized(
        &mut self,
        bytes: &'static [u8],
        min_svg_px: u32,
    ) -> Result<ImageAsset> {
        self.assets.load_static_bytes_sized(bytes, min_svg_px)
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
    pub fn load_image_async(
        &mut self,
        path: impl AsRef<Path>,
        map: impl FnOnce(Result<ImageAsset>) -> M + 'static,
    ) {
        self.pending_asset_jobs
            .push((path.as_ref().to_path_buf(), Box::new(map)));
    }

    /// Dispatches an arbitrary blocking closure to WayWi's lazy background task worker thread and
    /// delivers `map(result)` back to `Application::update` via automatic `calloop` wakeup.
    ///
    /// No thread is spawned unless this method is called. In [`HeadlessRuntime`](crate::app::HeadlessRuntime),
    /// tasks execute deterministically inline so tests require no sleeping or thread synchronization.
    pub fn spawn_blocking<T, F, G>(&mut self, task: F, map: G)
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
        G: FnOnce(T) -> M + 'static,
    {
        let job: BlockingTaskJob = Box::new(move || Box::new(task()));
        let cb: BlockingTaskCallback<M> = Box::new(move |boxed| {
            let value = boxed
                .downcast::<T>()
                .expect("spawn_blocking downcast type matches job output");
            map(*value)
        });
        self.pending_blocking_tasks.push((job, cb));
    }

    /// Reloads `asset` from `path`, updating its revision counter so dependent surfaces invalidate
    /// and redraw cleanly.
    pub fn reload_image(&mut self, asset: ImageAsset, path: impl AsRef<Path>) -> Result<()> {
        let updated = self.assets.reload(asset, path)?;
        let rev = self.assets.revision_of(updated.id);
        self.reloaded_assets.push((updated.id, rev));
        self.evicted_assets.push(updated.id);
        Ok(())
    }

    /// Configures the decoded pixel memory retention policy for `asset`.
    ///
    /// Returns `false` if `asset` is unknown/unloaded, or if
    /// [`AssetRetention::ReclaimAfterPresent`] is requested for a raw pixel asset created via
    /// [`Self::load_rgba_image`].
    pub fn set_image_retention(&mut self, asset: ImageAsset, retention: AssetRetention) -> bool {
        self.assets.set_retention(asset.id, retention)
    }

    /// Returns the active [`AssetRetention`] policy for `asset`, or `None` if unloaded.
    #[must_use]
    pub fn image_retention(&self, asset: ImageAsset) -> Option<AssetRetention> {
        self.assets.retention(asset.id)
    }

    /// Immediately frees the decoded RGBA8 pixel buffer (and any cached GPU texture) for `asset`
    /// while keeping its [`ImageAsset`] handle, dimensions, revision, and backing source intact.
    pub fn evict_image_pixels(&mut self, asset: ImageAsset) -> bool {
        let evicted = self.assets.evict_decoded_pixels(asset.id);
        if evicted {
            self.evicted_assets.push(asset.id);
        }
        evicted
    }

    /// Removes `asset` permanently from the internal asset cache (and any cached GPU texture) to
    /// reclaim all associated memory.
    pub fn unload_image(&mut self, asset: ImageAsset) -> bool {
        let unloaded = self.assets.unload(asset.id);
        if unloaded {
            self.evicted_assets.push(asset.id);
        }
        unloaded
    }

    /// Schedules an imperative one-shot message to be delivered after `delay`.
    ///
    /// Prefer [`Timer::once`](crate::timer::Timer::once) in `subscriptions` when cancellation or
    /// reset-on-rekey semantics are needed.
    pub fn after(&mut self, delay: Duration, msg: M) {
        self.pending_after.push((delay, msg));
    }

    /// Requests a Wayland `xdg_activation_v1` focus-handoff token asynchronously (non-blocking).
    ///
    /// When the compositor issues the token, `map(Ok(token))` is delivered to
    /// [`Application::update`](crate::app::Application::update), where it can be passed to
    /// [`DesktopEntry::launch_with`](crate::desktop::DesktopEntry::launch_with) or
    /// [`Self::activate_surface`]. If the compositor does not advertise `xdg_activation_v1`,
    /// `map(Err(WaywiError::UnsupportedCapability(Capability::XdgActivation)))` is delivered.
    pub fn request_activation_token(
        &mut self,
        app_id: Option<&str>,
        map: impl FnOnce(Result<String>) -> M + 'static,
    ) {
        let app_id = app_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        self.pending_activation_requests
            .push((app_id, Box::new(map)));
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

    /// Captures pixels from a Wayland output or output sub-region asynchronously (non-blocking).
    ///
    /// Prefers `ext_image_copy_capture_v1` + `ext_output_image_capture_source_manager_v1` and
    /// falls back to `zwlr_screencopy_manager_v1`. Delivers `map(Ok(ScreencopyFrame))` normalized
    /// to top-to-bottom RGBA8, or `map(Err(WaywiError::UnsupportedCapability(Capability::Screencopy)))`
    /// when unsupported.
    pub fn capture_screen(
        &mut self,
        target: ScreencopyTarget,
        overlay_cursor: bool,
        map: impl FnOnce(Result<ScreencopyFrame>) -> M + 'static,
    ) {
        self.pending_screencopy_requests
            .push((target, overlay_cursor, Box::new(map)));
    }

    /// Activates/focuses a foreign toplevel window (`zwlr_foreign_toplevel_management_v1`).
    pub fn activate_toplevel(&mut self, id: ForeignToplevelId) -> Result<()> {
        self.capabilities
            .require(Capability::ForeignToplevelControl)?;
        self.pending_toplevel_actions
            .push(ForeignToplevelAction::Activate(id));
        Ok(())
    }

    /// Requests closing a foreign toplevel window (`zwlr_foreign_toplevel_management_v1`).
    pub fn close_toplevel(&mut self, id: ForeignToplevelId) -> Result<()> {
        self.capabilities
            .require(Capability::ForeignToplevelControl)?;
        self.pending_toplevel_actions
            .push(ForeignToplevelAction::Close(id));
        Ok(())
    }

    /// Uploads a custom hardware RGB [`GammaRamp`] to matching Wayland output(s) via
    /// `zwlr_gamma_control_manager_v1`.
    pub fn set_output_gamma(&mut self, output: Output, ramp: GammaRamp) -> Result<()> {
        self.capabilities.require(Capability::GammaControl)?;
        ramp.validate()?;
        self.pending_gamma_actions
            .push(GammaAction::SetRamp { output, ramp });
        Ok(())
    }

    /// Applies a night-light / color-temperature ramp (`kelvin` in `1000..=25000`, `brightness`
    /// in `0.05..=1.0`) to matching Wayland output(s) via `zwlr_gamma_control_manager_v1`.
    pub fn set_output_color_temperature(
        &mut self,
        output: Output,
        kelvin: u32,
        brightness: f32,
    ) -> Result<()> {
        self.capabilities.require(Capability::GammaControl)?;
        let brightness = if brightness.is_finite() {
            brightness.clamp(0.05, 1.0)
        } else {
            1.0
        };
        self.pending_gamma_actions
            .push(GammaAction::SetTemperature {
                output,
                kelvin: kelvin.clamp(1000, 25000),
                brightness,
            });
        Ok(())
    }

    /// Restores the compositor's default hardware gamma table for matching Wayland output(s).
    pub fn reset_output_gamma(&mut self, output: Output) -> Result<()> {
        self.capabilities.require(Capability::GammaControl)?;
        self.pending_gamma_actions
            .push(GammaAction::Reset { output });
        Ok(())
    }

    /// Minimizes or unminimizes a foreign toplevel window (`zwlr_foreign_toplevel_management_v1`).
    pub fn set_toplevel_minimized(&mut self, id: ForeignToplevelId, minimized: bool) -> Result<()> {
        self.capabilities
            .require(Capability::ForeignToplevelControl)?;
        self.pending_toplevel_actions
            .push(ForeignToplevelAction::SetMinimized(id, minimized));
        Ok(())
    }

    /// Maximizes or unmaximizes a foreign toplevel window (`zwlr_foreign_toplevel_management_v1`).
    pub fn set_toplevel_maximized(&mut self, id: ForeignToplevelId, maximized: bool) -> Result<()> {
        self.capabilities
            .require(Capability::ForeignToplevelControl)?;
        self.pending_toplevel_actions
            .push(ForeignToplevelAction::SetMaximized(id, maximized));
        Ok(())
    }

    /// Fullscreens or unfullscreens a foreign toplevel window (`zwlr_foreign_toplevel_management_v1`).
    pub fn set_toplevel_fullscreen(
        &mut self,
        id: ForeignToplevelId,
        fullscreen: bool,
        output: Option<OutputId>,
    ) -> Result<()> {
        self.capabilities
            .require(Capability::ForeignToplevelControl)?;
        self.pending_toplevel_actions
            .push(ForeignToplevelAction::SetFullscreen(id, fullscreen, output));
        Ok(())
    }

    /// Activates an `ext_workspace_v1` workspace and commits the manager transaction.
    pub fn activate_ext_workspace(&mut self, id: ExtWorkspaceId) -> Result<()> {
        self.capabilities.require(Capability::ExtWorkspace)?;
        self.pending_ext_workspace_actions
            .push(ExtWorkspaceAction::Activate(id));
        Ok(())
    }

    /// Deactivates an `ext_workspace_v1` workspace and commits the manager transaction.
    pub fn deactivate_ext_workspace(&mut self, id: ExtWorkspaceId) -> Result<()> {
        self.capabilities.require(Capability::ExtWorkspace)?;
        self.pending_ext_workspace_actions
            .push(ExtWorkspaceAction::Deactivate(id));
        Ok(())
    }

    /// Creates a named workspace in an `ext_workspace_v1` workspace group and commits.
    pub fn create_ext_workspace(
        &mut self,
        group: ExtWorkspaceGroupId,
        name: impl Into<String>,
    ) -> Result<()> {
        self.capabilities.require(Capability::ExtWorkspace)?;
        self.pending_ext_workspace_actions
            .push(ExtWorkspaceAction::Create {
                group,
                name: name.into(),
            });
        Ok(())
    }

    /// Removes an `ext_workspace_v1` workspace and commits the manager transaction.
    pub fn remove_ext_workspace(&mut self, id: ExtWorkspaceId) -> Result<()> {
        self.capabilities.require(Capability::ExtWorkspace)?;
        self.pending_ext_workspace_actions
            .push(ExtWorkspaceAction::Remove(id));
        Ok(())
    }

    /// Starts an outgoing Wayland Drag & Drop operation (`wl_data_device.start_drag`) originating
    /// from `surface_key` using the most recent pointer/touch press serial.
    pub fn start_drag(
        &mut self,
        surface_key: impl IntoSurfaceKey,
        entries: Vec<(String, Vec<u8>)>,
        actions: DndAction,
    ) -> Result<()> {
        self.capabilities.require(Capability::DragAndDrop)?;
        if entries.is_empty() {
            return Err(WaywiError::InvalidState(
                "start_drag requires at least one (mime_type, payload) entry".to_owned(),
            ));
        }
        self.pending_drag_starts.push(DragStartRequest {
            surface_key: surface_key.into_surface_key(),
            entries,
            actions,
        });
        Ok(())
    }

    /// Injects a Linux evdev keycode press or release via `zwp_virtual_keyboard_v1` (uploading a
    /// default US XKB keymap automatically on first use if [`Self::virtual_keymap`] was not called).
    pub fn virtual_key(&mut self, keycode: u32, state: VirtualKeyState) -> Result<()> {
        self.capabilities.require(Capability::VirtualKeyboard)?;
        self.pending_virtual_keyboard_actions
            .push(VirtualKeyboardAction::Key { keycode, state });
        Ok(())
    }

    /// Updates virtual keyboard XKB modifier masks (`zwp_virtual_keyboard_v1.modifiers`).
    pub fn virtual_modifiers(
        &mut self,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    ) -> Result<()> {
        self.capabilities.require(Capability::VirtualKeyboard)?;
        self.pending_virtual_keyboard_actions
            .push(VirtualKeyboardAction::Modifiers {
                depressed,
                latched,
                locked,
                group,
            });
        Ok(())
    }

    /// Uploads a custom XKB keymap string to the virtual keyboard (`zwp_virtual_keyboard_v1.keymap`).
    pub fn virtual_keymap(&mut self, xkb_keymap: impl Into<String>) -> Result<()> {
        self.capabilities.require(Capability::VirtualKeyboard)?;
        let km = xkb_keymap.into();
        if km.is_empty() {
            return Err(WaywiError::InvalidState(
                "virtual_keymap must not be empty".to_owned(),
            ));
        }
        self.pending_virtual_keyboard_actions
            .push(VirtualKeyboardAction::Keymap(km));
        Ok(())
    }

    /// Accesses the active audio control handle (`set_volume`, `set_mute`, `set_port`, etc.).
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
    pub fn hyprland(&self) -> Result<&crate::compositor::hyprland::HyprlandClient> {
        self.capabilities.require(Capability::HyprlandIpc)?;
        self.hyprland
            .ok_or(WaywiError::UnsupportedCapability(Capability::HyprlandIpc))
    }

    /// Accesses the Sway IPC client when running under Sway / i3-compatible socket.
    #[cfg(feature = "sway-ipc")]
    pub fn sway(&self) -> Result<&crate::compositor::sway::SwayClient> {
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
            return Ok(crate::compositor::CompositorClient::Hyprland(h));
        }
        #[cfg(feature = "sway-ipc")]
        if let Some(s) = self.sway
            && self.capabilities.has(Capability::SwayIpc)
        {
            return Ok(crate::compositor::CompositorClient::Sway(s));
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

    /// Accesses the D-Bus session/system client (`.mpris()`, `.nm()`, `.bluetooth()`, `.udisks()`,
    /// `.logind()`, `.tray()`, `.notifications()`, `.power_profiles()`, `.upower()`, `.modem()`,
    /// `.portal()`, `.screensaver()`, `.geoclue()`, and raw queries/calls).
    #[cfg(feature = "dbus")]
    pub fn dbus(&self) -> Result<&crate::dbus::DbusClient> {
        self.capabilities.require(Capability::Dbus)?;
        Ok(self.dbus)
    }

    /// Accesses the audio SFX player (`load_file`, `load_bytes`, `load_preset`, `load_system_sound`,
    /// `play`, `play_at`, `play_trigger`, `stop_all`, `unload`, `clear_cache`).
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

    /// Sets laptop backlight brightness (`0.0..=100.0` %), trying direct `/sys/class/backlight`
    /// first and falling back to the shared non-blocking `systemd-logind` D-Bus client when
    /// `dbus` is enabled and available.
    pub fn set_backlight_percent(&self, device_name: &str, percent: f32) -> Result<()> {
        let raw = crate::system::hardware::backlight_percent_to_raw(device_name, percent)?;
        match crate::system::hardware::write_backlight_sysfs_raw(device_name, raw) {
            Ok(()) => Ok(()),
            Err(sysfs_err) => {
                #[cfg(feature = "dbus")]
                if self.capabilities.has(Capability::Dbus)
                    && let Ok(val32) = u32::try_from(raw)
                    && self
                        .dbus
                        .logind()
                        .set_brightness("backlight", device_name, val32)
                        .is_ok()
                {
                    return Ok(());
                }
                Err(sysfs_err)
            }
        }
    }

    /// Publishes UTF-8 text to the Wayland standard clipboard (`ClipboardSelection::Clipboard`).
    #[cfg(feature = "clipboard")]
    pub fn set_clipboard_text(&mut self, text: impl Into<String>) -> Result<()> {
        let bytes = text.into().into_bytes();
        self.set_clipboard(
            crate::clipboard::ClipboardSelection::Clipboard,
            vec![
                ("text/plain;charset=utf-8".to_owned(), bytes.clone()),
                ("UTF8_STRING".to_owned(), bytes.clone()),
                ("text/plain".to_owned(), bytes),
            ],
        )
    }

    /// Publishes UTF-8 text to the Wayland primary selection (`ClipboardSelection::Primary`).
    #[cfg(feature = "clipboard")]
    pub fn set_clipboard_primary_text(&mut self, text: impl Into<String>) -> Result<()> {
        let bytes = text.into().into_bytes();
        self.set_clipboard(
            crate::clipboard::ClipboardSelection::Primary,
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

    /// Registers custom font bytes (TTF/OTF/TTC) into the shared text engine so application-local
    /// or embedded icon/UI fonts can be referenced by [`Font::family`](crate::text::Font::family)
    /// without system installation. Retained across `trim_memory()` and fallback-font rebuilds.
    ///
    /// Returns the number of font faces loaded from `bytes`.
    #[cfg(feature = "text")]
    pub fn load_font_bytes(&mut self, bytes: Vec<u8>) -> Result<usize> {
        self.text_engine
            .load_font_source(std::sync::Arc::new(bytes))
    }

    /// Registers static embedded font bytes (e.g. `include_bytes!(...)`) into the shared text engine.
    ///
    /// Returns the number of font faces loaded from `bytes`.
    #[cfg(feature = "text")]
    pub fn load_font_static(&mut self, bytes: &'static [u8]) -> Result<usize> {
        self.text_engine
            .load_font_source(std::sync::Arc::new(bytes))
    }

    /// Loads a custom font file (TTF/OTF/TTC) from disk into the shared text engine.
    ///
    /// Returns the number of font faces loaded from `path`.
    #[cfg(feature = "text")]
    pub fn load_font_file(&mut self, path: impl AsRef<Path>) -> Result<usize> {
        self.text_engine.load_font_file(path.as_ref())
    }

    /// Measures shaped text bounds and exact cursor `(x, y)` coordinates at `cursor_byte`
    /// using the default typography line height (`1.25`) and left alignment matching [`crate::text::Text::new`].
    #[cfg(feature = "text")]
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
            overflow: crate::text::TextOverflow::Clip,
            max_lines: None,
            spans: None,
        };
        let shared = crate::text::SharedStr::from_str_inline_or_arc(text);
        self.text_engine
            .measure_text_cursor(&shared, style, cursor_byte, self.frame_seq)
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
        self.text_engine
            .measure_text_cursor(&shared, style, cursor_byte, self.frame_seq)
    }

    /// Measures visual highlight rectangles covering `start_byte..end_byte` in shaped text.
    #[cfg(feature = "text")]
    pub fn measure_text_selection(
        &mut self,
        text: &str,
        options: &crate::text::TextLayoutOptions,
        start_byte: usize,
        end_byte: usize,
    ) -> crate::text::TextSelectionMetrics {
        let shared = crate::text::SharedStr::from_str_inline_or_arc(text);
        let style = crate::text::TextMeasureStyle::from_options(options);
        self.text_engine.measure_text_selection(
            &shared,
            style,
            start_byte,
            end_byte,
            self.frame_seq,
        )
    }

    /// Hit-tests a logical `(x, y)` point relative to the text origin and returns the closest
    /// valid UTF-8 byte offset inside `text` (for click-to-cursor and drag selection).
    #[cfg(feature = "text")]
    pub fn hit_test_text_byte(
        &mut self,
        text: &str,
        options: &crate::text::TextLayoutOptions,
        point: crate::geometry::Point,
    ) -> usize {
        let shared = crate::text::SharedStr::from_str_inline_or_arc(text);
        let style = crate::text::TextMeasureStyle::from_options(options);
        self.text_engine
            .hit_test_text_byte(&shared, style, point, self.frame_seq)
    }
}
