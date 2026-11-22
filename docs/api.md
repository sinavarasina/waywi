# API Overview

Core types are re-exported from `waywi::prelude::*`.

## Application & Effects

Implement `Application`:

- `init(ctx: &mut EffectCtx<'_>) -> Result<Self::State>`: runs after Wayland connection and `Capabilities` probing.
- `update(state: &mut Self::State, msg: Self::Message, ctx: &mut EffectCtx<'_>) -> Result<()>`: mutates state and triggers imperative actions (`ctx.quit()`, `ctx.after(...)`, asset loading, `ctx.system()`, `ctx.audio()`, `ctx.hyprland()`, `ctx.sway()`, `ctx.compositor()`, `ctx.dbus()`).
- `view(state: &Self::State) -> Surfaces<Self::Message>`: returns surface definitions for the current state.
- `subscriptions(state: &Self::State) -> Subscriptions<Self::Message>`: lists active timers, system samplers, and protocol/IPC streams. Unlisted keys stop automatically.

## Geometry & Units

- `LogicalPx(f32)` (`.logical_px()`): surface-local logical coordinates.
- `BufferPx(i32)`: physical buffer coordinates derived via `ScaleFactor` (`1/120` fixed-point units matching `wp_fractional_scale_v1`).
- `Percent(f32)` (`.percent()`, `.of(...)`): percentage lengths and offsets.
- `Size`: `Size::px(w, h)`, `Size::new(width, height)`, `Size::fill()`, `Size::auto()`.
- `Position`: `Auto`, `absolute(x, y)`, `relative_to_surface(px, py)`, `relative_to_parent(px, py)` with `.with_offset(dx, dy)` and `ElementAnchor`.
- `Transform`: `identity()`, `translate(dx, dy)`, `scale(s)`, `scale_xy(sx, sy)`, `rotate_deg(deg)`, `rotate_rad(rad)`, `pivot(ElementAnchor)`, `pivot_point(px, py)`.

## Surfaces & Elements

- Surfaces: `Surface::floating` (widget bounds), `Surface::panel` (edge-anchored with `.exclusive_zone(...)`), `Surface::overlay` (full-output layer), `Surface::window` (`xdg_toplevel`), `Surface::memo`, with `.buffer_retention(BufferRetention::Retain | ReleaseAfterCommit)` for static surfaces.
- Containers & primitives: `Row`, `Column`, `Group`, `Clip`, `Rect`, `Text` (with `.bounds_mode(TextBoundsMode::Typographic | Optical)` / `.optical_bounds(...)`), `Image` (with `.source_rect`, `.region`, `.flip_x`, `.flip_y`, `.filter`), `Sprite` (with `SpriteSheet` grids/regions, timed/sequence/range frames, `.on_animation_end`), `Canvas`, `Element::memo`.
- Event hooks: `.on_click(...)`, `.on_hover(...)`, `.on_leave(...)`, `.on_scroll(...)`, `.on_pointer_down(...)`, `.on_pointer_up(...)`, `.on_pointer_move(...)`, `.on_key(...)`, `.ime(...)` / `.on_ime(...)`, plus optional `.on_click_sound(...)` / `.on_hover_sound(...)`, and headless `TextInputState` / `ScrollState` controllers (pairable with `ctx.measure_text_cursor(...)` for proportional cursor placement).
- Accessibility hooks: `.accessible_role(...)`, `.accessible_label(...)`, `.accessible_description(...)`, `.accessible_value(...)`, `.accessible_hidden(...)`.
- Composition: components use `.map(...)` on elements, surfaces, and subscriptions to lift child messages.

## Assets, Timers & System Telemetry

- Assets: `ctx.load_image(path)`, `ctx.load_image_sized(path, min_svg_px)`, `ctx.load_image_bytes(bytes)`, `ctx.load_image_bytes_sized(bytes, min_svg_px)`, `ctx.load_rgba_image(...)`, `ctx.load_image_async(...)`, `ctx.set_asset_retention(asset, AssetRetention::KeepDecoded | ReclaimAfterPresent)`, `ctx.asset_retention(asset)`, `ctx.evict_asset_pixels(asset)`, `ctx.reload_asset(...)`, `ctx.unload_asset(...)`.
- Timers: `Timer::once(key, duration, msg)` (resets on key change) and `Timer::every(key, interval, msg)`.
- System telemetry: `System::{cpu, memory, time, gpu, disk, network, battery, sensors, uptime, brightness, privacy}().every(...)` subscriptions and direct `ctx.system()` queries, plus formatted units (`ByteSize`, `DataRate`, `RateUnit`, `FreqUnit`, `LocalDateTime`, `CalendarMonth`).
