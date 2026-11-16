# Library Reference

Public API reference for `waywi`. Core types, builders, and helpers are re-exported by `waywi::prelude::*`. Feature-gated items list their Cargo feature flag.

## Prelude & Top-Level Modules

- `waywi::prelude::*`: re-exports core app, surface, geometry, primitive, text, input, timer, system, desktop, and enabled integration symbols.
- Top-level public modules: `accessibility`, `app`, `audio`, `capabilities`, `clipboard` (`clipboard`), `compositor`, `control` (`control-ipc`), `dbus` (`dbus`), `desktop`, `display`, `effect`, `error`, `geometry`, `hyprland` / `sway` (under `compositor` or root re-exports when enabled), `idle` (`idle-notify`), `image`, `input`, `render`, `sprite`, `subscription`, `surface`, `system`, `text`, `timer`.
- Top-level type alias: `waywi::Result<T>` (`waywi::error::Result<T>`).

## Application Runtime (`app`)

- `Application` trait (`app::runner`):
  - `type State;`
  - `type Message: Clone + 'static;`
  - `fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State>;`
  - `fn update(state: &mut Self::State, msg: Self::Message, ctx: &mut EffectCtx<'_>) -> Result<()>;`
  - `fn view(state: &Self::State) -> Surfaces<Self::Message>;`
  - `fn subscriptions(state: &Self::State) -> Subscriptions<Self::Message>;`
- `App` (`app::runner`):
  - `App::new() -> Self`
  - `.renderer(RendererPreference) -> Self`
  - `.name(impl Into<String>) -> Self`
  - `.control_socket(impl Into<PathBuf>) -> Self` (`control-ipc`)
  - `.run<A: Application>(self) -> Result<()>`
  - `.run_on_connection<A: Application>(self, conn: wayland_client::Connection) -> Result<()>`
  - `App::start<A: Application>() -> Result<()>`
- `RendererPreference` (`render::backend`): `Auto`, `Software`, `Gpu`.
- `HeadlessRuntime<A: Application>` (`app::headless`):
  - Constructors: `new(capabilities, outputs)`, `new_with_renderer(...)`.
  - Execution & events: `enqueue(msg)`, `step()`, `step_at(now)`, `click_surface_point(...)`, `pointer_down_surface_point(...)`, `pointer_move_surface_point(...)`, `pointer_leave_surface(...)`, `pointer_up_surface_point(...)`, `simulate_surface_configure(...)`.
  - Inspection & accessibility: `capabilities()`, `state()`, `quit_requested()`, `surface_count()`, `surface_logical_size(...)`, `arena_heap_fallbacks()`, `rendered_buffer(...)`, `accessibility_tree()`, `dispatch_accessibility_action(...)`.

## Effects (`effect`)

`EffectCtx<'a>` (`effect::context`) is passed into `Application::init` and `Application::update`:

- **Lifecycle & runtime**: `quit()`, `capabilities() -> &Capabilities`, `outputs() -> &[OutputInfo]`, `after<M>(delay, make)` (`M: Send + 'static`), `system() -> &mut SystemProviders`.
- **Image & sprite assets**: `load_image(path)`, `load_image_sized(path, min_svg_px)`, `load_image_bytes(bytes)`, `load_image_bytes_sized(bytes, min_svg_px)`, `load_rgba_image(width, height, stride, pixels)`, `load_image_async(source, map)`, `reload_asset(asset, path)`, `unload_asset(asset) -> bool`.
- **Audio (`audio` / `audio-playback`)**: `audio() -> Result<&AudioClient>`, `player() -> Result<&AudioPlayer>`, `load_sound(path)`, `load_sound_bytes(bytes)`, `load_sound_preset(preset)`, `load_system_sound(name)`, `play_sound(sound)`, `play_sound_volume(sound, volume)`, `play_sound_preset(preset, volume)`, `play_system_sound(name, volume)`, `stop_sounds()`.
- **Compositors**: `hyprland()` (`hyprland-ipc`), `sway()` (`sway-ipc`), `compositor() -> Result<CompositorClient<'_>>`.
- **D-Bus & hardware**: `dbus()` (`dbus`), `set_backlight_percent(device_name, percent)`, `set_ddc_brightness(i2c_bus, percent)`, `privacy_snapshot() -> PrivacySnapshot`.
- **Clipboard (`clipboard`)**: `set_clipboard_text(text)`, `set_clipboard_primary_text(text)`, `set_clipboard(selection, entries)`.

