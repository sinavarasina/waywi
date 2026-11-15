use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use waywi::prelude::*;

struct DispatchState {
    clicks: u64,
}

#[derive(Clone)]
enum DispatchMsg {
    TileClicked(u32),
    BatchTick,
}

struct DispatchGridApp<const ROWS: usize, const COLS: usize, const TILE_PX: u32>;

impl<const ROWS: usize, const COLS: usize, const TILE_PX: u32> Application
    for DispatchGridApp<ROWS, COLS, TILE_PX>
{
    type State = DispatchState;
    type Message = DispatchMsg;

    fn init(_ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
        Ok(DispatchState { clicks: 0 })
    }

    fn update(state: &mut Self::State, msg: Self::Message, _ctx: &mut EffectCtx<'_>) -> Result<()> {
        match msg {
            DispatchMsg::TileClicked(idx) => {
                state.clicks = state.clicks.wrapping_add(u64::from(idx) + 1);
            }
            DispatchMsg::BatchTick => {
                state.clicks = state.clicks.wrapping_add(1);
            }
        }
        Ok(())
    }

    fn view(state: &Self::State) -> Surfaces<Self::Message> {
        let total = (ROWS * COLS) as u32;
        let gap = if ROWS == 1 { 4.0 } else { 2.0 };
        let mut col = Column::new()
            .padding(Edges::all(8.logical_px()))
            .gap(gap.logical_px());

        for r in 0..ROWS {
            let mut row = Row::new().gap(gap.logical_px());
            for c in 0..COLS {
                let idx = (r * COLS + c) as u32;
                let active = (state.clicks % u64::from(total)) as u32 == idx;
                row = row.child(
                    Rect::new()
                        .key(("hit", idx))
                        .size(Size::px(
                            (TILE_PX as f32).logical_px(),
                            (TILE_PX as f32).logical_px(),
                        ))
                        .fill(if active {
                            Color::rgb(163, 190, 140)
                        } else {
                            Color::rgb(59, 66, 82)
                        })
                        .on_click(DispatchMsg::TileClicked(idx)),
                );
            }
            col = col.child(row);
        }

        let (w, h) = if ROWS == 1 {
            (780.0, 44.0)
        } else {
            (820.0, 520.0)
        };
        let mut surfaces = Surfaces::new();
        surfaces.push(
            Surface::floating("board", Output::Primary)
                .size(Size::px(w.logical_px(), h.logical_px()))
                .child(col),
        );
        surfaces
    }
}

fn bench_event_dispatch(c: &mut Criterion) {
    let caps = Capabilities::from_compile_features();

    c.bench_function("mvu_dispatch_100_messages_zero_alloc_arena", |b| {
        let mut rt = HeadlessRuntime::<DispatchGridApp<1, 32, 20>>::new(
            caps.clone(),
            vec![OutputInfo::default()],
        )
        .expect("headless runtime init");
        let now = Instant::now();
        rt.step_at(now).expect("warmup");
        b.iter(|| {
            for _ in 0..100 {
                rt.enqueue(DispatchMsg::BatchTick);
            }
            rt.step_at(now).expect("step");
            black_box(rt.state().clicks);
        });
    });

    c.bench_function("hit_test_click_dispatch_32_targets", |b| {
        let mut rt = HeadlessRuntime::<DispatchGridApp<1, 32, 20>>::new(
            caps.clone(),
            vec![OutputInfo::default()],
        )
        .expect("headless runtime init");
        let now = Instant::now();
        rt.step_at(now).expect("warmup");
        b.iter(|| {
            let hit = rt.click_surface_point("board", 18.0, 18.0);
            rt.step_at(now).expect("step");
            black_box((hit, rt.state().clicks));
        });
    });

    c.bench_function("hit_test_click_dispatch_1000_targets", |b| {
        let mut rt = HeadlessRuntime::<DispatchGridApp<25, 40, 18>>::new(
            caps.clone(),
            vec![OutputInfo::default()],
        )
        .expect("headless runtime init");
        let now = Instant::now();
        rt.step_at(now).expect("warmup");
        b.iter(|| {
            let hit = rt.click_surface_point("board", 780.0, 490.0);
            rt.step_at(now).expect("step");
            black_box((hit, rt.state().clicks));
        });
    });
}

criterion_group!(benches, bench_event_dispatch);
criterion_main!(benches);
