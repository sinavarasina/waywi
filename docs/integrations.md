# Integrations & Cargo Features

Optional Linux subsystems are selected at compile time via Cargo features. Missing runtime services or sockets report `Capability` status or `WaywiError::UnsupportedCapability`.

## Cargo Features

| Feature | Default | Dependencies | Scope |
|---|---:|---|---|
| `renderer-software` | Yes | `tiny-skia` | CPU 2D software renderer and pixel blitter |
| `renderer-gpu` | No | `femtovg`, `wgpu` | GPU renderer via `wgpu`/`femtovg` with SHM fallback on `Auto` |
| `text` | Yes | `cosmic-text`, `fontdb` | Text shaping, layout, and optional system font fallback |
| `image-png` | Yes | `png` | PNG decoder |
| `image-svg` | No | `resvg` | SVG rasterizer |
| `image-gif` | No | `gif` | Static first-frame GIF decoder |
| `image-webp` | No | `image-webp` | Static first-frame WebP decoder |
| `animation` | Yes | - | Pure-mechanism `Easing`/`Lerp`/`Spring`/`Tween`/`Animated`, `Animation::frames` VSync ticks, retained transitions, and `SpriteSheet`/`Sprite` playback |
| `fractional-scale` | No | `wayland-protocols` | `wp_fractional_scale_v1` + `wp_viewporter` scaling |
| `clipboard` | No | `wayland-protocols`, `wayland-protocols-wlr` | `ext-data-control-v1` / `zwlr-data-control-v1` clipboard (zero-thread `calloop` pipe I/O) |
| `idle-notify` | No | `wayland-protocols` | `ext-idle-notify-v1` user/input idle monitor |
| `ime` | No | `wayland-protocols` | `zwp_text_input_manager_v3` / `zwp_text_input_v3` IME composition support |
| `pointer-gestures` | No | `wayland-protocols` | `zwp_pointer_gestures_v1` multi-finger touchpad swipe, pinch, and hold gestures |
| `tablet` | No | `wayland-protocols` | `zwp_tablet_manager_v2` pen stylus axes and express-key pad input |
| `cursor-shape` | No | `wayland-protocols` | `wp_cursor_shape_v1` server-side cursor shapes (`Element::cursor` / `Surface::cursor`) |
| `session-lock` | No | `wayland-protocols` | `ext_session_lock_v1` native multi-output Wayland lockscreen surfaces |
| `screencopy` | No | `wayland-protocols`, `wayland-protocols-wlr` | `ext_image_copy_capture_v1` + `zwlr_screencopy_manager_v1` async screen/region pixel capture |
| `foreign-toplevel` | No | `wayland-protocols`, `wayland-protocols-wlr` | `zwlr_foreign_toplevel_management_v1` + `ext_foreign_toplevel_list_v1` window listing & control |
| `ext-workspace` | No | `wayland-protocols` | `ext_workspace_v1` cross-compositor workspace snapshots and batched mutations |
| `idle-inhibit` | No | `wayland-protocols` | `zwp_idle_inhibit_manager_v1` declarative per-surface idle inhibition |
| `virtual-keyboard` | No | `wayland-protocols-misc` | `zwp_virtual_keyboard_v1` synthetic key/modifier injection and XKB keymap upload |
| `pointer-constraints` | No | `wayland-protocols` | `zwp_pointer_constraints_v1` lock/confine and `zwp_relative_pointer_v1` relative motion |
| `control-ipc` | No | `calloop/signals` | Unix control socket (under `$XDG_RUNTIME_DIR`, falling back to `/run/user/<uid>`; same-user peers only) and `SIGUSR1`/`SIGUSR2` |
| `hyprland-ipc` | No | `serde`, `serde_json` | Hyprland command socket and `socket2` event stream (`waywi::compositor::hyprland`) |
| `sway-ipc` | No | `serde`, `serde_json` | Sway `i3-ipc` binary client and event stream (`waywi::compositor::sway`) |
| `dbus` | No | `zbus`, `futures-lite`, `serde` | Async `zbus` workers and 28 domain clients (Notifications, Tray, MPRIS, NetworkManager, PowerProfiles, UPower, logind, BlueZ, ModemManager1, ScreenSaver, GeoClue2, UDisks2, XDG Portals, Polkit, Sensors, systemd, timedate1, hostname1, locale1, AccountsService, Fprintd, GameMode, Secrets, fwupd, PackageKit, CUPS, KDE Connect, Colord, Avahi) |
| `global-shortcuts` | No | `zbus`, `futures-lite` | `org.freedesktop.portal.GlobalShortcuts` via `Dbus::global_shortcuts` (implies `dbus`) |
| `accessibility` | No | `accesskit`, `accesskit_unix` | On-demand AT-SPI2 tree publishing and action dispatch |
| `pipewire` | No | `pipewire` | PipeWire audio mixer, SFX output, and privacy graph monitor |
| `pulseaudio` | No | `libpulse-binding` | PulseAudio mixer and SFX output |
| `alsa` | No | `alsa` | ALSA mixer and SFX output |
| `audio-playback` | No | `rodio` | `rodio` / `symphonia` audio decoding (`OGG`, `MP3`, `FLAC`, `WAV`) |

