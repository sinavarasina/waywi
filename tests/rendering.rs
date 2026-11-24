#![cfg(any(feature = "renderer-software", feature = "renderer-gpu"))]

#[cfg(feature = "image-png")]
#[path = "support/png.rs"]
mod png;

use waywi::prelude::*;

#[derive(Default)]
struct RenderState {
    sheet: Option<SpriteSheet>,
    sprite_frame: usize,
    canvas_color: Color,
}

#[derive(Clone)]
enum RenderMsg {
    #[cfg(feature = "image-png")]
    SetSpriteFrame(usize),
    SetCanvasColor(Color),
}

struct RenderTestApp;

impl Application for RenderTestApp {
    type State = RenderState;
    type Message = RenderMsg;

    fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
        #[cfg(feature = "image-png")]
        let sheet = {
            let bytes = png::make_rgba_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
            let asset = ctx.load_image_bytes(&bytes)?;
            Some(SpriteSheet::new(asset).frame_size(1, 1))
        };
        #[cfg(not(feature = "image-png"))]
        let sheet = {
            let _ = ctx;
            None
        };
        Ok(RenderState {
            sheet,
            sprite_frame: 0,
            canvas_color: Color::rgb(0, 200, 0),
        })
    }

    fn update(
        state: &mut Self::State,
        msg: Self::Message,
        _ctx: &mut EffectCtx<'_, Self::Message>,
    ) -> Result<()> {
        match msg {
            #[cfg(feature = "image-png")]
            RenderMsg::SetSpriteFrame(f) => state.sprite_frame = f,
            RenderMsg::SetCanvasColor(c) => state.canvas_color = c,
        }
        Ok(())
    }

    fn view(state: &Self::State) -> Surfaces<Self::Message> {
        let mut s = Surfaces::new();
        let canvas_color = state.canvas_color;

        let mut surf = Surface::floating("canvas", Output::Primary)
            .size((40.logical_px(), 40.logical_px()))
            .background(Color::rgb(10, 20, 30))
            .child(
                Rect::new()
                    .position(Position::absolute(0.logical_px(), 0.logical_px()))
                    .size((16.logical_px(), 16.logical_px()))
                    .fill(Color::rgb(255, 0, 0)),
            )
            .child(
                Canvas::new((16.logical_px(), 16.logical_px()), move |p| {
                    p.clear(canvas_color);
                })
                .cache_key((canvas_color.r, canvas_color.g, canvas_color.b))
                .position(Position::absolute(20.logical_px(), 0.logical_px())),
            );

        if let Some(ref sheet) = state.sheet {
            surf = surf.child(
                Sprite::new(sheet)
                    .frame(state.sprite_frame)
                    .size((8.logical_px(), 8.logical_px()))
                    .position(Position::absolute(0.logical_px(), 20.logical_px())),
            );
        }

        #[cfg(feature = "text")]
        {
            surf = surf.child(
                Text::new("Hi")
                    .font_size(14.logical_px())
                    .color(Color::WHITE)
                    .position(Position::absolute(12.logical_px(), 20.logical_px())),
            );
        }

        s.push(surf);
        s
    }
}

#[cfg(feature = "renderer-software")]
#[test]
fn software_renderer_draws_argb8888_rects_canvas_sprites_and_text() {
    let mut rt = HeadlessRuntime::<RenderTestApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
        RendererPreference::Software,
    )
    .unwrap();
    assert_rendered_pixels(&mut rt);
}

#[cfg(feature = "renderer-gpu")]
#[test]
fn gpu_renderer_draws_argb8888_rects_canvas_sprites_and_text() {
    match HeadlessRuntime::<RenderTestApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
        RendererPreference::Gpu,
    ) {
        Ok(mut rt) => {
            if rt.capabilities().has(Capability::GpuRenderer) {
                assert_rendered_pixels(&mut rt);
            }
        }
        Err(WaywiError::UnsupportedCapability(Capability::GpuRenderer)) => {}
        Err(err) => panic!("unexpected GPU headless init error: {err}"),
    }
}