## Errors & Capabilities (`error`, `capabilities`)

- `Result<T>`: `std::result::Result<T, WaywiError>`.
- `ErrorKind`: `NotAvailable`, `NotSupported`, `TemporarilyUnavailable`, `PermissionDenied`, `OperationFailed`, `InvalidState`, `Timeout`, `Shutdown`.
- `WaywiError` (`.kind(&self) -> ErrorKind`): `WaylandConnection`, `UnsupportedCapability(Capability)`, `OutputUnavailable`, `SurfaceError`, `RenderError`, `AssetDecode`, `UnsupportedAssetFormat`, `ProviderUnavailable`, `PermissionDenied`, `IpcError`, `DbusError`, `AudioError`, `InvalidGeometry`, `InvalidState`, `Timeout`, `Io(std::io::Error)`, `Shutdown`, `Custom`.
- `Capability`: `Wayland`, `LayerShell`, `XdgShell`, `FractionalScale`, `Viewporter`, `PointerInput`, `KeyboardInput`, `TouchInput`, `SoftwareRenderer`, `GpuRenderer`, `Text`, `ImagePng`, `ImageSvg`, `ImageGif`, `ImageWebp`, `Animation`, `ControlIpc`, `HyprlandIpc`, `SwayIpc`, `PipeWire`, `PulseAudio`, `Alsa`, `Dbus`, `GlobalShortcuts`, `Accessibility`, `AudioPlayback`, `Clipboard`, `IdleNotify`, `KeyboardLayout`.
- `Capabilities`: `new()`, `from_compile_features()`, `has(cap)`, `set(cap, enabled)`, `require(cap) -> Result<()>`, `iter()`.

## Geometry & Units (`geometry`)

- **Scalar units (`geometry::units`)**:
  - `LogicalPx(pub f32)`: `ZERO`, `new`, `get`, `round_i32`, `ceil_u32`, `floor_i32`, `min`, `max`, `clamp`, `to_buffer(scale)`, `to_buffer_ceil(scale)`.
  - `BufferPx(pub i32)`: `ZERO`, `new`, `get`, `as_u32`, `to_logical(scale)`.
  - `Percent(pub f32)`: `new`, `get`, `fraction`, `of(extent)`.
  - `ScaleFactor`: `ONE`, `from_integer`, `from_120ths`, `from_f32`, `as_120ths`, `as_f32`, `integer_ceil`, `is_integer`.
  - Extension traits: `LogicalPxExt::logical_px(self)`, `PercentExt::percent(self)`.
- **Shapes & lengths (`geometry::primitives`, `geometry::rect`)**:
  - `Point`: `ZERO`, `new(x, y)`.
  - `LogicalSize`: `ZERO`, `new(width, height)`, `to_buffer_ceil(scale)`.
  - `Length`: `Px(LogicalPx)`, `Percent(Percent)`, `Fill`, `Auto`.
  - `Size`: `new(width, height)`, `fill()`, `auto()`, `px(width, height)`.
  - `Edges`: `ZERO`, `new(top, right, bottom, left)`, `all`, `symmetric`, `top`, `right`, `bottom`, `left`, `top_left`, `top_right`, `bottom_left`, `bottom_right`.
  - `Rect` (aliases `Bounds`, `GeoRect`): `ZERO`, `new`, `from_xywh`, `right`, `bottom`, `center`, `is_empty`, `contains`, `intersect`, `union`, `outset`, `to_buffer_rect`.
  - `BufferRect`: `is_empty`, `right`, `bottom`, `intersect`, `union`.
