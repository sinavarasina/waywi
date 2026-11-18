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

    fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
        #[cfg(feature = "image-png")]
        let sheet = {
            let bytes = png::make_rgba_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
            let asset = ctx.load_image_bytes(&bytes)?;
            Some(SpriteSheet::new(asset).frame_size(1.logical_px(), 1.logical_px()))
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

    fn update(state: &mut Self::State, msg: Self::Message, _ctx: &mut EffectCtx<'_>) -> Result<()> {
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
            .size(Size::px(40.logical_px(), 40.logical_px()))
            .background(Color::rgb(10, 20, 30))
            .child(
                Rect::new()
                    .position(Position::absolute(0.logical_px(), 0.logical_px()))
                    .size(Size::px(16.logical_px(), 16.logical_px()))
                    .fill(Color::rgb(255, 0, 0)),
            )
            .child(
                Canvas::new(
                    Size::px(16.logical_px(), 16.logical_px()),
                    (canvas_color.r, canvas_color.g, canvas_color.b),
                    move |p| {
                        p.clear(canvas_color);
                    },
                )
                .position(Position::absolute(20.logical_px(), 0.logical_px())),
            );

        if let Some(sheet) = state.sheet {
            surf = surf.child(
                Sprite::new(sheet)
                    .frame(state.sprite_frame)
                    .size(Size::px(8.logical_px(), 8.logical_px()))
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

        fn init(_ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
            Ok(ScrollClipState {
                scroll: ScrollState::new(20.0, 40.0),
                clicked: Vec::new(),
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            _ctx: &mut EffectCtx<'_>,
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
                    .size(Size::px(40.logical_px(), 40.logical_px()))
                    .transform(
                        Transform::identity()
                            .translate(0.logical_px(), -state.scroll.offset().logical_px()),
                    )
                    .child(
                        Rect::new()
                            .key("item-1")
                            .size(Size::px(40.logical_px(), 20.logical_px()))
                            .fill(Color::rgb(255, 0, 0))
                            .on_click(ScrollClipMsg::Clicked(1)),
                    )
                    .child(
                        Rect::new()
                            .key("item-2")
                            .size(Size::px(40.logical_px(), 20.logical_px()))
                            .fill(Color::rgb(0, 255, 0))
                            .on_click(ScrollClipMsg::Clicked(2)),
                    ),
            )
            .position(Position::absolute(0.logical_px(), 10.logical_px()))
            .size(Size::px(40.logical_px(), 20.logical_px()))
            .on_scroll(ScrollClipMsg::Scrolled);

            s.push(
                Surface::floating("scroll-clip", Output::Primary)
                    .size(Size::px(40.logical_px(), 40.logical_px()))
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

        fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
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
            ctx: &mut EffectCtx<'_>,
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
                    .size(Size::px(180.logical_px(), 48.logical_px()))
                    .background(Color::rgb(12, 16, 22))
                    .ime(
                        ImeConfig::enabled()
                            .from_text_input(&state.input)
                            .cursor_rect(GeoRect::new(
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
                            .size(Size::px(2.logical_px(), state.metrics.line_height))
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
