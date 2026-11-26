# Library Reference

Public API reference for `waywi`. Core types, builders, and helpers are re-exported by `waywi::prelude::*`. Feature-gated items list their Cargo feature flag.

## Prelude & Top-Level Modules

- `waywi::prelude::*`: re-exports core app, surface, geometry, primitive, text, input, timer, system, desktop, and enabled integration symbols.
- Top-level public modules: `accessibility`, `animation`, `app`, `audio`, `capabilities`, `clipboard` (`clipboard`), `compositor` (`hyprland-ipc` / `sway-ipc`), `control` (`control-ipc`), `dbus` (`dbus`), `desktop`, `display`, `effect`, `error`, `geometry`, `idle` (`idle-notify`), `image`, `input`, `render`, `sprite`, `subscription`, `surface`, `system`, `text`, `timer`, `wayland_ext`.
- Top-level re-exports: `App`, `Application`, `GpuMemoryPolicy`, `HeadlessRuntime`, `MemoryConfig`, `Capabilities`, `Capability`, `Output`, `OutputId`, `OutputInfo`, `OutputTransform`, `EffectCtx`, `ErrorKind`, `Result`, `WaywiError`, `Surface`, `Surfaces`.

## Application Runtime (`app`)

- `Application` trait (`app::runner`):
  - `type State;`
  - `type Message: Clone + 'static;`
  - `fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State>;`
  - `fn update(state: &mut Self::State, msg: Self::Message, ctx: &mut EffectCtx<'_, Self::Message>) -> Result<()>;`
  - `fn view(state: &Self::State) -> Surfaces<Self::Message>;`
  - `fn subscriptions(state: &Self::State) -> Subscriptions<Self::Message>;`
- `App` (`app::runner`):
  - `App::new() -> Self`
  - `.renderer(RendererPreference) -> Self`
  - `.memory(MemoryConfig) -> Self`
  - `.name(impl Into<String>) -> Self`
  - `.control_socket(impl Into<PathBuf>) -> Self`
  - `.run<A: Application>(self) -> Result<()>`
  - `.run_on_connection<A: Application>(self, conn: wayland_client::Connection) -> Result<()>`
- `MemoryConfig` / `GpuMemoryPolicy` (`app::memory`):
  - Presets: `balanced()` (default), `low_memory()`, `high_throughput()`.
  - `GpuMemoryPolicy`: `MemoryUsage` (default), `Minimal`, `Performance`.
  - Builders: `default_buffer_retention`, `default_asset_retention`, `arena_limits`, `text_cache`, `text_max_mapped_font_files`, `load_system_fonts`, `software_scratch_bytes`, `canvas_cache_bytes`, `gpu_cache`, `gpu_memory_policy`, `sound_cache_bytes`, `icon_lookup_max_entries`, `dbus_cache_snapshots`, `dbus_idle_disconnect`.
- `RendererPreference` (`render::backend`): `Auto`, `Software`, `Gpu`.
- `HeadlessRuntime<A: Application>` (`app::headless`):
  - Constructors: `new(capabilities, outputs)`, `new_with_renderer(...)`, `with_memory(capabilities, outputs, renderer_pref, memory)`.
  - Execution & events: `enqueue(msg)`, `step()`, `step_at(now)`, `tick_animations_at(now)`, `trim_memory()`, `click_surface_point(...)`, `pointer_down_surface_point(...)`, `pointer_move_surface_point(...)`, `pointer_leave_surface(...)`, `pointer_up_surface_point(...)`, `scroll_surface_point(...)`, `scroll_surface_event(...)`, `touch_surface(...)`, `gesture_surface(...)`, `tablet_surface(...)`, `key_surface(...)`, `simulate_ime(...)`, `simulate_relative_pointer(...)`, `simulate_dnd(...)`, `simulate_popup_dismiss(...)`, `simulate_surface_configure(...)`.
  - Inspection & accessibility: `capabilities()`, `memory_config()`, `state()`, `quit_requested()`, `surface_count()`, `surface_logical_size(...)`, `surface_ime_config(...)`, `surface_cursor_at(...)`, `surface_decoration(...)`, `surface_inhibits_idle(...)`, `surface_pointer_constraint(...)`, `activated_surfaces()`, `set_synthetic_screencopy(...)`, `recorded_toplevel_actions()`, `recorded_ext_workspace_actions()`, `recorded_virtual_keyboard_actions()`, `recorded_drag_starts()`, `arena_heap_fallbacks()`, `asset_pixels_resident(asset)`, `decoded_asset_bytes()`, `rendered_buffer(...)`, `accessibility_tree()`, `dispatch_accessibility_action(...)`.

## Effects (`effect`)

`EffectCtx<'a, M>` (`effect::context`) is passed into `Application::init` and `Application::update`:

- **Lifecycle & memory**: `quit()`, `capabilities() -> &Capabilities`, `outputs() -> &[OutputInfo]`, `memory_config() -> &MemoryConfig`, `trim_memory()`, `after(delay, msg)`, `request_activation_token(app_id, map)`, `activate_surface(surface_key, token)`, `system() -> &mut SystemProviders`, `set_backlight_percent(device_name, percent)`.
- **Wayland desktop extensions (`wayland_ext`, `input`)**: `capture_screen(target, overlay_cursor, map)` (`screencopy`), `activate_toplevel`, `close_toplevel`, `set_toplevel_minimized`, `set_toplevel_maximized`, `set_toplevel_fullscreen` (`foreign-toplevel`), `activate_ext_workspace`, `deactivate_ext_workspace`, `create_ext_workspace`, `remove_ext_workspace` (`ext-workspace`), `start_drag(surface_key, entries, actions)`, `virtual_key`, `virtual_modifiers`, `virtual_keymap` (`virtual-keyboard`).
- **Image assets**: `load_image(path)`, `load_image_sized(path, min_svg_px)`, `load_image_bytes(bytes)`, `load_image_bytes_sized(bytes, min_svg_px)`, `load_image_static(bytes)`, `load_image_static_sized(bytes, min_svg_px)`, `load_rgba_image(width, height, rowstride, has_alpha, data)`, `load_image_async(path, map)`, `set_image_retention(asset, retention) -> bool`, `image_retention(asset) -> Option<AssetRetention>`, `evict_image_pixels(asset) -> bool`, `reload_image(asset, path)`, `unload_image(asset) -> bool`.
- **Audio (`audio` / `audio-playback`)**: `audio() -> Result<&AudioClient>`, `player() -> Result<&AudioPlayer>`.
- **Compositors**: `hyprland()` (`hyprland-ipc`), `sway()` (`sway-ipc`), `compositor() -> Result<CompositorClient<'_>>`.
- **D-Bus (`dbus`)**: `dbus() -> Result<&DbusClient>`.
- **Clipboard (`clipboard`)**: `set_clipboard_text(text)`, `set_clipboard_primary_text(text)`, `set_clipboard(selection, entries)`.
- **Text cursor layout (`text`)**: `measure_text_cursor(text, font, font_size, cursor_byte)`, `measure_text_cursor_with(text, options, cursor_byte)`.

## Errors & Capabilities (`error`, `capabilities`)

- `Result<T>`: `std::result::Result<T, WaywiError>`.
- `ErrorKind`: `NotAvailable`, `NotSupported`, `TemporarilyUnavailable`, `PermissionDenied`, `OperationFailed`, `InvalidState`, `Timeout`, `Shutdown`.
- `WaywiError` (`.kind(&self) -> ErrorKind`): `WaylandConnection`, `UnsupportedCapability(Capability)`, `OutputUnavailable`, `SurfaceError`, `RenderError`, `AssetDecode`, `UnsupportedAssetFormat`, `ProviderUnavailable`, `PermissionDenied`, `IpcError`, `DbusError`, `AudioError`, `InvalidGeometry`, `InvalidState`, `Timeout`, `Io(std::io::Error)`, `Shutdown`, `Custom`.
- `Capability`: `Wayland`, `LayerShell`, `XdgShell`, `XdgActivation`, `XdgDecoration`, `FractionalScale`, `Viewporter`, `PointerInput`, `KeyboardInput`, `TouchInput`, `PointerGestures`, `TabletInput`, `CursorShape`, `SessionLock`, `Screencopy`, `ForeignToplevel`, `ForeignToplevelControl`, `ExtWorkspace`, `DragAndDrop`, `IdleInhibit`, `VirtualKeyboard`, `PointerConstraints`, `RelativePointer`, `SoftwareRenderer`, `GpuRenderer`, `Text`, `ImagePng`, `ImageSvg`, `ImageGif`, `ImageWebp`, `Animation`, `ControlIpc`, `HyprlandIpc`, `SwayIpc`, `PipeWire`, `PulseAudio`, `Alsa`, `Dbus`, `GlobalShortcuts`, `Accessibility`, `AudioPlayback`, `Clipboard`, `IdleNotify`, `TextInput`, `KeyboardLayout`.
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
  - `Size`: `new(width, height)`, `fill()`, `auto()`, `From<(W, H)>`, `From<LogicalSize>`.
  - `Edges`: `ZERO`, `new(top, right, bottom, left)`, `all`, `symmetric`, `top`, `right`, `bottom`, `left`, `top_left`, `top_right`, `bottom_left`, `bottom_right`, `From<LogicalPx>`, `From<(LogicalPx, LogicalPx)>`.
  - `Bounds`: `ZERO`, `new`, `from_xywh`, `right`, `bottom`, `center`, `is_empty`, `contains`, `intersect`, `union`, `outset`, `to_buffer_rect`.
  - `BufferRect`: `is_empty`, `right`, `bottom`, `intersect`, `union`.