- **Placement & transforms (`geometry::placement`, `geometry::transform`)**:
  - `Position`: `Auto`, `Absolute { x, y }`, `RelativeToSurface { x, y, offset_x, offset_y }`, `RelativeToParent { x, y, offset_x, offset_y }`; constructors `absolute`, `relative_to_surface`, `relative_to_parent`, `.with_offset(dx, dy)`.
  - `ElementAnchor`: `TopLeft`, `TopCenter`, `TopRight`, `CenterLeft`, `Center`, `CenterRight`, `BottomLeft`, `BottomCenter`, `BottomRight`; method `top_left_offset(width, height)`.
  - `Transform`: `identity`, `is_identity`, `is_pure_translation`, `translate`, `scale`, `scale_xy`, `rotate_deg`, `rotate_rad`, `to_affine_around(pivot)`.
  - `Affine2D`: `IDENTITY`, `is_identity`, `is_axis_aligned_unscaled`, `then`, `transform_point`, `inverse`, `transform_rect`.

## Surfaces & Displays (`surface`, `display`)

- `SurfaceKey(pub u64)` (`from_hashable`) and `IntoSurfaceKey::into_surface_key`.
- `Layer`: `Background`, `Bottom`, `Top`, `Overlay`.
- `Anchor` bitflags: `TOP` (`Top`), `BOTTOM` (`Bottom`), `LEFT` (`Left`), `RIGHT` (`Right`).
- `KeyboardInteractivity`: `None`, `OnDemand`, `Exclusive`.
- `InputMode`: `Full`, `Selective`, `PassThrough`.
- `SurfaceKind`: `Panel`, `Floating`, `Overlay`, `Window`.
- `Surface<M>` (`surface::builder`):
  - Constructors: `Surface::panel(key, output)`, `Surface::floating(key, output)`, `Surface::overlay(key, output)`, `Surface::window(key)`, `Surface::memo(key, dep, build)`.
  - Builders: `.layer`, `.anchor`, `.margin`, `.size`, `.exclusive_zone`, `.ignore_exclusive_zone`, `.keyboard_interactivity`, `.input_mode`, `.visible`, `.background`, `.title`, `.namespace`, `.accessible_role`, `.accessible_label`, `.accessible_description`, `.accessible_hidden`, `.child`, `.children`, `.on_close`, `.on_key`, `.on_pointer_move`, `.on_pointer_up`, `.map`.
- `Surfaces<M>` (`surface::collection`): `new()`, `push(surface)`, `extend(surfaces)`, `len()`, `is_empty()`, `map(f)`.
- `OutputId(pub u64)` (`new`, `get`), `Output` (`primary()`, `named(name)`, `id(OutputId)`, `all()`), `OutputInfo`, `Transform`, `resolve_outputs`, `select_primary_output`.

## Elements, Styling, Canvas, Text, Images & Sprites

- **`Element<M>` (`render::elements::element`)**: `Element::memo(key, dep, build)`, `.map(f)`. All elements convert into `Element<M>` and share common setters from `CommonProps`:
  - Identity & layout: `.key`, `.element_key`, `.position`, `.size`, `.anchor`, `.visible`, `.opacity`, `.transform`, `.z_index`, `.input_enabled`.
  - Interaction: `.on_click`, `.on_hover`, `.on_leave`, `.on_scroll`, `.on_pointer_down`, `.on_pointer_up`, `.on_pointer_move`, `.on_key`.
  - Audio feedback: `.on_click_sound`, `.on_click_sound_volume`, `.on_hover_sound`, `.on_hover_sound_volume`.
  - Accessibility: `.accessible_role`, `.accessible_label`, `.accessible_description`, `.accessible_value`, `.accessible_hidden`.
- **Containers (`render::elements::containers`)**:
  - `Group<M>`: `new`, `child`, `children`, `map`.
  - `Row<M>` / `Column<M>`: `new`, `gap`, `align`, `padding`, `child`, `children`, `map`.
  - `Clip<M>`: `new(child)`, `corner_radius`, `map`.
- **`Rect<M>` (`render::elements::rect`)**: `new`, `fill`, `corner_radius`, `border`, `map`.
- **Style (`render::style`)**:
  - `Color`: `TRANSPARENT`, `BLACK`, `WHITE`, `GRAY`, `RED`, `GREEN`, `BLUE`, `rgba`, `rgb`, `rgba_f32`, `from_hex_rgba`, `from_hex_rgb`, `with_opacity`, `to_premultiplied_rgba`, `to_premultiplied_bgra`.
  - `CornerRadius`: `ZERO`, `all`, `new`, `is_zero`.
  - `Border`, `Align` (`Start`, `Center`, `End`).
