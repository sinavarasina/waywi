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
                .size((80.logical_px(), 32.logical_px()))
                .fill(Color::rgb(40, 120, 220))
                .on_click(ChildMsg::Pressed),
        )
        .child(
            Rect::new()
                .key("indicator")
                .size((20.logical_px(), 32.logical_px()))
                .fill(if count > 0 { Color::GREEN } else { Color::GRAY }),
        )
        .into()
}

struct ReconcileTestApp;

impl Application for ReconcileTestApp {
    type State = ReconcileState;
    type Message = ReconcileMsg;

    fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
        Ok(ReconcileState {
            visible: true,
            ..Default::default()
        })
    }

    fn update(
        state: &mut Self::State,
        msg: Self::Message,
        ctx: &mut EffectCtx<'_, Self::Message>,
    ) -> Result<()> {
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
                .size((40.logical_px(), 20.logical_px()))
                .fill(Color::RED)
                .into()
        });

        surfaces.push(
            Surface::floating("main-card", Output::Primary)
                .size((240.logical_px(), 80.logical_px()))
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
                .size((40.logical_px(), 40.logical_px()))
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
        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
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
            _ctx: &mut EffectCtx<'_, Self::Message>,
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
                        .size((120.logical_px(), 40.logical_px()))
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

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            #[cfg(any(
                feature = "pipewire",
                feature = "pulseaudio",
                feature = "alsa",
                feature = "audio-playback"
            ))]
            let sound = Some(ctx.player()?.load_preset(SoundPreset::Click));
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
            _ctx: &mut EffectCtx<'_, Self::Message>,
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
                .size((100.logical_px(), 40.logical_px()))
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
                btn.on_click_sound(snd).on_hover_sound((snd, 0.5))
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

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(HugState)
        }

        fn update(
            _state: &mut Self::State,
            _msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
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
                                    .size((40.logical_px(), 12.logical_px()))
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

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(ResizeState {
                year_mode: false,
                auto_lines: 1,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
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
                    .anchor(Anchor::TOP | Anchor::RIGHT)
                    .size((w.logical_px(), h.logical_px()))
                    .child(Rect::new().size(Size::fill()).fill(Color::BLACK)),
            );

            let mut col = Column::new().padding(Edges::all(8.logical_px()));
            for i in 0..state.auto_lines {
                col = col.child(
                    Rect::new()
                        .size((((80 + i * 30) as f32).logical_px(), 20.logical_px()))
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

#[test]
fn headless_runtime_routes_ime_events_and_reconciles_ime_config() {
    #[derive(Default)]
    struct ImeState {
        input: TextInputState,
        focused: bool,
    }

    #[derive(Clone)]
    enum ImeMsg {
        Ime(ImeEvent),
        Key(KeyEvent),
    }

    struct ImeApp;

    impl Application for ImeApp {
        type State = ImeState;
        type Message = ImeMsg;

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(ImeState::default())
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                ImeMsg::Ime(ev) => {
                    match &ev {
                        ImeEvent::Enter => state.focused = true,
                        ImeEvent::Leave => state.focused = false,
                        ImeEvent::Update { .. } => {}
                    }
                    state.input.handle_ime(&ev);
                }
                ImeMsg::Key(ev) => {
                    state.input.handle_key(&ev);
                }
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            let cursor_x = (state.input.before_cursor().chars().count() as f32 * 8.0).logical_px();
            s.push(
                Surface::floating("ime-input", Output::Primary)
                    .size((240.logical_px(), 44.logical_px()))
                    .ime(
                        ImeConfig::enabled()
                            .from_text_input(&state.input)
                            .cursor_rect(Bounds::new(
                                cursor_x,
                                8.logical_px(),
                                2.logical_px(),
                                20.logical_px(),
                            ))
                            .content_purpose(ImeContentPurpose::Normal),
                    )
                    .on_ime(|ev| Some(ImeMsg::Ime(ev)))
                    .on_key(|ev| Some(ImeMsg::Key(ev))),
            );
            s
        }
    }

    let mut rt = HeadlessRuntime::<ImeApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
    )
    .unwrap();
    rt.step().unwrap();

    assert!(
        rt.surface_ime_config("ime-input")
            .is_some_and(|c| c.enabled)
    );

    // IME Enter -> Preedit -> Commit -> Surrounding text synced cleanly to SurfaceSpec.
    assert!(rt.simulate_ime("ime-input", ImeEvent::Enter));
    rt.step().unwrap();
    assert!(rt.state().focused);

    assert!(rt.simulate_ime(
        "ime-input",
        ImeEvent::Update {
            delete_before: 0,
            delete_after: 0,
            commit: None,
            preedit: Some(ImePreedit {
                text: "nihon".into(),
                cursor_begin: Some(0),
                cursor_end: Some(5),
            }),
        }
    ));
    rt.step().unwrap();
    assert_eq!(rt.state().input.display_segments(), ("", "nihon", ""));
    // Preedit must NOT leak into surrounding plain text sent back to compositor!
    assert_eq!(
        rt.surface_ime_config("ime-input")
            .and_then(|c| c.surrounding_text.as_ref())
            .map(|s| s.text.as_str()),
        Some("")
    );

    assert!(rt.simulate_ime(
        "ime-input",
        ImeEvent::Update {
            delete_before: 0,
            delete_after: 0,
            commit: Some("日本".into()),
            preedit: None,
        }
    ));
    rt.step().unwrap();
    assert_eq!(rt.state().input.text(), "日本");
    let cfg = rt.surface_ime_config("ime-input").unwrap();
    assert_eq!(
        cfg.surrounding_text
            .as_ref()
            .map(|s| (s.text.as_str(), s.cursor)),
        Some(("日本", "日本".len()))
    );
    assert_eq!(
        cfg.cursor_rect,
        Some(Bounds::new(
            16.logical_px(),
            8.logical_px(),
            2.logical_px(),
            20.logical_px()
        ))
    );
    let seq_after_ime = cfg.external_change_seq;

    // External backspace bumps external_change_seq so compositor receives ChangeCause::Other.
    assert!(rt.key_surface(
        "ime-input",
        KeyEvent {
            key: Key::Backspace,
            modifiers: Modifiers::empty(),
            repeat: false,
            utf8: None,
        }
    ));
    rt.step().unwrap();
    assert_eq!(rt.state().input.text(), "日");
    assert!(
        rt.surface_ime_config("ime-input")
            .unwrap()
            .external_change_seq
            > seq_after_ime
    );
}

#[test]
fn headless_popup_dismiss_and_xdg_activation_capabilities() {
    #[derive(Clone)]
    enum Msg {
        Dismissed,
        RequestToken,
        GotToken(std::result::Result<String, String>),
    }

    #[derive(Default)]
    struct State {
        popup_open: bool,
        dismissed_count: usize,
        last_token: Option<std::result::Result<String, String>>,
    }

    struct PopupAndActApp;

    impl Application for PopupAndActApp {
        type State = State;
        type Message = Msg;

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(State {
                popup_open: true,
                ..State::default()
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::Dismissed => {
                    state.dismissed_count += 1;
                    state.popup_open = false;
                }
                Msg::RequestToken => {
                    ctx.request_activation_token(Some("org.waywi.Test"), |res| {
                        Msg::GotToken(res.map_err(|e| format!("{e}")))
                    });
                }
                Msg::GotToken(res) => {
                    if let Ok(ref tok) = res {
                        ctx.activate_surface("bar", tok.clone())?;
                    }
                    state.last_token = Some(res);
                }
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(
                Surface::panel("bar", Output::Primary).size((800.logical_px(), 32.logical_px())),
            );
            if state.popup_open {
                s.push(
                    Surface::popup("menu", "bar")
                        .size((160.logical_px(), 96.logical_px()))
                        .positioner(
                            PopupPositioner::new(Bounds::from_xywh(10.0, 0.0, 60.0, 32.0))
                                .anchor(PopupAnchor::BottomLeft)
                                .gravity(PopupGravity::BottomRight),
                        )
                        .on_close(Msg::Dismissed),
                );
            }
            s
        }
    }

    // 1. With Capability::XdgActivation enabled: token succeeds and activation is recorded.
    let mut caps_enabled = Capabilities::from_compile_features();
    caps_enabled.set(Capability::XdgActivation, true);
    let outputs = vec![OutputInfo {
        id: OutputId(1),
        logical_size: LogicalSize::new(800.logical_px(), 600.logical_px()),
        ..OutputInfo::default()
    }];
    let mut rt = HeadlessRuntime::<PopupAndActApp>::new(caps_enabled, outputs.clone()).unwrap();
    assert_eq!(rt.surface_count(), 2);
    assert_eq!(
        rt.surface_logical_size("menu"),
        Some(LogicalSize::new(160.logical_px(), 96.logical_px()))
    );

    // Dismissing popup fires on_close without setting quit_requested.
    assert!(rt.simulate_popup_dismiss("menu"));
    rt.step().unwrap();
    assert_eq!(rt.state().dismissed_count, 1);
    assert!(!rt.quit_requested());
    assert_eq!(rt.surface_count(), 1);

    // Request activation token + activate_surface.
    rt.enqueue(Msg::RequestToken);
    rt.step().unwrap();
    rt.step().unwrap();
    assert_eq!(
        rt.state()
            .last_token
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(String::as_str),
        Some("headless-activation-token-1")
    );
    assert_eq!(
        rt.activated_surfaces(),
        &[(
            "bar".into_surface_key(),
            "headless-activation-token-1".to_owned()
        )]
    );

    // 2. With Capability::XdgActivation disabled: honest UnsupportedCapability error.
    let mut caps_disabled = Capabilities::from_compile_features();
    caps_disabled.set(Capability::XdgActivation, false);
    let mut rt2 = HeadlessRuntime::<PopupAndActApp>::new(caps_disabled, outputs).unwrap();
    rt2.enqueue(Msg::RequestToken);
    rt2.step().unwrap();
    rt2.step().unwrap();
    assert!(
        rt2.state()
            .last_token
            .as_ref()
            .is_some_and(std::result::Result::is_err)
    );
    assert!(rt2.activated_surfaces().is_empty());
}

#[test]
fn typed_keys_avoid_double_hashing_on_passthrough() {
    let ek = ElementKey::from_hashable(&"btn-primary");
    assert_eq!(ek.into_element_key(), ek);
    assert_eq!("btn-primary".into_element_key(), ek);

    let sk = SurfaceKey::from_hashable(&("panel", 1u32));
    assert_eq!(sk.into_surface_key(), sk);
    assert_eq!(("panel", 1u32).into_surface_key(), sk);

    let subk = SubscriptionKey::from_hashable(&"cpu-tick");
    assert_eq!(subk.into_subscription_key(), subk);
    assert_eq!("cpu-tick".into_subscription_key(), subk);
}

#[test]
fn nested_memo_with_capturing_closures_preserves_entries_and_zero_arena_fallbacks() {
    thread_local! {
        static OUTER_BUILDS: Cell<usize> = const { Cell::new(0) };
        static INNER_BUILDS: Cell<usize> = const { Cell::new(0) };
    }

    #[derive(Default)]
    struct NestedState {
        outer_ver: u32,
        inner_ver: u32,
        unrelated: u32,
        clicks: u32,
    }

    #[derive(Clone)]
    enum NestedMsg {
        BumpUnrelated,
        BumpOuter,
        Clicked(u32),
    }

    struct NestedApp;
    impl Application for NestedApp {
        type State = NestedState;
        type Message = NestedMsg;

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(NestedState::default())
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                NestedMsg::BumpUnrelated => state.unrelated += 1,
                NestedMsg::BumpOuter => state.outer_ver += 1,
                NestedMsg::Clicked(v) => state.clicks += v,
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let inner_ver = state.inner_ver;
            let outer_ver = state.outer_ver;
            let outer = Element::memo("outer-memo", &outer_ver, move |&ov| {
                OUTER_BUILDS.with(|c| c.set(c.get() + 1));
                let captured = ov + 7;
                let inner = Element::memo("inner-memo", &inner_ver, move |&_iv| {
                    INNER_BUILDS.with(|c| c.set(c.get() + 1));
                    Rect::new()
                        .key("inner-btn")
                        .size((48.logical_px(), 24.logical_px()))
                        .fill(Color::BLUE)
                        .on_pointer_down(move |_| NestedMsg::Clicked(captured))
                        .into()
                });
                Column::new().child(inner).into()
            });

            let mut surfaces = Surfaces::new();
            surfaces.push(
                Surface::floating("nested-host", Output::Primary)
                    .size((120.logical_px(), 64.logical_px()))
                    .child(outer),
            );
            surfaces
        }
    }

    OUTER_BUILDS.with(|c| c.set(0));
    INNER_BUILDS.with(|c| c.set(0));

    let mut rt = HeadlessRuntime::<NestedApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
    )
    .unwrap();
    assert_eq!(OUTER_BUILDS.with(Cell::get), 1);
    assert_eq!(INNER_BUILDS.with(Cell::get), 1);

    // 8 consecutive unrelated updates hitting outer memo must preserve inner memo across
    // `prune_stale()` AND produce 0 FrameArenaPool heap fallbacks (no slab pinning!).
    for _ in 0..8 {
        rt.enqueue(NestedMsg::BumpUnrelated);
        rt.step().unwrap();
        assert_eq!(rt.arena_heap_fallbacks(), 0);
    }
    assert_eq!(OUTER_BUILDS.with(Cell::get), 1);
    assert_eq!(INNER_BUILDS.with(Cell::get), 1);

    // Invalidating outer memo while inner_ver stays constant must hit inner memo!
    rt.enqueue(NestedMsg::BumpOuter);
    rt.step().unwrap();
    assert_eq!(OUTER_BUILDS.with(Cell::get), 2);
    assert_eq!(INNER_BUILDS.with(Cell::get), 1);
}

#[test]
fn memory_config_presets_trim_memory_and_headless_async_image_delivery() {
    assert_eq!(
        MemoryConfig::low_memory().default_buffer_retention,
        BufferRetention::ReleaseAfterCommit
    );
    assert_eq!(
        MemoryConfig::low_memory().default_asset_retention,
        AssetRetention::ReclaimAfterPresent
    );
    assert!(
        MemoryConfig::high_throughput().text_max_cached_buffers
            > MemoryConfig::balanced().text_max_cached_buffers
    );

    #[derive(Default)]
    struct MemState {
        async_done: bool,
        async_err: bool,
        trimmed: bool,
    }

    #[derive(Clone)]
    enum MemMsg {
        RequestMissingAsync,
        AsyncLoaded(bool),
        Trim,
    }

    struct MemApp;
    impl Application for MemApp {
        type State = MemState;
        type Message = MemMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            assert_eq!(
                ctx.memory_config().default_asset_retention,
                AssetRetention::ReclaimAfterPresent
            );
            Ok(MemState::default())
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                MemMsg::RequestMissingAsync => {
                    ctx.load_image_async("/nonexistent/waywi-v021-test.png", |res| {
                        MemMsg::AsyncLoaded(res.is_err())
                    });
                }
                MemMsg::AsyncLoaded(is_err) => {
                    state.async_done = true;
                    state.async_err = is_err;
                }
                MemMsg::Trim => {
                    ctx.trim_memory();
                    state.trimmed = true;
                }
            }
            Ok(())
        }

        fn view(_state: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::none()
        }
    }

    let mut rt = HeadlessRuntime::<MemApp>::with_memory(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
        RendererPreference::Auto,
        MemoryConfig::low_memory(),
    )
    .unwrap();

    rt.enqueue(MemMsg::RequestMissingAsync);
    rt.step().unwrap();
    // Poll briefly until background worker returns the error result to HeadlessRuntime.
    for _ in 0..50 {
        if rt.state().async_done {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        rt.step().unwrap();
    }
    assert!(rt.state().async_done);
    assert!(rt.state().async_err);

    rt.enqueue(MemMsg::Trim);
    rt.step().unwrap();
    assert!(rt.state().trimmed);
    rt.trim_memory();
}