fn assert_rendered_pixels(rt: &mut HeadlessRuntime<RenderTestApp>) {
    let buf = rt.rendered_buffer("canvas").expect("buffer rendered");
    assert_eq!(buf.len(), 40 * 40 * 4);

    // Pixel (2, 2): red Rect in little-endian ARGB8888 = [B=0, G=0, R=255, A=255].
    let p_red = &buf[(2 * 40 + 2) * 4..(2 * 40 + 2) * 4 + 4];
    assert_eq!(p_red, &[0, 0, 255, 255]);

    // Pixel (24, 2): green Canvas in little-endian ARGB8888 = [0, 200, 0, 255].
    let p_canvas = &buf[(2 * 40 + 24) * 4..(2 * 40 + 24) * 4 + 4];
    assert_eq!(p_canvas, &[0, 200, 0, 255]);

    // Sprite frame 0 at (2, 22): pure red -> [0, 0, 255, 255].
    #[cfg(feature = "image-png")]
    {
        let p_sprite0 = &buf[(22 * 40 + 2) * 4..(22 * 40 + 2) * 4 + 4];
        assert_eq!(p_sprite0, &[0, 0, 255, 255]);

        rt.enqueue(RenderMsg::SetSpriteFrame(1));
        rt.step().unwrap();
        let buf2 = rt.rendered_buffer("canvas").unwrap();
        let p_sprite1 = &buf2[(22 * 40 + 2) * 4..(22 * 40 + 2) * 4 + 4];
        assert_eq!(p_sprite1, &[255, 0, 0, 255]);
    }

    // Canvas render key change redraws Canvas tile.
    rt.enqueue(RenderMsg::SetCanvasColor(Color::rgb(12, 34, 56)));
    rt.step().unwrap();
    let buf3 = rt.rendered_buffer("canvas").unwrap();
    let p_canvas2 = &buf3[(2 * 40 + 24) * 4..(2 * 40 + 24) * 4 + 4];
    assert_eq!(p_canvas2, &[56, 34, 12, 255]);
}

