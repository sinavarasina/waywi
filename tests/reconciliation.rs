use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use waywi::prelude::*;

thread_local! {
    static MEMO_BUILDS: Cell<usize> = const { Cell::new(0) };
}

#[derive(Default)]
struct ReconcileState {
    count: u32,
    visible: bool,
    osd_seq: u64,
    hide_fired: usize,
    memo_version: u32,
    keyed_tick: usize,
}

#[derive(Clone)]
enum ReconcileMsg {
    ShowOsd,
    HideOsd,
    Sub(ChildMsg),
    BumpMemo,
    ToggleVisible(bool),
    Quit,
}

#[derive(Clone)]
enum ChildMsg {
    Pressed,
}

fn child_component(count: u32) -> Element<ChildMsg> {
    Row::new()
        .gap(6.logical_px())
        .child(
            Rect::new()
                .key("btn")
                .size(Size::px(80.logical_px(), 32.logical_px()))
                .fill(Color::rgb(40, 120, 220))
                .on_click(ChildMsg::Pressed),
        )
        .child(
            Rect::new()
                .key("indicator")
                .size(Size::px(20.logical_px(), 32.logical_px()))
                .fill(if count > 0 { Color::GREEN } else { Color::GRAY }),
        )
        .into()
}

struct ReconcileTestApp;

impl Application for ReconcileTestApp {
    type State = ReconcileState;
    type Message = ReconcileMsg;

    fn init(_ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
        Ok(ReconcileState {
            visible: true,
            ..Default::default()
        })
    }

    fn update(state: &mut Self::State, msg: Self::Message, ctx: &mut EffectCtx<'_>) -> Result<()> {
        match msg {
            ReconcileMsg::ShowOsd => {
                state.visible = true;
                state.osd_seq += 1;
            }
            ReconcileMsg::HideOsd => {
                state.hide_fired += 1;
                state.visible = false;
            }
            ReconcileMsg::Sub(ChildMsg::Pressed) => state.count += 10,
            ReconcileMsg::BumpMemo => state.memo_version += 1,
            ReconcileMsg::ToggleVisible(v) => state.visible = v,
            ReconcileMsg::Quit => ctx.quit(),
        }
        Ok(())
    }

    fn view(state: &Self::State) -> Surfaces<Self::Message> {
        let mut surfaces = Surfaces::new();
        if !state.visible {
            return surfaces;
        }

        let mapped_child = child_component(state.count).map(ReconcileMsg::Sub);
        let memo_el = Element::memo("memo-box", &state.memo_version, |_v| {
            MEMO_BUILDS.with(|c| c.set(c.get() + 1));
            Rect::new()
                .key("inner-memo")
                .size(Size::px(40.logical_px(), 20.logical_px()))
                .fill(Color::RED)
                .into()
        });

        surfaces.push(
            Surface::floating("main-card", Output::Primary)
                .size(Size::px(240.logical_px(), 80.logical_px()))
                .background(Color::rgba(20, 20, 30, 220))
                .child(
                    Column::new()
                        .padding(Edges::all(8.logical_px()))
                        .gap(4.logical_px())
                        .child(mapped_child)
                        .child(memo_el),
                ),
        );

        surfaces.push(Surface::memo("static-badge", &state.memo_version, |_v| {
            Surface::floating("static-badge-surf", Output::Primary)
                .size(Size::px(40.logical_px(), 40.logical_px()))
                .child(Rect::new().size(Size::fill()).fill(Color::BLUE))
        }));

        let _ = state.keyed_tick;
        surfaces
    }

    fn subscriptions(state: &Self::State) -> Subscriptions<Self::Message> {
        let mut subs = Subscriptions::new();
        if state.osd_seq > 0 && state.visible {
            subs.push(Timer::once(
                ("osd-hide", state.osd_seq),
                Duration::from_millis(500),
                ReconcileMsg::HideOsd,
            ));
        }
        subs
    }
}

