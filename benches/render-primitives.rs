use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use waywi::prelude::*;

struct BenchState {
    tick: u64,
    mode: BenchMode,
}

#[derive(Clone, Copy)]
enum BenchMode {
    UnmemoizedCards,
    MemoizedIdle,
}

#[derive(Clone)]
enum BenchMsg {
    MutateOne,
    SetMode(BenchMode),
}

struct RenderGridApp<const ROWS: usize, const COLS: usize, const TILE_PX: u32>;

impl<const ROWS: usize, const COLS: usize, const TILE_PX: u32> Application
    for RenderGridApp<ROWS, COLS, TILE_PX>
{
    type State = BenchState;
    type Message = BenchMsg;

    fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
        Ok(BenchState {
            tick: 0,
            mode: BenchMode::UnmemoizedCards,
        })
    }

    fn update(
        state: &mut Self::State,
        msg: Self::Message,
        _ctx: &mut EffectCtx<'_, Self::Message>,
    ) -> Result<()> {
        match msg {
            BenchMsg::MutateOne => state.tick = state.tick.wrapping_add(1),
            BenchMsg::SetMode(mode) => state.mode = mode,
        }
        Ok(())
    }

    fn view(state: &Self::State) -> Surfaces<Self::Message> {
        let count = ROWS * COLS;
        let body: Element<Self::Message> = match state.mode {
            BenchMode::MemoizedIdle => Element::memo(("card-grid", count), &0u64, |_| {
                build_grid(0, ROWS, COLS, TILE_PX as f32).into()
            }),
            BenchMode::UnmemoizedCards => build_grid(state.tick, ROWS, COLS, TILE_PX as f32).into(),
        };

        let (surf_w, surf_h) = if count <= 50 {
            (500.0, 300.0)
        } else {
            (920.0, 600.0)
        };
        let mut surfaces = Surfaces::new();
        surfaces.push(
            Surface::floating("bench-surface", Output::Primary)
                .size((surf_w.logical_px(), surf_h.logical_px()))
                .child(body),
        );
        surfaces
    }
}

fn build_grid(active_tick: u64, rows: usize, cols: usize, tile_px: f32) -> Column<BenchMsg> {
    let mut col = Column::new()
        .padding(Edges::all(8.logical_px()))
        .gap(4.logical_px());

    for row_idx in 0..rows {
        let mut row = Row::new().gap(4.logical_px());
        for col_idx in 0..cols {
            let idx = row_idx * cols + col_idx;
            let fill = if idx == 0 && active_tick % 2 == 1 {
                Color::rgb(136, 192, 208)
            } else {
                Color::rgb(46, 52, 64)
            };
            row = row.child(
                Rect::new()
                    .key(("tile", idx))
                    .size((tile_px.logical_px(), tile_px.logical_px()))
                    .fill(fill)
                    .corner_radius(6.logical_px())
                    .border(1.logical_px(), Color::rgb(76, 86, 106)),
            );
        }
        col = col.child(row);
    }
    col
}

fn bench_grid<const ROWS: usize, const COLS: usize, const TILE_PX: u32>(
    c: &mut Criterion,
    caps: &Capabilities,
    label: &str,
) {
    let dirty_name = format!("render_{label}_rounded_rects_dirty_one");
    c.bench_function(&dirty_name, |b| {
        let mut rt = HeadlessRuntime::<RenderGridApp<ROWS, COLS, TILE_PX>>::new(
            caps.clone(),
            vec![OutputInfo::default()],
        )
        .expect("headless runtime init");
        let now = Instant::now();
        rt.step_at(now).expect("warmup step");
        b.iter(|| {
            rt.enqueue(BenchMsg::MutateOne);
            rt.step_at(now).expect("step");
            black_box(rt.rendered_buffer("bench-surface").map(|buf| buf.len()));
        });
    });

    let memo_name = format!("render_{label}_rounded_rects_memo_hit");
    c.bench_function(&memo_name, |b| {
        let mut rt = HeadlessRuntime::<RenderGridApp<ROWS, COLS, TILE_PX>>::new(
            caps.clone(),
            vec![OutputInfo::default()],
        )
        .expect("headless runtime init");
        let now = Instant::now();
        rt.enqueue(BenchMsg::SetMode(BenchMode::MemoizedIdle));
        rt.step_at(now).expect("warmup step");
        b.iter(|| {
            rt.enqueue(BenchMsg::MutateOne);
            rt.step_at(now).expect("step");
            black_box(rt.surface_count());
        });
    });
}

fn bench_render_primitives(c: &mut Criterion) {
    let caps = Capabilities::from_compile_features();
    bench_grid::<5, 10, 42>(c, &caps, "50");
    bench_grid::<25, 40, 18>(c, &caps, "1000");
}

criterion_group!(benches, bench_render_primitives);
criterion_main!(benches);
