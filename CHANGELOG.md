# Changelog

All notable changes to this project will be documented in this file.

## [0.3.1] - 2026-11-28

### Breaking Changes
- **Unified `Fill` type on `Rect` and `Painter` (`render`)**:
  - `Rect::fill` and `Painter::{fill_rect, fill_rounded_rect, fill_circle, fill_path}` now accept `impl Into<Fill>` (`Fill::Solid(Color)`, `Fill::Linear(LinearGradient)`, `Fill::Radial(RadialGradient)`). Existing call sites passing `Color` continue to compile transparently via `From<Color> for Fill`, while code matching or reading internal `Fill` values must handle gradient variants.
- **New `Capability` variants (`capabilities`)**:
  - Added `Capability::ImageJpeg`, `Capability::AudioCapture`, `Capability::BackgroundEffect`, `Capability::ToplevelIcon`, `Capability::GammaControl`, and `Capability::ColorManagement`; exhaustive matches on `Capability` must cover these variants.
- **Structured `PipeWireNodeProps` parameter for `classify_pipewire_node` (`system::privacy`)**:
  - `classify_pipewire_node` now takes `PipeWireNodeProps<'_>` instead of 7 positional arguments so PipeWire link-graph topology (`linked_from_output_monitor`, `linked_from_screencast`) and authoritative protocol properties (`stream.capture.sink`, `stream.monitor`, `device.api`, `target.object`) govern Microphone vs. AudioCapture and Camera vs. ScreenShare classification.

### Added
- **Audio capture/monitor streaming & per-stream routing (`audio`)**:
  - Added `Audio::capture(key, target, config, map)` (`AudioCaptureTarget::{DefaultOutputMonitor, DefaultInput, SinkMonitor, Source, DeviceName}`, `AudioCaptureConfig`, `AudioCaptureFrame`) with real-time VU peak/RMS metering, dBFS peak level, and optional interleaved `f32` PCM output across PipeWire (`STREAM_CAPTURE_SINK`), PulseAudio (`@DEFAULT_MONITOR@`/`@DEFAULT_SOURCE@`), and ALSA (`Direction::Capture`). Capture workers spawn lazily on demand, suppress event-loop wakeups completely during sustained silence, coalesce buffered frames, and tear down immediately on unsubscribe.
  - Added `AudioClient::move_stream(direction, stream_id, device_id)` across PipeWire (`target.node`/`target.object` metadata) and PulseAudio (`move_sink_input_by_index` / `move_source_output_by_index`).
- **JPEG decoding (`image-jpeg`, `Capability::ImageJpeg`)**:
  - Added opt-in `image-jpeg` Cargo feature backed by `zune-jpeg` (`ImageFormat::Jpeg`) supporting baseline/progressive JPEG decoding (RGB, RGBA, and grayscale) into premultiplied RGBA8 across `AssetCache` and `EffectCtx::load_image*`.
- **Text overflow, custom/embedded fonts, rich spans & selection metrics (`text`, `effect`, `input`)**:
  - Added `TextOverflow::{Clip, EllipsisStart, EllipsisMiddle, EllipsisEnd}` and `Text::{overflow, ellipsis, max_lines}` (backed natively by `cosmic-text` `Ellipsize`), plus matching `TextLayoutOptions::{overflow, ellipsis, max_lines}`.
  - Added `EffectCtx::{load_font_bytes, load_font_static, load_font_file}` so applications can bundle custom UI/icon fonts via `include_bytes!` or load local font files; loaded custom fonts persist cleanly across `TextEngine::trim()` and fallback-storm rebuilds and sync automatically to the GPU renderer.
  - Added multi-style rich text spans (`TextSpan`, `Text::rich`, `Text::span`) supporting per-span color, font family/weight/style, and font size on both CPU and GPU renderers.
  - Added `TextSelectionMetrics`, `EffectCtx::{measure_text_selection, hit_test_text_byte}`, and `TextInputState::{clear_selection, delete_selection}` for full text selection, mouse drag highlighting, and copy/cut workflows.
- **Main-axis layout justification, flex weights & min/max size constraints (`render`, `input`, `reconcile`)**:
  - Added `Justify::{Start, Center, End, SpaceBetween, SpaceAround, SpaceEvenly}` with `Row::justify` and `Column::justify`.
  - Added `.flex(f32)` and `.min_width`, `.max_width`, `.min_height`, `.max_height`, `.min_size`, `.max_size` on `CommonProps` (all elements and containers), with an O(N) fast-path when `Justify::Start` and no flex children are present.
- **Linear/radial gradients & box shadows across CPU and GPU (`render`)**:
  - Added `GradientStop`, `LinearGradient`, `RadialGradient`, `Fill`, and `BoxShadow` (`Rect::shadow`), implemented with CPU (`tiny-skia`) and GPU (`femtovg`) parity across both `Rect` and `Canvas` (`Painter`), including automatic dirty/world bounds expansion for shadows without inflating hit-test bounds.
- **Zero-thread `inotify` filesystem watcher & generic background tasks (`system`, `effect`, `app`)**:
  - Added `FileWatch::{path, paths}` (`FileWatchEvent`, `FileWatchKind`) registered directly into `calloop::generic::Generic` via `rustix::fs::inotify` with zero background threads and parent-directory watching for files so atomic-rename editor saves never lose watches.
  - Added `EffectCtx::spawn_blocking(task, map)` backed by a lazy single worker thread waking `calloop` automatically via `ping` (and executing deterministically inline in `HeadlessRuntime`).
- **4 Additional Wayland Protocols (`wayland`, `wayland_ext`, `surface`, `effect`, `app`)**:
  - **Background blur (`background-effect`, `Capability::BackgroundEffect`)**: `ext_background_effect_v1` with declarative `BackgroundBlur::{None, Full, Region(Bounds)}` via `Surface::background_blur(bool)` / `Surface::background_blur_region(Option<Bounds>)`.
  - **Toplevel window icons (`toplevel-icon`, `Capability::ToplevelIcon`)**: `xdg_toplevel_icon_v1` with declarative `WindowIcon::{Name, Asset}` via `Surface::window_icon`, `Surface::window_icon_name`, and `Surface::clear_window_icon` (automatically center-padding non-square assets into square SHM icon buffers per spec).
  - **Display gamma & color temperature (`gamma-control`, `Capability::GammaControl`)**: `zwlr_gamma_control_manager_v1` with `GammaRamp` (`identity`, `from_temperature`, `from_rgb_scales`, `resample`), `kelvin_to_rgb_multipliers`, and `EffectCtx::{set_output_gamma, set_output_color_temperature, reset_output_gamma}` with automatic gamma restoration on reset/drop.
  - **Color management & HDR (`color-management`, `Capability::ColorManagement`)**: `wp_color_management_v1` with `ColorSpace::{Default, Srgb, DisplayP3, Rec2020Pq, LinearSrgb}`, `OutputColorInfo`, and declarative `Surface::color_space(ColorSpace)`.
  - Added `HeadlessRuntime` inspection hooks `surface_background_blur`, `surface_window_icon`, `surface_color_space`, `recorded_gamma_actions`, and `HeadlessGammaAction`.

### Fixed
- **Authoritative Linux telemetry & privacy classification (`system`, `audio`)**:
  - Upgraded `PrivacyMonitor` (`system::privacy`) to track PipeWire `ObjectType::Link` topology and upstream node classes (`Audio/Sink`, `Stream/Output/Audio`, `Audio/Source`, `Video/Source` hardware vs. screencast) plus `stream.capture.sink`, `stream.monitor`, `media.role`, `device.api`, and `target.object` updates in `Node::info`, preventing output-monitor streams (such as `waywi-capture`, OBS desktop audio, or CAVA) from being misclassified as `Microphone` and preventing hardware webcams whose names contain `"screen"` from being misclassified as `ScreenShare`.
  - Tightened `/proc/<pid>/fd` device matching in `system::privacy` to strict `/dev/snd/pcmC<card>D<dev>c` and `/dev/video<digits>` formats (ignoring `" (deleted)"` suffixes).
  - Filtered WayWi's internal `waywi-capture` stream from mixer `AudioSnapshot::streams` in both PipeWire and PulseAudio backends, and added `target.object` parsing alongside `node.target`/`target.node` in PipeWire stream metadata.
  - Upgraded `classify_interface` (`system::network`) to consult authoritative kernel `sysfs` facts (`type`, `wireless/`, `uevent` `DEVTYPE`, `bridge/`, `tun_flags`, and backing hardware `device/`) before falling back to interface name prefixes.
  - Tightened `is_whole_block_device` (`system::disk`) to exclude stacked device-mapper (`dm-*`), `zram*`, `mmcblk*boot*`/`rpmb`, and `md*p*`/`nbd*p*`/`xvd*<digit>` partitions from whole-disk I/O totals.

## [0.3.0] - 2026-11-27

