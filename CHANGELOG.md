# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- Optional allocator features: `allocator-mimalloc` (`mimalloc` 0.1.x) and `allocator-jemalloc` (`tikv-jemallocator` 0.7). Both are opt-in, non-default, and mutually exclusive (guarded with `compile_error!`); they select the process-wide global allocator for the final binary while the per-frame bump arena stays unchanged on top of either choice.

### Fixed
- Fixed compositor crashes (`SIGBUS` / `BUS_ADRERR` during buffer commits, observed on Hyprland) by keeping SHM backing `memfd`s valid while the compositor may still map previously committed buffers: hiding or removing a surface now destroys pool/buffer proxies and closes the `memfd` without truncating it.
- Stale `wl_buffer.release` events can no longer free a recycled buffer slot after a resize (buffer incarnations now carry generational tags); `BufferReleased` is only emitted when a slot was actually freed.
- Wayland startup roundtrips now propagate disconnect errors instead of silently continuing with an empty state.

### Changed
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