## Subsystem Notes

- **Compositors (`hyprland-ipc`, `sway-ipc`)**: Direct IPC bindings live under `waywi::compositor::{hyprland, sway}` alongside the unified `CompositorClient` enum (`ctx.compositor()?`), `Compositor::subscribe` (`CompositorEvent`), and unified `WorkspaceTarget<'a>` (`Id`, `Name`, `Relative`, `Previous`) handling Hyprland v0.57+ special/named workspaces (`id <= 0`), Lua dispatchers (`hl.dsp.*`, `eval`), and Sway workspaces. Both Hyprland and Sway event streams run over a shared non-blocking `StreamWorkerHandle` with instant cancellation on drop.
- **Audio & Playback (`pipewire`, `pulseaudio`, `alsa`, `audio-playback`)**: `AudioClient` probes `PipeWire -> PulseAudio -> ALSA` with runtime fallback across compiled-in backends and exposes a target-based mixer API (`AudioTarget`, `AudioPortTarget`, `AudioDirection`, `AudioVolume`, unified `AudioStream`, `Arc<AudioSnapshot>`). Volume scaling preserves per-channel stereo and surround L/R balance (including across `0.0`), spatial channel maps (`AudioChannelPosition`), manual port/route switching, and card profile switching. `AudioClient` uses lock-free `AtomicBool` pending notifications and tears down its worker cleanly via `stop_if_idle()` when all audio subscriptions stop. `AudioPlayer` decouples format decoding (`WAV` natively; `OGG`/`.oga`/`MP3`/`FLAC` via `rodio::Decoder` when `audio-playback` is enabled) from native `PipeWire -> PulseAudio -> ALSA -> Rodio` output selection, deduplicates files by content hash under an LRU cache limit (`MemoryConfig::sound_cache_max_bytes`, `unload`, `clear_cache`), plays `Arc<[f32]>` without copying, releases idle ALSA `PCM` handles after 2s, drains PulseAudio playback latency cleanly before corking, and tags PipeWire/PulseAudio streams with `media.role = "Event"` corked when idle.
- **D-Bus (`dbus`, `global-shortcuts`)**: All D-Bus queries and workers share async `zbus::Connection` handles (no `zbus::blocking::Connection`). `DbusClient` exposes 28 lightweight `Copy` domain clients (`.mpris()`, `.nm()`, `.bluetooth()`, `.udisks()`, `.logind()`, `.tray()`, `.notifications()`, `.power_profiles()`, `.upower()`, `.modem()`, `.portal()`, `.screensaver()`, `.geoclue()`, `.sensors()`, `.polkit()`, `.systemd()`, `.timedate()`, `.hostname()`, `.locale()`, `.accounts()`, `.fprint()`, `.gamemode()`, `.secrets()`, `.fwupd()`, `.packagekit()`, `.kdeconnect()`, `.colord()`, `.avahi()`), typed `DbusActionReply` via `Dbus::action_results`, targeted match rules on `TrayWorker`, burst coalescing in `ServiceWatchWorker`, and configurable snapshot caching / idle disconnect via `MemoryConfig` and `DbusClient::trim_cache()`.
- **Control IPC (`control-ipc`)**: `Control::signal`, `Control::socket`, and `Control::default_socket` deliver `ControlRequest` (`Clone + Send + Sync + Deref<Target = str>`) directly to `Application::update`, paired with `waywi::control::{send_command, send_command_with_reply}`. Multiple distinct socket paths are supported concurrently, sockets are bound only while subscribed, clients are read non-blocking inside `calloop` (`4096`-byte input cap, `64` KiB reply cap, `16` pending clients with FIFO eviction, `2s` deadline), and slow-client reply writer tokens are tracked and cleaned up on removal.
- **Brightness & Privacy permissions**:
  - `waywi::system::hardware::set_backlight_percent` writes `/sys/class/backlight/*` directly (`video` group). `EffectCtx::set_backlight_percent` tries direct sysfs first and falls back to the shared non-blocking `systemd-logind` D-Bus client (`dbus` feature).
  - External monitor DDC/CI (`/dev/i2c-*`) requires read/write access (`i2c` group); `DdcWorker` stops cleanly when brightness subscriptions end.
  - `sample_privacy_oneshot()` / `ctx.system().sample_privacy()` performs a stateless `/proc/<pid>/fd` scan with zero background threads, while `System::privacy()` subscriptions run live PipeWire and cancellable GeoClue2 monitors alongside `/proc/<pid>/fd`.