- **Placement & transforms (`geometry::placement`, `geometry::transform`)**:
  - `Position`: `Auto`, `Absolute { x, y }`, `RelativeToSurface { x, y, offset_x, offset_y }`, `RelativeToParent { x, y, offset_x, offset_y }`; constructors `absolute`, `relative_to_surface`, `relative_to_parent`, `.with_offset(dx, dy)`.
  - `ElementAnchor`: `TopLeft`, `TopCenter`, `TopRight`, `CenterLeft`, `Center`, `CenterRight`, `BottomLeft`, `BottomCenter`, `BottomRight`; method `top_left_offset(width, height)`.
  - `Transform`: `identity`, `is_identity`, `is_pure_translation`, `translate`, `scale`, `scale_xy`, `rotate_deg`, `rotate_rad`, `pivot`, `pivot_point`, `to_affine_around(pivot)`.
  - `Affine2D`: `IDENTITY`, `is_identity`, `is_axis_aligned_unscaled`, `then`, `transform_point`, `inverse`, `transform_rect`.

## Surfaces & Displays (`surface`, `display`)

- `SurfaceKey(pub u64)` (`from_hashable`) and `IntoSurfaceKey::into_surface_key`.
- `Layer`: `Background`, `Bottom`, `Top`, `Overlay`.
- `Anchor` bitflags: `TOP`, `BOTTOM`, `LEFT`, `RIGHT`.
- `KeyboardInteractivity`: `None`, `OnDemand`, `Exclusive`.
- `InputMode`: `Full`, `Selective`, `PassThrough`.
- `BufferRetention`: `Retain`, `ReleaseAfterCommit`.
- `SurfaceKind`: `Panel`, `Floating`, `Overlay`, `Window`, `Popup`, `SessionLock`.
- `WindowDecoration`: `Server` (default), `Auto`, `Client`, `None`.
- Popup positioning (`surface::types`): `PopupAnchor` (default `BottomLeft`), `PopupGravity` (default `BottomRight`), `PopupConstraintAdjustment` (`SLIDE_X`, `SLIDE_Y`, `FLIP_X`, `FLIP_Y`, `RESIZE_X`, `RESIZE_Y`), `PopupPositioner` (`new(anchor_rect)`, `anchor`, `gravity`, `offset`, `constraint_adjustment`, `reactive`).
- `Surface<M>` (`surface::builder`):
  - Constructors: `Surface::panel(key, output)`, `Surface::floating(key, output)`, `Surface::overlay(key, output)`, `Surface::window(key)`, `Surface::popup(key, parent_surface)`, `Surface::session_lock(key, output)`, `Surface::memo(key, dep, build)`.
  - Builders: `.layer`, `.anchor`, `.margin`, `.size`, `.exclusive_zone`, `.ignore_exclusive_zone`, `.keyboard_interactivity`, `.input_mode`, `.buffer_retention`, `.output`, `.positioner`, `.popup_grab`, `.visible`, `.background`, `.title`, `.namespace`, `.cursor`, `.decoration`, `.inhibit_idle`, `.pointer_constraint`, `.dnd_mime_types`, `.accessible_role`, `.accessible_label`, `.accessible_description`, `.accessible_hidden`, `.ime`, `.on_ime`, `.child`, `.children`, `.on_close`, `.on_key`, `.on_pointer_move`, `.on_pointer_up`, `.on_relative_pointer`, `.on_dnd`, `.on_touch`, `.on_gesture`, `.on_tablet`, `.map`.
- `Surfaces<M>` (`surface::collection`): `new()`, `none()`, `push(surface)`, `extend(surfaces)`, `len()`, `is_empty()`, `map(f)`.
- `OutputId(pub u64)` (`new`, `get`), `Output` (`primary()`, `named(name)`, `id(OutputId)`, `all()`), `OutputInfo`, `OutputTransform`, `resolve_outputs`, `select_primary_output`.

## Elements, Styling, Canvas, Text, Images & Sprites

- **`Element<M>` (`render::elements::element`)**: `Element::memo(key, dep, build)`, `.map(f)`. Common setters from `CommonProps`:
  - Identity & layout: `.key`, `.position`, `.size`, `.anchor`, `.visible`, `.opacity`, `.transform`, `.transition_opacity`, `.transition_transform`, `.z_index`, `.input_enabled`, `.cursor`.
  - Interaction: `.on_click`, `.on_hover`, `.on_leave`, `.on_scroll`, `.on_pointer_down`, `.on_pointer_up`, `.on_pointer_move`, `.on_touch`, `.on_gesture`, `.on_tablet`.
  - Audio feedback: `.on_click_sound(impl Into<SoundPlay>)`, `.on_hover_sound(impl Into<SoundPlay>)`.
  - Accessibility: `.accessible_role`, `.accessible_label`, `.accessible_description`, `.accessible_value`, `.accessible_hidden`.