#[test]
fn zero_alloc_steady_state_memoization_and_mapped_click_routing() {
    MEMO_BUILDS.with(|c| c.set(0));
    let mut rt = HeadlessRuntime::<ReconcileTestApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
    )
    .unwrap();
    assert_eq!(rt.surface_count(), 2);
    assert_eq!(MEMO_BUILDS.with(|c| c.get()), 1);

    // Steady-state re-evaluation should hit memo and use 0 arena heap fallbacks.
    for _ in 0..5 {
        rt.step().unwrap();
    }
    assert_eq!(rt.arena_heap_fallbacks(), 0);
    assert_eq!(MEMO_BUILDS.with(|c| c.get()), 1);

    // Bumping memo key triggers rebuild once.
    rt.enqueue(ReconcileMsg::BumpMemo);
    rt.step().unwrap();
    assert_eq!(MEMO_BUILDS.with(|c| c.get()), 2);

    // Click mapped child inside `Column` (8px top/left padding -> button is at ~16,16).
    assert!(rt.click_surface_point("main-card", 16.0, 16.0));
    rt.step().unwrap();
    assert_eq!(rt.state().count, 10);

    // Hiding surface reconciles live surfaces cleanly down to 0.
    rt.enqueue(ReconcileMsg::ToggleVisible(false));
    rt.step().unwrap();
    assert_eq!(rt.surface_count(), 0);

    rt.enqueue(ReconcileMsg::Quit);
    rt.step().unwrap();
    assert!(rt.quit_requested());
}

#[test]
fn keyed_timer_resets_when_sequence_key_changes() {
    let t0 = Instant::now();
    let mut rt = HeadlessRuntime::<ReconcileTestApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
    )
    .unwrap();

    rt.enqueue(ReconcileMsg::ShowOsd);
    rt.step_at(t0).unwrap();
    assert!(rt.state().visible);

    // At +300ms, sequence changes -> old 500ms timer cancelled, new one expires at +800ms.
    rt.enqueue(ReconcileMsg::ShowOsd);
    rt.step_at(t0 + Duration::from_millis(300)).unwrap();

    // At +550ms, old timer would have fired; new key keeps OSD visible!
    rt.step_at(t0 + Duration::from_millis(550)).unwrap();
    assert!(rt.state().visible);
    assert_eq!(rt.state().hide_fired, 0);

    // At +820ms, new keyed timer fires cleanly.
    rt.step_at(t0 + Duration::from_millis(820)).unwrap();
    assert!(!rt.state().visible);
    assert_eq!(rt.state().hide_fired, 1);

    let _ = Rc::new(0);
}

