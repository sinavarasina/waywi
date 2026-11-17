# Performance & Benchmarks

WayWi minimizes idle wakeups, steady-state allocations, and pixel writes across frames.

## Cost Model

- **Scoped view arenas (`FrameArenaPool`)**: `view` and `subscriptions` run inside alternating `bumpalo::Bump` slabs reset once per dirty sync. Containers (`FrameVec`) and `.map(...)` closures (`FrameSharedFn`) allocate in the arena; zero-sized closures (`fn(A) -> M` enum tuple constructors) incur no closure storage.
- **Compact strings (`SharedStr`)**: Stores `&'static str`, up to 22 UTF-8 bytes inline on the stack (24-byte enum), or `Arc<str>`.
- **Subtree memoization**: `Surface::memo` and `Element::memo` skip subtree construction and tree diffing when dependency keys match the prior frame.
- **Dirty damage & rasterization**: Only changed retained nodes mark dirty `BufferRect` regions (`wl_surface.damage_buffer`). Solid fills and 1:1 blits use direct fast paths in software mode; unchanged frames skip rendering and commits.
- **Lazy fonts & buffers**: `cosmic-text` scans system fallback fonts via `fontdb` only when an uncovered glyph is encountered. `wl_shm` pools double-buffer only under compositor backpressure, and hiding/removing a surface closes the backing `memfd` without truncating it (compositors may keep mapping previously committed buffers).

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