#[cfg(feature = "renderer-software")]
#[test]
fn scroll_state_clip_and_translate_blocks_clipped_clicks_and_pixels() {
    struct ScrollClipState {
        scroll: ScrollState,
        clicked: Vec<u8>,
    }

    #[derive(Clone)]
    enum ScrollClipMsg {
        Scrolled(ScrollEvent),
        Clicked(u8),
    }

    struct ScrollClipApp;

    impl Application for ScrollClipApp {
        type State = ScrollClipState;
        type Message = ScrollClipMsg;

        fn init(_ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            Ok(ScrollClipState {
                scroll: ScrollState::new(20.0, 40.0),
                clicked: Vec::new(),
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                ScrollClipMsg::Scrolled(ev) => {
                    state.scroll.apply_scroll(&ev, 20.0);
                }
                ScrollClipMsg::Clicked(id) => {
                    state.clicked.push(id);
                }
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            let viewport = Clip::new(
                Column::new()
                    .size((40.logical_px(), 40.logical_px()))
                    .transform(
                        Transform::identity()
                            .translate(0.logical_px(), -state.scroll.offset().logical_px()),
                    )
                    .child(
                        Rect::new()
                            .key("item-1")
                            .size((40.logical_px(), 20.logical_px()))
                            .fill(Color::rgb(255, 0, 0))
                            .on_click(ScrollClipMsg::Clicked(1)),
                    )
                    .child(
                        Rect::new()
                            .key("item-2")
                            .size((40.logical_px(), 20.logical_px()))
                            .fill(Color::rgb(0, 255, 0))
                            .on_click(ScrollClipMsg::Clicked(2)),
                    ),
            )
            .position(Position::absolute(0.logical_px(), 10.logical_px()))
            .size((40.logical_px(), 20.logical_px()))
            .on_scroll(ScrollClipMsg::Scrolled);

            s.push(
                Surface::floating("scroll-clip", Output::Primary)
                    .size((40.logical_px(), 40.logical_px()))
                    .background(Color::rgb(10, 20, 30))
                    .child(viewport),
            );
            s
        }
    }

    let mut rt = HeadlessRuntime::<ScrollClipApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
        RendererPreference::Software,
    )
    .unwrap();
    rt.step().unwrap();

    let pixel_at = |buf: &[u8], x: usize, y: usize| -> [u8; 4] {
        let i = (y * 40 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };

    // Before scroll (offset = 0): Clip covers y = 10..30; item-1 (red) is visible, item-2 (green) is clipped at y >= 30.
    let buf0 = rt.rendered_buffer("scroll-clip").unwrap();
    assert_eq!(pixel_at(buf0, 10, 5), [30, 20, 10, 255]);
    assert_eq!(pixel_at(buf0, 10, 15), [0, 0, 255, 255]);
    assert_eq!(pixel_at(buf0, 10, 35), [30, 20, 10, 255]);

    // Click outside Clip (above and below) must NOT hit either item.
    assert!(!rt.click_surface_point("scroll-clip", 10.0, 5.0));
    assert!(!rt.click_surface_point("scroll-clip", 10.0, 35.0));
    assert!(rt.click_surface_point("scroll-clip", 10.0, 15.0));
    rt.step().unwrap();
    assert_eq!(rt.state().clicked, vec![1]);

    // Scroll down while pointer hovers over item-1 inside Clip -> Clip receives on_scroll and shifts offset by +20.
    assert!(rt.scroll_surface_point("scroll-clip", 10.0, 15.0, (0.0, 0.0), (0, 1)));
    rt.step().unwrap();
    assert!((rt.state().scroll.offset() - 20.0).abs() < f32::EPSILON);

    // After scroll (offset = 20): item-1 is shifted above Clip (y = -10..10), item-2 occupies y = 10..30 inside Clip.
    let buf1 = rt.rendered_buffer("scroll-clip").unwrap();
    assert_eq!(pixel_at(buf1, 10, 5), [30, 20, 10, 255]);
    assert_eq!(pixel_at(buf1, 10, 15), [0, 255, 0, 255]);
    assert_eq!(pixel_at(buf1, 10, 35), [30, 20, 10, 255]);

    // Clicking above Clip (where item-1 was translated) must NOT hit item-1; clicking inside Clip now hits item-2.
    assert!(!rt.click_surface_point("scroll-clip", 10.0, 5.0));
    assert!(rt.click_surface_point("scroll-clip", 10.0, 15.0));
    rt.step().unwrap();
    assert_eq!(rt.state().clicked, vec![1, 2]);
}

#[cfg(all(feature = "renderer-software", feature = "text"))]
#[test]
fn proportional_text_cursor_line_renders_at_measured_x_inside_text() {
    struct CursorRenderState {
        input: TextInputState,
        metrics: TextCursorMetrics,
        narrow_x: LogicalPx,
        wide_x: LogicalPx,
    }

    #[derive(Clone)]
    enum CursorRenderMsg {
        MoveLeft,
    }

    struct CursorRenderApp;

    impl Application for CursorRenderApp {
        type State = CursorRenderState;
        type Message = CursorRenderMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            let mut input = TextInputState::with_text("iiiWWW");
            // Place cursor strictly in the middle (byte 3, between "iii" and "WWW").
            input.set_cursor_bytes(3, false);

            let font = Font::sans();
            let font_size = 20.logical_px();
            let metrics = ctx.measure_text_cursor(
                &input.display_text(),
                &font,
                font_size,
                input.display_cursor_bytes(),
            );
            let narrow_x = ctx.measure_text_cursor("iii", &font, font_size, 3).cursor_x;
            let wide_x = ctx.measure_text_cursor("WWW", &font, font_size, 3).cursor_x;

            Ok(CursorRenderState {
                input,
                metrics,
                narrow_x,
                wide_x,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                CursorRenderMsg::MoveLeft => {
                    state.input.move_left(false);
                    state.metrics = ctx.measure_text_cursor(
                        &state.input.display_text(),
                        &Font::sans(),
                        20.logical_px(),
                        state.input.display_cursor_bytes(),
                    );
                }
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            let origin_x = 12.logical_px();
            let origin_y = 10.logical_px();
            let cursor_x = origin_x + state.metrics.cursor_x;
            let cursor_y = origin_y + state.metrics.cursor_y;

            s.push(
                Surface::floating("prop-cursor", Output::Primary)
                    .size((180.logical_px(), 48.logical_px()))
                    .background(Color::rgb(12, 16, 22))
                    .ime(
                        ImeConfig::enabled()
                            .from_text_input(&state.input)
                            .cursor_rect(Bounds::new(
                                cursor_x,
                                cursor_y,
                                2.logical_px(),
                                state.metrics.line_height,
                            )),
                    )
                    .child(
                        Text::new(state.input.display_text())
                            .font(Font::sans())
                            .font_size(20.logical_px())
                            .color(Color::WHITE)
                            .position(Position::absolute(origin_x, origin_y)),
                    )
                    .child(
                        Rect::new()
                            .size((2.logical_px(), state.metrics.line_height))
                            .fill(Color::rgb(0, 255, 0))
                            .position(Position::absolute(cursor_x, cursor_y)),
                    ),
            );
            s
        }
    }

    let mut rt = HeadlessRuntime::<CursorRenderApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
        RendererPreference::Software,
    )
    .unwrap();
    rt.step().unwrap();

    let st = rt.state();
    assert!(st.narrow_x.0 < st.wide_x.0);
    assert!(st.metrics.cursor_x.0 > 0.0 && st.metrics.cursor_x.0 < st.metrics.size.width.0 * 0.5);

    let px_x = (12.0 + st.metrics.cursor_x.0).round() as usize;
    let px_y = 14;
    let buf = rt.rendered_buffer("prop-cursor").unwrap();
    let idx = (px_y * 180 + px_x) * 4;
    assert_eq!(&buf[idx..idx + 4], &[0, 255, 0, 255]);

    // Moving cursor left 1 char reduces cursor_x and moves the rendered cursor bar & IME rect.
    let prev_x = st.metrics.cursor_x.0;
    rt.enqueue(CursorRenderMsg::MoveLeft);
    rt.step().unwrap();
    let new_x = rt.state().metrics.cursor_x.0;
    assert!(new_x < prev_x && new_x > 0.0);
    let new_px_x = (12.0 + new_x).round() as usize;
    let buf2 = rt.rendered_buffer("prop-cursor").unwrap();
    let idx2 = (px_y * 180 + new_px_x) * 4;
    assert_eq!(&buf2[idx2..idx2 + 4], &[0, 255, 0, 255]);
    assert_eq!(
        rt.surface_ime_config("prop-cursor")
            .and_then(|c| c.cursor_rect)
            .map(|r| r.x.0),
        Some(12.0 + new_x)
    );
}

#[cfg(all(feature = "renderer-software", feature = "animation"))]
#[test]
fn sprite_flip_rotate_nearest_and_timed_on_animation_end_work_end_to_end() {
    use std::time::{Duration, Instant};

    struct SpriteTestState {
        sheet: SpriteSheet,
        flip_x: bool,
        rotate_90: bool,
        anim_ended_count: usize,
        unrelated_tick: usize,
    }

    #[derive(Clone)]
    enum SpriteTestMsg {
        SetFlipX(bool),
        SetRotate90(bool),
        AnimEnded,
        BumpUnrelated,
    }

    struct SpriteTestApp;

    impl Application for SpriteTestApp {
        type State = SpriteTestState;
        type Message = SpriteTestMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            // 4x2 sheet containing two 2x2 cells:
            // Frame 0 (cols 0..2): TL=Red, TR=Green, BL=Blue, BR=White.
            // Frame 1 (cols 2..4): solid Yellow.
            #[rustfmt::skip]
            let pixels: [u8; 4 * 2 * 4] = [
                // Row 0: (0,0) Red, (1,0) Green | (2,0) Yellow, (3,0) Yellow
                255, 0, 0, 255,   0, 255, 0, 255,   255, 255, 0, 255,   255, 255, 0, 255,
                // Row 1: (0,1) Blue, (1,1) White | (2,1) Yellow, (3,1) Yellow
                0, 0, 255, 255,   255, 255, 255, 255, 255, 255, 0, 255, 255, 255, 0, 255,
            ];
            let asset = ctx.load_rgba_image(4, 2, 16, true, &pixels)?;
            let mut sheet = SpriteSheet::new(asset).frame_size(2, 2).timed_animation(
                String::from("land_once"),
                [
                    (0, Duration::from_millis(60)),
                    (1, Duration::from_millis(90)),
                ],
                PlaybackMode::Once,
            );
            // Register >16 dynamic animations to verify unbounded animation storage.
            for i in 0..24 {
                sheet =
                    sheet.animation(format!("dynamic_{i}"), Frames::sequence([0, 1, 0]), 8.fps());
            }
            assert_eq!(sheet.animation_count(), 25);
            assert!(sheet.get_animation("dynamic_23").is_some());

            Ok(SpriteTestState {
                sheet,
                flip_x: false,
                rotate_90: false,
                anim_ended_count: 0,
                unrelated_tick: 0,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                SpriteTestMsg::SetFlipX(v) => state.flip_x = v,
                SpriteTestMsg::SetRotate90(v) => state.rotate_90 = v,
                SpriteTestMsg::AnimEnded => state.anim_ended_count += 1,
                SpriteTestMsg::BumpUnrelated => state.unrelated_tick += 1,
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            let mut transform = Transform::identity();
            if state.rotate_90 {
                transform = transform.rotate_deg(90.0);
            }
            // Static asymmetric sprite scaled 4x (2x2 -> 8x8) at (10, 10) with Nearest filter.
            let static_sprite = Sprite::new(&state.sheet)
                .key("asymmetric")
                .frame(0)
                .filter(ImageFilter::Nearest)
                .flip_x(state.flip_x)
                .transform(transform)
                .position(Position::absolute(10.logical_px(), 10.logical_px()))
                .size((8.logical_px(), 8.logical_px()));

            // Timed one-shot animated sprite inside a memoized element verifying deadline
            // preservation and `on_animation_end` delivery after the final frame's full duration.
            let sheet_clone = state.sheet.clone();
            let animated_sprite = Element::memo("memo-anim", &(), move |()| {
                Sprite::new(&sheet_clone)
                    .key("once-anim")
                    .playing("land_once")
                    .filter(ImageFilter::Nearest)
                    .on_animation_end(SpriteTestMsg::AnimEnded)
                    .position(Position::absolute(24.logical_px(), 10.logical_px()))
                    .size((4.logical_px(), 4.logical_px()))
                    .into()
            });

            s.push(
                Surface::floating("sprite-surf", Output::Primary)
                    .size((40.logical_px(), 32.logical_px()))
                    .background(Color::BLACK)
                    .child(static_sprite)
                    .child(animated_sprite),
            );
            s
        }
    }

    let mut rt = HeadlessRuntime::<SpriteTestApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
        RendererPreference::Software,
    )
    .unwrap();

    let pixel = |buf: &[u8], x: usize, y: usize| -> [u8; 4] {
        let i = (y * 40 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };

    // Unflipped 8x8 nearest-neighbor sprite at (10..18, 10..18):
    // TL=(11,11) Red [0,0,255,255], TR=(16,11) Green [0,255,0,255],
    // BL=(11,16) Blue [255,0,0,255], BR=(16,16) White [255,255,255,255].
    let buf0 = rt.rendered_buffer("sprite-surf").unwrap();
    assert_eq!(pixel(buf0, 11, 11), [0, 0, 255, 255]);
    assert_eq!(pixel(buf0, 16, 11), [0, 255, 0, 255]);
    assert_eq!(pixel(buf0, 11, 16), [255, 0, 0, 255]);
    assert_eq!(pixel(buf0, 16, 16), [255, 255, 255, 255]);

    // Flip horizontally: stays strictly inside (10..18, 10..18), left and right halves swap!
    rt.enqueue(SpriteTestMsg::SetFlipX(true));
    rt.step().unwrap();
    let buf1 = rt.rendered_buffer("sprite-surf").unwrap();
    assert_eq!(pixel(buf1, 11, 11), [0, 255, 0, 255]); // Green now on left
    assert_eq!(pixel(buf1, 16, 11), [0, 0, 255, 255]); // Red now on right
    assert_eq!(pixel(buf1, 11, 16), [255, 255, 255, 255]); // White now on left
    assert_eq!(pixel(buf1, 16, 16), [255, 0, 0, 255]); // Blue now on right
    // Outside rect remains untouched black background.
    assert_eq!(pixel(buf1, 9, 11), [0, 0, 0, 255]);
    assert_eq!(pixel(buf1, 19, 11), [0, 0, 0, 255]);

    // Unflip and rotate 90 deg clockwise around center:
    // Old TL (Red) moves to TR (16, 11); old TR (Green) moves to BR (16, 16);
    // old BR (White) moves to BL (11, 16); old BL (Blue) moves to TL (11, 11).
    rt.enqueue(SpriteTestMsg::SetFlipX(false));
    rt.enqueue(SpriteTestMsg::SetRotate90(true));
    rt.step().unwrap();
    let buf2 = rt.rendered_buffer("sprite-surf").unwrap();
    assert_eq!(pixel(buf2, 16, 11), [0, 0, 255, 255]); // Red -> TR
    assert_eq!(pixel(buf2, 16, 16), [0, 255, 0, 255]); // Green -> BR
    assert_eq!(pixel(buf2, 11, 16), [255, 255, 255, 255]); // White -> BL
    assert_eq!(pixel(buf2, 11, 11), [255, 0, 0, 255]); // Blue -> TL

    // Advance timed `land_once` clip (60ms frame 0, then 90ms frame 1 = 150ms total):
    let t_base = Instant::now();
    // At +70ms: should be showing frame 1 (solid Yellow [0,255,255,255]) and NOT yet ended.
    rt.step_at(t_base + Duration::from_millis(70)).unwrap();
    let buf_mid = rt.rendered_buffer("sprite-surf").unwrap();
    assert_eq!(pixel(buf_mid, 25, 11), [0, 255, 255, 255]);
    assert_eq!(rt.state().anim_ended_count, 0);

    // Trigger an unrelated reconcile hitting the memoized sprite at +100ms: must still not end early.
    rt.enqueue(SpriteTestMsg::BumpUnrelated);
    rt.step_at(t_base + Duration::from_millis(100)).unwrap();
    assert_eq!(rt.state().anim_ended_count, 0);

    // At +180ms (> 150ms): clip completes and fires `on_animation_end` exactly once!
    rt.step_at(t_base + Duration::from_millis(180)).unwrap();
    assert_eq!(rt.state().anim_ended_count, 1);

    // Further ticks must not re-fire `on_animation_end`.
    rt.step_at(t_base + Duration::from_millis(400)).unwrap();
    assert_eq!(rt.state().anim_ended_count, 1);
}

#[cfg(all(feature = "renderer-software", feature = "animation"))]
#[test]
fn layered_paper_doll_character_from_irregular_atlas_and_custom_pivot() {
    use std::time::{Duration, Instant};

    struct LayeredState {
        atlas: ImageAsset,
        eye_sheet: SpriteSheet,
        happy_mouth: bool,
        arm_angle_deg: f32,
    }

    #[derive(Clone)]
    enum LayeredMsg {
        SetHappyMouth(bool),
        SetArmAngle(f32),
    }

    struct LayeredApp;

    impl Application for LayeredApp {
        type State = LayeredState;
        type Message = LayeredMsg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            // Single 16x16 irregular texture atlas:
            // - Body (0, 0, 8, 12): solid Blue [0, 0, 255, 255]
            // - Eyes open (8, 0, 4, 2): solid Green [0, 255, 0, 255]
            // - Eyes closed (8, 2, 4, 2): solid Yellow [255, 255, 0, 255]
            // - Mouth neutral (8, 4, 2, 2): solid White [255, 255, 255, 255]
            // - Mouth happy (10, 4, 2, 2): solid Red [255, 0, 0, 255]
            // - Arm (8, 8, 6, 2): solid Cyan [0, 255, 255, 255]
            let mut pixels = vec![0u8; 16 * 16 * 4];
            let mut fill_rect = |rx: usize, ry: usize, rw: usize, rh: usize, rgba: [u8; 4]| {
                for y in ry..(ry + rh) {
                    for x in rx..(rx + rw) {
                        let off = (y * 16 + x) * 4;
                        pixels[off..off + 4].copy_from_slice(&rgba);
                    }
                }
            };
            fill_rect(0, 0, 8, 12, [0, 0, 255, 255]);
            fill_rect(8, 0, 4, 2, [0, 255, 0, 255]);
            fill_rect(8, 2, 4, 2, [255, 255, 0, 255]);
            fill_rect(8, 4, 2, 2, [255, 255, 255, 255]);
            fill_rect(10, 4, 2, 2, [255, 0, 0, 255]);
            fill_rect(8, 8, 6, 2, [0, 255, 255, 255]);

            let atlas = ctx.load_rgba_image(16, 16, 64, true, &pixels)?;
            let eye_sheet = SpriteSheet::from_regions(
                atlas,
                [SourceRect::new(8, 0, 4, 2), SourceRect::new(8, 2, 4, 2)],
            )
            .timed_animation(
                "blink",
                [
                    (0, Duration::from_millis(50)),
                    (1, Duration::from_millis(50)),
                ],
                PlaybackMode::Loop,
            );

            assert_eq!(eye_sheet.total_frames(), 2);
            assert_eq!(eye_sheet.frame_source_rect(0), (8, 0, 4, 2));
            assert_eq!(eye_sheet.frame_source_rect(1), (8, 2, 4, 2));

            Ok(LayeredState {
                atlas,
                eye_sheet,
                happy_mouth: false,
                arm_angle_deg: 0.0,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                LayeredMsg::SetHappyMouth(v) => state.happy_mouth = v,
                LayeredMsg::SetArmAngle(deg) => state.arm_angle_deg = deg,
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut s = Surfaces::new();
            let mouth_region = if state.happy_mouth {
                SourceRect::new(10, 4, 2, 2)
            } else {
                SourceRect::new(8, 4, 2, 2)
            };

            let character = Group::new()
                .position(Position::absolute(10.logical_px(), 10.logical_px()))
                .size((24.logical_px(), 24.logical_px()))
                // Layer 1: Base body (auto-sized to 8x12 from source region, using Bicubic filter)
                .child(
                    Image::new(state.atlas)
                        .region((0, 0, 8, 12))
                        .filter(ImageFilter::Bicubic),
                )
                // Layer 2: Blinking eyes from irregular atlas SpriteSheet at (2, 2)
                .child(
                    Sprite::new(&state.eye_sheet)
                        .key("eyes")
                        .playing("blink")
                        .filter(ImageFilter::Nearest)
                        .position(Position::absolute(2.logical_px(), 2.logical_px())),
                )
                // Layer 3: Swappable mouth region at (3, 6)
                .child(
                    Image::new(state.atlas)
                        .region(mouth_region)
                        .filter(ImageFilter::Nearest)
                        .position(Position::absolute(3.logical_px(), 6.logical_px())),
                )
                // Layer 4: Arm attached at shoulder (8, 4), rotating around left-center pivot (0.0, 0.5)
                .child(
                    Image::new(state.atlas)
                        .region((8, 8, 6, 2))
                        .filter(ImageFilter::Nearest)
                        .position(Position::absolute(8.logical_px(), 4.logical_px()))
                        .transform(
                            Transform::identity()
                                .pivot(0.0, 0.5)
                                .rotate_deg(state.arm_angle_deg),
                        ),
                );

            s.push(
                Surface::floating("char-surf", Output::Primary)
                    .size((40.logical_px(), 40.logical_px()))
                    .background(Color::BLACK)
                    .child(character),
            );
            s
        }
    }

    let mut rt = HeadlessRuntime::<LayeredApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
        RendererPreference::Software,
    )
    .unwrap();

    let pixel = |buf: &[u8], x: usize, y: usize| -> [u8; 4] {
        let i = (y * 40 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };

    // Initial pose (BGRA little-endian):
    // Body at (10..18, 10..22): Blue [255, 0, 0, 255]
    // Eyes open at (12..16, 12..14): Green [0, 255, 0, 255]
    // Mouth neutral at (13..15, 16..18): White [255, 255, 255, 255]
    // Arm horizontal at (18..24, 14..16): Cyan [255, 255, 0, 255]
    let buf0 = rt.rendered_buffer("char-surf").unwrap();
    assert_eq!(pixel(buf0, 10, 10), [255, 0, 0, 255]); // body
    assert_eq!(pixel(buf0, 13, 12), [0, 255, 0, 255]); // eyes open
    assert_eq!(pixel(buf0, 13, 16), [255, 255, 255, 255]); // neutral mouth
    assert_eq!(pixel(buf0, 22, 14), [255, 255, 0, 255]); // horizontal arm

    // Swap mouth to happy (Red) & swing arm 90 deg down around shoulder pivot (18, 15):
    // Swung arm should now occupy x=17..19, y=15..21 (pointing straight down from shoulder!).
    rt.enqueue(LayeredMsg::SetHappyMouth(true));
    rt.enqueue(LayeredMsg::SetArmAngle(90.0));
    let t_base = Instant::now();
    // Advance +60ms so eyes also blink to closed (Yellow [0, 255, 255, 255]).
    rt.step_at(t_base + Duration::from_millis(60)).unwrap();

    let buf1 = rt.rendered_buffer("char-surf").unwrap();
    assert_eq!(pixel(buf1, 10, 10), [255, 0, 0, 255]); // body unchanged
    assert_eq!(pixel(buf1, 13, 12), [0, 255, 255, 255]); // eyes blinked closed (yellow)
    assert_eq!(pixel(buf1, 13, 16), [0, 0, 255, 255]); // happy mouth (red)
    assert_eq!(pixel(buf1, 22, 14), [0, 0, 0, 255]); // old horizontal arm cleared
    assert_eq!(pixel(buf1, 18, 19), [255, 255, 0, 255]); // arm now points downward from shoulder!
}

#[cfg(all(feature = "renderer-software", feature = "text"))]
#[test]
fn optical_text_bounds_centers_ink_pixels_symmetrically_in_box() {
    struct OpticalApp;

    impl Application for OpticalApp {
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
            s.push(
                Surface::floating("optical-box", Output::Primary)
                    .size((48.logical_px(), 48.logical_px()))
                    .buffer_retention(BufferRetention::ReleaseAfterCommit)
                    .background(Color::TRANSPARENT)
                    .child(
                        Text::new("O")
                            .font(Font::sans())
                            .font_size(24.logical_px())
                            .line_height(1.5)
                            .bounds_mode(TextBoundsMode::Optical)
                            .color(Color::WHITE)
                            .position(Position::absolute(24.logical_px(), 24.logical_px()))
                            .anchor(ElementAnchor::Center),
                    ),
            );
            s
        }
    }

    let rt = HeadlessRuntime::<OpticalApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
        RendererPreference::Software,
    )
    .unwrap();

    let buf = rt.rendered_buffer("optical-box").unwrap();
    let mut min_x = 48usize;
    let mut max_x = 0usize;
    let mut min_y = 48usize;
    let mut max_y = 0usize;
    for y in 0..48 {
        for x in 0..48 {
            let alpha = buf[(y * 48 + x) * 4 + 3];
            if alpha > 0 {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    assert!(min_x < max_x && min_y < max_y);
    // Optical center of inked pixels must lie within <= 1.0 px of box center (23.5, 23.5).
    let ink_cx = (min_x + max_x) as f32 * 0.5;
    let ink_cy = (min_y + max_y) as f32 * 0.5;
    assert!(
        (ink_cx - 23.5).abs() <= 1.0,
        "optical horizontal center {ink_cx} drifted from 23.5 (bounds {min_x}..={max_x})"
    );
    assert!(
        (ink_cy - 23.5).abs() <= 1.0,
        "optical vertical center {ink_cy} drifted from 23.5 (bounds {min_y}..={max_y})"
    );
}
