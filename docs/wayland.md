# Wayland Backend

WayWi connects via `smithay-client-toolkit` and `wayland-client` driven by a `calloop` event loop (`calloop-wayland-source`).

## Surfaces & Protocol Errors

| Constructor | Protocol | Buffer Sizing | Use Case |
|---|---|---|---|
| `Surface::floating(key, output)` | `zwlr_layer_surface_v1` | Widget bounds | OSDs, popups, tooltips |
| `Surface::panel(key, output)` | `zwlr_layer_surface_v1` | Anchored bar bounds | Status bars, docks |
| `Surface::overlay(key, output)` | `zwlr_layer_surface_v1` | Output bounds | Launchers, full-screen overlays |
| `Surface::window(key)` | `xdg_toplevel` | Window bounds | Compositors without layer-shell |
| `Surface::popup(key, parent)` | `xdg_popup` (+ `zwlr_layer_surface_v1.get_popup` when parent is layer-shell) | Positioner bounds | Context menus, anchored popovers, nested submenus |

If a layer-shell surface is used where `zwlr_layer_shell_v1` is absent, `App` returns `Err(WaywiError::UnsupportedCapability(Capability::LayerShell))`. Check `ctx.capabilities().has(Capability::LayerShell)` in `init` if an `xdg_toplevel` fallback is desired. Popups require `Capability::XdgShell` (`xdg_wm_base`), use `xdg_positioner` for anchor/gravity/offset/constraint adjustments (and `xdg_popup.reposition` on `xdg_wm_base >= 3`), are always destroyed before their parent surfaces, and map `xdg_popup.popup_done` to `Surface::on_close` without requesting application exit.

## Scaling & Buffer Management

- **Fractional scale (`fractional-scale`)**: When `wp_fractional_scale_v1` and `wp_viewporter` exist, buffers use scaled `BufferPx` dimensions (`to_buffer_ceil(scale)`) and set `wp_viewport` destination to `LogicalSize`. Otherwise integer `wl_output` scale factors are used.
- **SHM lifecycle (`SurfaceShmPool`)**: Surfaces start single-buffered and allocate a second buffer slot only when a dirty frame arrives before `wl_buffer.release`. Hiding or removing a surface destroys pool/buffer proxies, unmaps views, and closes the backing `memfd` **without truncating it**: a compositor may still map previously committed buffers, and shrinking the file under such a mapping causes `SIGBUS` on texture upload. Each buffer incarnation carries a generation tag, so stale `wl_buffer.release` events cannot free a recycled slot. Static surfaces (such as `Layer::Background` wallpapers) can opt into `Surface::buffer_retention(BufferRetention::ReleaseAfterCommit)` to run `shrink_and_unmap()` automatically once `wl_buffer.release` marks all slots idle with no redraw pending, releasing client-side SHM pools to 0 bytes while the compositor retains the committed texture.
- **Software vs GPU (`renderer-software` / `renderer-gpu`)**: Software rendering rasterizes dirty `BufferRect` regions into `wl_shm` ARGB8888 buffers. GPU mode (`RendererPreference::Gpu`) uses `wgpu` + `femtovg` on native `wl_surface` handles with fallback to software SHM if GPU setup fails.

## Input, Clipboard & Idle

- **Input modes**: `InputMode::Full`, `InputMode::Selective` (constructs `wl_region` boxes around interactive elements), `InputMode::PassThrough`.
- **Keyboard focus**: `KeyboardInteractivity::{None, OnDemand, Exclusive}`.
- **Clipboard (`clipboard`)**: Supports `ext-data-control-v1` (preferred) and `zwlr-data-control-v1` with bounded pipe reads.
- **Idle (`idle-notify`)**: Uses `ext-idle-notify-v1` for user and input idle transitions.
- **IME (`ime`)**: Uses per-seat `zwp_text_input_manager_v3` / `zwp_text_input_v3` double-buffered composition (`preedit_string`, `commit_string`, `delete_surrounding_text`, `done`), surrounding-text sync (`<= 4000` UTF-8 bytes), cursor rectangle placement hints, and text-change cause tracking.
- **Activation (`xdg-activation-v1`)**: When the compositor advertises `xdg_activation_v1` (`Capability::XdgActivation`), `ctx.request_activation_token(...)` asynchronously requests a focus-handoff token (attaching the latest input seat/serial/surface when available) for `XDG_ACTIVATION_TOKEN` child launches (`DesktopEntry::launch_with_token`, `spawn_detached_with_env`) or `ctx.activate_surface(surface_key, token)`.
- **Keyboard layout**: Tracks XKB layout names and active group transitions via `xkbcommon`.