- **Containers & shapes (`render::elements`)**: `Group<M>`, `Row<M>`, `Column<M>`, `Clip<M>`, `Rect<M>`.
- **Style (`render::style`)**: `Color`, `CornerRadius`, `Border`, `Align` (`Start`, `Center`, `End`).
- **Canvas (`render::canvas`)**: `PathOp`, `PathBuilder`, `Painter<'a>`, `Canvas<M>` (`new(size, draw)`, `.cache_key(key)`, `.map(f)`).
- **Text (`text`)**: `FontWeight`, `FontStyle`, `Font`, `TextAlign`, `TextBoundsMode` (`Typographic`, `Optical`), `Text<M>` (`new`, `font_size`, `line_height`, `color`, `font`, `align`, `bounds_mode`, `map`), `TextCursorMetrics`, `TextLayoutOptions`, `SharedStr`.
- **Images & Assets (`image`)**: `AssetId`, `AssetRetention` (`KeepDecoded`, `ReclaimAfterPresent`), `ImageAsset`, `SourceRect` (`new`, `as_tuple`, `From<(u32, u32, u32, u32)>`), `ImageFit`, `ImageFilter`, `Image<M>` (`new`, `fit`, `filter`, `region`, `flip_x`, `flip_y`, `map`).
- **Sprites (`sprite`)**: `PlaybackMode`, `Fps`, `FpsExt`, `SpriteFrame`, `Frames`, `AnimationDef`, `SpriteSheet` (`new`, `grid`, `from_regions`, `regions`, `region_of`, `frame_size(u32, u32)`, `animation`, `animation_with_mode`, `timed_animation`, `animation_count`, `animations`, `get_animation`, `columns`, `rows`, `total_frames`, `frame_source_rect`), `Sprite<M>`.

## Input, Accessibility, Clipboard & Idle (`input`, `accessibility`, `clipboard`, `idle`)

- `ElementKey`, `IntoElementKey`, `NodeId`, `MouseButton`, `PointerEvent`, `ScrollSource`, `ScrollEvent`, `TouchPhase`, `TouchEvent`, `GesturePhase`, `GestureKind`, `GestureEvent`, `Modifiers`, `Key`, `KeyChord`, `KeyEvent`.
- Pen tablet & stylus (`input::tablet`, protocol enabled by `tablet` feature): `TabletId`, `TabletToolId`, `TabletPadId`, `TabletToolType`, `TabletToolAxes`, `TabletToolEventKind`, `TabletToolEvent`, `TabletPadAxisSource`, `TabletPadEventKind`, `TabletPadEvent`, `TabletEvent`.
- Wayland IME (`input::ime`): `MAX_SURROUNDING_BYTES`, `ImeContentHint`, `ImeContentPurpose`, `ImeSurroundingText`, `ImeConfig`, `ImePreedit`, `ImeEvent`.
- Headless input & scroll controllers (`input::text_input`, `input::scroll`): `TextInputOutcome`, `TextInputState`, `ScrollState`.
- `KeyboardLayout::subscribe(map)` and `KeyboardLayoutInfo`.
- `AccessibleRole` (`accessibility::props`), plus AccessKit types when `accessibility` is enabled.
- `Clipboard` (`clipboard`): `Clipboard::subscribe`, `Clipboard::subscribe_selection`, `select_preferred_mime`, `ClipboardSelection`, `ClipboardSnapshot`, `ClipboardEvent`.
- `Idle` (`idle-notify`): `Idle::notify`, `Idle::input_notify`, `IdleEvent`.

## Subscriptions, Animation & Timers (`subscription`, `animation`, `timer`, `wayland_ext`)

- `SubscriptionKey`, `IntoSubscriptionKey`, `Subscription<M>` (`none`, `batch`, `map`), `Subscriptions<M>` (`new`, `none`, `push`, `extend`, `len`, `is_empty`, `map`), `Timer` (`once`, `every`).
- Animation (`animation`): `Easing`, `Lerp`, `SpringConfig`, `Spring`, `Tween<T>`, `Animated<T>`, `TransitionSpec`, `Animation::frames(key, map)`.
- Wayland extensions (`wayland_ext`): `SessionLock::subscribe` (`SessionLockEvent`), `ScreencopyTarget`, `ScreencopyFrame`, `ForeignToplevel::subscribe` (`ForeignToplevelId`, `ForeignToplevelState`, `ForeignToplevelInfo`, `ForeignToplevelSnapshot`), `ExtWorkspace::subscribe` (`ExtWorkspaceId`, `ExtWorkspaceGroupId`, `ExtWorkspaceState`, `ExtWorkspaceCapabilities`, `ExtWorkspaceInfo`, `ExtWorkspaceGroupInfo`, `ExtWorkspaceSnapshot`).
- Cursor, DnD & constraints (`input`): `CursorShape`, `PointerConstraintMode`, `RelativePointerEvent`, `VirtualKeyState`, `DndAction`, `DndEvent`.

## System Telemetry & Units (`system`)

