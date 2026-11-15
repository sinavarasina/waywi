# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-11-15

### Added
- Wayland layer-shell, XDG popup/window, and headless runtime (`App`, `Application`, `HeadlessRuntime`, `EffectCtx`).
- Software 2D renderer (`tiny-skia`) and optional GPU renderer (`femtovg` / `wgpu`) with shared text shaping (`cosmic-text`).
- Element tree, layout, hit-testing, input events, animations, timers, and `AccessKit` accessibility tree support.
- System telemetry providers (`cpu`, `memory`, `time`, `gpu`, `disk`, `network`, `battery`, `sensors`, `uptime`, `brightness`, `privacy`) and unit helpers.
- Desktop entry discovery/launching, XDG icon lookup, clipboard, idle notifications, compositor IPC (`hyprland-ipc`, `sway-ipc`), control IPC, audio backends (`pipewire`, `pulseaudio`, `alsa`, `audio-playback`), and D-Bus integrations (`dbus`, `global-shortcuts`).
