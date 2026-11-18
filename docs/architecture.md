# Architecture

`waywi` is structured as a single crate running a Model-View-Update (MVU) event loop over `calloop` and `smithay-client-toolkit`.

## Runtime Loop

```text
Wayland / Calloop / Timers / System Samplers / IPC / D-Bus
                            |
                            v
          Application::update(&mut state, msg, &mut EffectCtx)
                            |
            +---------------+---------------+
            |                               |
            v                               v
  FrameArenaPool Scope            MemoScope + Reconciler
  Application::subscriptions      Application::view
            |                               |
            v                               v
  Active Subscriptions            Keyed Diff & Layout
                                            |
                                            v
                                  Software / GPU Render + Commit
```

## Module Layout

Submodules follow the `<name>.rs` + `<name>/` convention (no `mod.rs`):

- `src/app.rs`, `src/app/`: `Application`, `App`, `HeadlessRuntime`, `subscription_mgr`, and phased `event_loop` (`sources`, `wayland_dispatch`, `present`).
- `src/arena.rs`, `src/arena/`: `FrameArenaPool` double-buffered `bumpalo` slabs, `FrameVec`, and `FrameSharedFn`.
- `src/surface.rs`, `src/surface/`: `Surface` builders (`panel`, `floating`, `overlay`, `window`, `memo`) and `Surfaces`.
- `src/reconcile.rs`, `src/reconcile/`: `Reconciler`, output expansion, generational `NodeIdAllocator`, layout pass, animation ticking, and dirty-rect tracking.
- `src/render.rs`, `src/render/`: `RenderBackend` trait, `Element<M>`, primitives (`Rect`, `Row`, `Column`, `Group`, `Clip`), `Canvas`, memo cache, `software` (`tiny-skia`), and optional `gpu` (`femtovg` + `wgpu`).
- `src/geometry.rs`, `src/geometry/`: `LogicalPx`, `BufferPx`, `Percent`, `ScaleFactor`, `Size`, `Position`, `ElementAnchor`, `Transform`, `Affine2D`, and rects.
- `src/input.rs`, `src/input/`: `ElementKey`, `NodeId`, `Key`, `Modifiers`, pointer/scroll/touch events, `KeyboardLayout`, Wayland IME config/events (`ime`), headless `TextInputState` / `ScrollState`, and inverse-affine `hit_test`.
- `src/text.rs`, `src/text/`: `SharedStr`, `Font`, `Text`, and shared `TextEngine` (`cosmic-text` + `fontdb`).
- `src/asset.rs`, `src/asset/`, `src/image.rs`, `src/sprite.rs`: content-addressed `AssetCache`, PNG/SVG/GIF/WebP decoders, `Image`, and `Sprite` / `SpriteSheet`.
- `src/effect.rs`, `src/effect/`: `EffectCtx` imperative effect boundary and `AsyncAssetWorker`.
- `src/timer.rs`, `src/subscription.rs`: keyed `Timer` specs and `Subscription` / `Subscriptions`.
- `src/wayland.rs`, `src/wayland/`: SCTK delegates, `SurfaceShmPool`, layer-shell, xdg-shell, output, seat, clipboard, idle, and IME (`zwp_text_input_v3`) protocols.
- `src/system.rs`, `src/system/`: Linux `/proc` and `/sys` telemetry (`cpu`, `memory`, `time`, `gpu`, `disk`, `network`, `battery`, `sensors`, `os`, `brightness`, `privacy`, `units`).
- `src/desktop.rs`, `src/desktop/`: XDG `.desktop` parser, launcher filtering/search, icon theme lookup (`XdgIconLookup`), and detached process spawning.
- `src/accessibility.rs`, `src/accessibility/`: accessible metadata props and optional `accesskit_unix` bridge.
- `src/audio.rs`, `src/audio/`: mixer `client`, sound decoding/themes/presets (`sound`), polyphonic `player` (`AudioPlayer`), and subscriptions (`types`).
- `src/compositor.rs`, `src/dbus.rs`, `src/control.rs`, `src/clipboard.rs`, `src/idle.rs`: feature-gated desktop and system integrations.
