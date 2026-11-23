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
  - Execution & events: `enqueue(msg)`, `step()`, `step_at(now)`, `click_surface_point(...)`, `pointer_down_surface_point(...)`, `pointer_move_surface_point(...)`, `pointer_leave_surface(...)`, `pointer_up_surface_point(...)`, `scroll_surface_point(...)`, `key_surface(...)`, `simulate_ime(...)`, `simulate_popup_dismiss(...)`, `simulate_surface_configure(...)`.
  - Inspection & accessibility: `capabilities()`, `state()`, `quit_requested()`, `surface_count()`, `surface_logical_size(...)`, `surface_ime_config(...)`, `activated_surfaces()`, `arena_heap_fallbacks()`, `asset_pixels_resident(asset)`, `decoded_asset_bytes()`, `rendered_buffer(...)`, `accessibility_tree()`, `dispatch_accessibility_action(...)`.

## Effects (`effect`)

`EffectCtx<'a>` (`effect::context`) is passed into `Application::init` and `Application::update`:

- **Lifecycle & runtime**: `quit()`, `capabilities() -> &Capabilities`, `outputs() -> &[OutputInfo]`, `after<M>(delay, make)` (`M: Send + 'static`), `request_activation_token<M>(app_id, map)` (`xdg-activation-v1`), `activate_surface(surface_key, token)` (`xdg-activation-v1`), `system() -> &mut SystemProviders`.
- **Image & sprite assets**: `load_image(path)`, `load_image_sized(path, min_svg_px)`, `load_image_bytes(bytes)`, `load_image_bytes_sized(bytes, min_svg_px)`, `load_rgba_image(width, height, stride, pixels)`, `load_image_async(source, map)`, `set_asset_retention(asset, retention) -> bool`, `asset_retention(asset) -> Option<AssetRetention>`, `evict_asset_pixels(asset) -> bool`, `reload_asset(asset, path)`, `unload_asset(asset) -> bool`.
- **Audio (`audio` / `audio-playback`)**: `audio() -> Result<&AudioClient>`, `player() -> Result<&AudioPlayer>`, `load_sound(path)`, `load_sound_bytes(bytes)`, `load_sound_preset(preset)`, `load_system_sound(name)`, `play_sound(sound)`, `play_sound_volume(sound, volume)`, `play_sound_preset(preset, volume)`, `play_system_sound(name, volume)`, `stop_sounds()`.
- **Compositors**: `hyprland()` (`hyprland-ipc`), `sway()` (`sway-ipc`), `compositor() -> Result<CompositorClient<'_>>`.
- **D-Bus & hardware**: `dbus()` (`dbus`), `set_backlight_percent(device_name, percent)`, `set_ddc_brightness(i2c_bus, percent)`, `privacy_snapshot() -> PrivacySnapshot`.
- **Clipboard (`clipboard`)**: `set_clipboard_text(text)`, `set_clipboard_primary_text(text)`, `set_clipboard(selection, entries)`.
- **Text cursor layout (`text`)**: `measure_text_cursor(text, font, font_size, cursor_byte)`, `measure_text_cursor_with(text, options, cursor_byte)`.

## Errors & Capabilities (`error`, `capabilities`)

- `Result<T>`: `std::result::Result<T, WaywiError>`.
- `ErrorKind`: `NotAvailable`, `NotSupported`, `TemporarilyUnavailable`, `PermissionDenied`, `OperationFailed`, `InvalidState`, `Timeout`, `Shutdown`.
- `WaywiError` (`.kind(&self) -> ErrorKind`): `WaylandConnection`, `UnsupportedCapability(Capability)`, `OutputUnavailable`, `SurfaceError`, `RenderError`, `AssetDecode`, `UnsupportedAssetFormat`, `ProviderUnavailable`, `PermissionDenied`, `IpcError`, `DbusError`, `AudioError`, `InvalidGeometry`, `InvalidState`, `Timeout`, `Io(std::io::Error)`, `Shutdown`, `Custom`.
- `Capability`: `Wayland`, `LayerShell`, `XdgShell`, `XdgActivation`, `FractionalScale`, `Viewporter`, `PointerInput`, `KeyboardInput`, `TouchInput`, `SoftwareRenderer`, `GpuRenderer`, `Text`, `ImagePng`, `ImageSvg`, `ImageGif`, `ImageWebp`, `Animation`, `ControlIpc`, `HyprlandIpc`, `SwayIpc`, `PipeWire`, `PulseAudio`, `Alsa`, `Dbus`, `GlobalShortcuts`, `Accessibility`, `AudioPlayback`, `Clipboard`, `IdleNotify`, `TextInput`, `KeyboardLayout`.
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
  - `Transform`: `identity`, `is_identity`, `is_pure_translation`, `translate`, `scale`, `scale_xy`, `rotate_deg`, `rotate_rad`, `pivot`, `pivot_point`, `to_affine_around(pivot)`.
  - `Affine2D`: `IDENTITY`, `is_identity`, `is_axis_aligned_unscaled`, `then`, `transform_point`, `inverse`, `transform_rect`.