- **Canvas (`render::canvas`)**:
  - `PathOp`, `PathBuilder` (`new`, `move_to`, `line_to`, `quad_to`, `cubic_to`, `close`).
  - `Painter<'a>`: `size`, `clear`, `fill_rect`, `fill_rounded_rect`, `stroke_rect`, `fill_circle`, `stroke_line`, `fill_path`, `stroke_path`.
  - `Canvas<M>`: `new(size, render_key, draw)`, `render_key`, `map`.
- **Text (`text`)**:
  - `FontWeight`: `Thin`, `Light`, `Normal`, `Medium`, `SemiBold`, `Bold`, `ExtraBold` (`to_numeric`).
  - `FontStyle`: `Normal`, `Italic`.
  - `Font`: `sans`, `monospace`, `family`, `weight`, `style`.
  - `TextAlign`: `Left`, `Center`, `Right`.
  - `Text<M>`: `new`, `font_size`, `line_height`, `color`, `font`, `align`, `map`.
  - `SharedStr`: `EMPTY`, `from_static`, `from_str_inline_or_arc`, `format_inline`, `as_str`, `is_inline`, `is_static`.
- **Images & Assets (`image`, `asset::types`)**:
  - `AssetId(pub u64)` (`new`, `get`), `ImageAsset`, `DecodedImage`, `DecodedGifFrame`, `ImageFormat` (`Png`, `Svg`, `Gif`, `Webp`).
  - `ImageFit`: `Fill`, `Contain`, `Cover`, `Original`.
  - `Image<M>`: `new(asset)`, `fit`, `map`.
- **Sprites (`sprite`)**:
  - `PlaybackMode`: `Once`, `Loop`, `PingPong`.
  - `Fps(pub f32)` (`new`, `get`, `frame_duration`) and `FpsExt::fps(self)`.
  - `Frames` (`range`, `count`, `frame_at`), `AnimationDef`.
  - `SpriteSheet`: `new(asset)`, `frame_size`, `animation`, `animation_with_mode`, `get_animation`, `columns`, `rows`, `total_frames`, `frame_source_rect`.
  - `Sprite<M>`: `new(sheet)`, `frame`, `playing`, `playback`, `map`.

## Input, Accessibility, Clipboard & Idle (`input`, `accessibility`, `clipboard`, `idle`)

- `ElementKey(pub u64)` (`from_hashable`), `IntoElementKey::into_element_key`, `NodeId { index, generation }`.
- `MouseButton`: `Left`, `Right`, `Middle`, `Side`, `Extra`, `Other(u32)`, `from_linux_button`.
- `PointerEvent`, `ScrollEvent`, `TouchEvent`.
- `Modifiers` bitflags: `CTRL` (`CONTROL`), `ALT`, `SHIFT`, `SUPER` (`LOGO`).
- `Key` (`from_keysym`), `KeyChord::new(modifiers, key)`, `KeyEvent::matches(chord)`.
- `KeyboardLayout::subscribe(map)` and `KeyboardLayoutInfo` (`from_layouts`, `short_label`, `short_label_from_name`).
- `AccessibleRole` (`accessibility::props`): `Generic`, `Window`, `Panel`, `Group`, `Button`, `Label`, `Image`, `Canvas`, `Row`, `Column`, `List`, `ListItem`, `Switch`, `CheckBox`, `Slider`, `ProgressIndicator`, `Status`. Additional AccessKit types re-exported when `accessibility` is enabled.
- `Clipboard` (`clipboard`): `Clipboard::subscribe(map)`, `Clipboard::subscribe_selection(selection, map)`, `select_preferred_mime(mimes)`; `ClipboardSelection` (`Clipboard`, `Primary`), `ClipboardSnapshot`, `ClipboardEvent` (`Changed`, `Error`).
- `Idle` (`idle-notify`): `Idle::notify(timeout, map)`, `Idle::input_notify(timeout, map)`, `IdleEvent` (`Idled`, `Resumed`).

## Subscriptions & Timers (`subscription`, `timer`)