#[test]
fn surface_memo_and_nested_element_memo_invalidate_on_configure_resize() {
    thread_local! {
        static SHELL_BUILDS: Cell<usize> = const { Cell::new(0) };
        static CHILD_BUILDS: Cell<usize> = const { Cell::new(0) };
    }

    struct ResizeMemoApp;

    impl Application for ResizeMemoApp {
        type State = ();
        type Message = ();

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(())
        }

        fn update(
            _state: &mut Self::State,
            _msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            Ok(())
        }

        fn view(_state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(Surface::memo("memo-win", &1u32, |_| {
                SHELL_BUILDS.with(|c| c.set(c.get() + 1));
                Surface::window("memo-win")
                    .size((200.logical_px(), 100.logical_px()))
                    .child(Element::memo("inner-rect", &1u32, |_| {
                        CHILD_BUILDS.with(|c| c.set(c.get() + 1));
                        Rect::new().size(Size::fill()).fill(Color::BLUE).into()
                    }))
            }));
            s
        }
    }

    SHELL_BUILDS.with(|c| c.set(0));
    CHILD_BUILDS.with(|c| c.set(0));

    let mut rt = HeadlessRuntime::<ResizeMemoApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
    )
    .unwrap();
    assert_eq!(SHELL_BUILDS.with(|c| c.get()), 1);
    assert_eq!(CHILD_BUILDS.with(|c| c.get()), 1);
    assert_eq!(
        rt.surface_logical_size("memo-win"),
        Some(LogicalSize::new(200.logical_px(), 100.logical_px()))
    );

    // Steady-state frame hits both memos cleanly.
    rt.step().unwrap();
    assert_eq!(SHELL_BUILDS.with(|c| c.get()), 1);
    assert_eq!(CHILD_BUILDS.with(|c| c.get()), 1);

    // Compositor resize invalidates the surface memo + nested Element::memo so layout updates.
    assert!(rt.simulate_surface_configure(
        "memo-win",
        LogicalSize::new(640.logical_px(), 480.logical_px()),
    ));
    rt.step().unwrap();
    assert_eq!(SHELL_BUILDS.with(|c| c.get()), 2);
    assert_eq!(CHILD_BUILDS.with(|c| c.get()), 2);
    assert_eq!(
        rt.surface_logical_size("memo-win"),
        Some(LogicalSize::new(640.logical_px(), 480.logical_px()))
    );
}