- **Builders (`system::builders`)**: `SnapshotState<T>`, `System` factory: `cpu()`, `memory()`, `time()`, `gpu()`, `disk()`, `network()`, `battery()`, `sensors()`, `uptime()`, `brightness()`, `privacy()` -> `.every(interval, map)` or `.every_keyed(key, interval, map)`.
- **Providers (`system::providers::SystemProviders`)**: `new`, `sample_cpu`, `sample_memory`, `sample_time`, `sample_gpu`, `sample_disk`, `sample_network`, `sample_battery`, `sample_temperatures`, `sample_brightness`, `sample_privacy`, `sample_usb`, `os_info`, `hostname`, `uptime`, `load_average`, `set_backlight_percent`, `set_ddc_brightness`, `cpu_sample_count`.
- **Snapshots & parsers**: `CpuCounters`, `CpuSnapshot`, `CpuSampler`, `MemoryPsi`, `MemorySnapshot`, `TimeSnapshot`, `TimeFormat`, `LocalDateTime`, `CalendarMonth`, `GpuVendor`, `GpuDeviceSnapshot`, `GpuSnapshot`, `DiskMount`, `DiskDeviceIo` (`read_rate: DataRate`, `write_rate: DataRate`), `DiskSnapshot` (`total_read_rate: DataRate`, `total_write_rate: DataRate`), `DiskSampler`, `NetInterfaceKind`, `NetLinkState`, `NetInterfaceSnapshot`, `NetworkSnapshot`, `NetworkSampler`, `PowerStatus`, `BatteryInfo`, `BatterySnapshot`, `ThermalSensor`, `ThermalSnapshot`, `BacklightDevice`, `DdcMonitorInfo`, `BrightnessSnapshot`, `UsbDevice`, `OsInfo`, `LoadAverage`, `UptimeSnapshot`, `ProcessCgroupInfo`, `PrivacyAccessKind`, `PrivacyAccessSource`, `PrivacyAccessEntry`, `PrivacySnapshot`, `PrivacyMonitor`, `sample_privacy_oneshot`, `ByteSize`, `FreqUnit`, `RateUnit`, `DataRate`.

## Desktop Utilities (`desktop`)

- `LaunchOptions` (`new`, `working_dir`, `activation_token`, `env`), `SpawnOptions` (`new`, `working_dir`, `env`).
- `DesktopEntry`: `exec_argv`, `try_exec_available`, `should_show_in_launcher`, `dbus_service_name`, `dbus_object_path`, `action`, `terminal_argv`, `launch() -> Result<u32>`, `launch_with(&LaunchOptions) -> Result<u32>`, `launch_action(action_id, &LaunchOptions) -> Result<u32>`.
- `DesktopAction`: `exec_argv`, `launch(entry) -> Result<u32>`, `launch_with(entry, &LaunchOptions) -> Result<u32>`.
- Entry discovery & search: `default_application_dirs`, `scan_desktop_entries`, `scan_desktop_entries_in`, `filter_launcher_entries`, `DesktopSearchMatch`, `search_desktop_entries`, `search_desktop_entries_with_weights`, `find_desktop_entry`, `parse_desktop_entry`.
- XDG Base & User Directories (`desktop::xdg`): `XdgBaseDirs` (`from_env`, `from_lookup`, `all_config_dirs`, `all_data_dirs`, `application_dirs`, `icon_dirs`, `sound_dirs`, `find_config_file`, `find_data_file`), `XdgUserDirs` (`load`, `parse`).
- Icons (`desktop::icons`): `IconDirSpec`, `IconThemeInfo`, `XdgIconLookup` (`new`, `with_max_entries`, `with_dirs`, `set_max_entries`, `clear`, `clear_results`, `resolve`), `resolve_icon_path`, `default_icon_dirs`, `detect_active_icon_theme`, `parse_icon_theme_index`.
- Process execution: `spawn_detached(program, args) -> Result<u32>`, `spawn_with(program, args, &SpawnOptions) -> Result<u32>`, `spawn_shell(command, &SpawnOptions) -> Result<u32>`.

## Audio (`audio`)

- **Audio mixer (`pipewire`, `pulseaudio`, or `alsa`)**:
  - Types: `AudioDirection` (`Output`, `Input`), `AudioTarget` (`DefaultSink`, `DefaultSource`, `Sink(u32)`, `Source(u32)`, `PlaybackStream(u32)`, `CaptureStream(u32)`), `AudioPortTarget` (`DefaultSink`, `DefaultSource`, `Sink(u32)`, `Source(u32)`), `AudioChannelPosition`, `AudioVolume` (`linear`, `channels`, `channel_map`, `db`, `muted`; `percent`, `balance`, `channel_count`), `AudioPortAvailability`, `AudioPort`, `AudioProfile`, `AudioCard`, `AudioDevice`, `AudioStream` (unified playback & capture), `AudioSnapshot` (`sinks`, `sources`, `default_sink_id`, `default_source_id`, `streams`, `cards`; `default_sink`, `default_source`, `playback_streams`, `capture_streams`), `AudioEvent`, `compute_balance_with_map`, `compute_stereo_balance`.
  - Subscriptions: `Audio::subscribe(impl Fn(Arc<AudioSnapshot>) -> M)`, `Audio::events(impl Fn(AudioEvent) -> M)`.
  - `AudioClient`: `new`, `snapshot() -> Option<Arc<AudioSnapshot>>`, `set_volume(target, linear)`, `set_volume_percent(target, percent)`, `adjust_volume(target, delta_linear)`, `set_channel_volumes(target, channels)`, `set_balance(target, balance)`, `set_mute(target, muted)`, `toggle_mute(target)`, `set_default_device(direction, device_id)`, `set_port(target, port_name)`, `set_card_profile(card_id, profile_name)`.