### Breaking Changes
- **Extended `TouchEvent` and `ScrollEvent` fields (`input`)**:
  - `TouchEvent` now includes `phase: TouchPhase` (`Down`, `Move`, `Up`, `Cancel`), `shape: Option<(f32, f32)>`, `orientation: Option<f32>`, and `modifiers: Modifiers` (matching `wl_touch` full contact lifecycle).
  - `ScrollEvent` now includes `source: ScrollSource` (`Wheel`, `Finger`, `Continuous`, `WheelTilt`, `Unknown`), `stop_x: bool`, and `stop_y: bool` (matching `wl_pointer.axis_source` and `wl_pointer.axis_stop`), and now implements `Default`. Code constructing `TouchEvent` or `ScrollEvent` struct literals directly must populate these fields (or use `..Default::default()` for `ScrollEvent`).
- **Extended D-Bus snapshot and option structs (`dbus`)**:
  - `MprisSnapshot` now includes `rate: f64`, `minimum_rate: f64`, and `maximum_rate: f64` (dropping `Eq` on `MprisSnapshot`/`MprisEvent` due to `f64`, while preserving `PartialEq` and `Default`).
  - `UDisksDrive` now includes `smart: Option<Box<UDisksSmartHealth>>` (dropping `Eq` on `UDisksDrive`/`UDisksSnapshot` due to `f64` temperature, while preserving `PartialEq` and `Default`).
  - `NmActiveConnection` now includes `vpn: bool` and `vpn_state: Option<NmVpnState>`, and `WifiConnectOptions` now includes `enterprise: Option<WifiEnterpriseOptions>` (both implement `Default`).
- **New `SurfaceKind::SessionLock` variant (`surface`)**:
  - Added `SurfaceKind::SessionLock` for native `ext_session_lock_v1` lockscreen surfaces; exhaustive `match` expressions on `SurfaceKind` must now cover `SessionLock`.
- **Extended `MemoryConfig` fields (`app::memory`)**:
  - `MemoryConfig` now includes `text_max_mapped_font_files: usize` and `gpu_memory_policy: GpuMemoryPolicy`. Struct literals constructing `MemoryConfig` directly must populate these fields (or use presets/builders or `..MemoryConfig::default()`).

### Added
- **GPU memory allocation policy & bounded mapped font reclamation (`app`, `render`, `text`)**:
  - Added backend-agnostic `GpuMemoryPolicy` (`MemoryUsage`, `Minimal`, `Performance`, re-exported at the crate root and `waywi::prelude`) and `MemoryConfig::{gpu_memory_policy, text_max_mapped_font_files}` builders.
- **VSync-paced Animation Mechanism & Retained Transitions (`animation`, `reconcile`, `app`)**:
  - Added zero-dependency, stack-allocated animation primitives in `waywi::animation` (re-exported in `waywi::prelude`): `Easing` (`Linear`, `Quad*`, `Cubic*`, `QuartOut`, `ExpoOut`, `BackOut`, `CubicBezier`), `Lerp` (`f32`, `f64`, `LogicalPx`, `Percent`, `Point`, `LogicalSize`, `Bounds`, `Color`, `Transform`), `SpringConfig`, `Spring` (sub-stepped `<= 4ms` integration with automatic settled detection), `Tween<T>`, `Animated<T>`, and `TransitionSpec`.
  - Added `Animation::frames(key, |now| ...)` synchronized directly to Wayland `wl_surface.frame` VSync callbacks (tracking monitor refresh rates natively and dropping to 0% idle CPU/GPU wakeups when DPMS off, occluded, or unsubscribed), plus deterministic delivery in `HeadlessRuntime::step_at`.
  - Added opt-in retained transitions `.transition_opacity(TransitionSpec)` and `.transition_transform(TransitionSpec)` evaluated directly on `RetainedNode`s on per-surface `wl_surface.frame` VSync (using single-pass `local_affine` propagation safe across `Transform::scale(0.0)`, overshoot-clamped opacity, and NaN-guarded springs) without rebuilding `Application::view(state)`, stopping automatically when settled, plus `HeadlessRuntime::tick_animations_at`.
- **10 Cross-Compositor Wayland Desktop Protocols (`wayland`, `wayland_ext`, `input`, `surface`, `effect`, `capabilities`, `app`)**:
  - **Server-side cursor shapes (`cursor-shape`, `Capability::CursorShape`)**: `wp_cursor_shape_v1` (v1..=2) with `CursorShape` (all standard CSS/Wayland shapes plus `CursorShape::Hidden` via `wl_pointer.set_cursor(None)`), declarative `Element::cursor` & `Surface::cursor` with `InputMode::Selective` region inclusion, single-pass hit-testing, O(1) fast-path when unused, and unified deduplication.
  - **Native session lockscreen (`session-lock`, `Capability::SessionLock`)**: `ext_session_lock_v1` with `SurfaceKind::SessionLock`, `Surface::session_lock(key, output)`, `SessionLock::subscribe` (`SessionLockEvent::{Locked, Finished}`), automatic `lock()` acquisition, atomic `ack_configure` on buffer attach/commit, and guaranteed lock-surface teardown before `unlock_and_destroy()`.
  - **Screen & region pixel capture (`screencopy`, `Capability::Screencopy`)**: Non-blocking `EffectCtx::capture_screen(ScreencopyTarget, overlay_cursor, map)` preferring `ext_image_copy_capture_v1` + `ext_output_image_capture_source_manager_v1` and falling back to `zwlr_screencopy_manager_v1`, preserving lossless `WaywiError` variants and normalizing SHM pixel formats (`Argb8888`/`Xrgb8888`/`Abgr8888`/`Xbgr8888`), `y_invert`, and all 8 `wl_output::Transform` orientations into top-to-bottom RGBA8 `ScreencopyFrame` (`pixel_rgba`, `pixel_color`) with immediate SHM cleanup.
  - **Foreign toplevel window management (`foreign-toplevel`, `Capability::ForeignToplevel`, `Capability::ForeignToplevelControl`)**: Prefers `zwlr_foreign_toplevel_management_v1` (listing + state + `EffectCtx::{activate_toplevel, close_toplevel, set_toplevel_minimized, set_toplevel_maximized, set_toplevel_fullscreen}`) with automatic fallback to `ext_foreign_toplevel_list_v1` via `ForeignToplevel::subscribe` (coalesced per wakeup, zero allocations when unsubscribed).
  - **Standard workspaces (`ext-workspace`, `Capability::ExtWorkspace`)**: `ext_workspace_v1` with coalesced atomic `done`-synchronized `ExtWorkspace::subscribe` (`ExtWorkspaceSnapshot`, `ExtWorkspaceGroupInfo`, `ExtWorkspaceInfo`, `ExtWorkspaceState`, `ExtWorkspaceCapabilities`) and single-`commit()` batched mutations `EffectCtx::{activate_ext_workspace, deactivate_ext_workspace, create_ext_workspace, remove_ext_workspace}`.
  - **Core Drag & Drop (`Capability::DragAndDrop`)**: Built-in `wl_data_device_manager` / `wl_data_device` incoming and outgoing DnD (`DndAction`, `DndEvent::{Enter, Motion, Leave, Drop, DropError}`, `Surface::dnd_mime_types`, `Surface::on_dnd`, `EffectCtx::start_drag`) driven by zero-thread `calloop::generic::Generic` non-blocking FD sources with strict MIME filtering and `wl_data_offer.finish()` sent strictly after clean bounded EOF.
  - **XDG window decorations (`Capability::XdgDecoration`)**: Built-in `zxdg_decoration_manager_v1` negotiation (`WindowDecoration::{Server, Auto, Client, None}`, defaulting to `Server`, configured via `Surface::decoration`).
  - **Per-surface idle inhibition (`idle-inhibit`, `Capability::IdleInhibit`)**: Declarative `Surface::inhibit_idle(bool)` managing `zwp_idle_inhibit_manager_v1` / `zwp_idle_inhibitor_v1` automatically with surface lifecycle.
  - **Virtual keyboard (`virtual-keyboard`, `Capability::VirtualKeyboard`)**: `zwp_virtual_keyboard_v1` (`VirtualKeyState`, `EffectCtx::{virtual_key, virtual_modifiers, virtual_keymap}`) with lazy device creation, automatic rewound `memfd` XKB keymap upload, and seat-removal cleanup.
  - **Pointer constraints & relative pointer (`pointer-constraints`, `Capability::PointerConstraints`, `Capability::RelativePointer`)**: `zwp_pointer_constraints_v1` (`PointerConstraintMode::{None, Locked, Confined}`, `Surface::pointer_constraint`) and `zwp_relative_pointer_v1` (`RelativePointerEvent`, `Surface::on_relative_pointer`).
  - **Honest `HeadlessRuntime` testing hooks (`app::headless`)**: Added `set_synthetic_screencopy`, `recorded_toplevel_actions`, `recorded_ext_workspace_actions`, `recorded_virtual_keyboard_actions`, `recorded_drag_starts`, and corresponding `HeadlessToplevelAction` / `HeadlessExtWorkspaceAction` / `HeadlessVirtualKeyboardAction` / `HeadlessDragStart` record types.
