# Integrations & Cargo Features

Optional Linux subsystems are selected at compile time via Cargo features. Missing runtime services or sockets report `Capability` status or `WaywiError::UnsupportedCapability`.

## Cargo Features

| Feature | Default | Dependencies | Scope |
|---|---:|---|---|
| `renderer-software` | Yes | `tiny-skia` | CPU 2D software renderer and pixel blitter |
| `renderer-gpu` | No | `femtovg`, `wgpu` | GPU renderer via `wgpu`/`femtovg` with SHM fallback |
| `text` | Yes | `cosmic-text`, `fontdb` | Text shaping, layout, and system font fallback |
| `image-png` | Yes | `png` | PNG decoder |
| `image-svg` | No | `resvg` | SVG rasterizer |
| `image-gif` | No | `gif` | GIF decoder |
| `image-webp` | No | `image-webp` | Static and animated WebP decoder |
| `animation` | Yes | - | Frame-clock `SpriteSheet` / `Sprite` playback |
| `fractional-scale` | No | `wayland-protocols` | `wp_fractional_scale_v1` + `wp_viewporter` scaling |
| `clipboard` | No | `wayland-protocols`, `wayland-protocols-wlr` | `ext-data-control-v1` / `zwlr-data-control-v1` clipboard |
| `idle-notify` | No | `wayland-protocols` | `ext-idle-notify-v1` user/input idle monitor |
| `ime` | No | `wayland-protocols` | `zwp_text_input_manager_v3` / `zwp_text_input_v3` IME composition support |
| `control-ipc` | No | `calloop/signals` | Unix control socket (under `$XDG_RUNTIME_DIR`, falling back to `/run/user/<uid>`; same-user peers only) and `SIGUSR1`/`SIGUSR2` |
| `hyprland-ipc` | No | `serde`, `serde_json` | Hyprland command socket and `socket2` event stream |
| `sway-ipc` | No | `serde`, `serde_json` | Sway `i3-ipc` binary client and event stream |
| `dbus` | No | `zbus`, `futures-lite`, `serde` | Notifications, StatusNotifier tray host, MPRIS, NetworkManager, PowerProfiles, UPower, logind, BlueZ, ModemManager1, ScreenSaver, GeoClue2, UDisks2, XDG Portal Settings |
| `global-shortcuts` | No | `zbus`, `futures-lite` | `org.freedesktop.portal.GlobalShortcuts` (implies `dbus`) |
| `accessibility` | No | `accesskit`, `accesskit_unix` | AT-SPI2 tree publishing and action dispatch |
| `pipewire` | No | `pipewire` | PipeWire audio mixer, SFX output, and privacy graph monitor |
| `pulseaudio` | No | `libpulse-binding` | PulseAudio mixer and SFX output |
| `alsa` | No | `alsa` | ALSA mixer and SFX output |
| `audio-playback` | No | `rodio` | `rodio` / `symphonia` audio decoding (`OGG`, `MP3`, `FLAC`, `WAV`) |

## Subsystem Notes

- **Compositors (`hyprland-ipc`, `sway-ipc`)**: Direct IPC bindings (`Hyprland`, `Sway`) plus the unified `CompositorClient` (`ctx.compositor()?`) for workspace and layout switching. Hyprland v0.55+/v0.57 Lua dispatchers (`hl.dsp.*`, `eval` config writes) are supported: the client detects `configProvider: lua` via `j/systeminfo` and falls back from string-based IPC automatically.
- **Audio & Playback (`pipewire`, `pulseaudio`, `alsa`, `audio-playback`)**: `AudioClient` probes `PipeWire -> PulseAudio -> ALSA` and exposes sink/source volume (preserving per-channel stereo L/R balance across master volume changes, including across `0.0`), explicit stereo balance and per-channel volume controls (`set_balance`, `set_channel_volumes`, `set_source_balance`, `set_source_channel_volumes`, `set_stream_balance`, `set_stream_channel_volumes`, `set_capture_balance`, `set_capture_channel_volumes`), mute, device selection, switchable output/input port enumeration and manual selection (`AudioPort`, `AudioPortAvailability`, `select_port`, `select_device_port`, `select_source_port`, `select_source_device_port`), sound card profile enumeration and manual selection (`AudioCard`, `AudioProfile`, `select_card_profile`), per-app playback (sink input / `Stream/Output/Audio`) and capture (source output / `Stream/Input/Audio`) stream controls, and hardware route updates — including microphone (default source) volume, mute, balance, and selection in every backend. The PipeWire backend tracks `Device::info` (`DeviceChangeMask::PARAMS` / `SPA_PARAM_INFO_SERIAL`) for `ParamType::Route`, `ParamType::EnumRoute`, `ParamType::Profile`, and `ParamType::EnumProfile` (pruning stale batches by async `seq`) and encodes inner `SPA_PARAM_ROUTE_props` with `ParamType::Route`. The ALSA backend maps source controls to the hardware capture element (supporting stereo `FrontLeft`/`FrontRight` balance) and has no per-app stream or port/profile routing. `AudioPlayer` supports WAV, PCM, synthesized presets (`SoundPreset`), XDG Sound Theme lookup, and optional `rodio` decoding.
- **D-Bus (`dbus`, `global-shortcuts`)**: Dedicated `zbus` background workers bridged into `calloop` for session/system queries and subscriptions, `org.freedesktop.Notifications` (full XDG v1.2 hints and configurable `NotificationCapabilities`), `StatusNotifierItem` + `dbusmenu` tray hosting, MPRIS media player streams and controls, NetworkManager Wi-Fi connection management, `systemd-logind` session/power actions and `Can*` capability queries, BlueZ device controls and `Agent1` pairing, UDisks2 storage (snapshot, mount/unmount, eject/power-off, encrypted unlock/lock with Polkit `auth.no_user_interaction`), `org.freedesktop.portal.Settings` appearance queries & live `SettingChanged` signals (color-scheme, contrast, accent-color, raw settings — exposed purely as data without imposing any UI theme), and XDG portal global shortcuts.
- **Control IPC (`control-ipc`)**: `Control::signal`, `Control::socket`, and `Control::socket_requests` with `waywi::control::send_command(...)` and `waywi::control::send_command_with_reply(...)` for single-instance remote triggers and optional one-shot replies. Control socket clients are read non-blocking inside `calloop` until `EOF` (dispatching complete `\n`-terminated lines immediately) with per-connection byte limits (`4096` bytes), FIFO eviction above `16` pending clients, and a `2s` connection deadline so stalled local peers never block the event loop.
- **Brightness & Privacy permissions**:
  - Backlight writes use `systemd-logind` (`dbus`) or `/sys/class/backlight/*` (`video` group).
  - External monitor DDC/CI (`/dev/i2c-*`) requires read/write access (`i2c` group).
  - Privacy capture fallback inspects `/proc/<pid>/fd` for the current user alongside PipeWire and GeoClue2.