#[test]
fn pointer_buttons_and_surface_drag_routing() {
    struct DragState {
        x: f32,
        y: f32,
        dragging: bool,
        offset_x: f32,
        offset_y: f32,
        opened: bool,
        deleted: bool,
    }
    #[derive(Clone)]
    enum DragMsg {
        Down(PointerEvent),
        Move(PointerEvent),
        Up(PointerEvent),
    }
    struct DragApp;
    impl Application for DragApp {
        type State = DragState;
        type Message = DragMsg;
        fn init(_ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
            Ok(DragState {
                x: 40.0,
                y: 50.0,
                dragging: false,
                offset_x: 0.0,
                offset_y: 0.0,
                opened: false,
                deleted: false,
            })
        }
        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_>,
        ) -> Result<()> {
            match msg {
                DragMsg::Down(ev) => match ev.button {
                    Some(MouseButton::Left) => {
                        state.dragging = true;
                        state.offset_x = ev.position.x.0 - state.x;
                        state.offset_y = ev.position.y.0 - state.y;
                    }
                    Some(MouseButton::Middle) => state.opened = true,
                    Some(MouseButton::Right) => state.deleted = true,
                    _ => {}
                },
                DragMsg::Move(ev) if state.dragging => {
                    state.x = ev.position.x.0 - state.offset_x;
                    state.y = ev.position.y.0 - state.offset_y;
                }
                DragMsg::Move(_) => {}
                DragMsg::Up(ev) if ev.button == Some(MouseButton::Left) => {
                    state.dragging = false;
                }
                DragMsg::Up(_) => {}
            }
            Ok(())
        }
        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            let mut overlay = Surface::overlay("drag-overlay", Output::Primary)
                .input_mode(if state.dragging {
                    InputMode::Full
                } else {
                    InputMode::Selective
                })
                .on_pointer_move(|ev| Some(DragMsg::Move(ev)))
                .on_pointer_up(|ev| Some(DragMsg::Up(ev)));
            if !state.deleted {
                overlay = overlay.child(
                    Rect::new()
                        .key("pill")
                        .position(Position::Absolute {
                            x: LogicalPx(state.x),
                            y: LogicalPx(state.y),
                        })
                        .size(Size::px(120.logical_px(), 40.logical_px()))
                        .fill(Color::rgb(60, 120, 200))
                        .on_pointer_down(DragMsg::Down),
                );
            }
            s.push(overlay);
            s
        }
    }

    let mut rt = HeadlessRuntime::<DragApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(800.logical_px(), 600.logical_px()),
            ..OutputInfo::default()
        }],
    )
    .unwrap();
    rt.step().unwrap();

    // Left press on pill at (60, 65) starts drag with grab offset (20, 15).
    assert!(rt.pointer_down_surface_point("drag-overlay", 60.0, 65.0, MouseButton::Left));
    rt.step().unwrap();
    assert!(rt.state().dragging);

    // Drag across overlay surface to (300, 220) -> pill lands at (280, 205).
    assert!(rt.pointer_move_surface_point("drag-overlay", 300.0, 220.0));
    rt.step().unwrap();
    assert!((rt.state().x - 280.0).abs() < 0.1);
    assert!((rt.state().y - 205.0).abs() < 0.1);

    // Release Left button ends drag.
    assert!(rt.pointer_up_surface_point("drag-overlay", 300.0, 220.0, MouseButton::Left));
    rt.step().unwrap();
    assert!(!rt.state().dragging);

    // Middle click at new pill coordinates (310, 215) opens notification.
    assert!(rt.pointer_down_surface_point("drag-overlay", 310.0, 215.0, MouseButton::Middle));
    rt.step().unwrap();
    assert!(rt.state().opened);

    // Right click at new pill coordinates deletes notification.
    assert!(rt.pointer_down_surface_point("drag-overlay", 310.0, 215.0, MouseButton::Right));
    rt.step().unwrap();
    assert!(rt.state().deleted);
}

