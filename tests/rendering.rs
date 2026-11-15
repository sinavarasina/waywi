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