#[test]
fn touch_capture_multitouch_and_primary_pointer_drag_fallback() {
    type TouchLogEntry = (i32, TouchPhase, f32, Option<(f32, f32)>, Option<f32>);

    #[derive(Default)]
    struct TouchState {
        touch_log: Vec<TouchLogEntry>,
        clicks: usize,
        ptr_downs: usize,
        ptr_moves: Vec<f32>,
        ptr_ups: usize,
    }

    #[derive(Clone)]
    enum TouchMsg {
        CanvasTouch(TouchEvent),
        ButtonClick,
        ButtonDown,
        SurfaceMove(f32),
        SurfaceUp,
    }

    struct TouchApp;
    impl Application for TouchApp {
        type State = TouchState;
        type Message = TouchMsg;

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(TouchState::default())
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                TouchMsg::CanvasTouch(ev) => state.touch_log.push((
                    ev.id,
                    ev.phase,
                    ev.local_position.x.0,
                    ev.shape,
                    ev.orientation,
                )),
                TouchMsg::ButtonClick => state.clicks += 1,
                TouchMsg::ButtonDown => state.ptr_downs += 1,
                TouchMsg::SurfaceMove(x) => state.ptr_moves.push(x),
                TouchMsg::SurfaceUp => state.ptr_ups += 1,
            }
            Ok(())
        }

        fn view(_state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(
                Surface::window("touch-win")
                    .size((240.logical_px(), 100.logical_px()))
                    .on_pointer_move(|e| Some(TouchMsg::SurfaceMove(e.position.x.0)))
                    .on_pointer_up(|_| Some(TouchMsg::SurfaceUp))
                    .child(
                        Row::new()
                            .gap(10.logical_px())
                            .child(
                                Rect::new()
                                    .key("canvas")
                                    .size((100.logical_px(), 100.logical_px()))
                                    .fill(Color::BLUE)
                                    .on_touch(TouchMsg::CanvasTouch),
                            )
                            .child(
                                Rect::new()
                                    .key("slider-btn")
                                    .size((100.logical_px(), 100.logical_px()))
                                    .fill(Color::GREEN)
                                    .on_click(TouchMsg::ButtonClick)
                                    .on_pointer_down(|_| TouchMsg::ButtonDown),
                            ),
                    ),
            );
            s
        }
    }

    let mut rt = HeadlessRuntime::<TouchApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
    )
    .unwrap();

    // Multi-touch contacts on canvas: id=1 stays captured even when dragging over the button.
    let make_touch = |id, phase, x, shape, orientation| TouchEvent {
        id,
        phase,
        position: Point::new(LogicalPx(x), LogicalPx(40.0)),
        local_position: Point::ZERO,
        shape,
        orientation,
        modifiers: Modifiers::empty(),
    };
    assert!(rt.touch_surface(
        "touch-win",
        make_touch(1, TouchPhase::Down, 20.0, Some((10.0, 6.0)), Some(30.0)),
    ));
    assert!(rt.touch_surface(
        "touch-win",
        make_touch(2, TouchPhase::Down, 50.0, Some((8.0, 8.0)), Some(0.0)),
    ));
    assert!(rt.touch_surface(
        "touch-win",
        make_touch(1, TouchPhase::Move, 160.0, Some((10.0, 6.0)), Some(30.0)),
    ));
    assert!(rt.touch_surface(
        "touch-win",
        make_touch(1, TouchPhase::Up, 160.0, Some((10.0, 6.0)), Some(30.0)),
    ));
    assert!(rt.touch_surface(
        "touch-win",
        make_touch(2, TouchPhase::Cancel, 50.0, Some((8.0, 8.0)), Some(0.0)),
    ));
    rt.step().unwrap();

    assert_eq!(rt.state().touch_log.len(), 5);
    assert_eq!(
        rt.state().touch_log[0],
        (1, TouchPhase::Down, 20.0, Some((10.0, 6.0)), Some(30.0))
    );
    assert_eq!(
        rt.state().touch_log[2],
        (1, TouchPhase::Move, 160.0, Some((10.0, 6.0)), Some(30.0))
    );
    assert_eq!(rt.state().touch_log[4].1, TouchPhase::Cancel);
    // Claimed contacts must never double-fire pointer fallback.
    assert_eq!(rt.state().clicks, 0);
    assert_eq!(rt.state().ptr_downs, 0);
    assert!(rt.state().ptr_moves.is_empty());
    assert_eq!(rt.state().ptr_ups, 0);

    // Unclaimed primary touch on slider/button falls back cleanly to click + drag + release.
    assert!(rt.touch_surface(
        "touch-win",
        make_touch(3, TouchPhase::Down, 140.0, None, None)
    ));
    assert!(rt.touch_surface(
        "touch-win",
        make_touch(3, TouchPhase::Move, 180.0, None, None)
    ));
    assert!(rt.touch_surface(
        "touch-win",
        make_touch(3, TouchPhase::Up, 180.0, None, None)
    ));
    rt.step().unwrap();

    assert_eq!(rt.state().clicks, 1);
    assert_eq!(rt.state().ptr_downs, 1);
    assert_eq!(rt.state().ptr_moves, vec![180.0]);
    assert_eq!(rt.state().ptr_ups, 1);
}