## Surfaces & Displays (`surface`, `display`)

- `SurfaceKey(pub u64)` (`from_hashable`) and `IntoSurfaceKey::into_surface_key`.
- `Layer`: `Background`, `Bottom`, `Top`, `Overlay`.
- `Anchor` bitflags: `TOP` (`Top`), `BOTTOM` (`Bottom`), `LEFT` (`Left`), `RIGHT` (`Right`).
- `KeyboardInteractivity`: `None`, `OnDemand`, `Exclusive`.
- `InputMode`: `Full`, `Selective`, `PassThrough`.
- `BufferRetention`: `Retain`, `ReleaseAfterCommit`.
- `SurfaceKind`: `Panel`, `Floating`, `Overlay`, `Window`, `Popup`.
- Popup positioning (`surface::types`): `PopupAnchor` (`None`, `Top`, `Bottom`, `Left`, `Right`, `TopLeft`, `BottomLeft`, `TopRight`, `BottomRight`), `PopupGravity` (`None`, `Top`, `Bottom`, `Left`, `Right`, `TopLeft`, `BottomLeft`, `TopRight`, `BottomRight`), `PopupConstraintAdjustment` (`SLIDE_X`, `SLIDE_Y`, `FLIP_X`, `FLIP_Y`, `RESIZE_X`, `RESIZE_Y`), `PopupPositioner` (`new(anchor_rect)`, `anchor`, `gravity`, `offset`, `constraint_adjustment`, `reactive`).
- `Surface<M>` (`surface::builder`):
  - Constructors: `Surface::panel(key, output)`, `Surface::floating(key, output)`, `Surface::overlay(key, output)`, `Surface::window(key)`, `Surface::popup(key, parent_surface)`, `Surface::memo(key, dep, build)`.
  - Builders: `.layer`, `.anchor`, `.margin`, `.size`, `.exclusive_zone`, `.ignore_exclusive_zone`, `.keyboard_interactivity`, `.input_mode`, `.buffer_retention`, `.output`, `.positioner`, `.anchor_rect`, `.popup_anchor`, `.popup_gravity`, `.popup_offset`, `.popup_constraints`, `.popup_reactive`, `.popup_grab`, `.visible`, `.background`, `.title`, `.namespace`, `.accessible_role`, `.accessible_label`, `.accessible_description`, `.accessible_hidden`, `.ime`, `.on_ime`, `.child`, `.children`, `.on_close`, `.on_key`, `.on_pointer_move`, `.on_pointer_up`, `.map`.
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
  - `TextAlign`: `Left`, `Center`, `Right`; `TextBoundsMode`: `Typographic`, `Optical`.
  - `Text<M>`: `new`, `font_size`, `line_height`, `color`, `font`, `align`, `bounds_mode`, `optical_bounds`, `map`.
  - `TextCursorMetrics` (`size`, `cursor_x`, `cursor_y`, `line_height`) and `TextLayoutOptions` (`new`, `line_height`, `max_width`, `align`, `bounds_mode`).
  - `SharedStr`: `EMPTY`, `from_static`, `from_str_inline_or_arc`, `format_inline`, `as_str`, `is_inline`, `is_static`.
- **Images & Assets (`image`, `asset::types`)**:
  - `AssetId(pub u64)` (`new`, `get`), `AssetRetention` (`KeepDecoded`, `ReclaimAfterPresent`), `ImageAsset`, `DecodedImage`, `DecodedGifFrame`, `ImageFormat` (`Png`, `Svg`, `Gif`, `Webp`).
  - `SourceRect` (`new`, `as_tuple`), `ImageFit`: `Fill`, `Contain`, `Cover`, `Original`; `ImageFilter`: `Bilinear`, `Bicubic`, `Nearest`.
  - `Image<M>`: `new(asset)`, `fit`, `filter`, `source_rect`, `region`, `flip_x`, `flip_y`, `map`.