- `SubscriptionKey(pub u64)` (`from_hashable`), `IntoSubscriptionKey::into_subscription_key`.
- `Subscription<M>`: `Subscription::none()`, `Subscription::batch(subs)`, `.map(f)`.
- `Subscriptions<M>`: `new()`, `none()`, `push(sub)`, `extend(subs)`, `len()`, `is_empty()`, `.map(f)`.
- `Timer` (`timer`): `Timer::once(key, duration, msg)`, `Timer::every(key, interval, msg)`.

## System Telemetry & Units (`system`)

- **Builders (`system::builders`)**: `SnapshotState<T>` (`Pending`, `Ready(T)`, `is_ready`, `as_ref`, `into_option`), `System` factory: `cpu()`, `memory()`, `time()`, `gpu()`, `disk()`, `network()`, `battery()`, `sensors()`, `uptime()`, `brightness()`, `privacy()` -> `.every(interval, map)` or `.every_keyed(key, interval, map)`.
- **Providers (`system::providers::SystemProviders`)**: `new`, `cpu_snapshot`, `poll_cpu_now`, `memory_snapshot`, `time_snapshot`, `gpu_snapshot`, `poll_disk_now`, `poll_network_now`, `os_info`, `hostname`, `batteries`, `sensors`, `uptime`, `load_average`, `brightness_snapshot`, `set_backlight_percent`, `set_ddc_brightness`, `privacy_snapshot`, `cpu_subscriber_count`, `cpu_sample_count`.
- **CPU (`system::cpu`)**: `CpuCounters` (`idle_total`, `total`, `utilization_since`), `CpuSnapshot` (`avg_freq_khz`, `avg_freq`, `min_freq`, `max_freq`), `CpuSampler` (`new`, `reset`, `sample_reads`, `sample_proc_stat`, `feed_proc_stat`, `parse_proc_stat`).
- **Memory (`system::memory`)**: `MemoryPsi`, `MemorySnapshot`, `read_proc_meminfo`, `parse_meminfo`, `parse_memory_psi`.
- **Time & Calendar (`system::time`)**: `TimeSnapshot` (`local()`, `now_snapshot()`), `TimeFormat` (`H24`, `H12`), `LocalDateTime` (`now`, `from_system_time`, `is_pm`, `am_pm`, `hour_12`, `format_hm`, `format_hms`, `weekday_name`, `month_name`, `format_long_date`, `format_iso_date`), `CalendarMonth::build`, `CalendarRow`, `CalendarDayCell`, `month_name`, `is_leap_year`, `days_in_month`, `shift_year_month`, `weekday_mon0`, `day_of_year`, `iso_week_number`.
- **GPU (`system::gpu`)**: `GpuVendor` (`Amd`, `Intel`, `Nvidia`, `Other(u16)`, `from_pci_id`), `GpuDeviceSnapshot`, `GpuSnapshot` (`primary()`), `read_gpus`, `read_gpus_from`, `amd::parse_pp_dpm_active_mhz`, `intel::parse_drm_fdinfo_engines`, `nvidia::parse_nvidia_gpu_info`.
- **Disk (`system::disk`)**: `DiskMount` (`usage_percent()`), `DiskDeviceIo`, `DiskSnapshot`, `ProcMountEntry`, `DiskIoCounters`, `DiskSampler` (`new`, `reset`, `sample`, `feed_snapshot`), `io::parse_proc_diskstats`, `mounts::{parse_proc_mounts, read_disk_mounts}`.
- **Network (`system::network`)**: `NetInterfaceKind` (`Ethernet`, `Wifi`, `Modem`, `Loopback`, `Virtual`, `Other`), `NetLinkState` (`Up`, `Down`, `Unknown`), `NetInterfaceSnapshot` (`rx_rate`, `tx_rate`, `wireless_freq_ghz`), `NetworkSnapshot` (`total_rx_rate`, `total_tx_rate`, `active_interface`), `NetDevCounters`, `NetworkSampler` (`new`, `reset`, `sample`, `feed_proc_net_dev`), `proc_net::{parse_proc_net_dev, classify_interface}`.
- **Hardware (`system::hardware`)**:
  - `power`: `PowerStatus` (`Charging`, `Discharging`, `Full`, `NotCharging`, `Unknown`), `BatteryInfo`, `read_batteries`, `read_batteries_from`.
  - `sensors`: `ThermalSensor`, `read_sensors`, `read_sensors_from`.
  - `brightness`: `VCP_LUMINANCE`, `BacklightDevice` (`percent()`), `DdcMonitorInfo` (`percent()`), `BrightnessSnapshot`, `read_backlights`, `read_backlights_from`, `set_backlight_percent`, `set_backlight_raw`, `encode_ddc_get_vcp`, `encode_ddc_set_vcp`, `decode_ddc_vcp_reply`.