#[test]
fn scroll_metadata_and_gesture_dispatch() {
    #[derive(Default)]
    struct PadState {
        scrolls: Vec<(ScrollSource, f32, bool, bool, bool)>,
        node_gestures: Vec<(GesturePhase, GestureKind, u32, f32)>,
        surf_gestures: Vec<(GesturePhase, f32)>,
    }

    #[derive(Clone)]
    enum PadMsg {
        Scroll(ScrollEvent),
        NodeGesture(GestureEvent),
        SurfaceGesture(GestureEvent),
    }

    struct PadApp;
    impl Application for PadApp {
        type State = PadState;
        type Message = PadMsg;

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(PadState::default())
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                PadMsg::Scroll(e) => state.scrolls.push((
                    e.source,
                    e.delta_y,
                    e.stop_x,
                    e.stop_y,
                    e.modifiers.contains(Modifiers::CTRL),
                )),
                PadMsg::NodeGesture(g) => {
                    state
                        .node_gestures
                        .push((g.phase, g.kind, g.fingers, g.local_position.x.0))
                }
                PadMsg::SurfaceGesture(g) => state.surf_gestures.push((g.phase, g.position.x.0)),
            }
            Ok(())
        }

        fn view(_state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(
                Surface::window("pad-win")
                    .size((200.logical_px(), 120.logical_px()))
                    .on_gesture(|g| Some(PadMsg::SurfaceGesture(g)))
                    .child(
                        Rect::new()
                            .key("viewport")
                            .position(Position::absolute(20.logical_px(), 10.logical_px()))
                            .size((150.logical_px(), 100.logical_px()))
                            .fill(Color::GRAY)
                            .on_scroll(PadMsg::Scroll)
                            .on_gesture(PadMsg::NodeGesture),
                    ),
            );
            s
        }
    }

    let mut rt = HeadlessRuntime::<PadApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
    )
    .unwrap();

    assert!(rt.scroll_surface_event(
        "pad-win",
        ScrollEvent {
            position: Point::new(LogicalPx(50.0), LogicalPx(40.0)),
            local_position: Point::ZERO,
            delta_x: 0.0,
            delta_y: 3.5,
            discrete_x: 0,
            discrete_y: 0,
            source: ScrollSource::Finger,
            stop_x: false,
            stop_y: false,
            modifiers: Modifiers::CTRL,
        },
    ));
    assert!(rt.scroll_surface_event(
        "pad-win",
        ScrollEvent {
            position: Point::new(LogicalPx(50.0), LogicalPx(40.0)),
            local_position: Point::ZERO,
            delta_x: 0.0,
            delta_y: 0.0,
            discrete_x: 0,
            discrete_y: 0,
            source: ScrollSource::Finger,
            stop_x: false,
            stop_y: true,
            modifiers: Modifiers::empty(),
        },
    ));
    assert!(rt.gesture_surface(
        "pad-win",
        GestureEvent {
            kind: GestureKind::Pinch {
                dx: 1.0,
                dy: -2.0,
                scale: 1.25,
                rotation: 12.0,
            },
            phase: GesturePhase::Update,
            fingers: 2,
            position: Point::new(LogicalPx(70.0), LogicalPx(50.0)),
            local_position: Point::ZERO,
            modifiers: Modifiers::empty(),
        },
    ));
    rt.step().unwrap();

    assert_eq!(
        rt.state().scrolls,
        vec![
            (ScrollSource::Finger, 3.5, false, false, true),
            (ScrollSource::Finger, 0.0, false, true, false),
        ]
    );
    assert_eq!(rt.state().node_gestures.len(), 1);
    assert_eq!(rt.state().node_gestures[0].0, GesturePhase::Update);
    assert_eq!(rt.state().node_gestures[0].2, 2);
    assert_eq!(rt.state().node_gestures[0].3, 50.0); // 70 - 20 offset
    assert_eq!(rt.state().surf_gestures, vec![(GesturePhase::Update, 70.0)]);
}