- **Sprites (`sprite`)**:
  - `PlaybackMode`: `Once`, `Loop`, `PingPong`.
  - `Fps(pub f32)` (`new`, `get`, `frame_duration`) and `FpsExt::fps(self)`.
  - `SpriteFrame` (`new(index, duration)`), `Frames` (`range`, `sequence`, `timed`, `count`, `first_frame`, `frame_at`), `AnimationDef`.
  - `SpriteSheet`: `new(asset)`, `from_regions(asset, regions)`, `regions`, `region_of`, `frame_size`, `animation`, `animation_with_mode`, `timed_animation`, `animation_count`, `animations`, `get_animation`, `columns`, `rows`, `total_frames`, `frame_source_rect`.
  - `Sprite<M>`: `new(sheet)`, `frame`, `playing`, `playback`, `filter`, `flip_x`, `flip_y`, `on_animation_end`, `map`.

## Input, Accessibility, Clipboard & Idle (`input`, `accessibility`, `clipboard`, `idle`)

- `ElementKey(pub u64)` (`from_hashable`), `IntoElementKey::into_element_key`, `NodeId { index, generation }`.
- `MouseButton`: `Left`, `Right`, `Middle`, `Side`, `Extra`, `Other(u32)`, `from_linux_button`.
- `PointerEvent`, `ScrollEvent`, `TouchEvent`.
- `Modifiers` bitflags: `CTRL` (`CONTROL`), `ALT`, `SHIFT`, `SUPER` (`LOGO`).
- `Key` (`from_keysym`), `KeyChord::new(modifiers, key)`, `KeyEvent::matches(chord)`.
- Wayland IME (`input::ime`, `ime` feature for compositor protocol): `MAX_SURROUNDING_BYTES`, `ImeContentHint` (`COMPLETION`, `SPELLCHECK`, `AUTO_CAPITALIZATION`, `LOWERCASE`, `UPPERCASE`, `TITLECASE`, `HIDDEN_TEXT`, `SENSITIVE_DATA`, `LATIN`, `MULTILINE`, `ON_SCREEN_INPUT_PROVIDED`, `NO_EMOJI`, `PREEDIT_SHOWN`), `ImeContentPurpose` (`Normal`, `Alpha`, `Digits`, `Number`, `Phone`, `Url`, `Email`, `Name`, `Password`, `Pin`, `Date`, `Time`, `Datetime`, `Terminal`; `to_wire_u32`), `ImeSurroundingText` (`from_slice`), `ImeConfig` (`disabled`, `enabled`, `from_text_input`, `surrounding_text`, `cursor_rect`, `content_hint`, `content_purpose`, `external_change_seq`), `ImePreedit`, `ImeEvent` (`Enter`, `Leave`, `Update`).
- Headless input & scroll controllers (`text_input`, `scroll`): `TextInputOutcome` (`Edited`, `CursorMoved`, `Submitted`, `Cancelled`, `Unchanged`; `text_changed`, `needs_redraw`), `TextInputState` (`new`, `with_text`, `with_max_chars`, `text`, `is_empty`, `len_chars`, `cursor_bytes`, `cursor_chars`, `anchor_bytes`, `external_change_seq`, `before_cursor`, `after_cursor`, `selection_range`, `selected_text`, `preedit`, `has_preedit`, `display_segments`, `display_text`, `display_cursor_bytes`, `masked`, `set_text`, `clear`, `select_all`, `set_cursor_bytes`, `insert_str`, `backspace`, `delete_forward`, `delete_word_backward`, `delete_word_forward`, `move_left`, `move_right`, `move_word_left`, `move_word_right`, `move_home`, `move_end`, `handle_key`, `handle_ime`), `ScrollState` (`new`, `offset`, `viewport`, `content`, `max_offset`, `can_scroll`, `progress`, `set_bounds`, `set_offset`, `scroll_by`, `apply_scroll`, `apply_scroll_x`, `ensure_visible`, `visible_range`, `thumb_metrics`).
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
- **Providers (`system::providers::SystemProviders`)**: `new`, `cpu_snapshot`, `poll_cpu_now`, `memory_snapshot`, `time_snapshot`, `gpu_snapshot`, `poll_disk_now`, `poll_network_now`, `os_info`, `hostname`, `batteries`, `sensors`, `usb_devices`, `uptime`, `load_average`, `brightness_snapshot`, `set_backlight_percent`, `set_ddc_brightness`, `privacy_snapshot`, `cpu_subscriber_count`, `cpu_sample_count`.
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
  - `usb`: `UsbDevice`, `read_usb_devices`, `read_usb_devices_from`.
