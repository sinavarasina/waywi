# Wayland Backend

WayWi connects via `smithay-client-toolkit` and `wayland-client` driven by a `calloop` event loop (`calloop-wayland-source`).

## Surfaces & Protocol Errors

| Constructor | Protocol | Buffer Sizing | Use Case |
|---|---|---|---|
| `Surface::floating(key, output)` | `zwlr_layer_surface_v1` | Widget bounds | OSDs, popups, tooltips |
| `Surface::panel(key, output)` | `zwlr_layer_surface_v1` | Anchored bar bounds | Status bars, docks |
| `Surface::overlay(key, output)` | `zwlr_layer_surface_v1` | Output bounds | Launchers, full-screen overlays |
| `Surface::window(key)` | `xdg_toplevel` | Window bounds | Compositors without layer-shell |

If a layer-shell surface is used where `zwlr_layer_shell_v1` is absent, `App` returns `Err(WaywiError::UnsupportedCapability(Capability::LayerShell))`. Check `ctx.capabilities().has(Capability::LayerShell)` in `init` if an `xdg_toplevel` fallback is desired.

## Scaling & Buffer Management

- **Fractional scale (`fractional-scale`)**: When `wp_fractional_scale_v1` and `wp_viewporter` exist, buffers use scaled `BufferPx` dimensions (`to_buffer_ceil(scale)`) and set `wp_viewport` destination to `LogicalSize`. Otherwise integer `wl_output` scale factors are used.
- **SHM lifecycle (`SurfaceShmPool`)**: Surfaces start single-buffered and allocate a second buffer slot only when a dirty frame arrives before `wl_buffer.release`. Hiding or removing a surface unmaps views, truncates the backing `memfd` (`ftruncate(fd, 0)`), and destroys pool/buffer proxies.
- **Software vs GPU (`renderer-software` / `renderer-gpu`)**: Software rendering rasterizes dirty `BufferRect` regions into `wl_shm` ARGB8888 buffers. GPU mode (`RendererPreference::Gpu`) uses `wgpu` + `femtovg` on native `wl_surface` handles with fallback to software SHM if GPU setup fails.

## Input, Clipboard & Idle

- **Input modes**: `InputMode::Full`, `InputMode::Selective` (constructs `wl_region` boxes around interactive elements), `InputMode::PassThrough`.
- **Keyboard focus**: `KeyboardInteractivity::{None, OnDemand, Exclusive}`.
- **Clipboard (`clipboard`)**: Supports `ext-data-control-v1` (preferred) and `zwlr-data-control-v1` with bounded pipe reads.
- **Idle (`idle-notify`)**: Uses `ext-idle-notify-v1` for user and input idle transitions.
- **Keyboard layout**: Tracks XKB layout names and active group transitions via `xkbcommon`.