#[test]
fn tablet_stroke_capture_mixed_toolbar_fallback_and_pad() {
    #[derive(Default)]
    struct TabState {
        canvas_strokes: Vec<(TabletToolEventKind, f32, f32)>,
        pad_events: Vec<TabletPadEventKind>,
        toolbar_clicks: usize,
        toolbar_hovered: usize,
        toolbar_left: usize,
        toolbar_down_buttons: Vec<MouseButton>,
        surf_moves: Vec<f32>,
        surf_ups: usize,
    }

    #[derive(Clone)]
    enum TabMsg {
        Canvas(TabletEvent),
        PadSurf(TabletEvent),
        ToolbarClick,
        ToolbarHover,
        ToolbarLeave,
        ToolbarDown(MouseButton),
        SurfaceMove(f32),
        SurfaceUp,
    }

    struct TabApp;
    impl Application for TabApp {
        type State = TabState;
        type Message = TabMsg;

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(TabState::default())
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                TabMsg::Canvas(TabletEvent::Tool(ev)) => {
                    state
                        .canvas_strokes
                        .push((ev.kind, ev.local_position.x.0, ev.axes.pressure));
                }
                TabMsg::Canvas(TabletEvent::Pad(_)) => {}
                TabMsg::PadSurf(TabletEvent::Pad(p)) => state.pad_events.push(p.kind),
                TabMsg::PadSurf(TabletEvent::Tool(_)) => {}
                TabMsg::ToolbarClick => state.toolbar_clicks += 1,
                TabMsg::ToolbarHover => state.toolbar_hovered += 1,
                TabMsg::ToolbarLeave => state.toolbar_left += 1,
                TabMsg::ToolbarDown(btn) => state.toolbar_down_buttons.push(btn),
                TabMsg::SurfaceMove(x) => state.surf_moves.push(x),
                TabMsg::SurfaceUp => state.surf_ups += 1,
            }
            Ok(())
        }

        fn view(_state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            s.push(
                Surface::window("studio")
                    .size((240.logical_px(), 100.logical_px()))
                    .on_pointer_move(|e| Some(TabMsg::SurfaceMove(e.position.x.0)))
                    .on_pointer_up(|_| Some(TabMsg::SurfaceUp))
                    .child(
                        Row::new()
                            .gap(10.logical_px())
                            .child(
                                Rect::new()
                                    .key("draw-canvas")
                                    .size((100.logical_px(), 100.logical_px()))
                                    .fill(Color::WHITE)
                                    .on_tablet(TabMsg::Canvas),
                            )
                            .child(
                                Rect::new()
                                    .key("toolbar-btn")
                                    .size((100.logical_px(), 100.logical_px()))
                                    .fill(Color::BLUE)
                                    .on_click(TabMsg::ToolbarClick)
                                    .on_hover(TabMsg::ToolbarHover)
                                    .on_leave(TabMsg::ToolbarLeave)
                                    .on_pointer_down(|e| {
                                        TabMsg::ToolbarDown(e.button.unwrap_or(MouseButton::Left))
                                    }),
                            ),
                    ),
            );
            s.push(
                Surface::window("pad-only")
                    .size((100.logical_px(), 40.logical_px()))
                    .on_tablet(|ev| Some(TabMsg::PadSurf(ev))),
            );
            s
        }
    }

    let mut rt = HeadlessRuntime::<TabApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
    )
    .unwrap();

    let tool_ev = |kind, x, pressure, tip_down| {
        TabletEvent::Tool(TabletToolEvent {
            tool_id: TabletToolId(1),
            tablet_id: Some(TabletId(1)),
            tool_type: TabletToolType::Pen,
            hardware_serial: 0x1234,
            hardware_id_wacom: 0x5678,
            kind,
            position: Point::new(LogicalPx(x), LogicalPx(50.0)),
            local_position: Point::ZERO,
            axes: TabletToolAxes {
                pressure,
                ..Default::default()
            },
            tip_down,
            modifiers: Modifiers::empty(),
        })
    };

    // Stroke starting on canvas stays captured even when moving over toolbar.
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(TabletToolEventKind::Down, 25.0, 0.5, true)
    ));
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(TabletToolEventKind::Motion, 150.0, 0.9, true)
    ));
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(TabletToolEventKind::Up, 150.0, 0.0, false)
    ));
    rt.step().unwrap();

    assert_eq!(
        rt.state().canvas_strokes,
        vec![
            (TabletToolEventKind::Down, 25.0, 0.5),
            (TabletToolEventKind::Motion, 150.0, 0.9),
            (TabletToolEventKind::Up, 150.0, 0.0),
        ]
    );
    assert_eq!(rt.state().toolbar_clicks, 0);
    assert!(rt.state().surf_moves.is_empty());

    // Stylus interacting with toolbar falls back cleanly to hover, click, drag (even across
    // canvas), right-click barrel button, release, and proximity-out leave.
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(TabletToolEventKind::ProximityIn, 150.0, 0.0, false)
    ));
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(TabletToolEventKind::Down, 150.0, 0.6, true)
    ));
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(TabletToolEventKind::Motion, 40.0, 0.6, true)
    ));
    assert!(rt.tablet_surface("studio", tool_ev(TabletToolEventKind::Up, 40.0, 0.0, false)));
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(TabletToolEventKind::Motion, 150.0, 0.0, false)
    ));
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(
            TabletToolEventKind::Button {
                button: 0x14b,
                pressed: true,
            },
            150.0,
            0.0,
            false,
        )
    ));
    assert!(rt.tablet_surface(
        "studio",
        tool_ev(TabletToolEventKind::ProximityOut, 150.0, 0.0, false)
    ));
    rt.step().unwrap();

    assert_eq!(rt.state().toolbar_hovered, 2);
    assert_eq!(rt.state().toolbar_clicks, 2); // tip down + barrel button down
    assert_eq!(
        rt.state().toolbar_down_buttons,
        vec![MouseButton::Left, MouseButton::Right]
    );
    assert_eq!(rt.state().surf_moves, vec![40.0, 150.0]);
    assert_eq!(rt.state().surf_ups, 1);
    assert_eq!(rt.state().toolbar_left, 2);

    // Express-key pad events route directly to Surface::on_tablet.
    assert!(rt.tablet_surface(
        "pad-only",
        TabletEvent::Pad(TabletPadEvent {
            pad_id: TabletPadId(1),
            tablet_id: Some(TabletId(1)),
            kind: TabletPadEventKind::Dial {
                dial: 0,
                delta: 1.0,
                value120: 120,
            },
            modifiers: Modifiers::empty(),
        })
    ));
    rt.step().unwrap();
    assert_eq!(
        rt.state().pad_events,
        vec![TabletPadEventKind::Dial {
            dial: 0,
            delta: 1.0,
            value120: 120,
        }]
    );
}
