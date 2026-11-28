# waywi

Wayland widget toolkit for Linux panels, overlays, OSDs, popups, and desktop utilities.

## Usage

Add `waywi` to `Cargo.toml` (default features enable `renderer-software`, `text`, `image-png`, `animation`):

```bash
cargo add waywi
```

Minimal application skeleton:

```rust,no_run
use waywi::prelude::*;

struct State {
    count: i64,
}

#[derive(Clone)]
enum Msg {
    Inc,
    Quit,
}

struct CounterApp;

impl Application for CounterApp {
    type State = State;
    type Message = Msg;

    fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
        Ok(State { count: 0 })
    }

    fn update(
        state: &mut Self::State,
        msg: Self::Message,
        ctx: &mut EffectCtx<'_, Self::Message>,
    ) -> Result<()> {
        match msg {
            Msg::Inc => state.count += 1,
            Msg::Quit => ctx.quit(),
        }
        Ok(())
    }

    fn view(state: &Self::State) -> Surfaces<Self::Message> {
        let mut surfaces = Surfaces::new();
        surfaces.push(
            Surface::floating("counter", Output::Primary)
                .size((240.logical_px(), 96.logical_px()))
                .keyboard_interactivity(KeyboardInteractivity::OnDemand)
                .on_key(|ev| (ev.key == Key::Escape).then_some(Msg::Quit))
                .child(
                    Rect::new()
                        .size(Size::fill())
                        .fill(Color::rgba(30, 34, 42, 235))
                        .corner_radius(12.logical_px())
                        .on_click(Msg::Inc),
                )
                .child(
                    Text::new(format!("Clicks: {}", state.count))
                        .position(Position::relative_to_parent(
                            Percent::new(50.0),
                            Percent::new(50.0),
                        ))
                        .anchor(ElementAnchor::Center),
                ),
        );
        surfaces
    }
}

fn main() -> Result<()> {
    App::new()
        .name("counter")
        .memory(MemoryConfig::balanced())
        .run::<CounterApp>()
}
```

Configure buffer retention and cache limits with `MemoryConfig::low_memory()`, `MemoryConfig::balanced()`, or `MemoryConfig::high_throughput()` on `App::memory(...)` (see [Memory Configuration](docs/performance.md#memory-configuration)), and call `ctx.trim_memory()` to reclaim idle caches immediately.

## Dependencies

- Core: `libwayland-client`, `libxkbcommon`
- `text`: `libfontconfig`
- `renderer-gpu`: Vulkan or OpenGL ES driver (`libvulkan` / `libEGL`)
- Audio backends: `libpipewire-0.3` (`pipewire`), `libpulse` (`pulseaudio`), `alsa-lib` (`alsa`)

See [Integrations](docs/integrations.md) for the complete list of Cargo features and Linux permission notes.

## Documentation

- [Architecture](docs/architecture.md)
- [API Overview](docs/api.md)
- [Library Reference](docs/reference.md)
- [Wayland Backend](docs/wayland.md)
- [Integrations & Features](docs/integrations.md)
- [Performance & Benchmarks](docs/performance.md)
- [Contributing](CONTRIBUTING.md)

## License

Licensed under the MIT License. See [LICENSE](LICENSE).

> [!CAUTION]
> Built with LLM assistance and still experimental. Any version update may introduce breaking changes or regressions — check [CHANGELOG.md](CHANGELOG.md) before upgrading.