#[test]
fn headless_dispatches_hover_leave_and_click_sound() {
    #[derive(Default)]
    struct HoverState {
        hovered: u32,
        left: u32,
        clicked: u32,
        #[cfg(any(
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa",
            feature = "audio-playback"
        ))]
        sound: Option<SoundAsset>,
    }

    #[derive(Clone)]
    enum HoverMsg {
        Hovered,
        Left,
        Clicked,
    }

    struct HoverApp;

    impl Application for HoverApp {
        type State = HoverState;
        type Message = HoverMsg;

        fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            let sound = Some(ctx.load_sound_preset(SoundPreset::Click));
            #[cfg(not(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            )))]
            let _ = ctx;
            Ok(HoverState {
                #[cfg(any(
                    feature = "pipewire",
                    feature = "pulseaudio",
                    feature = "alsa",
                    feature = "audio-playback"
                ))]
                sound,
                ..Default::default()
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_>,
        ) -> Result<()> {
            match msg {
                HoverMsg::Hovered => state.hovered += 1,
                HoverMsg::Left => state.left += 1,
                HoverMsg::Clicked => state.clicked += 1,
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let btn = Rect::new()
                .key("btn")
                .size(Size::px(100.logical_px(), 40.logical_px()))
                .fill(Color::BLUE)
                .on_hover(HoverMsg::Hovered)
                .on_leave(HoverMsg::Left)
                .on_click(HoverMsg::Clicked);
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            let btn = if let Some(snd) = state.sound {
                btn.on_click_sound(snd).on_hover_sound_volume(snd, 0.5)
            } else {
                btn
            };
            #[cfg(not(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            )))]
            let _ = state;

            let mut surfaces = Surfaces::new();
            surfaces.push(Surface::overlay("hover-bar", Output::Primary).child(btn));
            surfaces
        }
    }

    let mut rt = HeadlessRuntime::<HoverApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(400.logical_px(), 100.logical_px()),
            ..OutputInfo::default()
        }],
    )
    .unwrap();
    rt.step().unwrap();

    // Moving onto the button fires `on_hover` once.
    assert!(rt.pointer_move_surface_point("hover-bar", 20.0, 20.0));
    rt.step().unwrap();
    assert_eq!(rt.state().hovered, 1);
    assert_eq!(rt.state().left, 0);

    // Moving within the same node does not re-fire `on_hover`.
    assert!(!rt.pointer_move_surface_point("hover-bar", 30.0, 20.0));
    rt.step().unwrap();
    assert_eq!(rt.state().hovered, 1);

    // Clicking fires `on_click` (and `on_click_sound` when audio features are enabled).
    assert!(rt.click_surface_point("hover-bar", 30.0, 20.0));
    rt.step().unwrap();
    assert_eq!(rt.state().clicked, 1);

    // Moving off the button fires `on_leave`.
    assert!(rt.pointer_move_surface_point("hover-bar", 250.0, 20.0));
    rt.step().unwrap();
    assert_eq!(rt.state().left, 1);

    // Re-entering and leaving the surface also fires `on_leave`.
    assert!(rt.pointer_move_surface_point("hover-bar", 20.0, 20.0));
    rt.step().unwrap();
    assert!(rt.pointer_leave_surface("hover-bar"));
    rt.step().unwrap();
    assert_eq!(rt.state().hovered, 2);
    assert_eq!(rt.state().left, 2);
}

#[test]
fn auto_group_with_fill_background_hugs_intrinsic_siblings() {
    #[derive(Default)]
    struct HugState;

    #[derive(Clone)]
    enum HugMsg {
        LeftHit,
        RightHit,
    }

    struct HugApp;

    impl Application for HugApp {
        type State = HugState;
        type Message = HugMsg;

        fn init(_ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
            Ok(HugState)
        }

        fn update(
            _state: &mut Self::State,
            _msg: Self::Message,
            _ctx: &mut EffectCtx<'_>,
        ) -> Result<()> {
            Ok(())
        }

        fn view(_state: &Self::State) -> Surfaces<Self::Message> {
            let pill = |label: &str, msg: HugMsg| {
                Group::new()
                    .size(Size::new(Length::Auto, 24.logical_px()))
                    .on_click(msg)
                    .child(Rect::new().size(Size::fill()).fill(Color::rgb(24, 24, 37)))
                    .child(
                        Row::new()
                            .size(Size::new(Length::Auto, 24.logical_px()))
                            .padding(Edges::symmetric(0.logical_px(), 6.logical_px()))
                            .align(Align::Center)
                            .child(
                                Rect::new()
                                    .size(Size::px(40.logical_px(), 12.logical_px()))
                                    .fill(Color::WHITE),
                            ),
                    )
                    .accessible_label(label.to_owned())
            };
            let right_group = Row::new()
                .position(Position::relative_to_parent(
                    Percent::new(100.0),
                    Percent::new(50.0),
                ))
                .anchor(ElementAnchor::CenterRight)
                .child(pill("right", HugMsg::RightHit));

            let mut surfaces = Surfaces::new();
            surfaces.push(
                Surface::panel("bar", Output::Primary)
                    .size(Size::new(Length::Fill, 24.logical_px()))
                    .child(
                        Group::new()
                            .size(Size::fill())
                            .child(Row::new().child(pill("left", HugMsg::LeftHit)))
                            .child(right_group),
                    ),
            );
            surfaces
        }
    }

    let mut rt = HeadlessRuntime::<HugApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1000.logical_px(), 24.logical_px()),
            ..OutputInfo::default()
        }],
    )
    .unwrap();
    rt.step().unwrap();

    // Left pill hugs 40 + 12 = 52px width cleanly (x: 0..52), leaving middle empty.
    assert!(rt.click_surface_point("bar", 26.0, 12.0));
    assert!(!rt.click_surface_point("bar", 500.0, 12.0));
    // Right pill is anchored cleanly to the right edge (x: 948..1000).
    assert!(rt.click_surface_point("bar", 974.0, 12.0));
}

