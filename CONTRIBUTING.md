# Contributing

Changes to `waywi` should follow its crate structure, runtime boundaries, and isolation rules.

## Crate Architecture

- **Single crate, domain modules**: Code lives inside one `waywi` crate split into domain directories (`app`, `arena`, `asset`, `audio`, `compositor`, `dbus`, `desktop`, `effect`, `geometry`, `input`, `reconcile`, `render`, `sprite`, `subscription`, `surface`, `system`, `text`, `wayland`).
- **No `mod.rs` layout**: Define module entry points as `src/<domain>.rs` paired with `src/<domain>/<submodule>.rs`.

## Runtime & Effect Patterns

- **Model-View-Update separation**: Keep `Application::view` and `Application::subscriptions` pure functions of state. All state mutation, timers, asset loading, IPC calls, and system commands execute inside `Application::init` and `Application::update` through `&mut EffectCtx`.
- **Off-thread I/O workers**: Blocking or protocol-specific loops (Wayland clipboard pipes, D-Bus workers, PipeWire/PulseAudio/ALSA backends, async image decoding, DDC/CI) run on dedicated background threads and deliver events back to `calloop` via channels or ping wakers.
- **Arena & retained reconciliation**: `view` and `subscriptions` run within double-buffered `FrameArenaPool` scopes (`FrameVec`, `FrameSharedFn`, inline `SharedStr`). Prefer key-based subtree diffing (`key(...)`, `Surface::memo`, `Element::memo`) and generational `NodeId` handles over ad-hoc retained state outside `Reconciler`.
- **Composable mapping**: Generic components are plain functions returning `Element<SubMsg>` or `Surface<SubMsg>` lifted into parent messages via `.map(...)`.

## Feature Gates & Capabilities

- **Strict feature isolation**: Optional integrations (`renderer-gpu`, `text`, image formats, compositor IPC, `dbus`, `global-shortcuts`, audio backends, `accessibility`, `clipboard`, `idle-notify`, `control-ipc`) must stay gated behind Cargo features so minimal builds compile cleanly without pulling optional dependencies.
- **Explicit capability reporting**: Probe protocol/backend availability into `Capabilities` and return `WaywiError::UnsupportedCapability(...)` when a required protocol or integration is missing.

## Test Isolation

- **No live compositor dependency**: Tests must run in-process without connecting to the user's graphical session. Use `HeadlessRuntime` for MVU/layout/render tests and `wayland-server` over `UnixStream::pair()` or local `UnixListener` sockets for protocol tests.
