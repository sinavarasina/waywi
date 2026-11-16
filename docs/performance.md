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

Run benchmarks or smoke-check benchmark compilation (`--all-features` is unavailable because the mutually exclusive `allocator-*` features guard each other):

```bash
cargo bench --features ci-all
cargo bench --features ci-all -- --test
```

## Allocators

The global allocator is a compile-time choice of the final binary: `allocator-mimalloc` (mimalloc 0.1.x) or `allocator-jemalloc` (tikv-jemallocator 0.7), both opt-in, non-default, and mutually exclusive. Default stays the system allocator. The per-frame bump arena sits on top of whichever allocator is active, so it needs no changes.

Measured on the `ci-all` feature set (Linux x86_64, criterion, `--sample-size 30 --warm-up-time 1 --measurement-time 3`), change vs. system allocator baseline:

| Benchmark | mimalloc | jemalloc |
| --- | --- | --- |
| `render_50_rounded_rects_dirty_one` | -0.8% | -0.1% |
| `render_50_rounded_rects_memo_hit` | **+16.5%** | +2.7% |
| `render_1000_rounded_rects_dirty_one` | **-3.1%** | **-2.8%** |
| `render_1000_rounded_rects_memo_hit` | +5.9% | **+12.6%** |
| `mvu_dispatch_100_messages_zero_alloc_arena` | +1.3% (n.s.) | +0.9% (n.s.) |
| `hit_test_click_dispatch_32_targets` | -0.3% (n.s.) | +1.5% (n.s.) |
| `hit_test_click_dispatch_1000_targets` | +1.5% | +0.1% (n.s.) |

(n.s. = not statistically significant, p > 0.05)

Verdict: a wash for typical widget workloads. Allocation-heavy steady-state rendering (`*_dirty_one`) gains ~3% at best, while memo-hit paths regress up to 16%. The frame arena already removes per-frame allocations, so there is little left for a global allocator to win. Ship the default unless a specific binary profile shows a benefit; the features exist so binaries can opt in when their workload differs.