- **OS (`system::os`)**: `OsInfo`, `LoadAverage`, `UptimeSnapshot`, `ProcessCgroupInfo`, `release::{read_os_info, parse_os_release, read_hostname}`, `uptime::{read_uptime, read_load_average, parse_uptime, parse_loadavg}`, `cgroup::{read_process_cgroup, parse_process_cgroup}`.
- **Privacy (`system::privacy`)**: `PrivacyAccessKind`, `PrivacyAccessSource`, `PrivacyAccessEntry`, `PrivacySnapshot` (`any_active()`), `PrivacyMonitor` (`new`, `ensure_started`, `stop_background`, `sample`), `classify_pipewire_node`, `scan_proc_fd`, `scan_proc_fd_from`.
- **Units (`system::units`)**:
  - `ByteSize`: `ZERO`, `from_bytes`, `from_kib`, `from_mib`, `as_bytes`, `as_kib`, `as_mib`, `as_gib`, `saturating_sub`.
  - `FreqUnit` (`Khz`, `Mhz`, `Ghz`): `convert_khz`, `format_khz`.
  - `RateUnit` (`AutoBytes`, `AutoBits`, `BytesPerSec`, `KilobytesPerSec`, `MegabytesPerSec`, `GigabytesPerSec`, `BitsPerSec`, `KilobitsPerSec`, `MegabitsPerSec`, `GigabitsPerSec`).
  - `DataRate`: `ZERO`, `from_bytes_per_sec`, `as_bytes_per_sec`, `as_bits_per_sec`, `value_in(RateUnit)`, `format(RateUnit)`.

## Desktop Utilities (`desktop`)

- `DesktopEntry` (`dbus_activatable`, `actions: Vec<DesktopAction>`): `exec_argv()`, `try_exec_available()`, `should_show_in_launcher()`, `dbus_service_name()`, `dbus_object_path()`, `action(id)`, `terminal_argv(terminal)`, `launch()`, `launch_detached()`, `launch_with_token(token)`, `launch_with_terminal(terminal)`, `launch_with_terminal_and_token(terminal, token)`, `launch_action(action_id, token)`.
- `DesktopAction` (`[Desktop Action <id>]`): `id`, `name`, `icon`, `exec`, `exec_argv()`, `launch_detached()`, `launch_with_token(token)`.
- Entry discovery & search: `default_application_dirs()`, `scan_desktop_entries()`, `scan_desktop_entries_in(dirs)`, `filter_launcher_entries(entries)`, `DesktopSearchMatch`, `search_desktop_entries(entries, query)`, `search_desktop_entries_with_weights(entries, query, launch_counts)`, `find_desktop_entry(id_or_wm_class)`, `parse_desktop_entry(contents, id, path)`.
- XDG Base & User Directories (`desktop::xdg`): `XdgBaseDirs` (`from_env`, `from_lookup`, `all_config_dirs`, `all_data_dirs`, `find_config_file`, `find_data_file`), `XdgUserDirs` (`load`, `parse`), and helpers `xdg_config_home`, `xdg_config_dirs`, `xdg_data_home`, `xdg_data_dirs`, `xdg_cache_home`, `xdg_state_home`, `xdg_runtime_dir`.
- Icons (`desktop::icons`): `IconDirSpec`, `IconThemeInfo`, `XdgIconLookup` (`new`, `with_dirs`, `resolve`), `resolve_icon_path`, `default_icon_dirs`, `detect_active_icon_theme`, `parse_icon_theme_index`.
- Process execution: `spawn_detached(program, args)`, `spawn_detached_in(program, args, working_dir)`, `spawn_detached_with_env(program, args, working_dir, extra_env)`, `spawn_shell_detached(command)`.

## Audio (`audio`)