- **Comprehensive D-Bus desktop services & enhancements (`dbus`, `subscription`, `app`)**:
  - **Polkit Authentication Agent & Authority (`dbus.polkit()`, `Dbus::polkit_agent`)**: Native `org.freedesktop.PolicyKit1.AuthenticationAgent` server driving `polkit-agent-helper-1` on background threads (`PolkitAuthRequest`, `PolkitAgentReply`, `PolkitAgentEvent`, `PolkitIdentity`) plus `PolkitClient::check_authorization_blocking`.
  - **Hardware sensors (`dbus.sensors()`, `Dbus::sensors`)**: Zero-polling `net.hadess.SensorProxy` (`iio-sensor-proxy`) accelerometer orientation, ambient light, and proximity subscription with automatic claim/release lifecycle (`SensorOrientation`, `LightLevelUnit`, `SensorProxySnapshot`).
  - **Expanded XDG Desktop Portals (`dbus.portal()`)**: Added race-free `Request.Response` support for `OpenURI` (`open_uri`, `open_file_uri`, `open_directory`), `Screenshot` (`screenshot_blocking`, `pick_color_blocking`), `FileChooser` (`open_file_blocking`, `save_file_blocking`), `Inhibit` (`inhibit_blocking`, `close_request`), `Trash` (`trash_file_blocking`), `Background` (`request_background_blocking`), and `ScreenCast` (`screencast_start_blocking`, `close_session`).
  - **systemd, timedate1, hostname1 & locale1 (`dbus.systemd()`, `dbus.timedate()`, `dbus.hostname()`, `dbus.locale()`, `Dbus::systemd_units`, `Dbus::timedate`)**: User & system unit monitoring + `start_unit`/`stop_unit`/`restart_unit`/`reload_unit`/`enable_units`/`disable_units`/`daemon_reload`, timezone/NTP/RTC control, host/hardware metadata, and locale/X11 keymap configuration.
  - **AccountsService, Fprintd, GameMode, Secret Service, fwupd & PackageKit**: Added `dbus.accounts()` / `Dbus::accounts`, `dbus.fprint()` / `Dbus::fprint_verify` (auto claim/verify/release), `dbus.gamemode()` / `Dbus::gamemode`, `dbus.secrets()` (`org.freedesktop.secrets` plain-session keyring search/read/store/delete), `dbus.fwupd()` / `Dbus::fwupd`, and `dbus.packagekit()` (`check_updates_blocking`).
  - **CUPS, KDE Connect, Colord & Avahi**: Added `Dbus::cups` (`org.cups.cupsd.Notifier`), `dbus.kdeconnect()` / `Dbus::kdeconnect` (`ping`, `ring`, `share_url`, `share_text`), `dbus.colord()` / `Dbus::colord` (`make_profile_default`), and `dbus.avahi()` (`snapshot_blocking`, zero-polling `browse_services_blocking`).
  - **Existing D-Bus module enhancements**:
    - `NotificationsClient`: added client-side `OutgoingNotification`, `send`, `send_blocking`, and `close_notification`.
    - `TrayClient`: added `about_to_show_blocking` and `submenu_layout_blocking`, invoking `com.canonical.dbusmenu.AboutToShow` before `GetLayout`.
    - `MprisClient`: added `raise`, `quit`, `set_rate`, and `open_uri` plus playback rate metadata on `MprisSnapshot`.
    - `NetworkManagerClient`: added VPN state/helpers (`NmVpnState`, `active_vpns`, `saved_vpns`, `activate_vpn`, `deactivate_vpn`), Wi-Fi Hotspot (`HotspotOptions`, `start_hotspot`), WPA-Enterprise (`WifiEnterpriseOptions`), and `Dbus::nm_secret_agent` / `reply_secret_agent`.
    - `BluetoothClient`: added `connect_profile`, `disconnect_profile`, and `obex_send_file` (`org.bluez.obex`).
    - `UDisksClient`: added ATA S.M.A.R.T. health (`UDisksSmartHealth`) and `UDisksFormatOptions` / `format_block`.
- **Full Touchscreen (`wl_touch`) support (`input`, `wayland`, `reconcile`, `app`)**:
  - Added `TouchPhase`, `Element::on_touch`, `Surface::on_touch`, and `HeadlessRuntime::touch_surface`.
  - Implemented complete `TouchHandler` wiring (`down`, `motion`, `up`, same-frame atomic `shape` and `orientation` enrichment, and `cancel` draining all active contacts) with per-contact capture across `RetainedSurface` reconciliations.
  - Added primary-touch pointer fallback: unclaimed primary contacts automatically drive `on_click`, `on_click_sound`, `on_pointer_down`, `on_pointer_move`, and `on_pointer_up` (so standard buttons and sliders drag smoothly on touchscreens without double-firing when claimed by `on_touch`).
- **Touchpad scroll metadata & `zwp_pointer_gestures_v1` (`input`, `wayland`, `capabilities`, `reconcile`, `app`)**:
  - Added `ScrollSource`, `stop_x`/`stop_y` kinetic-scroll stop flags on `ScrollEvent`, and `HeadlessRuntime::scroll_surface_event`.
  - Added opt-in Cargo feature `pointer-gestures`, `Capability::PointerGestures`, `GesturePhase`, `GestureKind` (`Swipe`, `Pinch`, `Hold`), `GestureEvent`, `Element::on_gesture`, `Surface::on_gesture`, `HeadlessRuntime::gesture_surface`, and full `zwp_pointer_gestures_v1` (v1..=3) protocol support.
- **Native Pen Tablet / Stylus & Pad (`zwp_tablet_manager_v2`) support (`input`, `wayland`, `capabilities`, `reconcile`, `app`)**:
  - Added opt-in Cargo feature `tablet`, `Capability::TabletInput`, `TabletId`, `TabletToolId`, `TabletPadId`, `TabletToolType`, `TabletToolAxes` (`pressure`, `distance`, `tilt`, `rotation`, `slider`, `wheel_degrees`, `wheel_clicks`), `TabletToolEventKind`, `TabletToolEvent`, `TabletPadAxisSource`, `TabletPadEventKind` (`Enter`, `Leave`, `Button`, `ModeSwitch`, `Ring`, `Strip`, `Dial`), `TabletPadEvent`, `TabletEvent`, `Element::on_tablet`, `Surface::on_tablet`, and `HeadlessRuntime::tablet_surface`.
  - Implemented full `zwp_tablet_manager_v2` (v1..=2) protocol binding with frame-atomic stylus tool batching, per-tool stroke capture, proper `destroy()` lifecycle on `removed` and drop, and automatic pointer fallback (hover, left-click/drag, barrel buttons -> right/middle/side) for unclaimed stylus actions on standard widgets.

### Changed
- **Fallback-font reclamation, GPU `MemoryHints`, and SHM pool shrinking (`text`, `render`, `wayland`)**:
  - `TextEngine::trim()` now rebuilds `cosmic_text::FontSystem` from the unmapped static `fontdb::Database` (releasing all mmapped system font files without re-scanning Fontconfig), and `TextEngine::finish_frame()` automatically reclaims mmapped fallback fonts at end-of-frame when an unknown Unicode codepoint triggers an exhaustive fallback scan past `text_max_mapped_font_files` (`32` balanced, `16` low_memory, `64` high_throughput; `0` disables auto-reclaim) while preserving warm `buffer_cache` and `swash_cache`.
  - `GpuContext::new()` now forwards `MemoryConfig::gpu_memory_policy` to `wgpu::DeviceDescriptor.memory_hints` (`MemoryUsage` 8–64 MiB blocks for `balanced()`, `Minimal` 4–16 MiB for `low_memory()`, `Performance` 128–256 MiB for `high_throughput()`), and `GpuContext::trim_offscreen()` polls `wgpu::Device` non-blockingly.
  - `SurfaceShmPool::trim_idle_slots()` now closes oversized or double-buffered `memfd` mappings (without truncating) while keeping right-sized single-slot pools warm, and `acquire_for_draw()` automatically replaces oversized idle `memfd` pools when a surface shrinks by >2× (above 64 KiB).
- **Zero-thread clipboard pipe I/O (`wayland`)**: Migrated `clipboard` pipe reads and writes onto non-blocking `calloop::generic::Generic` FD sources and bounded timers matching Drag & Drop.
- **Documentation**: Updated `README.md` and `docs/{architecture,api,integrations,performance,reference,wayland}.md` for `v0.3.0`.

## [0.2.2] - 2026-11-25