- **OS (`system::os`)**: `OsInfo`, `LoadAverage`, `UptimeSnapshot`, `release::{read_os_info, parse_os_release, read_hostname}`, `uptime::{read_uptime, read_load_average, parse_uptime, parse_loadavg}`.
- **Privacy (`system::privacy`)**: `PrivacyAccessKind`, `PrivacyAccessSource`, `PrivacyAccessEntry`, `PrivacySnapshot` (`any_active()`), `PrivacyMonitor` (`new`, `ensure_started`, `stop_background`, `sample`), `classify_pipewire_node`, `scan_proc_fd`, `scan_proc_fd_from`.
- **Units (`system::units`)**:
  - `ByteSize`: `ZERO`, `from_bytes`, `from_kib`, `from_mib`, `as_bytes`, `as_kib`, `as_mib`, `as_gib`, `saturating_sub`.
  - `FreqUnit` (`Khz`, `Mhz`, `Ghz`): `convert_khz`, `format_khz`.
  - `RateUnit` (`AutoBytes`, `AutoBits`, `BytesPerSec`, `KilobytesPerSec`, `MegabytesPerSec`, `GigabytesPerSec`, `BitsPerSec`, `KilobitsPerSec`, `MegabitsPerSec`, `GigabitsPerSec`).
  - `DataRate`: `ZERO`, `from_bytes_per_sec`, `as_bytes_per_sec`, `as_bits_per_sec`, `value_in(RateUnit)`, `format(RateUnit)`.

## Desktop Utilities (`desktop`)

- `DesktopEntry`: `exec_argv()`, `should_show_in_launcher()`, `launch()`, `launch_detached()`.
- Entry discovery: `default_application_dirs()`, `scan_desktop_entries()`, `scan_desktop_entries_in(dirs)`, `find_desktop_entry(id_or_wm_class)`, `parse_desktop_entry(contents, id, path)`.
- Icons (`desktop::icons`): `IconDirSpec`, `IconThemeInfo`, `XdgIconLookup` (`new`, `with_dirs`, `resolve`), `resolve_icon_path`, `default_icon_dirs`, `detect_active_icon_theme`, `parse_icon_theme_index`.
- Process execution: `spawn_detached(program, args)`, `spawn_shell_detached(command)`.

## Audio (`audio`)

- **Audio mixer (`pipewire`, `pulseaudio`, or `alsa`)**:
  - `AudioDevice`, `AudioSnapshot` (`percent()`, `source_percent()`), `AudioEvent`.
  - `Audio::subscribe(map)`, `Audio::subscribe_events(map)`.
  - `AudioClient` (`new`, `snapshot`, `set_volume_linear`, `set_muted`, `toggle_mute`, `select_device`, `set_source_volume_linear`, `set_source_muted`, `toggle_source_mute`, `select_source`).
- **Audio playback (`audio-playback` or native backend)**:
  - `SoundAsset`, `SoundPreset` (`Click`, `DragTick`, `Hover`, `Notification`, `Warning`).
  - `SoundData`: `from_pcm_f32`, `duration`, `from_file`, `from_bytes`, `from_bytes_with_hint`, `from_wav_bytes`, `tone`, `from_preset`, `to_stereo_48k`.
  - `AudioPlayer`: `new`, `load_file`, `load_bytes`, `load_system_sound`, `load_preset`, `play`, `play_with_volume`, `play_data`, `play_preset`, `play_system_sound`, `stop_all`.
  - Theme lookup: `resolve_sound_theme_path`, `default_sound_dirs`, `resolve_sound_theme_path_in`.

## Compositor & Control IPC (`compositor`, `control`)

