# Changelog

All notable changes to this project will be documented in this file.

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