### Fixed
- **Wayland layer-shell close, output hotplug & safe teardown (`wayland`, `app`, `reconcile`)**:
  - Split `WaylandEvent::SurfaceClosed` into `WindowCloseRequested`, `PopupDismissed`, and `LayerClosed` so `zwlr_layer_surface_v1.closed` (e.g. monitor OSD HDR/VRR mode switch, DisplayPort re-train, or DPMS deep sleep) tears down the layer surface and its child popups without setting `quit_requested = true`.
  - Moved `WaylandState::refresh_outputs()` into `WaylandEvent::OutputsChanged` dispatch so destroyed `wl_output`s are refreshed after `smithay-client-toolkit` removes them from `output_state` instead of leaving released proxies behind.
  - Enforced GPU-before-Wayland teardown ordering (`drop_surface_target` before `destroy_surface`) across surface removal, dismissal, and shutdown; dismissed orphan `xdg_popup`s cleanly when their parent layer surface is unmapped; and tracked `wl_seat` identity in `WaylandIdleState` so idle notifications rebind on seat replacement.
- **Memoization invalidation on output, resize, and scale changes (`render`, `app`)**: Added `MemoStore::{invalidate_all, invalidate_surface}` and invoked them on `OutputsChanged`, `SurfaceConfigured` size changes, `SurfaceScaleChanged`, and `LayerClosed` (plus `HeadlessRuntime::simulate_surface_configure`), preventing `Surface::memo(Output::All)` and nested `Element::memo` entries from disappearing or keeping stale layouts on newly created or resized outputs.
- **GPU Wayland presentation, `wl_shm` isolation & frame pacing (`render`, `wayland`, `app`)**:
  - Replaced ambiguous `bool` GPU present results with `WaylandPresentOutcome { Committed, Deferred, FallbackToShm }` and `WaylandPresentTarget`.
  - Switched `GpuRenderer::present_wayland_surface` to acquire swapchain textures *before* encoding `femtovg::Canvas` commands, recreate `WaylandGpuTarget` on `CurrentSurfaceTexture::Lost`, and never fall back to `wl_shm` on a `wl_surface` already bound to a GPU swapchain.
  - Gated redraws and reclaimable image re-decodes on `wl_surface.frame` pacing (arming the frame callback only right before commit) while clearing `frame_pending` on every compositor `configure`, integer/fractional scale change, and `OutputsChanged` so surfaces stay at zero idle redraws during DPMS off/fullscreen occlusion without stalling across mode transitions.
- **Reclaimable `SoundAsset` cache & self-healing audio playback (`audio`, `app`)**:
  - Added `SoundSource` (`File`, `EncodedBytes`, `Preset`) and `SoundAssetStore::trim_reclaimable()` so `CoreEngine::trim_memory()` and LRU cache eviction release decoded PCM buffers while keeping live `SoundAsset` handles valid for transparent on-demand re-decoding (matching `AssetCache`).
  - Added `PlaybackDriver::is_alive()` with automatic lazy driver respawn on `AudioPlayer::enqueue_play`, a `.state_changed(...)` listener on PipeWire SFX streams, clean exit on PulseAudio `Failed`/`Terminated` states, and immediate dead-handle reset on unrecoverable ALSA `writei` failures.

## [0.2.1] - 2026-11-24

### Breaking Changes
- **Runtime & `EffectCtx` (`app`, `effect`, `render`)**:
  - `Application::init` and `Application::update` now take `&mut EffectCtx<'_, Self::Message>` instead of an untyped `&mut EffectCtx<'_>`.
  - `EffectCtx::after(delay, msg)` now takes `msg: M` directly instead of a closure; `load_image_async` and `request_activation_token` no longer require `Send` on callbacks/messages.
  - Removed duplicate `App::start` (use `App::run`).
  - Standardized `EffectCtx` image lifecycle methods to `reload_image`, `set_image_retention`, `image_retention`, `evict_image_pixels`, and `unload_image` (replacing the `*_asset_*` names).
  - Removed 9 redundant `EffectCtx::*_sound*` wrappers (use `ctx.player()?`) and `EffectCtx::{set_ddc_brightness, privacy_snapshot}` (use `ctx.system()`).
  - Gated all clipboard setters on `EffectCtx` consistently under the `clipboard` feature.
  - `ActiveRenderer::from_preference` now returns `Result` and fails with `WaywiError::UnsupportedCapability` when an explicitly requested `RendererPreference::Gpu` or `RendererPreference::Software` backend is unavailable.
- **Geometry, Display, Surface & Elements (`geometry`, `display`, `surface`, `render`, `image`, `sprite`)**:
  - Replaced `geometry::Rect` and `GeoRect` with `Bounds`; removed `Size::px` (use `Size::new` or `(w, h)` tuples); renamed `display::Transform` -> `OutputTransform`.
  - Removed PascalCase `Anchor::{Top, Bottom, Left, Right}` duplicates (use bitflags such as `Anchor::TOP | Anchor::LEFT`).
  - Removed the 6 `Surface` popup passthrough setters in favor of `Surface::positioner(PopupPositioner::new(...))`; changed default `PopupPositioner` anchor/gravity to `PopupAnchor::BottomLeft` / `PopupGravity::BottomRight`.
  - Removed `CommonProps::element_key` (use `.key`), dead element-level `.on_key` (use `Surface::on_key`), `Image::source_rect` (use `.region`), `Text::optical_bounds` (use `.bounds_mode(TextBoundsMode::Optical)`), and `DecodedGifFrame`.
  - Changed `SpriteSheet::frame_size` to take pixel integers `(u32, u32)`, made `Canvas::new(size, draw)` draw directly without caching by default (pass `.cache_key(...)` to opt in), and unified `.on_click_sound` / `.on_hover_sound` onto `impl Into<SoundPlay>` (removing `.on_click_sound_volume` / `.on_hover_sound_volume`).
- **Audio (`audio`)**:
  - Replaced the 27 per-target methods on `AudioClient` with 10 target-based methods (`set_volume`, `set_volume_percent`, `adjust_volume`, `set_channel_volumes`, `set_balance`, `set_mute`, `toggle_mute`, `set_default_device`, `set_port`, `set_card_profile`) taking `AudioTarget` / `AudioPortTarget` / `AudioDirection`.
  - Merged `AudioCapture` into `AudioStream`, moved volume/mute/channel fields on `AudioDevice` and `AudioStream` into `AudioVolume`, switched `Audio::subscribe` to emit `Arc<AudioSnapshot>`, and renamed `Audio::subscribe_events` -> `Audio::events`.
- **D-Bus (`dbus`)**:
  - Removed `zbus::blocking::Connection` and all blocking D-Bus helpers.
  - Replaced the 81 flat methods on `DbusClient` with 13 domain clients (`mpris`, `nm`, `bluetooth`, `udisks`, `logind`, `tray`, `notifications`, `power_profiles`, `upower`, `modem`, `portal`, `screensaver`, `geoclue`).
  - Removed `set_property_value`, `call_method_unit`, `drain_action_results`, `last_action_*`, and the duplicate `GlobalShortcuts` struct (use `Dbus::global_shortcuts`).
- **System & Desktop (`system`, `desktop`)**:
  - Replaced raw `f64` disk byte-rate fields with `DataRate`.
  - Split `SystemProviders` (`sample_*`) from internal typed `SystemSamplers<M>` (removing `Box<dyn Any>`).
  - Made `system::hardware::set_backlight_percent` pure sysfs (use `EffectCtx::set_backlight_percent` for sysfs-first with shared logind D-Bus fallback).
  - Collapsed `DesktopEntry::launch*` and `spawn_detached*` variants onto `LaunchOptions` and `SpawnOptions` (`spawn_with`, `spawn_shell`), and removed the 7 redundant `xdg_*` free functions in favor of `XdgBaseDirs`.
- **Compositor, Control & Prelude (`compositor`, `control`, `lib`, `prelude`)**:
  - Removed crate-root `waywi::{hyprland, sway}` aliases (use `waywi::compositor::{hyprland, sway}`).
  - Unified `switch_workspace` onto `WorkspaceTarget<'_>`, removed `sway::WorkspaceRef` and redundant Hyprland workspace/keyboard helpers, and renamed `WorkspaceRefJson` -> `HyprlandWorkspaceRef`.
  - Unified `Control::socket` onto `ControlRequest -> M` and removed `Control::socket_requests`.
  - Pruned internal/leaf types from `waywi::prelude`.