- **Unified adapter (`compositor::adapter`)**: `CompositorWorkspace`, `WorkspaceTarget` (`Id(i32)`, `Name(String)`), `CompositorClient<'a>` (`workspaces`, `switch_workspace`, `next_keyboard_layout`, `switch_keyboard_layout`).
- **Hyprland (`hyprland-ipc`)**:
  - Types: `WorkspaceId(i32)` (`new`, `get`), `WorkspaceInfo`, `ClientInfo`, `WorkspaceRefJson`, `MonitorInfo`, `HyprlandEvent`, `HyprlandSockets` (`discover`, `from_base`).
  - `Hyprland::subscribe(map)`.
  - `HyprlandClient`: `from_env`, `with_sockets`, `with_timeout`, `sockets`, `workspaces`, `active_workspace`, `active_client`, `monitors`, `switch_workspace`, `dpms_off`, `dpms_on`, `next_keyboard_layout`, `switch_keyboard_layout`, `switch_keyboard_layout_cmd`, `dispatch`, `raw_command`, `apply_workspace_event`, `stream::parse_event_line`.
- **Sway (`sway-ipc`)**:
  - Types: `WorkspaceRef<'a>`, `SwayWorkspace`, `SwayOutput`, `SwayVersion`, `SwayEvent`.
  - `Sway::subscribe(map)`.
  - `SwayClient`: `from_env`, `with_socket`, `with_timeout`, `socket_path`, `workspaces`, `outputs`, `tree`, `version`, `switch_workspace`, `next_keyboard_layout`, `switch_keyboard_layout`, `raw_command`, `encode_frame`, `stream::parse_event_frame`.
- **Control IPC (`control-ipc`)**:
  - `Signal` (`Usr1`, `Usr2`), `Control::signal(signal, msg)`, `Control::socket(socket_name_or_path, map)`.
  - `control::socket::{resolve_socket_path, send_command}`.

## D-Bus Subsystems (`dbus`)

Available when the `dbus` Cargo feature is enabled (`global_shortcuts` also exposes `GlobalShortcuts::subscribe` when `global-shortcuts` is enabled):