- **Audio mixer (`pipewire`, `pulseaudio`, or `alsa`)**:
  - `AudioChannelPosition` (`Unknown`, `Mono`, `FrontLeft`, `FrontRight`, `FrontCenter`, `Lfe`, `RearLeft`, `RearRight`, `RearCenter`, `SideLeft`, `SideRight`, `FrontLeftCenter`, `FrontRightCenter`, `TopCenter`, `TopFrontLeft`, `TopFrontCenter`, `TopFrontRight`, `TopRearLeft`, `TopRearCenter`, `TopRearRight`, `Aux(u16)`; `is_left()`, `is_right()`, `is_center_or_lfe()`, `is_front()`, `is_rear()`, `short_name()`, `default_map_for_count(count)`), `AudioPortAvailability` (`Unknown`, `Available`, `Unavailable`; `is_available()`), `AudioPort` (`index`, `name`, `description`, `priority`, `availability`), `AudioProfile` (`index`, `name`, `description`, `priority`, `available`), `AudioCard` (`id`, `name`, `description`, `active_profile`, `profiles`), `AudioDevice` (`id`, `name`, `description`, `card_id`, `active_port`, `ports`, `channel_map`), `AudioStream` (`id`, `name`, `binary`, `icon_name`, `app_id`, `sink_id`, `linear`, `channels`, `channel_map`, `db`, `muted`; `balance()`), `AudioCapture` (`id`, `name`, `binary`, `icon_name`, `app_id`, `source_id`, `linear`, `channels`, `channel_map`, `db`, `muted`; `balance()`), `AudioSnapshot` (`linear`, `channels`, `channel_map`, `db`, `muted`, `default_sink`, `devices`, `source_linear`, `source_channels`, `source_channel_map`, `source_db`, `source_muted`, `default_source`, `sources`, `cards`, `streams`, `captures`; `percent()`, `source_percent()`, `balance()`, `source_balance()`, `ports()`, `active_port()`, `source_ports()`, `active_source_port()`, `card(id)`, `default_sink_card()`, `default_source_card()`, `stream_percent(id)`, `capture_percent(id)`), `compute_stereo_balance(channels)`, `compute_balance_with_map(channels, channel_map)`, `AudioEvent`.
  - `Audio::subscribe(map)`, `Audio::subscribe_events(map)`.
  - `AudioClient` (`new`, `snapshot`, `set_volume_linear`, `set_balance`, `set_channel_volumes`, `set_muted`, `toggle_mute`, `select_device`, `select_port`, `select_device_port`, `set_source_volume_linear`, `set_source_balance`, `set_source_channel_volumes`, `set_source_muted`, `toggle_source_mute`, `select_source`, `select_source_port`, `select_source_device_port`, `select_card_profile`, `set_stream_volume_linear`, `set_stream_balance`, `set_stream_channel_volumes`, `set_stream_muted`, `toggle_stream_mute`, `set_capture_volume_linear`, `set_capture_balance`, `set_capture_channel_volumes`, `set_capture_muted`, `toggle_capture_mute`).
- **Audio playback (`audio-playback` or native backend)**:
  - `SoundAsset`, `SoundPreset` (`Click`, `DragTick`, `Hover`, `Notification`, `Warning`).
  - `SoundData`: `from_pcm_f32`, `duration`, `from_file`, `from_bytes`, `from_bytes_with_hint`, `from_wav_bytes`, `tone`, `from_preset`, `to_stereo_48k`.
  - `AudioPlayer`: `new`, `load_file`, `load_bytes`, `load_system_sound`, `load_preset`, `play`, `play_with_volume`, `play_data`, `play_preset`, `play_system_sound`, `stop_all`.
  - Theme lookup: `resolve_sound_theme_path`, `default_sound_dirs`, `resolve_sound_theme_path_in`.

## Compositor & Control IPC (`compositor`, `control`)

