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

If a layer-shell surface is used where `zwlr_layer_shell_v1` is absent, `App` returns `Err(WaywiError::UnsupportedCapability(Capability::LayerShell))`. Check `ctx.capabilities().has(Capability::LayerShell)` in `init` if an `xdg_toplevel` fallback is desired. Popups require `Capability::XdgShell` (`xdg_wm_base`), configure placement via `.positioner(PopupPositioner::new(anchor_rect)...)` (defaulting to `PopupAnchor::BottomLeft` / `PopupGravity::BottomRight` and using `xdg_popup.reposition` on `xdg_wm_base >= 3`), are always destroyed before their parent surfaces, and map `xdg_popup.popup_done` to `Surface::on_close` without requesting application exit.

## Scaling & Buffer Management

- **Fractional scale (`fractional-scale`)**: When `wp_fractional_scale_v1` and `wp_viewporter` exist, buffers use scaled `BufferPx` dimensions (`to_buffer_ceil(scale)`) and set `wp_viewport` destination to `LogicalSize`. Otherwise integer `wl_output` scale factors are used.
- **SHM lifecycle (`SurfaceShmPool`)**: Surfaces start single-buffered and allocate a second buffer slot only when a dirty frame arrives before `wl_buffer.release`. Hiding or removing a surface destroys pool/buffer proxies, unmaps views, and closes the backing `memfd` **without truncating it**: a compositor may still map previously committed buffers, and shrinking the file under such a mapping causes `SIGBUS` on texture upload. Each buffer incarnation carries a generation tag, so stale `wl_buffer.release` events cannot free a recycled slot. Static surfaces (or apps configured with `MemoryConfig::low_memory()`) use `BufferRetention::ReleaseAfterCommit` to run `shrink_and_unmap()` automatically once `wl_buffer.release` marks all slots idle with no redraw pending, releasing client-side SHM pools to 0 bytes while the compositor retains the committed texture. `ctx.trim_memory()` also trims idle SHM slots on retained pools.
- **Software vs GPU (`renderer-software` / `renderer-gpu`)**: `RendererPreference::Auto` prefers GPU (`wgpu` + `femtovg`) when available and falls back to CPU `wl_shm` ARGB8888 software rendering. Explicit `RendererPreference::Gpu` or `RendererPreference::Software` fails honestly with `Err(WaywiError::UnsupportedCapability(...))` when that backend is unavailable.

## Input, Clipboard & Idle

- **Input modes**: `InputMode::Full`, `InputMode::Selective` (constructs `wl_region` boxes around interactive elements), `InputMode::PassThrough`.
- **Keyboard focus**: `KeyboardInteractivity::{None, OnDemand, Exclusive}`. Keyboard key events are handled at the surface level via `Surface::on_key`.
- **Touchscreen (`wl_touch`)**: Detects `Capability::TouchInput` and delivers multi-touch `Down`/`Move`/`Up`/`Cancel` events with atomic same-frame `shape` and `orientation` to `Element::on_touch` / `Surface::on_touch` (with per-contact capture). Unclaimed primary contacts automatically fall back to pointer click/down/move/up so standard buttons and sliders work out of the box without double-firing.
- **Touchpad (`wl_pointer` + `pointer-gestures`)**: Populates `ScrollEvent::{source, stop_x, stop_y}` from `wl_pointer.axis_source` and `wl_pointer.axis_stop` (preserving zero-delta kinetic stop frames). Enabling the `pointer-gestures` Cargo feature binds `zwp_pointer_gestures_v1` (v1..=3, `Capability::PointerGestures`) for multi-finger `Swipe`, `Pinch` (scale + rotation), and `Hold` gestures via `Element::on_gesture` / `Surface::on_gesture`.
- **Pen Tablet / Stylus (`tablet`)**: Enabling the `tablet` Cargo feature binds `zwp_tablet_manager_v2` (v1..=2, `Capability::TabletInput`) for frame-atomic stylus tools (`ProximityIn`, `ProximityOut`, `Down`, `Up`, `Motion`, `Button`, `pressure`, `distance`, `tilt`, `rotation`, `slider`, `wheel`) and express-key pads (`Enter`, `Leave`, `Button`, `ModeSwitch`, `Ring`, `Strip`, `Dial`) via `Element::on_tablet` / `Surface::on_tablet` (with per-tool stroke capture). Unclaimed stylus interactions over non-tablet widgets fall back cleanly to pointer hover, click/drag, and barrel-button right/middle clicks.
- **Clipboard (`clipboard`)**: Supports `ext-data-control-v1` (preferred) and `zwlr-data-control-v1` with bounded pipe reads.
- **Idle (`idle-notify`)**: Uses `ext-idle-notify-v1` for user and input idle transitions.
- **IME (`ime`)**: Uses per-seat `zwp_text_input_manager_v3` / `zwp_text_input_v3` double-buffered composition (`preedit_string`, `commit_string`, `delete_surrounding_text`, `done`), surrounding-text sync (`<= 4000` UTF-8 bytes), cursor rectangle placement hints, and text-change cause tracking.
- **Activation (`xdg-activation-v1`)**: When the compositor advertises `xdg_activation_v1` (`Capability::XdgActivation`), `ctx.request_activation_token(...)` asynchronously requests a focus-handoff token (attaching the latest input seat/serial/surface when available) for `XDG_ACTIVATION_TOKEN` child launches (`DesktopEntry::launch_with(&LaunchOptions::new().activation_token(token))`) or `ctx.activate_surface(surface_key, token)`.
- **Keyboard layout**: Tracks XKB layout names and active group transitions via `xkbcommon` (`Capability::KeyboardLayout` is set once a keyboard and non-empty keymap layouts are active).
- **Hotplug capability preservation**: `OutputsChanged` refreshes only Wayland-protocol and seat capability bits without overwriting runtime-probed IPC, GPU, audio, or accessibility flags.