#[test]
fn floating_surface_resizes_dynamically_with_px_and_auto_sizes() {
    #[derive(Clone, Default)]
    struct ResizeState {
        year_mode: bool,
        auto_lines: usize,
    }
    #[derive(Clone, Debug)]
    enum ResizeMsg {
        ToggleYear,
        SetLines(usize),
    }

    struct ResizeApp;
    impl Application for ResizeApp {
        type State = ResizeState;
        type Message = ResizeMsg;

        fn init(_ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
            Ok(ResizeState {
                year_mode: false,
                auto_lines: 1,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_>,
        ) -> Result<()> {
            match msg {
                ResizeMsg::ToggleYear => state.year_mode = !state.year_mode,
                ResizeMsg::SetLines(n) => state.auto_lines = n,
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut surfaces = Surfaces::new();
            let (w, h) = if state.year_mode {
                (690.0, 654.0)
            } else {
                (252.0, 198.0)
            };
            surfaces.push(
                Surface::floating("calendar", Output::Primary)
                    .anchor(Anchor::Top | Anchor::Right)
                    .size(Size::px(w.logical_px(), h.logical_px()))
                    .child(Rect::new().size(Size::fill()).fill(Color::BLACK)),
            );

            let mut col = Column::new().padding(Edges::all(8.logical_px()));
            for i in 0..state.auto_lines {
                col = col.child(
                    Rect::new()
                        .size(Size::px(
                            ((80 + i * 30) as f32).logical_px(),
                            20.logical_px(),
                        ))
                        .fill(Color::WHITE),
                );
            }
            surfaces.push(
                Surface::floating("auto-tip", Output::Primary)
                    .size(Size::auto())
                    .child(col),
            );
            surfaces
        }
    }

    let mut rt = HeadlessRuntime::<ResizeApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
    )
    .unwrap();
    rt.step().unwrap();
    assert_eq!(
        rt.surface_logical_size("calendar"),
        Some(LogicalSize::new(252.logical_px(), 198.logical_px()))
    );
    assert_eq!(
        rt.surface_logical_size("auto-tip"),
        Some(LogicalSize::new(96.logical_px(), 36.logical_px()))
    );

    // Simulate compositor configure locking previous size, then toggle larger -> must expand!
    assert!(rt.simulate_surface_configure(
        "calendar",
        LogicalSize::new(252.logical_px(), 198.logical_px())
    ));
    rt.enqueue(ResizeMsg::ToggleYear);
    rt.step().unwrap();
    assert_eq!(
        rt.surface_logical_size("calendar"),
        Some(LogicalSize::new(690.logical_px(), 654.logical_px()))
    );
    assert_eq!(rt.rendered_buffer("calendar").unwrap().len(), 690 * 654 * 4);

    // Simulate configure at 690x654, then toggle back -> must shrink cleanly without stale buffer!
    assert!(rt.simulate_surface_configure(
        "calendar",
        LogicalSize::new(690.logical_px(), 654.logical_px())
    ));
    rt.enqueue(ResizeMsg::ToggleYear);
    rt.step().unwrap();
    assert_eq!(
        rt.surface_logical_size("calendar"),
        Some(LogicalSize::new(252.logical_px(), 198.logical_px()))
    );
    assert_eq!(rt.rendered_buffer("calendar").unwrap().len(), 252 * 198 * 4);

    // Auto-sized tooltip surface grows cleanly when rows are added.
    rt.enqueue(ResizeMsg::SetLines(3));
    rt.step().unwrap();
    assert_eq!(
        rt.surface_logical_size("auto-tip"),
        Some(LogicalSize::new(156.logical_px(), 76.logical_px()))
    );
}