### Added
- **Memory configuration presets & manual trimming (`app`, `effect`, `render`, `asset`, `text`, `audio`, `dbus`, `wayland`)**: Added `MemoryConfig` (`MemoryConfig::balanced()`, `MemoryConfig::low_memory()`, `MemoryConfig::high_throughput()`, plus builders for `default_buffer_retention`, `default_asset_retention`, `arena_limits`, `text_cache`, `load_system_fonts`, `software_scratch_bytes`, `canvas_cache_bytes`, `gpu_cache`, `sound_cache_bytes`, `icon_lookup_max_entries`, `dbus_cache_snapshots`, and `dbus_idle_disconnect`), `App::memory(config)`, `HeadlessRuntime::{with_memory, memory_config, trim_memory}`, and `EffectCtx::{memory_config, trim_memory}`. Calling `ctx.trim_memory()` immediately trims reclaimable decoded image pixels, renderer text/canvas/scratch/GPU caches, `FrameArenaPool`, `MemoStore`, system/D-Bus caches, sound cache, and idle `wl_shm` pools.
- **Static image loading & builder ergonomics (`effect`, `geometry`, `image`, `sprite`, `surface`)**: Added `EffectCtx::{load_image_static, load_image_static_sized}` backed by `AssetSource::StaticBytes`, `SpriteSheet::grid(asset, cols, rows)`, `Surfaces::none()`, `From<(W, H)> for Size`, `From<LogicalSize> for Size`, `From<LogicalPx> for Edges`, `From<(LogicalPx, LogicalPx)> for Edges`, `From<(u32, u32, u32, u32)> for SourceRect`, and crate-root re-exports for `ErrorKind`, `MemoryConfig`, and `OutputTransform`.
- **Unified compositor events & workspace targeting (`compositor`)**: Added `Compositor::subscribe`, `CompositorEvent`, `CompositorClient` enum (`Hyprland`, `Sway`), `WorkspaceTarget<'a>` (`Id`, `Name`, `Relative { delta, existing_only }`, `Previous`), and `KeyboardLayoutSwitch` (`Next`, `Prev`, `Index(u32)`).
- **Target-based audio & sound API (`audio`)**: Added `SoundId`, `SoundPlay` (with `From` conversions for `SoundAsset`, `(SoundAsset, f32)`, `SoundPreset`, `(SoundPreset, f32)`), `AudioDirection`, `AudioTarget`, `AudioPortTarget`, `AudioVolume`, and `AudioPlayer::{unload, clear_cache, play_at, play_trigger}` with LRU cache limits.
- **Domain-grouped `DbusClient` API & typed action replies (`dbus`)**: Added 13 lightweight `Copy` domain clients (`mpris`, `nm`, `bluetooth`, `udisks`, `logind`, `tray`, `notifications`, `power_profiles`, `upower`, `modem`, `portal`, `screensaver`, `geoclue`), `DbusActionReply`, `WifiConnectOptions`, `UDisksMountOptions`, `PowerAction`, `BluetoothAgentReply`, and `DbusClient::trim_cache()`.
- **Unified desktop launch/spawn options & bounded icon lookup (`desktop`, `system`)**: Added `LaunchOptions`, `SpawnOptions`, `spawn_with`, `spawn_shell`, `XdgBaseDirs::{application_dirs, icon_dirs, sound_dirs}`, `XdgIconLookup::{with_max_entries, set_max_entries, clear, clear_results}`, `sample_privacy_oneshot()`, `BatterySnapshot`, and `ThermalSnapshot`.
- **Default control socket subscription (`control`)**: Added `Control::default_socket` and made `ControlRequest: Clone + Send + Sync + Deref<Target = str>`.

### Changed
- **Shared runtime engine & default surface resolution (`app`, `surface`, `render`, `error`)**: Extracted shared `CoreEngine<M>` across `LoopRuntime` and `HeadlessRuntime`, boxed `ElementKind::Sprite`, resolved empty surface `title`/`namespace` from `App::name` and omitted `buffer_retention` from `MemoryConfig::default_buffer_retention`, and updated `From<std::io::Error>` to wrap `WaywiError::Io` losslessly.
- **Documentation**: Updated `README.md` and all `docs/*.md` guides for `v0.2.1`.

### Fixed
- **Memoization & arena stability (`arena`, `render`)**: Fixed `FrameArenaPool` slab pinning under `Element::memo` / `Surface::memo` capturing closures by storing capturing `FrameSharedFn` closures in `Rc<F>` (keeping the zero-alloc ZST fast path), added parent-child `MemoId` dependency tracking so nested `Element::memo` entries survive `MemoStore::prune_stale()`, and eliminated double-hashing when pre-hashed `ElementKey`/`SurfaceKey`/`SubscriptionKey` values pass through `Into*Key`.
- **CPU wakeups, idle teardown & blocking calls (`dbus`, `audio`, `compositor`, `accessibility`, `app`, `wayland`, `system`, `control`)**:
  - Replaced `TrayWorker`'s unfiltered session-bus signal subscription with 3 targeted match rules and coalesced bursts in `ServiceWatchWorker`, `PowerProfilesWorker`, and `LogindWorker`.
  - Added lock-free `AtomicBool` pending checks and `stop_if_idle()` teardown on `AudioClient`; fixed `AudioPlayer` `RodioDriver` to play `Arc<[f32]>` without cloning, ALSA playback to release idle `PCM` handles after 2s, and PulseAudio playback to drain latency before corking.
  - Unified Hyprland and Sway event streams onto a shared non-blocking `StreamWorkerHandle`, fixing Sway's 2-second `Drop` stall and bounded `read_frame` allocation against oversized headers.
  - Gated `AccessibilityBridge::sync` behind screen-reader activation state so idle apps pay zero tree-build cost; stopped `OutputsChanged` from overwriting runtime-probed IPC/GPU/audio flags or invoking blocking D-Bus portal queries; eliminated per-message `wl.outputs.clone()` in `LoopRuntime::run_effect`; added zero-clone single-subscriber delivery in `BroadcastSlot`; wired `AsyncAssetWorker` draining in `HeadlessRuntime`; fixed `WaylandState::present_surface_with` to propagate render errors; fixed `DbusActionOutbox` duplicate replay on attach; fixed `kdeglobals` icon theme parsing to respect the `[Icons]` section; fixed `DesktopEntry::launch_action` to honor `Terminal=true`; and tracked/cleaned up slow-client control reply writer tokens on socket removal.

## [0.1.11] - 2026-11-23

### Added
- **Per-channel volumes, spatial channel maps & surround-aware L/R balance (`audio`)**: `AudioClient::set_volume_linear`, `set_source_volume_linear`, `set_stream_volume_linear`, and `set_capture_volume_linear` now scale existing per-channel linear volumes proportionally (`pa_cvolume_scale` semantics, preserving stereo and multi-channel balance even across `0.0` via a cached non-zero channel template) across PipeWire, PulseAudio, and ALSA. Added `AudioChannelPosition` (`is_left`, `is_right`, `is_center_or_lfe`, `is_front`, `is_rear`, `short_name`, `default_map_for_count`), `channels: Vec<f32>` and `channel_map: Vec<AudioChannelPosition>` on `AudioSnapshot` (`channels`, `channel_map`, `source_channels`, `source_channel_map`), `AudioDevice` (`channel_map`), `AudioStream`, and `AudioCapture` (populated from PipeWire `SPA_PROP_channelMap`, PulseAudio `pa_channel_map`, and ALSA `SelemChannelId`), balance getters (`AudioSnapshot::{balance, source_balance}`, `AudioStream::balance`, `AudioCapture::balance`, `compute_stereo_balance`, `compute_balance_with_map`), and surround-aware balance and per-channel setters on `AudioClient` (`set_balance`, `set_channel_volumes`, `set_source_balance`, `set_source_channel_volumes`, `set_stream_balance`, `set_stream_channel_volumes`, `set_capture_balance`, `set_capture_channel_volumes` — preserving Center/LFE levels and front-to-rear fade ratios on 4.0/5.1/7.1 surround setups).
- **Manual audio port (route) & sound card profile selection (`audio`)**: Added `AudioPortAvailability` (`Unknown`, `Available`, `Unavailable`; `is_available`), `AudioPort`, `AudioProfile`, and `AudioCard` (re-exported in `waywi::audio` and `waywi::prelude`). Extended `AudioDevice` with `card_id`, `active_port`, and `ports`, extended `AudioSnapshot` with `cards` and helpers (`ports`, `active_port`, `source_ports`, `active_source_port`, `card`, `default_sink_card`, `default_source_card`), and added `AudioClient::{select_port, select_device_port, select_source_port, select_source_device_port, select_card_profile}` backed by PipeWire `ParamType::{EnumRoute, Route, EnumProfile, Profile}` and PulseAudio port/card introspection.