- **Unified adapter (`compositor::adapter`)**: `CompositorWorkspace` (`id`, `name`, `monitor`, `focused`, `is_special`, `layout`), `WorkspaceTarget` (`Id(i32)`, `Name(String)`), `CompositorClient<'a>` (`workspaces`, `switch_workspace`, `next_keyboard_layout`, `switch_keyboard_layout`).
- **Hyprland (`hyprland-ipc`)**:
  - Types: `WorkspaceId(i32)` (`new`, `get`), `WorkspaceType` (`Normal`, `Special`, `Named`), `HyprlandLayout` (`Dwindle`, `Master`, `Monocle`, `Scrolling`, `Custom(String)`, `Unknown`; `from_hyprland_str`, `as_str`, `is_known`), `WorkspaceInfo` (`id`, `address`, `workspace_type`, `name`, `monitor`, `monitor_id`, `windows`, `has_fullscreen`, `last_window`, `last_window_title`, `is_persistent`, `layout`, `focused`; `is_special`, `is_named`, `is_normal`, `special_name`, `target_spec`, `matches_identity`, `matches_ref`), `WorkspaceRuleInfo` (`workspace_string`, `enabled`, `monitor`, `default`, `persistent`, `gaps_in`, `gaps_out`, `border_size`, `border`, `rounding`, `decorate`, `shadow`, `layout`), `ClientInfo` (`address`, `title`, `class`, `initial_class`, `initial_title`, `workspace`, `monitor`, `pid`, `content_type`, `fullscreen`, `fullscreen_mode`, `floating`, `pinned`, `xwayland`, `mapped`, `hidden`, `at`, `size`, `focus_history_id`, `inhibiting_idle`, `grouped`, `tags`), `WorkspaceRefJson` (`id`, `address`, `workspace_type`, `name`; `is_empty`, `is_special`, `special_name`), `MonitorInfo` (`id`, `name`, `description`, `make`, `model`, `width`, `height`, `refresh_rate`, `x`, `y`, `scale`, `transform`, `focused`, `dpms_status`, `vrr`, `disabled`, `active_workspace`, `special_workspace`), `CursorPos`, `HyprlandVersion`, `HyprlandDevices`, `HyprlandKeyboard`, `HyprlandMouse`, `HyprlandOption`, `HyprlandOptionValue`, `HyprlandEvent` (zero-polling `poll`+`eventfd` `socket2` listener), `HyprlandSockets` (`discover`, `from_base`).
  - `Hyprland::subscribe(map)`.
  - `HyprlandClient`: `from_env`, `with_sockets`, `with_timeout`, `sockets`, `workspaces`, `active_workspace`, `workspace_rules`, `default_layout`, `set_default_layout`, `layout_message`, `active_client`, `clients`, `monitors`, `devices`, `version`, `cursor_pos`, `get_option`, `switch_workspace`, `switch_workspace_name`, `switch_workspace_relative`, `switch_workspace_previous`, `move_workspace_to_monitor`, `rename_workspace`, `focus_window`, `close_window`, `move_window_to_workspace`, `toggle_special_workspace`, `dpms_off`, `dpms_on`, `next_keyboard_layout`, `switch_keyboard_layout`, `switch_keyboard_layout_cmd`, `keyword`, `eval`, `batch`, `dispatch`, `raw_command`, `apply_workspace_event`, `stream::parse_event_line`.
- **Sway (`sway-ipc`)**:
  - Types: `WorkspaceRef<'a>`, `SwayWorkspace`, `SwayOutput`, `SwayVersion`, `SwayEvent`.
  - `Sway::subscribe(map)`.
  - `SwayClient`: `from_env`, `with_socket`, `with_timeout`, `socket_path`, `workspaces`, `outputs`, `tree`, `version`, `switch_workspace`, `next_keyboard_layout`, `switch_keyboard_layout`, `raw_command`, `encode_frame`, `stream::parse_event_frame`.
- **Control IPC (`control-ipc`)**:
  - `Signal` (`Usr1`, `Usr2`), `Control::signal(signal, msg)`, `Control::socket(socket_name_or_path, map)`, `Control::socket_requests(socket_name_or_path, map)`.
  - `ControlRequest` (`command`, `into_command`, `can_reply`, `reply`), `control::socket::{resolve_socket_path, send_command, send_command_with_reply}`.
  - Socket names resolve under `$XDG_RUNTIME_DIR` (fallback `/run/user/<euid>`; explicit paths preserved). The listener only replaces stale socket files owned by the current user after an `ECONNREFUSED` probe, reads newline-terminated commands non-blocking from same-UID peers (`4096`-byte input cap, `64` KiB reply cap, `16` pending clients with FIFO eviction and `2s` deadline), and optionally sends one-shot replies cleanly.

## D-Bus Subsystems (`dbus`)

Available when the `dbus` Cargo feature is enabled (`global_shortcuts` also exposes `GlobalShortcuts::subscribe` when `global-shortcuts` is enabled):

