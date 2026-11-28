# Architecture

`waywi` is structured as a single crate running a Model-View-Update (MVU) event loop over `calloop` and `smithay-client-toolkit`, sharing Wayland-independent runtime state between `LoopRuntime` and `HeadlessRuntime` through `CoreEngine<M>`.

## Runtime Loop

```text
Wayland / Calloop / Timers / System Samplers / IPC / D-Bus
                            |
                            v
     Application::update(&mut state, msg, &mut EffectCtx<M>)
                            |
            +---------------+---------------+
            |                               |
            v                               v
  FrameArenaPool Scope            MemoScope + Reconciler
  Application::subscriptions      Application::view
            |                               |
            v                               v
  BroadcastSlot / Workers         Keyed Diff & Scale-Aware Layout
                                            |
                                            v
                                  ActiveRenderer (Shared TextEngine)
                                  Software / GPU Render + Commit
```

## Module Layout

Submodules follow the `<name>.rs` + `<name>/` convention (no `mod.rs`):

- `src/app.rs`, `src/app/`: `Application`, `App`, `MemoryConfig`, `GpuMemoryPolicy`, `CoreEngine<M>`, `HeadlessRuntime`, `subscription_mgr` (`BroadcastSlot`), and phased `event_loop` (`sources`, `wayland_dispatch`, `present`).
- `src/arena.rs`, `src/arena/`: `FrameArenaPool` double-buffered `bumpalo` slabs with configurable retain caps, `FrameVec`, `FrameSharedFn` (ZST fast path + `Rc<F>` for capturing closures), and `hash_typed_key`.
- `src/animation.rs`: pure-mechanism VSync animation primitives (`Easing`, `Lerp`, `SpringConfig`, `Spring`, `Tween`, `Animated`, `TransitionSpec`, `Animation::frames`).
- `src/surface.rs`, `src/surface/`: `Surface` builders (`panel`, `floating`, `overlay`, `window`, `popup`, `session_lock`, `memo`), `WindowDecoration`, `BufferRetention`, and `Surfaces`.
- `src/reconcile.rs`, `src/reconcile/`: `Reconciler`, output expansion, generational `NodeIdAllocator`, scale-aware layout pass, in-place sprite and retained-transition ticking, and dirty-rect tracking.
- `src/render.rs`, `src/render/`: `ActiveRenderer` (owning the single shared `TextEngine`), `RenderBackend` trait, `Element<M>`, primitives (`Rect`, `Row`, `Column`, `Group`, `Clip`), `Canvas`, `MemoStore` (supporting nested memo scopes), `software` (`tiny-skia`), and optional `gpu` (`femtovg` + `wgpu` with `GpuMemoryPolicy` -> `wgpu::MemoryHints`).
- `src/geometry.rs`, `src/geometry/`: `LogicalPx`, `BufferPx`, `Percent`, `ScaleFactor`, `Size`, `Position`, `ElementAnchor`, `Transform`, `Affine2D`, `Bounds`, and `BufferRect`.
- `src/input.rs`, `src/input/`: `ElementKey`, `NodeId`, `Key`, `Modifiers`, pointer/scroll/touch/gesture/tablet/relative-pointer events, `CursorShape`, `DndEvent`/`DndAction`, `PointerConstraintMode`, `VirtualKeyState`, `KeyboardLayout`, Wayland IME config/events (`ime`), headless `TextInputState` / `ScrollState`, and inverse-affine `hit_test`.
- `src/text.rs`, `src/text/`: `SharedStr`, `Font`, `Text`, and shared `TextEngine` (`cosmic-text` + `fontdb`) with configurable cache limits, Fontconfig toggle, and automatic fallback-font reclamation.
- `src/asset.rs`, `src/asset/`, `src/image.rs`, `src/sprite.rs`: content-addressed `AssetCache` (`Disk`, `CompressedBytes`, `StaticBytes`, `RawPixels`), PNG/SVG/GIF/WebP/JPEG decoders, `Image`, and `Sprite` / `SpriteSheet`.
- `src/effect.rs`, `src/effect/`: typed `EffectCtx<'a, M>` imperative effect boundary, `AsyncAssetWorker`, and lazy `AsyncTaskWorker` (`spawn_blocking`).
- `src/timer.rs`, `src/subscription.rs`: keyed `Timer` specs and `Subscription` / `Subscriptions`.
- `src/wayland_ext.rs`: public cross-compositor Wayland desktop extension types (`SessionLock`, `ScreencopyFrame`/`ScreencopyTarget`, `ForeignToplevel`, `ExtWorkspace`, `GammaRamp`, `OutputColorInfo`).
- `src/wayland.rs`, `src/wayland/`: SCTK delegates, `SurfaceShmPool`, layer-shell, xdg-shell, xdg-activation, xdg-decoration, output, seat, zero-thread `calloop` clipboard and Drag & Drop (`wl_data_device`), idle-notify, idle-inhibit, IME (`zwp_text_input_v3`), pointer-gestures, tablet-v2, cursor-shape, session-lock, screencopy, foreign-toplevel, ext-workspace, virtual-keyboard, pointer-constraints/relative-pointer, background-effect, toplevel-icon, gamma-control, and color-management protocols.
- `src/system.rs`, `src/system/`: Linux `/proc` and `/sys` telemetry (`cpu`, `memory`, `time`, `gpu`, `disk`, `network`, `hardware`, `os`, `privacy`, `units`), zero-thread `inotify` `FileWatch` (`watch`), separated into pure `SystemProviders` and typed `SystemSamplers<M>`.
- `src/desktop.rs`, `src/desktop/`: XDG `.desktop` parser, launcher filtering/search, `LaunchOptions`/`SpawnOptions`, `XdgBaseDirs`, `XdgUserDirs`, and bounded `XdgIconLookup`.
- `src/accessibility.rs`, `src/accessibility/`: accessible metadata props and `accesskit_unix` bridge (synced only while a screen reader is active).
- `src/audio.rs`, `src/audio/`: mixer `AudioClient` (including `move_stream`), on-demand `Audio::capture` stream (`capture` with VU/PCM), sound decoding/themes/presets (`sound`), polyphonic `AudioPlayer` with LRU cache limits, and `Audio` subscriptions (`types`).
- `src/compositor.rs`, `src/dbus.rs`, `src/control.rs`, `src/clipboard.rs`, `src/idle.rs`: feature-gated desktop and system integrations (with `compositor::hyprland`, `compositor::sway`, and shared non-blocking `stream_worker`).