### Changed
- **Decoupled `audio-playback` decoding from `AudioPlayer` output driver selection (`audio`)**: Enabling `audio-playback` (for `.oga`/`.ogg`/`MP3`/`FLAC` decoding via `rodio::Decoder`) no longer disables `NativeDriver` (`PipeWire`, `PulseAudio`, `ALSA`). `AudioPlayer` now prioritizes native `PipeWire -> PulseAudio -> ALSA` PCM drivers at runtime with connection verification and falls back to `RodioDriver` only when no native backend is available. Similarly, `AudioClient` now keeps all compiled-in backends as ordered runtime fallbacks instead of hiding fallback features at compile time.
- **Zero-polling on-demand PipeWire & PulseAudio SFX stream corking (`audio`)**: `run_pipewire_playback` now sets `MEDIA_ROLE => "Event"` and `APP_NAME => "waywi"`, replaces the `50ms` `loop_ref.iterate` polling loop with a zero-polling `pipewire::channel` wakeup, starts with `StreamFlags::INACTIVE`, activates the stream on-demand when voices are queued (`stream.set_active(true)`), and corks it (`stream.set_active(false)`) after draining `250ms` of post-playback silence (`run_pulse_playback` likewise sets `media.role = "event"` and corks when idle). Internal `waywi-audio` event streams are excluded from `AudioSnapshot::streams`, and hardware sinks can suspend normally when idle.
- **`AudioDevice`, `AudioStream`, `AudioCapture`, and `AudioSnapshot` fields (`audio`)**: All four types now implement `Default` and include the new port, card, channel map, and per-channel volume fields; struct literals constructing them directly should include `..Default::default()`.
- **Documentation**: Updated `docs/integrations.md` and `docs/reference.md` for all v0.1.11 audio APIs and playback behavior.

### Fixed
- **PipeWire hardware route sync on port/profile switches (`pipewire`)**: Added a `Device::info` listener watching `DeviceChangeMask::PARAMS` to invalidate stale routes immediately when `ParamType::Route` flags change (including `SPA_PARAM_INFO_SERIAL` toggles or unreadable profiles) and re-enumerate `ParamType::{Route, EnumRoute, Profile, EnumProfile}` whenever readable (pruning superseded batches by async `seq`). Prevents stale `route.index` writes from switching shared-sink multi-port cards (such as front-panel headphones vs. rear line-out) back to an inactive port on volume or mute updates.
- **PipeWire `SPA_PARAM_ROUTE_props` inner object ID (`pipewire`)**: Encoded the inner `Props` object inside `device.set_param(ParamType::Route, ...)` with `id = ParamType::Route` (`13`) instead of `ParamType::Props` (`2`) to match PipeWire and WirePlumber wire conventions.
- **PipeWire route matching, mono channel count, route-vs-props precedence & default metadata (`pipewire`)**: Stopped `resolve_route_for_node` from falling back across different `card.profile.device` sub-devices on the same card; used the active route's channel count instead of `max(route.channels, node.channels)` so mono routes (e.g., USB microphones) are not written as stereo; seeded nodes from already-enumerated `Device` routes and prevented `Node::Props` from overwriting active hardware `Route` volume/mute/channels; and tracked `default.audio.{sink,source}` separately from `default.configured.audio.{sink,source}` so an offline configured sink/source cannot clobber the active default.
- **Feature-gate hygiene (`system::privacy`)**: Scoped `AtomicBool`/`Ordering` imports in `src/system/privacy.rs` to `#[cfg(feature = "dbus")]` so `--no-default-features --features pipewire` compiles with zero warnings.

## [0.1.10] - 2026-11-23

### Added
- **`org.freedesktop.portal.Settings` support (`dbus`)**: Added pure-mechanism appearance queries and zero-polling `SettingChanged` subscriptions via `PortalColorScheme`, `PortalContrast`, `PortalAccentColor` (with finite `0.0..=1.0` validation and `.to_color()`), `PortalSettingsSnapshot`, `Dbus::portal_settings`, `DbusClient::portal_settings_snapshot`, and raw `DbusClient::portal_setting(namespace, key)`.
- **Wayland `xdg-activation-v1` focus handoff (`wayland`, `effect`, `capabilities`)**: Added `Capability::XdgActivation`, non-blocking `EffectCtx::request_activation_token` (attaching recent input seat/serial/surface when available), `EffectCtx::activate_surface`, honest `WaywiError::UnsupportedCapability(Capability::XdgActivation)` delivery when unavailable, and `HeadlessRuntime::activated_surfaces`.
- **Wayland `xdg-popup` & `zwlr_layer_surface_v1.get_popup` (`surface`, `wayland`, `reconcile`)**: Added `SurfaceKind::Popup`, `Surface::popup(key, parent_surface)`, `PopupAnchor`, `PopupGravity`, `PopupConstraintAdjustment`, `PopupPositioner`, layer-shell `get_popup` attachment before initial `wl_surface.commit`, `xdg_popup.reposition` (`xdg_wm_base >= 3`) on positioner or size updates, parent-first creation / child-first destruction ordering (including nested popups), `xdg_popup.popup_done` routing to `Surface::on_close` without requesting application exit, and `HeadlessRuntime::simulate_popup_dismiss`.
- **Public XDG Base/User Directories & Desktop Entry completeness (`desktop`)**: Exposed `pub mod desktop::xdg` with XDG Base Directory Spec v0.8 helpers (`XdgBaseDirs`, `xdg_config_home`, `xdg_config_dirs`, `xdg_data_home`, `xdg_data_dirs`, `xdg_cache_home`, `xdg_state_home`, `xdg_runtime_dir` rejecting relative paths) and `~/.config/user-dirs.dirs` parsing (`XdgUserDirs`), added `spawn_detached_with_env`, and extended `DesktopEntry` with `[Desktop Action <id>]` (`DesktopAction`), `dbus_activatable`, `dbus_service_name`, `dbus_object_path`, `action`, and `XDG_ACTIVATION_TOKEN` launch helpers (`launch_with_token`, `launch_with_terminal_and_token`, `launch_action`).
- **Hyprland v0.57+ workspace metadata, rules & tiling layouts (`hyprland-ipc`, `compositor`)**: Added `WorkspaceType`, `HyprlandLayout` (`Dwindle`, `Master`, `Monocle`, `Scrolling`, `Custom`, `Unknown`), `WorkspaceRuleInfo`, extended `WorkspaceInfo`, `WorkspaceRefJson`, `CompositorWorkspace`, and `SwayWorkspace`, and added `HyprlandClient::{workspace_rules, default_layout, set_default_layout, layout_message, switch_workspace_relative, switch_workspace_previous, move_workspace_to_monitor, rename_workspace}` with HyprLua fallback support.

### Changed
- **Batched Hyprland workspace queries (`hyprland-ipc`)**: `HyprlandClient::workspaces` now batches `j/activeworkspace` and `j/workspaces` into a single `[[BATCH]]` IPC roundtrip with automatic non-batch fallback.
- **`DesktopEntry` fields (`desktop`)**: `DesktopEntry` now implements `Default` and includes `dbus_activatable` and `actions`; struct literals constructing `DesktopEntry` directly should include `..Default::default()`.
- **Documentation**: Updated `docs/api.md`, `docs/integrations.md`, `docs/reference.md`, and `docs/wayland.md` for all v0.1.10 APIs.

### Fixed
- Fixed Hyprland v0.57+ workspace JSON and `socket2` `*v2` event parsing when special or named workspaces omit numeric `id` fields, preserving string addresses such as `special:magic` and preventing `WorkspaceId(0)` focus/tracking collisions.
- Fixed `WorkspaceInfo::target_spec` for renamed numbered workspaces on older Hyprland releases and `name:`-prefixed named workspaces, aligned `HyprlandClient::rename_workspace` with `hl.dsp.workspace.rename({ workspace, name? })` (supporting empty-name reset), and allowed relative monitor selectors (e.g., `+1`) in `move_workspace_to_monitor`.
- Stopped attaching a null buffer before destroying `xdg_popup` surfaces to stay compliant with `xdg-shell` protocol rules, and ensured popup positioner/size changes mark `config_dirty` during reconciliation.

## [0.1.9] - 2026-11-22

