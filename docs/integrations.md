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
| `control-ipc` | No | `calloop/signals` | Unix control socket (under `$XDG_RUNTIME_DIR`, falling back to `/run/user/<uid>`; same-user peers only) and `SIGUSR1`/`SIGUSR2` |
| `hyprland-ipc` | No | `serde`, `serde_json` | Hyprland command socket and `socket2` event stream |
| `sway-ipc` | No | `serde`, `serde_json` | Sway `i3-ipc` binary client and event stream |
| `dbus` | No | `zbus`, `futures-lite` | Notifications, StatusNotifier tray host, MPRIS, NetworkManager, PowerProfiles, UPower, logind, BlueZ, ModemManager1, ScreenSaver, GeoClue2 |
| `global-shortcuts` | No | `zbus`, `futures-lite` | `org.freedesktop.portal.GlobalShortcuts` (implies `dbus`) |
| `accessibility` | No | `accesskit`, `accesskit_unix` | AT-SPI2 tree publishing and action dispatch |
| `pipewire` | No | `pipewire` | PipeWire audio mixer, SFX output, and privacy graph monitor |
| `pulseaudio` | No | `libpulse-binding` | PulseAudio mixer and SFX output |
| `alsa` | No | `alsa` | ALSA mixer and SFX output |
| `audio-playback` | No | `rodio` | `rodio` / `symphonia` audio decoding (`OGG`, `MP3`, `FLAC`, `WAV`) |

## Subsystem Notes

- **Compositors (`hyprland-ipc`, `sway-ipc`)**: Direct IPC bindings (`Hyprland`, `Sway`) plus the unified `CompositorClient` (`ctx.compositor()?`) for workspace and layout switching.
- **Audio & Playback (`pipewire`, `pulseaudio`, `alsa`, `audio-playback`)**: `AudioClient` probes `PipeWire -> PulseAudio -> ALSA` and exposes sink/source volume, mute, device selection, per-app playback (sink input / `Stream/Output/Audio`) and capture (source output / `Stream/Input/Audio`) stream volume and mute, and hardware route updates. The ALSA fallback has no per-app stream control (hardware mixer only). `AudioPlayer` supports WAV, PCM, synthesized presets (`SoundPreset`), XDG Sound Theme lookup, and optional `rodio` decoding.
- **D-Bus (`dbus`, `global-shortcuts`)**: Dedicated `zbus` background workers bridged into `calloop` for session/system queries and subscriptions, `org.freedesktop.Notifications`, `StatusNotifierItem` + `dbusmenu` tray hosting, MPRIS media player streams and controls, NetworkManager Wi-Fi connection management, BlueZ device controls and `Agent1` pairing, and XDG portal global shortcuts.
- **Control IPC (`control-ipc`)**: `Control::signal` and `Control::socket` with `waywi::control::send_command(...)` for single-instance remote triggers.
- **Brightness & Privacy permissions**:
  - Backlight writes use `systemd-logind` (`dbus`) or `/sys/class/backlight/*` (`video` group).
  - External monitor DDC/CI (`/dev/i2c-*`) requires read/write access (`i2c` group).
  - Privacy capture fallback inspects `/proc/<pid>/fd` for the current user alongside PipeWire and GeoClue2.
