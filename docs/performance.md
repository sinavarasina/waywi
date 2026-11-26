# Performance & Benchmarks

WayWi minimizes idle wakeups, steady-state allocations, and pixel writes across frames, and lets each application configure buffer retention and cache limits via `MemoryConfig` and `EffectCtx::trim_memory()`.

## Memory Configuration

| Preset | Best For | Buffer / Asset Retention | Cache Budgets |
|---|---|---|---|
| `MemoryConfig::balanced()` *(default)* | Panels, bars, OSDs, interactive popups | `Retain` / `KeepDecoded` | 256 text buffers (32 mapped font files), 512 KiB scratch, 8 MiB canvas/sound, 32 MiB GPU (`MemoryUsage`), D-Bus cache on + idle disconnect on |
| `MemoryConfig::low_memory()` | Static wallpapers, minimal bars, low-RAM systems | `ReleaseAfterCommit` / `ReclaimAfterPresent` | 64 text buffers (16 mapped font files), 64 KiB scratch, 2 MiB canvas/sound, 8 MiB GPU (`Minimal`), D-Bus snapshot cache off |
| `MemoryConfig::high_throughput()` | Rapidly animated dashboards, high-churn launchers | `Retain` / `KeepDecoded` | 1024 text buffers (64 mapped font files), 2 MiB scratch, 32 MiB canvas/sound, 64 MiB GPU (`Performance`), persistent D-Bus connections |

### Preset Drawbacks

- **`MemoryConfig::balanced()`**: keeps SHM buffers and decoded images resident with moderate cache limits, uses `GpuMemoryPolicy::MemoryUsage` (8–64 MiB Vulkan sub-allocator blocks instead of `wgpu`'s 128–256 MiB default), and automatically reclaims mmapped fallback fonts if a missing Unicode codepoint exceeds 32 mapped font files. For static full-screen surfaces (such as wallpapers), this retains more RAM than needed unless you opt into `BufferRetention::ReleaseAfterCommit` and `AssetRetention::ReclaimAfterPresent` per surface/asset.
- **`MemoryConfig::low_memory()`**: minimizes resident memory by releasing idle SHM buffers after commit, reclaiming decoded image pixels after presentation, shrinking caches sooner, capping mapped fallback font files at 16, using `GpuMemoryPolicy::Minimal` (4–16 MiB Vulkan blocks), and disabling D-Bus snapshot caching. The drawback is higher CPU and I/O overhead when surfaces redraw or resize frequently (images must re-decode, SHM pools re-allocate, text and canvas entries evict earlier, and D-Bus `cached_snapshot()` helpers return `None`).
- **`MemoryConfig::high_throughput()`**: keeps larger arenas, caches, `GpuMemoryPolicy::Performance` (128–256 MiB Vulkan blocks), and persistent D-Bus connections to avoid repeated allocations, re-shaping, and re-decoding during heavy updates. The drawback is a noticeably larger baseline RAM and VRAM footprint even while idle, until `ctx.trim_memory()` is called.

Individual limits can be overridden via builder methods on `MemoryConfig` (for example, `.load_system_fonts(false)` for icon-only tools that do not need Fontconfig, `.text_max_mapped_font_files(n)` to tune or disable automatic fallback-font reclamation, or `.gpu_memory_policy(...)` for Vulkan sub-allocator block sizes). Calling `ctx.trim_memory()` immediately trims reclaimable image pixels, rebuilds `TextEngine`'s `FontSystem` from the unmapped metadata database (releasing mmapped font files), clears renderer text/canvas/GPU/scratch caches (polling the GPU device), trims excess `FrameArenaPool` slab capacity, `MemoStore` slack, system/D-Bus caches, sound cache, and releases idle/oversized `wl_shm` pools.

## Cost Model

- **Scoped view arenas (`FrameArenaPool`)**: `view` and `subscriptions` run inside alternating `bumpalo::Bump` slabs reset once per dirty sync and auto-shrunk if a spike exceeds `arena_max_retain_bytes`. Zero-sized closures (`fn(A) -> M` enum tuple constructors) incur zero closure storage, while capturing `FrameSharedFn` closures use `Rc<F>` so memoized trees never pin bump slabs across frames.
- **Compact strings (`SharedStr`)**: Stores `&'static str`, up to 22 UTF-8 bytes inline on the stack (24-byte enum), or `Arc<str>`.
- **Nested subtree memoization**: `Surface::memo` and `Element::memo` skip subtree construction and tree diffing when dependency keys match the prior frame, preserving nested child memo entries across `MemoStore::prune_stale()`.
- **Dirty damage, direct Canvas drawing, and shared `TextEngine`**: Only changed retained nodes mark dirty `BufferRect` regions (`wl_surface.damage_buffer`). `Canvas::new(size, draw)` draws directly each frame without allocating an intermediate pixmap unless `.cache_key(...)` is set. `ActiveRenderer` owns a single shared `TextEngine` used across layout, cursor measurement, software, and GPU paths, with scale-aware measurement and stale entry eviction. Sprite animation ticks update `PaintPayload::Image` `SourceRect` in-place without rebuilding command or hit-test lists.
- **Event-driven background workers**: D-Bus `TrayWorker` uses targeted match rules (avoiding unrelated session-bus signals); `ServiceWatchWorker` coalesces signal bursts; `AudioClient` uses lock-free `AtomicBool` pending checks and stops cleanly via `stop_if_idle()` when subscriptions drop to zero; `AudioPlayer` releases idle ALSA `PCM` handles and corks idle PipeWire/PulseAudio streams; `AccessibilityBridge` skips tree updates while no screen reader is active; and `BroadcastSlot` moves events without cloning when there is a single subscriber.
- **Lazy fonts, SHM, and reclaimable assets**: `cosmic-text` loads fallback fonts lazily; if an unknown Unicode codepoint triggers an exhaustive fallback scan past `text_max_mapped_font_files`, `TextEngine` automatically rebuilds `FontSystem` from the unmapped metadata database at the end of the frame (dropping all rejected `mmap`s while keeping `buffer_cache` and `swash_cache` warm). `wl_shm` pools double-buffer only under compositor backpressure, close `memfd`s without truncating, automatically replace oversized idle `memfd`s when a surface shrinks by >2× (>64 KiB), and release double-buffered/oversized idle `memfd`s on `ctx.trim_memory()`. Static assets loaded via `load_image_static` reuse `'static` bytes with zero heap copy when reclaimed.

## Benchmarks

Benchmark harnesses live in `benches/` and run offline over `HeadlessRuntime`:

- [`benches/render-primitives.rs`](../benches/render-primitives.rs):
  - `render_50_rounded_rects_dirty_one`, `render_50_rounded_rects_memo_hit`
  - `render_1000_rounded_rects_dirty_one`, `render_1000_rounded_rects_memo_hit`
- [`benches/event-dispatch.rs`](../benches/event-dispatch.rs):
  - `mvu_dispatch_100_messages_zero_alloc_arena`
  - `hit_test_click_dispatch_32_targets`
  - `hit_test_click_dispatch_1000_targets`

Run benchmarks or smoke-check benchmark compilation:

```bash
cargo bench --all-features
cargo bench --all-features -- --test
```