- **Core (`types`, `client`)**: `DbusBus` (`Session`, `System`), `DbusValue` (`String`, `Bool`, `I64`, `U64`, `F64`, `Array`, `Dict`; `.as_str`, `.as_bool`, `.as_i64`, `.as_u64`, `.as_f64`), `DbusSignalEvent`, `DbusActionLane`, `DbusActionResult` (`is_ok`, `is_err`, `to_result`), `Dbus::{subscribe_signal, action_results}`, `DbusClient` (`new`, `get_property`, `call_method_unit`, `drain_action_results`, `last_action_result`, `last_action_error`, `take_last_action_error`).
- **Notifications (`notifications`)**: `NotificationUrgency`, `NotificationCloseReason`, `NotificationImageData`, `DesktopNotification`, `NotificationEvent`, `Dbus::notifications`, `DbusClient::{close_notification, invoke_notification_action}`.
- **System Tray (`tray`)**: `TrayCategory` (`from_sni_str`), `TrayStatus` (`from_sni_str`), `TrayIconPixmap` (`to_rgba`, `best_for_size`), `TrayToolTip`, `TrayMenuItem`, `TrayItemSnapshot`, `TrayEvent`, `Dbus::tray`, `DbusClient::{tray_list_items, tray_snapshot, tray_activate, tray_secondary_activate, tray_context_menu, tray_scroll, tray_menu_layout, tray_menu_event}`.
- **MPRIS (`mpris`)**: `MprisPlaybackStatus` (`from_str_status`), `MprisLoopStatus` (`from_str_status`, `as_str`), `MprisSnapshot`, `MprisEvent` (`PlayersUpdated`, `PlayerChanged`, `PlayerAdded`, `PlayerRemoved`, `Seeked`), `Dbus::mpris`, `DbusClient::{list_mpris_players, mpris_snapshots, mpris_snapshot, mpris_play_pause, mpris_play, mpris_pause, mpris_stop, mpris_next, mpris_previous, mpris_seek, mpris_set_position, mpris_set_volume, mpris_set_shuffle, mpris_set_loop_status, mpris_raise, mpris_quit}`.
- **NetworkManager (`network_manager`)**: `NmState`, `NmConnectivity`, `NmDeviceType`, `NmWifiSecurity` (`Open`, `Wep`, `WpaPsk`, `WpaEap`, `Wpa3Sae`, `Owe`), `NmAccessPoint`, `NmSavedConnection`, `NmDeviceInfo`, `NmActiveConnection`, `NetworkManagerSnapshot`, `Dbus::network_manager`, `DbusClient::{network_manager_snapshot, nm_set_wireless_enabled, nm_set_networking_enabled, nm_scan_wifi, nm_list_wifi_access_points, nm_list_saved_connections, nm_connect_wifi, nm_connect_wifi_with_options, nm_connect_hidden_wifi, nm_connect_hidden_wifi_with_security, nm_activate_connection, nm_deactivate_connection, nm_disconnect_device, nm_forget_connection, nm_forget_wifi_ssid}`.
- **Power Profiles (`power_profiles`)**: `PowerProfileKind` (`PowerSaver`, `Balanced`, `Performance`, `Custom(String)`; `from_dbus_str`, `as_str`), `PowerProfileHold`, `PowerProfileSnapshot`, `Dbus::power_profiles`, `DbusClient::{power_profile_snapshot, set_power_profile, hold_power_profile, release_power_profile}`.
- **UPower (`upower`)**: `UPowerDeviceKind`, `UPowerDeviceState`, `UPowerDevice`, `UPowerSnapshot` (`display_device`, `peripherals`), `Dbus::upower`, `DbusClient::upower_snapshot`.
- **Logind (`logind`)**: `LogindInhibitor`, `LogindSnapshot`, `LogindEvent`, `Dbus::logind`, `DbusClient::{logind_snapshot, set_backlight_brightness, lock_session, unlock_session, suspend, hibernate, reboot, power_off, inhibit}`.
- **Bluetooth (`bluetooth`)**: `BluetoothAdapter`, `BluetoothDevice`, `BluetoothSnapshot`, `BluetoothTransportFilter` (`Auto`, `BrEdr`, `Le`; `as_str`), `BluetoothAgentRequest` (`RequestPinCode`, `DisplayPinCode`, `RequestPasskey`, `DisplayPasskey`, `RequestConfirmation`, `RequestAuthorization`, `AuthorizeService`), `BluetoothAgentEvent` (`Registered`, `Request`, `Cancelled`, `Released`, `Error`), `Dbus::{bluetooth, bluetooth_agent}`, `DbusClient::{bluetooth_snapshot, set_bluetooth_powered, set_bluetooth_discoverable, set_bluetooth_discoverable_timeout, set_bluetooth_pairable, set_bluetooth_pairable_timeout, set_bluetooth_adapter_alias, set_bluetooth_discovery_filter, start_bluetooth_discovery, stop_bluetooth_discovery, connect_bluetooth_device, connect_bluetooth_device_profile, disconnect_bluetooth_device, disconnect_bluetooth_device_profile, pair_bluetooth_device, cancel_bluetooth_pairing, trust_bluetooth_device, block_bluetooth_device, set_bluetooth_device_alias, remove_bluetooth_device, bluetooth_agent_confirm, bluetooth_agent_reply_pincode, bluetooth_agent_reply_passkey, bluetooth_agent_reject}`.
- **ModemManager (`modem`)**: `ModemState`, `ModemAccessTech`, `ModemRegistrationState`, `ModemInfo`, `ModemSnapshot`, `Dbus::modem`, `DbusClient::{modem_snapshot, enable_modem}`, `decode_access_tech_bitmask`.
- **ScreenSaver & GeoClue2 (`idle_privacy`)**: `ScreenSaverSnapshot`, `GeoClueSnapshot`, `Dbus::{screensaver, geoclue}`, `DbusClient::{screensaver_active, lock_screensaver, inhibit_screensaver, uninhibit_screensaver, geoclue_in_use}`.
- **Global Shortcuts (`global_shortcuts`, feature `global-shortcuts`)**: `GlobalShortcutDescriptor` (`new`, `preferred_trigger`), `BoundGlobalShortcut`, `GlobalShortcutEvent`, `GlobalShortcuts::subscribe`, `Dbus::global_shortcuts`, `DbusClient::has_global_shortcuts_portal`.