- **Audio playback (`audio-playback` or native backend)**:
  - `SoundId`, `SoundAsset`, `SoundPreset`, `SoundPlay`, `SoundData`.
  - `AudioPlayer`: `new`, `load_file`, `load_bytes`, `load_system_sound`, `load_preset`, `unload`, `clear_cache`, `play`, `play_at`, `play_trigger`, `play_data`, `play_preset`, `play_system_sound`, `stop_all`.
  - Theme lookup: `resolve_sound_theme_path`, `default_sound_dirs`, `sound::resolve_sound_theme_path_in`.

## Compositor & Control IPC (`compositor`, `control`)

- **Unified adapter (`compositor::adapter`)**: `CompositorWorkspace`, `WorkspaceTarget<'a>` (`Id(i32)`, `Name(&'a str)`, `Relative { delta, existing_only }`, `Previous`), `CompositorEvent`, `Compositor::subscribe(map)`, `CompositorClient<'a>` enum (`Hyprland(&'a HyprlandClient)`, `Sway(&'a SwayClient)`: `workspaces`, `switch_workspace`, `next_keyboard_layout`, `switch_keyboard_layout`).
- **Hyprland (`compositor::hyprland`, `hyprland-ipc`)**:
  - Types: `WorkspaceId`, `WorkspaceType`, `HyprlandLayout`, `WorkspaceInfo`, `WorkspaceRuleInfo`, `ClientInfo`, `HyprlandWorkspaceRef`, `KeyboardLayoutSwitch` (`Next`, `Prev`, `Index(u32)`), `MonitorInfo`, `CursorPos`, `HyprlandVersion`, `HyprlandDevices`, `HyprlandKeyboard`, `HyprlandMouse`, `HyprlandOption`, `HyprlandOptionValue`, `HyprlandEvent` (`parse_line`), `HyprlandSockets`.
  - `Hyprland::subscribe(map)`, `HyprlandClient` (`from_env`, `with_sockets`, `with_timeout`, `sockets`, `workspaces`, `active_workspace`, `workspace_rules`, `default_layout`, `set_default_layout`, `layout_message`, `active_client`, `clients`, `monitors`, `devices`, `version`, `cursor_pos`, `get_option`, `switch_workspace(impl Into<WorkspaceTarget<'_>>)`, `move_workspace_to_monitor`, `rename_workspace`, `focus_window`, `close_window`, `move_window_to_workspace`, `toggle_special_workspace`, `dpms_off`, `dpms_on`, `next_keyboard_layout`, `switch_keyboard_layout`, `switch_keyboard_layout_for`, `keyword`, `eval`, `batch`, `dispatch`, `raw_command`, `apply_workspace_event`).
- **Sway (`compositor::sway`, `sway-ipc`)**:
  - Types: `SwayWorkspace`, `SwayOutput`, `SwayVersion`, `SwayEvent` (`parse_frame`).
  - `Sway::subscribe(map)`, `SwayClient` (`from_env`, `with_socket`, `with_timeout`, `socket_path`, `workspaces`, `outputs`, `tree`, `version`, `switch_workspace(impl Into<WorkspaceTarget<'_>>)`, `next_keyboard_layout`, `switch_keyboard_layout`, `raw_command`).
- **Control IPC (`control-ipc`)**:
  - `Signal` (`Usr1`, `Usr2`), `Control::signal(key, signal, msg)`, `Control::socket(name_or_path, map)`, `Control::default_socket(map)`.
  - `ControlRequest: Clone + Send + Sync + Deref<Target = str>` (`command`, `into_command`, `can_reply`, `reply`), `control::{resolve_socket_path, send_command, send_command_with_reply}`.

## D-Bus Subsystems (`dbus`)

Available when the `dbus` Cargo feature is enabled (`Dbus::global_shortcuts` also requires `global-shortcuts`):