### Added
- **Reclaimable asset cache & on-demand re-decoding (`asset`, `effect`, `image`)**: Added `AssetRetention` (`KeepDecoded` default, `ReclaimAfterPresent`), `EffectCtx::{set_asset_retention, asset_retention, evict_asset_pixels}`, and `HeadlessRuntime::{asset_pixels_resident, decoded_asset_bytes}`. Path-backed assets drop decoded RGBA8 pixels to 0 bytes (and byte-backed assets keep only compressed encoded bytes) after presentation or explicit eviction, while preserving handles, dimensions, and revisions and transparently re-decoding `Image` and `Sprite` assets before any future redraw (including resize, hotplug, or hidden sprites reappearing). Multi-output commits decode shared reclaimable assets once per pass, and raw pixel assets (`load_rgba_image`) honestly reject eviction/reclaim.
- **SHM release-after-commit & software scratch auto-shrink (`surface`, `wayland`, `renderer-software`)**: Added `BufferRetention` (`Retain` default, `ReleaseAfterCommit`) and `Surface::buffer_retention(...)` so static surfaces (such as `Layer::Background` wallpapers) call `SurfaceShmPool::shrink_and_unmap()` once `wl_buffer.release` marks all slots idle with no redraw pending, releasing client-side SHM pools to 0 bytes without truncating backing `memfd`s. `SoftwareRenderer` also caps retained scratch buffers (`scratch_rgba`, `scratch_sprite_cell`) at `2 MiB` after large tile draws.
- **Optical / ink text bounds (`text`, `renderer-software`, `renderer-gpu`)**: Added `TextBoundsMode` (`Typographic` default, `Optical`), `Text::{bounds_mode, optical_bounds}`, and `TextLayoutOptions::bounds_mode` to measure actual rasterized glyph ink extents once per cache miss and center PUA / Nerd Font icons accurately across both Software and GPU renderers with zero overhead for standard typographic text.
- **Expanded `systemd-logind` power actions & `Can*` capability queries (`dbus`)**: Added `PowerActionSupport` (`Yes`, `Challenge`, `No`, `NotAvailable`; `from_logind_str`, `is_supported`, `needs_auth`), `LogindPowerCapabilities` (populated concurrently on `LogindSnapshot::power_capabilities` and `DbusClient::logind_power_capabilities`), and non-blocking `DbusClient::{hybrid_sleep, suspend_then_hibernate, reboot_to_firmware_setup, soft_reboot, halt, terminate_session}` with action deduplication and a 25s interactive Polkit timeout.
- **Full XDG Desktop Notifications v1.2 hints & configurable capabilities (`dbus`)**: Added `NotificationCapabilities` bitflags (`ACTION_ICONS`, `ACTIONS`, `BODY`, `BODY_HYPERLINKS`, `BODY_IMAGES`, `BODY_MARKUP`, `ICON_MULTI`, `ICON_STATIC`, `PERSISTENCE`, `SOUND`), `Dbus::notifications_with_capabilities`, and `DbusClient::{set_notification_capabilities, notification_capabilities}` backed by lock-free `AtomicU16`. Parsed all official XDG v1.2 hints into `DesktopNotification` (`category`, `action_icons`, `resident`, `transient`, `suppress_sound`, `sound_name`, `sound_file`, `position`) and implemented `Default` on `DesktopNotification`.
- **Upgraded sprites, texture atlases, resampling filters & transform pivots (`sprite`, `image`, `geometry`, `render`)**: Added `SourceRect`, `ImageFilter::{Bilinear, Bicubic, Nearest}`, `Image::{source_rect, region, flip_x, flip_y, filter}`, `Sprite::{flip_x, flip_y, filter, on_animation_end}`, `SpriteFrame`, `Frames::{Range, Sequence, Timed}`, `SpriteSheet::{from_regions, regions, region_of, timed_animation}` with unbounded `Arc<Vec<AnimationDef>>` and `SharedStr` names, `Transform::{pivot, pivot_point}`, reusable `SoftwareRenderer::scratch_sprite_cell`, and mipmapped GPU textures (`GENERATE_MIPMAPS`) with normalized cache keys.

### Changed
- **In-place sprite animation updates (`reconcile`)**: `Reconciler::tick_animations` now patches `PaintPayload::Sprite::frame_index` in-place on retained `DrawCommand`s by `NodeId` instead of rebuilding `commands` and `hit_nodes` on every animation frame.
- **`DesktopNotification` fields (`dbus`)**: Struct literals constructing `DesktopNotification` directly should include `..Default::default()` to cover the new XDG v1.2 hint fields.
- **Documentation**: Updated `docs/api.md`, `docs/integrations.md`, `docs/performance.md`, `docs/reference.md`, and `docs/wayland.md` for all new v0.1.9 APIs and retention policies.

### Fixed
- Fixed memoized sprite animation deadline calculation (`collect_subtree_deadlines` now preserves `SpriteTimeline::next_deadline` instead of resetting to the first frame duration) and improved `Fps::frame_duration` precision to `f64`.
- Fixed software bitmap renderer flip/rotate affine translation offsets, `prev.affine != affine` dirty detection in retained node reconciliation, and preservation of unpresented animation dirty rects across `Reconciler::reconcile_surface_instance`.

## [0.1.8] - 2026-11-21

### Added
- **Hyprland v0.57 hydration (`hyprland-ipc`)**: Expanded the Hyprland IPC layer for v0.57 — bounded request/response command client, full `socket2` event stream coverage (workspace/window/monitor/keyboard-layout/urgent/submap variants including the `v2` payloads), and HyprLua support: Lua-syntax dispatch fallbacks (`hl.dsp.*`) for window/workspace/DPMS dispatchers, `keyword` → `eval hl.config(...)` mapping, `eval`, `batch`, and `raw_command`. `Sway`'s event stream received the same zero-polling treatment.
- **Zero-polling IPC workers (`hyprland-ipc`, `sway-ipc`)**: Replaced timed socket polling with zero-polling `poll(2)` + `eventfd(2)` listeners, bounded steady-state buffers, exponential reconnect backoff, and instant shutdown on drop.
- **Generic D-Bus calls (`dbus`)**: `DbusClient::call_method`/`call_method_blocking` building request bodies from `DbusValue` (`Struct`, `Dict`, `Variant`, …), `Dbus::subscribe_signal` for arbitrary interfaces, and `Dbus::action_results` for lane-routed reply/error results; `DbusSignalEvent`/`DbusActionLane`/`DbusActionResult` types.
- **Process cgroup lookup (`system`)**: `ProcessCgroupInfo` + `system::os::cgroup::{read_process_cgroup, parse_process_cgroup}` reading `/proc/<pid>/cgroup` with cgroup v1/v2 parsing, re-exported under `system::os` (`system.rs` also re-exports it at the domain root).
- **UDisks2 storage (`dbus`)**: `UDisksDrive`, `UDisksBlockDevice`, `UDisksSnapshot`, and `Dbus::udisks` hotplug subscription tracking `org.freedesktop.UDisks2` ObjectManager events on the system bus; NUL-terminated UDisks2 byte-array decoding; non-blocking `DbusClient` actions for `Filesystem.{Mount,Unmount}`, `Drive.{Eject,PowerOff}`, and `Encrypted.{Unlock,Lock}` with explicit `auth.no_user_interaction` Polkit option support.
- **USB device enumeration (`system`)**: `UsbDevice`, `read_usb_devices`, `read_usb_devices_from`, and `SystemProviders::usb_devices` reading `/sys/bus/usb/devices`.

### Fixed
- `--no-default-features --features dbus` failed to compile: `tray.rs` uses `serde::Serialize` bounds but `dep:serde` was only pulled in via `hyprland-ipc`/`sway-ipc`; the `dbus` feature now declares `dep:serde`.
- `HyprlandClient` now probes `j/systeminfo` once per client and seeds the HyprLua dispatch mode directly when the compositor reports `configProvider: lua` (Hyprland v0.55+ Lua configs, mandatory on v0.57+), so the first dispatch/keyword no longer wastes a guaranteed-failing legacy roundtrip. The error-based fallback stays in charge whenever the field is missing or the probe fails, so behavior on older Hyprland is unchanged.

## [0.1.7] - 2026-11-19

### Added
- **Desktop launcher & entry helpers (`desktop`)**: `TryExec` executability checks in `should_show_in_launcher()` / `filter_launcher_entries()`, `Path=` working directory support (`working_dir`, `spawn_detached_in`), `Terminal=true` resolution and execution (`terminal_argv`, `launch_with_terminal`), and scored entry search helpers (`DesktopSearchMatch`, `search_desktop_entries`, `search_desktop_entries_with_weights`).
- **Headless input & scroll controllers (`input`)**: UI-agnostic `TextInputState` / `TextInputOutcome` (UTF-8 char-boundary editing, selection, word operations, masking, and IME composition handling) and `ScrollState` (offset clamping, wheel vs smooth delta handling, `ensure_visible`, `visible_range`, and `thumb_metrics`), plus `HeadlessRuntime::{scroll_surface_point, key_surface, simulate_ime, surface_ime_config}`.
- **Wayland IME (`ime` feature, `zwp_text_input_v3`)**: Per-seat `zwp_text_input_manager_v3` / `zwp_text_input_v3` double-buffered composition (`preedit_string`, `commit_string`, `delete_surrounding_text`, `done`), `4000`-byte UTF-8 surrounding-text windowing (`ImeSurroundingText`), `ImeConfig`, `ImeEvent`, `Surface::{ime, on_ime}`, `Capability::TextInput`, and `wayland-server` `socketpair` roundtrip tests.
- **Proportional text cursor measurement (`text`)**: `TextCursorMetrics`, `TextLayoutOptions`, and `EffectCtx::{measure_text_cursor, measure_text_cursor_with}` backed by `TextEngine`'s shared shaped buffer cache, along with `TextInputState::display_cursor_bytes` and software pixel cursor-line rendering tests.
- **Audio stream app metadata (`audio`)**: `icon_name` (`application.icon_name` / `application.icon-name`) and `app_id` (`application.id` / `pipewire.access.portal.app_id`) fields on `AudioStream` and `AudioCapture`, shared lazy `StreamMetadata::extract` / `first_non_empty_prop_by` helpers, and PipeWire `Node::info` stream metadata/target updates.
- **Two-way control socket requests (`control-ipc`)**: `Control::socket_requests`, `ControlRequest` (`command`, `into_command`, `can_reply`, `reply`), and `send_command_with_reply` for one-shot replies with non-blocking `Interest::WRITE` fallback and automatic write-half EOF on drop.

