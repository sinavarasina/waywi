# API Overview

Core types are re-exported from `waywi::prelude::*`.

## Application & Effects

Implement `Application`:

- `init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State>`: runs after Wayland connection and `Capabilities` probing.
- `update(state: &mut Self::State, msg: Self::Message, ctx: &mut EffectCtx<'_, Self::Message>) -> Result<()>`: mutates state and triggers imperative actions (`ctx.quit()`, `ctx.trim_memory()`, `ctx.after(delay, msg)`, `ctx.request_activation_token(...)`, `ctx.activate_surface(...)`, image loading, `ctx.system()`, `ctx.audio()`, `ctx.player()`, `ctx.hyprland()`, `ctx.sway()`, `ctx.compositor()`, `ctx.dbus()`).
- `view(state: &Self::State) -> Surfaces<Self::Message>`: returns surface definitions for the current state (or `Surfaces::none()`).
- `subscriptions(state: &Self::State) -> Subscriptions<Self::Message>`: lists active timers, system samplers, and protocol/IPC streams. Unlisted keys stop automatically and release idle workers.

Configure buffer retention and cache limits with `App::new().memory(MemoryConfig::balanced() | MemoryConfig::low_memory() | MemoryConfig::high_throughput())` (see [Memory Configuration](performance.md#memory-configuration) for preset behavior and drawbacks), and call `ctx.trim_memory()` to reclaim idle caches immediately.

## Geometry & Units

- `LogicalPx(f32)` (`.logical_px()`): surface-local logical coordinates.
- `BufferPx(i32)`: physical buffer coordinates derived via `ScaleFactor` (`1/120` fixed-point units matching `wp_fractional_scale_v1`).
- `Percent(f32)` (`.percent()`, `.of(...)`): percentage lengths and offsets.
- `Bounds`: logical axis-aligned rectangle (`Bounds::new`, `Bounds::from_xywh`).
- `Size`: `Size::new(width, height)`, `Size::fill()`, `Size::auto()`, plus `From<(W, H)>` and `From<LogicalSize>` (so `.size((240.logical_px(), 64.logical_px()))` works directly).
- `Edges`: `Edges::all`, `Edges::symmetric`, `From<LogicalPx>`, `From<(LogicalPx, LogicalPx)>`.
- `Position`: `Auto`, `absolute(x, y)`, `relative_to_surface(px, py)`, `relative_to_parent(px, py)` with `.with_offset(dx, dy)` and `ElementAnchor`.
- `Transform`: `identity()`, `translate(dx, dy)`, `scale(s)`, `scale_xy(sx, sy)`, `rotate_deg(deg)`, `rotate_rad(rad)`, `pivot(px, py)`, `pivot_point(Point)`.

## Surfaces & Elements

- Surfaces: `Surface::floating` (widget bounds), `Surface::panel` (edge-anchored with `.exclusive_zone(...)`), `Surface::overlay` (full-output layer), `Surface::window` (`xdg_toplevel` with `.decoration(WindowDecoration)`), `Surface::popup` (`xdg_popup` + `.positioner(PopupPositioner::new(anchor_rect)...)`), `Surface::session_lock` (`ext_session_lock_v1`), `Surface::memo`, with optional `.buffer_retention(...)`, `.cursor(CursorShape)`, `.inhibit_idle(bool)`, `.pointer_constraint(PointerConstraintMode)`, `.dnd_mime_types(...)` / `.on_dnd(...)`, and `.on_relative_pointer(...)`.
- Containers & primitives: `Row`, `Column`, `Group`, `Clip`, `Rect`, `Text` (with `.bounds_mode(TextBoundsMode::Typographic | Optical)`), `Image` (with `.region(impl Into<SourceRect>)`, `.flip_x`, `.flip_y`, `.filter`), `Sprite` (with `SpriteSheet::new`/`grid`/`from_regions`, `.frame_size(u32, u32)`, `.on_animation_end`), `Canvas::new(size, draw)` (drawn directly each frame by default; pass `.cache_key(...)` to cache the rendered bitmap), `Element::memo`.
- Animation & retained transitions (`animation`): pure-mechanism `Easing`, `Lerp`, `SpringConfig`, `Spring`, `Tween<T>`, `Animated<T>`, VSync-paced `Animation::frames(key, |now| ...)` synchronized to `wl_surface.frame`, and opt-in retained element transitions `.transition_opacity(TransitionSpec)` / `.transition_transform(TransitionSpec)` evaluated per-surface without rebuilding `view(state)` (elements require `.key(...)` and start directly at their initial declared value on mount).
- Event hooks: `.on_click(...)`, `.on_hover(...)`, `.on_leave(...)`, `.on_scroll(...)`, `.on_pointer_down(...)`, `.on_pointer_up(...)`, `.on_pointer_move(...)`, `.on_touch(...)`, `.on_gesture(...)`, `.on_tablet(...)`, surface-level `.on_key(...)`, `.ime(...)` / `.on_ime(...)`, plus optional `.on_click_sound(impl Into<SoundPlay>)` / `.on_hover_sound(impl Into<SoundPlay>)`, and headless `TextInputState` / `ScrollState` controllers (pairable with `ctx.measure_text_cursor(...)`).
- Accessibility hooks: `.accessible_role(...)`, `.accessible_label(...)`, `.accessible_description(...)`, `.accessible_value(...)`, `.accessible_hidden(...)`.
- Composition: components use `.map(...)` on elements, surfaces, and subscriptions to lift child messages.

## Assets, Timers & System Telemetry

- Image assets: `ctx.load_image(path)`, `ctx.load_image_sized(path, min_svg_px)`, `ctx.load_image_bytes(bytes)`, `ctx.load_image_bytes_sized(bytes, min_svg_px)`, `ctx.load_image_static(bytes)`, `ctx.load_image_static_sized(bytes, min_svg_px)`, `ctx.load_rgba_image(...)`, `ctx.load_image_async(path, map)`, `ctx.set_image_retention(asset, retention)`, `ctx.image_retention(asset)`, `ctx.evict_image_pixels(asset)`, `ctx.reload_image(asset, path)`, `ctx.unload_image(asset)`.
- Timers: `Timer::once(key, duration, msg)` (resets on key change) and `Timer::every(key, interval, msg)`.
- System telemetry: `System::{cpu, memory, time, gpu, disk, network, battery, sensors, uptime, brightness, privacy}().every(...)` subscriptions and direct `ctx.system().sample_*()` queries, plus formatted units (`ByteSize`, `DataRate`, `RateUnit`, `FreqUnit`, `LocalDateTime`, `CalendarMonth`).