- **Core (`types`, `client`)**: `DbusBus`, `DbusValue`, `DbusSignalEvent`, `DbusActionLane`, `DbusActionReply` (`Unit`, `ObjectPath`, `MountPath`, `Count`, `Values`), `DbusActionResult`, `Dbus::{subscribe_signal, action_results}`, `DbusClient` (`new`, `trim_cache`, `get_property`, `set_property`, `call_method`, `call_method_blocking`, plus all 28 domain sub-handles below).
- **Notifications (`dbus.notifications()`)**: `NotificationCapabilities`, `NotificationUrgency`, `NotificationCloseReason`, `NotificationImageData`, `DesktopNotification`, `OutgoingNotification`, `NotificationEvent`, `Dbus::{notifications, notifications_with_capabilities}`, `NotificationsClient` (`capabilities`, `set_capabilities`, `close`, `invoke_action`, `send`, `send_blocking`, `close_notification`).
- **System Tray (`dbus.tray()`)**: `TrayCategory`, `TrayStatus`, `TrayIconPixmap`, `TrayToolTip`, `TrayMenuItem`, `TrayItemSnapshot`, `TrayEvent`, `Dbus::tray`, `TrayClient` (`cached_items`, `items_blocking`, `activate`, `secondary_activate`, `context_menu`, `scroll`, `about_to_show_blocking`, `menu_layout_blocking`, `submenu_layout_blocking`, `trigger_menu_item`).
- **MPRIS (`dbus.mpris()`)**: `MprisPlaybackStatus`, `MprisLoopStatus`, `MprisSnapshot`, `MprisEvent`, `Dbus::mpris`, `MprisClient` (`cached_snapshots`, `snapshots_blocking`, `list_players_blocking`, `play`, `pause`, `play_pause`, `stop`, `next`, `previous`, `seek`, `set_position`, `set_volume`, `set_loop_status`, `set_shuffle`, `set_rate`, `raise`, `quit`, `open_uri`).
- **NetworkManager (`dbus.nm()`)**: `NmState`, `NmVpnState`, `NmConnectivity`, `NmDeviceType`, `NmWifiSecurity`, `NmAccessPoint`, `NmSavedConnection`, `NmDeviceInfo`, `NmActiveConnection`, `NetworkManagerSnapshot`, `WifiConnectOptions`, `WifiEnterpriseOptions`, `HotspotOptions`, `NmSecretAgentRequest`, `NmSecretAgentReply`, `NmSecretAgentEvent`, `Dbus::{network_manager, nm_secret_agent}`, `NetworkManagerClient` (`cached_snapshot`, `snapshot_blocking`, `set_networking`, `set_wireless`, `scan_wifi`, `connect_wifi`, `activate_connection`, `deactivate_connection`, `disconnect_device`, `forget_connection`, `forget_wifi_ssid`, `start_hotspot`, `activate_vpn`, `deactivate_vpn`, `reply_secret_agent`).
- **Power Profiles (`dbus.power_profiles()`)**: `PowerProfileKind`, `PowerProfileHold`, `PowerProfileSnapshot`, `Dbus::power_profiles`, `PowerProfilesClient` (`cached_snapshot`, `snapshot_blocking`, `set_profile`, `hold_blocking`, `release_hold`).
- **UPower (`dbus.upower()`)**: `UPowerDeviceKind`, `UPowerDeviceState`, `UPowerDevice`, `UPowerSnapshot`, `Dbus::upower`, `UPowerClient` (`cached_snapshot`, `snapshot_blocking`).
- **UDisks2 (`dbus.udisks()`)**: `UDisksDrive`, `UDisksSmartHealth`, `UDisksBlockDevice`, `UDisksSnapshot`, `UDisksMountOptions`, `UDisksFormatOptions`, `Dbus::udisks`, `UDisksClient` (`cached_snapshot`, `snapshot_blocking`, `mount`, `unmount`, `eject`, `power_off`, `unlock`, `lock`, `format_block`).
- **Logind (`dbus.logind()`)**: `PowerAction`, `PowerActionSupport`, `LogindPowerCapabilities`, `LogindInhibitor`, `LogindSnapshot`, `LogindEvent`, `Dbus::logind`, `LogindClient` (`cached_snapshot`, `snapshot_blocking`, `power_capabilities_blocking`, `power`, `lock_session`, `unlock_session`, `terminate_session`, `set_brightness`, `inhibit_blocking`).
- **Bluetooth (`dbus.bluetooth()`)**: `BluetoothAdapter`, `BluetoothDevice`, `BluetoothSnapshot`, `BluetoothTransportFilter`, `BluetoothAgentRequest`, `BluetoothAgentReply`, `BluetoothAgentEvent`, `Dbus::{bluetooth, bluetooth_agent}`, `BluetoothClient` (`cached_snapshot`, `snapshot_blocking`, `set_powered`, `set_discoverable`, `set_pairable`, `start_discovery`, `stop_discovery`, `connect`, `disconnect`, `pair`, `cancel_pairing`, `trust`, `remove`, `reply_agent`, `connect_profile`, `disconnect_profile`, `obex_send_file`).
- **ModemManager (`dbus.modem()`)**: `ModemState`, `ModemAccessTech`, `ModemRegistrationState`, `ModemInfo`, `ModemSnapshot`, `Dbus::modem`, `ModemClient` (`cached_snapshot`, `snapshot_blocking`, `set_enabled`).
- **ScreenSaver & GeoClue2 (`dbus.screensaver()`, `dbus.geoclue()`)**: `ScreenSaverSnapshot`, `GeoClueSnapshot`, `Dbus::{screensaver, geoclue}`, `ScreenSaverClient` (`active_blocking`, `lock`, `inhibit_blocking`, `uninhibit`), `GeoClueClient` (`in_use_blocking`).
- **XDG Desktop Portals (`dbus.portal()`)**: `PortalColorScheme`, `PortalContrast`, `PortalAccentColor`, `PortalSettingsSnapshot`, `PortalOpenUriOptions`, `PortalScreenshotOptions`, `PortalFileFilter`, `PortalFileChooserOptions`, `PortalInhibitFlags`, `PortalBackgroundOptions`, `PortalBackgroundResult`, `PortalScreenCastSourceType`, `PortalScreenCastCursorMode`, `PortalScreenCastOptions`, `PortalScreenCastStream`, `PortalScreenCastResult`, `Dbus::portal_settings`, `GlobalShortcutDescriptor`, `BoundGlobalShortcut`, `GlobalShortcutEvent`, `Dbus::global_shortcuts` (`global-shortcuts`), `PortalClient` (`settings_blocking`, `read_setting_blocking`, `has_global_shortcuts_blocking`, `open_uri`, `open_file_uri`, `open_directory`, `screenshot_blocking`, `pick_color_blocking`, `open_file_blocking`, `save_file_blocking`, `inhibit_blocking`, `close_request`, `close_session`, `trash_file_blocking`, `request_background_blocking`, `screencast_start_blocking`).
- **Polkit & Hardware Sensors (`dbus.polkit()`, `dbus.sensors()`)**: `PolkitIdentity`, `PolkitAuthRequest`, `PolkitAgentReply`, `PolkitAgentEvent`, `PolkitAuthResult`, `Dbus::polkit_agent`, `PolkitClient` (`reply`, `check_authorization_blocking`); `SensorOrientation`, `LightLevelUnit`, `SensorProxySnapshot`, `Dbus::sensors`, `SensorsClient` (`cached_snapshot`, `snapshot_blocking`).
- **systemd, timedate1, hostname1 & locale1 (`dbus.systemd()`, `dbus.timedate()`, `dbus.hostname()`, `dbus.locale()`)**: `SystemdUnitInfo`, `SystemdUnitsSnapshot`, `TimedateSnapshot`, `HostnameSnapshot`, `LocaleSnapshot`, `Dbus::{systemd_units, timedate}`, `SystemdClient` (`cached_snapshot`, `list_units_blocking`, `unit_state_blocking`, `start_unit`, `stop_unit`, `restart_unit`, `reload_unit`, `enable_units`, `disable_units`, `daemon_reload`), `TimedateClient` (`cached_snapshot`, `snapshot_blocking`, `list_timezones_blocking`, `set_timezone`, `set_ntp`, `set_local_rtc`, `set_time`), `HostnameClient` (`snapshot_blocking`, `set_pretty_hostname`, `set_static_hostname`), `LocaleClient` (`snapshot_blocking`, `set_x11_keyboard`, `set_locale`).
- **AccountsService, Fprintd, GameMode & Secrets (`dbus.accounts()`, `dbus.fprint()`, `dbus.gamemode()`, `dbus.secrets()`)**: `AccountType`, `UserAccount`, `AccountsSnapshot`, `Dbus::accounts`, `AccountsClient` (`cached_snapshot`, `snapshot_blocking`, `current_user_blocking`, `set_icon_file`, `set_real_name`); `FprintDeviceInfo`, `FprintVerifyResult`, `FprintEvent`, `Dbus::fprint_verify`, `FprintClient` (`devices_blocking`, `list_enrolled_fingers_blocking`); `GameModeSnapshot`, `Dbus::gamemode`, `GameModeClient` (`cached_snapshot`, `snapshot_blocking`, `query_status_blocking`, `register_game`, `unregister_game`); `SecretItemInfo`, `SecretsClient` (`list_collections_blocking`, `search_items_blocking`, `get_secret_blocking`, `store_secret_blocking`, `delete_item`).
- **Updates & Optional Desktop Services (`dbus.fwupd()`, `dbus.packagekit()`, `dbus.kdeconnect()`, `dbus.colord()`, `dbus.avahi()`, `Dbus::cups`)**: `FwupdDevice`, `FwupdUpdate`, `FwupdSnapshot`, `Dbus::fwupd`, `FwupdClient` (`cached_snapshot`, `snapshot_blocking`, `verify_device`); `PackageKitSnapshot`, `PackageKitUpdate`, `PackageKitClient` (`snapshot_blocking`, `check_updates_blocking`); `CupsEvent`, `Dbus::cups`; `KdeConnectDevice`, `KdeConnectSnapshot`, `Dbus::kdeconnect`, `KdeConnectClient` (`cached_snapshot`, `snapshot_blocking`, `ping`, `ring`, `share_url`, `share_text`); `ColordDevice`, `ColordProfile`, `ColordSnapshot`, `Dbus::colord`, `ColordClient` (`cached_snapshot`, `snapshot_blocking`, `make_profile_default`); `AvahiSnapshot`, `AvahiServiceEntry`, `AvahiClient` (`snapshot_blocking`, `browse_services_blocking`).