### Changed
- **Non-blocking control socket (`control-ipc`)**: Replaced the blocking `100ms` per-client read loop with non-blocking per-client `calloop` `Generic` sources and an on-demand `2s` deadline timer (dispatched immediately on `\n` while keeping connections open until `EOF` or `4096` bytes, and evicting the oldest pending connection FIFO above `16` concurrent peers).
- Updated `docs/api.md`, `docs/architecture.md`, `docs/integrations.md`, `docs/reference.md`, and `docs/wayland.md` to match all new APIs and feature flags.

### Fixed
- Scroll hit-testing (`hit_test_scroll` / `RetainedSurface::dispatch_scroll_at`) now targets the topmost node binding `on_scroll` instead of stopping on clickable child items without scroll handlers; verified end-to-end with `Clip`, `Transform::translate`, `HeadlessRuntime`, and `SoftwareRenderer`.

## [0.1.6] - 2026-11-17

### Added
- Default source (microphone) control in every backend: `set_source_volume_linear`, `set_source_muted`, `toggle_source_mute`, and `select_source` are no longer silently ignored. PulseAudio applies them via `set_source_volume`/`set_source_mute`/`set_default_source`; PipeWire drives the default source node and honors `select_source`; ALSA maps them to the hardware capture element (preferred `Capture`/`Mic`/`Internal Mic`/`Front Mic`/`Headset Mic`/`Input` selem, users can list and select capture elements) with `AudioSnapshot` filled from it. Sink monitors are excluded from PulseAudio source listings.
- Per-application audio streams: `AudioSnapshot` now carries `streams` (playback, PulseAudio sink inputs / PipeWire `Stream/Output/Audio`) and `captures` (recording, source outputs / `Stream/Input/Audio`) with app name, binary, output binding, volume (linear + dB), and mute; `AudioStream`/`AudioCapture` types and `stream_percent`/`capture_percent` helpers were added. `AudioClient` gained matching controls (`set_stream_volume_linear`, `set_stream_muted`, `toggle_stream_mute`, and the `capture_*` set); unknown stream IDs are ignored so stale commands from ended streams stay harmless. Pipewire and PulseAudio backends implement streams; the ALSA fallback keeps an empty stream list (hardware mixer only).

### Fixed
- The ALSA and PulseAudio backends no longer drop `SetSourceVolumeLinear`/`SetSourceMuted`/`ToggleSourceMute`/`SelectSource` commands, which also removes dead-code warnings in non-default feature combinations.

## [0.1.5] - 2026-11-17

### Removed
- `allocator-mimalloc` and `allocator-jemalloc` (and the `ci-all` meta-feature that existed to work around their mutual exclusion). Measured impact on widget workloads was neutral-to-negative (memo-hit paths regressed up to 16%), while the frame arena already removes most per-frame allocations; the allocator choice returns to being fully owned by the final binary via `#[global_allocator]`, unaffected by waywi.

## [0.1.4] - 2026-11-17

### Added
- Optional allocator features: `allocator-mimalloc` (`mimalloc` 0.1.x) and `allocator-jemalloc` (`tikv-jemallocator` 0.7). Both are opt-in, non-default, and mutually exclusive (guarded with `compile_error!`); they select the process-wide global allocator for the final binary while the per-frame bump arena stays unchanged on top of either choice.

### Fixed
- Fixed compositor crashes (`SIGBUS` / `BUS_ADRERR` during buffer commits, observed on Hyprland) by keeping SHM backing `memfd`s valid while the compositor may still map previously committed buffers: hiding or removing a surface now destroys pool/buffer proxies and closes the `memfd` without truncating it.
- Stale `wl_buffer.release` events can no longer free a recycled buffer slot after a resize (buffer incarnations now carry generational tags); `BufferReleased` is only emitted when a slot was actually freed.
- Wayland startup roundtrips now propagate disconnect errors instead of silently continuing with an empty state.

### Changed
- `#![deny(clippy::undocumented_unsafe_blocks)]` now enforces a `SAFETY:` comment on every `unsafe` block (two missing comments in `arena::shared_fn` and `system::network::proc_net` were added).
- `MemoStore` memo scopes are no longer re-entrant in a way that resurrected a stale TLS raw pointer after a nested scope (invalid pointer use under Stacked Borrows, caught by Miri); a nested `enter_memo_scope` now simply overwrites the active context and scope exit clears it. Production call sites never nested scopes, so behavior is unchanged. Unit tests were added for `MemoStore` hits/misses, pruning, and scope handling.
- CI gained a Miri job that runs the arena, memo, and `SharedStr` unit tests under the nightly interpreter to mechanically check the crate's hand-written `unsafe` code.
- Control socket (`control-ipc`): socket names no longer resolve into a world-writable directory when `$XDG_RUNTIME_DIR` is unset (fallback is `/run/user/<euid>`); the listener only replaces stale socket files owned by the current user (probed with `ECONNREFUSED`), rejects peers with a different UID via `SO_PEERCRED`, and drains newline-terminated commands across chunked writes.
- DDC/CI: monitor luminance writes are skipped until the bus's monitor has reported its own maximum luminance (previous silent fallback maximum removed).
- Two internal unit tests added for control-socket chunked-command accumulation and stale-socket replacement.

## [0.1.3] - 2026-11-16

### Added
- Asynchronous D-Bus action result and error reporting via `Dbus::action_results`, `DbusActionResult`, `DbusActionLane`, and `DbusClient::{drain_action_results, last_action_result, last_action_error, take_last_action_error}` (including NetworkManager `reply_object_path` and `nm_forget_wifi_ssid` deletion `count`).

### Changed
- Routed all `DbusClient` action methods through bounded, per-domain FIFO execution lanes (`BluetoothAdapter`, `BluetoothDevice`, `NetworkManager`, `Mpris`, `Desktop`) with action deduplication, a single shared timeout coordinator (`SharedDbusTimer`), and non-blocking `Drop`.
- Added in-memory `DbusSnapshotCache` populated by active D-Bus subscriptions (0 ms reads) with automatic cache invalidation when subscriptions stop.
- Updated `docs/reference.md` with the new D-Bus action result and NetworkManager helper APIs.

## [0.1.2] - 2026-11-16

### Added
- Reactive MPRIS media player subscription (`Dbus::mpris`, `MprisEvent`, `MprisLoopStatus`), extended metadata fields on `MprisSnapshot`, and playback/seek/volume/shuffle/loop/raise/quit controls on `DbusClient`.
- BlueZ `Agent1` pairing/authorization subscription (`Dbus::bluetooth_agent`, `BluetoothAgentEvent`, `BluetoothAgentRequest`), `BluetoothTransportFilter`, and extended adapter/device/profile controls on `DbusClient`.
- NetworkManager Wi-Fi connection management (`nm_connect_wifi`, `nm_connect_hidden_wifi`, `nm_activate_connection`, `nm_deactivate_connection`, `nm_disconnect_device`, `nm_forget_connection`, `nm_forget_wifi_ssid`), `NmWifiSecurity` (`Wpa3Sae`, `Owe`, etc.), `NmSavedConnection`, and cached worker updates.

### Changed
- Updated `docs/integrations.md` and `docs/reference.md` to document the new MPRIS, Bluetooth, and NetworkManager APIs.

## [0.1.1] - 2026-11-15

### Fixed
- Configured `[package.metadata.docs.rs]` with pure-Rust features so `docs.rs` builds succeed without system C audio development headers (`libpipewire-0.3`, `libpulse`, `alsa`).
- Removed redundant `package.homepage` field in `Cargo.toml`.

## [0.1.0] - 2026-11-15

### Added
- Wayland layer-shell, XDG popup/window, and headless runtime (`App`, `Application`, `HeadlessRuntime`, `EffectCtx`).
- Software 2D renderer (`tiny-skia`) and optional GPU renderer (`femtovg` / `wgpu`) with shared text shaping (`cosmic-text`).
- Element tree, layout, hit-testing, input events, animations, timers, and `AccessKit` accessibility tree support.
- System telemetry providers (`cpu`, `memory`, `time`, `gpu`, `disk`, `network`, `battery`, `sensors`, `uptime`, `brightness`, `privacy`) and unit helpers.
- Desktop entry discovery/launching, XDG icon lookup, clipboard, idle notifications, compositor IPC (`hyprland-ipc`, `sway-ipc`), control IPC, audio backends (`pipewire`, `pulseaudio`, `alsa`, `audio-playback`), and D-Bus integrations (`dbus`, `global-shortcuts`).