- **Core (`types`, `client`)**: `DbusBus` (`Session`, `System`), `DbusValue` (`String`, `ObjectPath`, `Bool`, `I32`, `U32`, `I64`, `U64`, `F64`, `Array`, `Dict`, `Struct`, `Variant`; `variant`, `unwrap_variant`, `.as_str`, `.as_object_path`, `.as_bool`, `.as_i32`, `.as_u32`, `.as_i64`, `.as_u64`, `.as_f64`, `.as_struct`, `.as_variant`), `DbusSignalEvent`, `DbusActionLane`, `DbusActionResult` (`reply`, `is_ok`, `is_err`, `to_result`), `Dbus::{subscribe_signal, action_results}`, `DbusClient` (`new`, `get_property`, `set_property_value`, `call_method_unit`, `call_method`, `call_method_blocking`, `drain_action_results`, `last_action_result`, `last_action_error`, `take_last_action_error`).
- **Notifications (`notifications`)**: `NotificationCapabilities` (`ACTION_ICONS`, `ACTIONS`, `BODY`, `BODY_HYPERLINKS`, `BODY_IMAGES`, `BODY_MARKUP`, `ICON_MULTI`, `ICON_STATIC`, `PERSISTENCE`, `SOUND`; `to_xdg_strings`), `NotificationUrgency`, `NotificationCloseReason`, `NotificationImageData`, `DesktopNotification` (full XDG v1.2 hints: `urgency`, `category`, `desktop_entry`, `image_path`, `image_data`, `action_icons`, `resident`, `transient`, `suppress_sound`, `sound_name`, `sound_file`, `position`; `Default`), `NotificationEvent`, `Dbus::{notifications, notifications_with_capabilities}`, `DbusClient::{set_notification_capabilities, notification_capabilities, close_notification, invoke_notification_action}`.
- **System Tray (`tray`)**: `TrayCategory` (`from_sni_str`), `TrayStatus` (`from_sni_str`), `TrayIconPixmap` (`to_rgba`, `best_for_size`), `TrayToolTip`, `TrayMenuItem`, `TrayItemSnapshot`, `TrayEvent`, `Dbus::tray`, `DbusClient::{tray_list_items, tray_snapshot, tray_activate, tray_secondary_activate, tray_context_menu, tray_scroll, tray_menu_layout, tray_menu_event}`.
- **MPRIS (`mpris`)**: `MprisPlaybackStatus` (`from_str_status`), `MprisLoopStatus` (`from_str_status`, `as_str`), `MprisSnapshot`, `MprisEvent` (`PlayersUpdated`, `PlayerChanged`, `PlayerAdded`, `PlayerRemoved`, `Seeked`), `Dbus::mpris`, `DbusClient::{list_mpris_players, mpris_snapshots, mpris_snapshot, mpris_play_pause, mpris_play, mpris_pause, mpris_stop, mpris_next, mpris_previous, mpris_seek, mpris_set_position, mpris_set_volume, mpris_set_shuffle, mpris_set_loop_status, mpris_raise, mpris_quit}`.
- **NetworkManager (`network_manager`)**: `NmState`, `NmConnectivity`, `NmDeviceType`, `NmWifiSecurity` (`Open`, `Wep`, `WpaPsk`, `WpaEap`, `Wpa3Sae`, `Owe`), `NmAccessPoint`, `NmSavedConnection`, `NmDeviceInfo`, `NmActiveConnection`, `NetworkManagerSnapshot`, `Dbus::network_manager`, `DbusClient::{network_manager_snapshot, nm_set_wireless_enabled, nm_set_networking_enabled, nm_scan_wifi, nm_list_wifi_access_points, nm_list_saved_connections, nm_connect_wifi, nm_connect_wifi_with_options, nm_connect_hidden_wifi, nm_connect_hidden_wifi_with_security, nm_activate_connection, nm_deactivate_connection, nm_disconnect_device, nm_forget_connection, nm_forget_wifi_ssid}`.
- **Power Profiles (`power_profiles`)**: `PowerProfileKind` (`PowerSaver`, `Balanced`, `Performance`, `Custom(String)`; `from_dbus_str`, `as_str`), `PowerProfileHold`, `PowerProfileSnapshot`, `Dbus::power_profiles`, `DbusClient::{power_profile_snapshot, set_power_profile, hold_power_profile, release_power_profile}`.
- **UPower (`upower`)**: `UPowerDeviceKind`, `UPowerDeviceState`, `UPowerDevice`, `UPowerSnapshot` (`display_device`, `peripherals`), `Dbus::upower`, `DbusClient::upower_snapshot`.
- **UDisks2 (`udisks`)**: `UDisksDrive`, `UDisksBlockDevice` (`is_mounted()`), `UDisksSnapshot` (`drive_for_block(block)`), `Dbus::udisks`, `DbusClient::{udisks_snapshot, udisks_mount, udisks_mount_with_options, udisks_unmount, udisks_unmount_with_options, udisks_eject_drive, udisks_eject_drive_with_options, udisks_power_off_drive, udisks_power_off_drive_with_options, udisks_unlock_encrypted, udisks_unlock_encrypted_with_options, udisks_lock_encrypted, udisks_lock_encrypted_with_options}`, `udisks::parse_udisks_objects`.
- **Logind (`logind`)**: `PowerActionSupport` (`Yes`, `Challenge`, `No`, `NotAvailable`; `from_logind_str`, `is_supported`, `needs_auth`), `LogindPowerCapabilities`, `LogindInhibitor`, `LogindSnapshot` (includes `power_capabilities`), `LogindEvent`, `Dbus::logind`, `DbusClient::{logind_snapshot, logind_power_capabilities, set_backlight_brightness, lock_session, unlock_session, terminate_session, suspend, hibernate, hybrid_sleep, suspend_then_hibernate, reboot, reboot_to_firmware_setup, soft_reboot, power_off, halt, inhibit}`.
- **Bluetooth (`bluetooth`)**: `BluetoothAdapter`, `BluetoothDevice`, `BluetoothSnapshot`, `BluetoothTransportFilter` (`Auto`, `BrEdr`, `Le`; `as_str`), `BluetoothAgentRequest` (`RequestPinCode`, `DisplayPinCode`, `RequestPasskey`, `DisplayPasskey`, `RequestConfirmation`, `RequestAuthorization`, `AuthorizeService`), `BluetoothAgentEvent` (`Registered`, `Request`, `Cancelled`, `Released`, `Error`), `Dbus::{bluetooth, bluetooth_agent}`, `DbusClient::{bluetooth_snapshot, set_bluetooth_powered, set_bluetooth_discoverable, set_bluetooth_discoverable_timeout, set_bluetooth_pairable, set_bluetooth_pairable_timeout, set_bluetooth_adapter_alias, set_bluetooth_discovery_filter, start_bluetooth_discovery, stop_bluetooth_discovery, connect_bluetooth_device, connect_bluetooth_device_profile, disconnect_bluetooth_device, disconnect_bluetooth_device_profile, pair_bluetooth_device, cancel_bluetooth_pairing, trust_bluetooth_device, block_bluetooth_device, set_bluetooth_device_alias, remove_bluetooth_device, bluetooth_agent_confirm, bluetooth_agent_reply_pincode, bluetooth_agent_reply_passkey, bluetooth_agent_reject}`.
- **ModemManager (`modem`)**: `ModemState`, `ModemAccessTech`, `ModemRegistrationState`, `ModemInfo`, `ModemSnapshot`, `Dbus::modem`, `DbusClient::{modem_snapshot, enable_modem}`, `decode_access_tech_bitmask`.
- **ScreenSaver & GeoClue2 (`idle_privacy`)**: `ScreenSaverSnapshot`, `GeoClueSnapshot`, `Dbus::{screensaver, geoclue}`, `DbusClient::{screensaver_active, lock_screensaver, inhibit_screensaver, uninhibit_screensaver, geoclue_in_use}`.
- **Portal Settings (`portal_settings`)**: `PortalColorScheme` (`NoPreference`, `PreferDark`, `PreferLight`; `from_portal_u32`), `PortalContrast` (`NoPreference`, `High`; `from_portal_u32`), `PortalAccentColor` (`r`, `g`, `b`; `from_rgb_f64`, `to_color`), `PortalSettingsSnapshot`, `Dbus::portal_settings`, `DbusClient::{portal_settings_snapshot, portal_setting}`.
- **Global Shortcuts (`global_shortcuts`, feature `global-shortcuts`)**: `GlobalShortcutDescriptor` (`new`, `preferred_trigger`), `BoundGlobalShortcut`, `GlobalShortcutEvent`, `GlobalShortcuts::subscribe`, `Dbus::global_shortcuts`, `DbusClient::has_global_shortcuts_portal`.
