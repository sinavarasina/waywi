#[cfg(feature = "image-png")]
#[path = "support/png.rs"]
mod png;
#[cfg(any(feature = "image-png", feature = "image-svg"))]
#[path = "support/temp_dir.rs"]
mod temp_dir;

use waywi::prelude::*;

struct AssetTestApp;

#[derive(Default)]
struct AssetTestState {
    asset: Option<ImageAsset>,
    svg_asset: Option<ImageAsset>,
    bad_err: Option<WaywiError>,
    reloaded: bool,
    unloaded: bool,
}

#[derive(Clone)]
enum AssetMsg {
    LoadSvg(Vec<u8>),
    #[cfg(feature = "image-png")]
    Reload(std::path::PathBuf),
    #[cfg(feature = "image-png")]
    Unload,
}

impl Application for AssetTestApp {
    type State = AssetTestState;
    type Message = AssetMsg;

    fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
        #[cfg(feature = "image-png")]
        let asset = {
            let bytes = png::make_rgba_png(
                2,
                2,
                &[
                    255, 0, 0, 255, 0, 255, 0, 255, // row 0
                    0, 0, 255, 255, 255, 255, 255, 255, // row 1
                ],
            );
            Some(ctx.load_image_bytes(&bytes)?)
        };
        #[cfg(not(feature = "image-png"))]
        let asset = None;
        let bad_err = ctx.load_image_bytes(b"not-an-image").err();
        Ok(AssetTestState {
            asset,
            svg_asset: None,
            bad_err,
            reloaded: false,
            unloaded: false,
        })
    }

    fn update(
        state: &mut Self::State,
        msg: Self::Message,
        ctx: &mut EffectCtx<'_, Self::Message>,
    ) -> Result<()> {
        match msg {
            AssetMsg::LoadSvg(svg_bytes) => {
                state.svg_asset = Some(ctx.load_image_bytes(&svg_bytes)?);
            }
            #[cfg(feature = "image-png")]
            AssetMsg::Reload(path) => {
                if let Some(a) = state.asset {
                    ctx.reload_image(a, path)?;
                    state.reloaded = true;
                }
            }
            #[cfg(feature = "image-png")]
            AssetMsg::Unload => {
                if let Some(a) = state.asset {
                    state.unloaded = ctx.unload_image(a);
                }
            }
        }
        Ok(())
    }

    fn view(_state: &Self::State) -> Surfaces<Self::Message> {
        Surfaces::new()
    }
}

#[test]
fn asset_decode_reload_unload_and_error_contracts() {
    let mut rt = HeadlessRuntime::<AssetTestApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
    )
    .expect("headless init");
    assert!(rt.state().bad_err.is_some());

    #[cfg(feature = "image-png")]
    {
        let asset = rt.state().asset.expect("PNG asset loaded");
        assert_eq!(asset.width, 2);
        assert_eq!(asset.height, 2);

        let dir = temp_dir::TestTempDir::new("asset-reload");
        let file_path = dir.path().join("reload.png");
        let new_png = png::make_rgba_png(4, 2, &[200u8; 4 * 2 * 4]);
        std::fs::write(&file_path, &new_png).unwrap();

        rt.enqueue(AssetMsg::Reload(file_path));
        rt.step().unwrap();
        assert!(rt.state().reloaded);

        rt.enqueue(AssetMsg::Unload);
        rt.step().unwrap();
        assert!(rt.state().unloaded);
    }

    #[cfg(not(feature = "image-png"))]
    {
        assert!(rt.state().asset.is_none());
        assert!(!rt.state().reloaded);
        assert!(!rt.state().unloaded);
    }

    #[cfg(feature = "image-svg")]
    {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="12"><rect width="16" height="12" fill="red"/></svg>"#;
        rt.enqueue(AssetMsg::LoadSvg(svg.to_vec()));
        rt.step().unwrap();
        let svg_asset = rt.state().svg_asset.expect("SVG asset loaded");
        assert_eq!(svg_asset.width, 16);
        assert_eq!(svg_asset.height, 12);
    }

    #[cfg(not(feature = "image-svg"))]
    {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="12"></svg>"#;
        rt.enqueue(AssetMsg::LoadSvg(svg.to_vec()));
        assert!(rt.step().is_err());
    }
}

#[test]
fn sprite_sheet_frame_and_playback_math() {
    #[cfg(feature = "image-png")]
    {
        struct SheetApp;
        #[derive(Default)]
        struct SheetState(Option<ImageAsset>);
        impl Application for SheetApp {
            type State = SheetState;
            type Message = ();
            fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
                let bytes = png::make_rgba_png(96, 48, &vec![255u8; 96 * 48 * 4]);
                Ok(SheetState(Some(ctx.load_image_bytes(&bytes)?)))
            }
            fn update(
                _: &mut Self::State,
                _: Self::Message,
                _: &mut EffectCtx<'_, Self::Message>,
            ) -> Result<()> {
                Ok(())
            }
            fn view(_: &Self::State) -> Surfaces<Self::Message> {
                Surfaces::new()
            }
        }
        let rt = HeadlessRuntime::<SheetApp>::new(
            Capabilities::from_compile_features(),
            vec![OutputInfo::default()],
        )
        .unwrap();
        let asset = rt.state().0.unwrap();
        let sheet = SpriteSheet::new(asset)
            .frame_size(32, 24)
            .animation("idle", Frames::range(0..3), 6.fps())
            .animation_with_mode(
                "bounce",
                Frames::range(0..4),
                10.fps(),
                PlaybackMode::PingPong,
            )
            .animation_with_mode("once", Frames::range(2..5), 10.fps(), PlaybackMode::Once);

        assert_eq!(sheet.columns(), 3);
        assert_eq!(sheet.rows(), 2);
        assert_eq!(sheet.total_frames(), 6);
        assert_eq!(sheet.frame_source_rect(4), (32, 24, 32, 24));

        let idle = sheet.get_animation("idle").unwrap();
        assert_eq!(idle.frames.frame_at(4, idle.mode), 1);

        let bounce = sheet.get_animation("bounce").unwrap();
        let seq: Vec<usize> = (0..7)
            .map(|s| bounce.frames.frame_at(s, bounce.mode))
            .collect();
        assert_eq!(seq, vec![0, 1, 2, 3, 2, 1, 0]);

        let once = sheet.get_animation("once").unwrap();
        assert_eq!(once.frames.frame_at(10, once.mode), 4);
    }

    let idle_frames = Frames::range(0..3);
    assert_eq!(idle_frames.frame_at(4, PlaybackMode::Loop), 1);
    let bounce_frames = Frames::range(0..4);
    let seq: Vec<usize> = (0..7)
        .map(|s| bounce_frames.frame_at(s, PlaybackMode::PingPong))
        .collect();
    assert_eq!(seq, vec![0, 1, 2, 3, 2, 1, 0]);
    assert_eq!(Frames::range(2..5).frame_at(10, PlaybackMode::Once), 4);
}

#[test]
fn raw_rgb_and_rgba_images_support_rowstride_and_premultiplication() {
    struct RawApp;
    #[derive(Default)]
    struct RawState {
        rgba_ok: Option<ImageAsset>,
        rgb_ok: Option<ImageAsset>,
        short_err: Option<WaywiError>,
    }
    impl Application for RawApp {
        type State = RawState;
        type Message = ();
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            // 2x2 RGBA with rowstride = 10 (2px*4 + 2 padding bytes on first row)
            let padded_rgba = vec![
                200, 100, 50, 128, 255, 255, 255, 255, 0, 0, // row 0 + pad
                10, 20, 30, 0, 40, 80, 120, 255, 0, 0, // row 1 + optional pad
            ];
            let rgba_ok = Some(ctx.load_rgba_image(2, 2, 10, true, &padded_rgba)?);
            let rgb = vec![10, 20, 30, 40, 50, 60];
            let rgb_ok = Some(ctx.load_rgba_image(2, 1, 6, false, &rgb)?);
            let short_err = ctx.load_rgba_image(4, 4, 16, true, &[0u8; 8]).err();
            Ok(RawState {
                rgba_ok,
                rgb_ok,
                short_err,
            })
        }
        fn update(
            _: &mut Self::State,
            _: Self::Message,
            _: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::new()
        }
    }

    let rt = HeadlessRuntime::<RawApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
    )
    .unwrap();
    assert_eq!(rt.state().rgba_ok.unwrap().width, 2);
    assert_eq!(rt.state().rgba_ok.unwrap().height, 2);
    assert_eq!(rt.state().rgb_ok.unwrap().width, 2);
    assert_eq!(rt.state().rgb_ok.unwrap().height, 1);
    assert!(matches!(
        rt.state().short_err,
        Some(WaywiError::AssetDecode(_))
    ));
}

#[cfg(feature = "image-svg")]
#[test]
fn sized_svg_loading_scales_small_icons_without_clobbering_intrinsic_cache() {
    struct SizedSvgApp;
    #[derive(Default)]
    struct SizedSvgState {
        intrinsic: Option<ImageAsset>,
        sized_bytes: Option<ImageAsset>,
        sized_file: Option<ImageAsset>,
    }
    impl Application for SizedSvgApp {
        type State = SizedSvgState;
        type Message = ();
        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="12"><rect width="16" height="12" fill="blue"/></svg>"#;
            let intrinsic = Some(ctx.load_image_bytes(svg)?);
            let sized_bytes = Some(ctx.load_image_bytes_sized(svg, 48)?);

            let dir = temp_dir::TestTempDir::new("sized-svg");
            let path = dir.path().join("tray.svg");
            std::fs::write(&path, svg).unwrap();
            let sized_file = Some(ctx.load_image_sized(&path, 48)?);

            Ok(SizedSvgState {
                intrinsic,
                sized_bytes,
                sized_file,
            })
        }
        fn update(
            _: &mut Self::State,
            _: Self::Message,
            _: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            Ok(())
        }
        fn view(_: &Self::State) -> Surfaces<Self::Message> {
            Surfaces::new()
        }
    }

    let rt = HeadlessRuntime::<SizedSvgApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
    )
    .unwrap();
    let st = rt.state();
    let intrinsic = st.intrinsic.unwrap();
    let sized_bytes = st.sized_bytes.unwrap();
    let sized_file = st.sized_file.unwrap();
    assert_eq!((intrinsic.width, intrinsic.height), (16, 12));
    assert_eq!((sized_bytes.width, sized_bytes.height), (48, 36));
    assert_eq!((sized_file.width, sized_file.height), (48, 36));
    assert_ne!(intrinsic.id, sized_bytes.id);
    assert_eq!(sized_bytes.id, sized_file.id);
}

#[cfg(all(feature = "image-png", feature = "renderer-software"))]
#[test]
fn reclaim_after_present_file_and_bytes_redecode_on_resize_and_reload() {
    let dir = temp_dir::TestTempDir::new("reclaim-assets");
    let red_path = dir.path().join("red.png");
    let blue_path = dir.path().join("blue.png");

    let red_png = png::make_rgba_png(2, 2, &[255, 0, 0, 255].repeat(4));
    let green_png = png::make_rgba_png(2, 2, &[0, 255, 0, 255].repeat(4));
    let blue_png = png::make_rgba_png(2, 2, &[0, 0, 255, 255].repeat(4));
    std::fs::write(&red_path, &red_png).unwrap();
    std::fs::write(&blue_path, &blue_png).unwrap();

    struct ReclaimApp;
    struct ReclaimState {
        file_asset: ImageAsset,
        bytes_asset: ImageAsset,
        raw_reclaim_rejected: bool,
        raw_evict_rejected: bool,
        retention_after_reload: Option<AssetRetention>,
    }
    #[derive(Clone)]
    enum Msg {
        ReloadFile(std::path::PathBuf),
    }

    static RED_PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    static GREEN_PNG: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let _ = RED_PATH.set(red_path.clone());
    let _ = GREEN_PNG.set(green_png.clone());

    impl Application for ReclaimApp {
        type State = ReclaimState;
        type Message = Msg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            let file_asset = ctx.load_image(RED_PATH.get().unwrap())?;
            let bytes_asset = ctx.load_image_bytes(GREEN_PNG.get().unwrap())?;
            assert!(ctx.set_image_retention(file_asset, AssetRetention::ReclaimAfterPresent));
            assert!(ctx.set_image_retention(bytes_asset, AssetRetention::ReclaimAfterPresent));
            assert_eq!(
                ctx.image_retention(file_asset),
                Some(AssetRetention::ReclaimAfterPresent)
            );

            let raw = ctx.load_rgba_image(1, 1, 4, true, &[255, 255, 255, 255])?;
            let raw_reclaim_rejected =
                !ctx.set_image_retention(raw, AssetRetention::ReclaimAfterPresent);
            let raw_evict_rejected = !ctx.evict_image_pixels(raw);
            assert_eq!(ctx.image_retention(raw), Some(AssetRetention::KeepDecoded));
            assert!(ctx.unload_image(raw));

            Ok(ReclaimState {
                file_asset,
                bytes_asset,
                raw_reclaim_rejected,
                raw_evict_rejected,
                retention_after_reload: None,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::ReloadFile(path) => {
                    ctx.reload_image(state.file_asset, path)?;
                    state.retention_after_reload = ctx.image_retention(state.file_asset);
                }
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut out = Surfaces::new();
            out.push(
                Surface::overlay("file-wp", Output::primary())
                    .layer(Layer::Background)
                    .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
                    .buffer_retention(BufferRetention::ReleaseAfterCommit)
                    .size(Size::fill())
                    .child(
                        Image::new(state.file_asset)
                            .size(Size::fill())
                            .fit(ImageFit::Fill),
                    ),
            );
            out.push(
                Surface::overlay("bytes-wp", Output::primary())
                    .layer(Layer::Background)
                    .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
                    .buffer_retention(BufferRetention::ReleaseAfterCommit)
                    .size(Size::fill())
                    .child(
                        Image::new(state.bytes_asset)
                            .size(Size::fill())
                            .fit(ImageFit::Fill),
                    ),
            );
            out
        }
    }

    let mut rt = HeadlessRuntime::<ReclaimApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            logical_size: LogicalSize::new(LogicalPx(16.0), LogicalPx(16.0)),
            ..OutputInfo::default()
        }],
        RendererPreference::Software,
    )
    .unwrap();

    assert!(rt.state().raw_reclaim_rejected);
    assert!(rt.state().raw_evict_rejected);
    let file_asset = rt.state().file_asset;
    let bytes_asset = rt.state().bytes_asset;

    // Initial render succeeded with exact ARGB8888 (little-endian BGRA) colors,
    // and both ReclaimAfterPresent assets immediately dropped their decoded pixels.
    assert_eq!(
        &rt.rendered_buffer("file-wp").unwrap()[0..4],
        &[0, 0, 255, 255]
    );
    assert_eq!(
        &rt.rendered_buffer("bytes-wp").unwrap()[0..4],
        &[0, 255, 0, 255]
    );
    assert!(!rt.asset_pixels_resident(file_asset));
    assert!(!rt.asset_pixels_resident(bytes_asset));
    assert_eq!(rt.decoded_asset_bytes(), 0);

    // Simulate monitor/surface resize: evicted assets must transparently re-decode,
    // paint the resized surfaces cleanly, and reclaim back to 0 decoded bytes.
    assert!(rt.simulate_surface_configure(
        "file-wp",
        LogicalSize::new(LogicalPx(32.0), LogicalPx(32.0))
    ));
    assert!(rt.simulate_surface_configure(
        "bytes-wp",
        LogicalSize::new(LogicalPx(32.0), LogicalPx(32.0))
    ));
    rt.step().unwrap();

    let file_buf = rt.rendered_buffer("file-wp").unwrap();
    let bytes_buf = rt.rendered_buffer("bytes-wp").unwrap();
    assert_eq!(file_buf.len(), 32 * 32 * 4);
    assert_eq!(bytes_buf.len(), 32 * 32 * 4);
    assert_eq!(&file_buf[0..4], &[0, 0, 255, 255]);
    assert_eq!(&bytes_buf[0..4], &[0, 255, 0, 255]);
    assert!(!rt.asset_pixels_resident(file_asset));
    assert!(!rt.asset_pixels_resident(bytes_asset));
    assert_eq!(rt.decoded_asset_bytes(), 0);

    // Reloading preserves ReclaimAfterPresent and reclaims after presenting the new image.
    rt.enqueue(Msg::ReloadFile(blue_path));
    rt.step().unwrap();
    assert_eq!(
        rt.state().retention_after_reload,
        Some(AssetRetention::ReclaimAfterPresent)
    );
    assert_eq!(
        &rt.rendered_buffer("file-wp").unwrap()[0..4],
        &[255, 0, 0, 255]
    );
    assert!(!rt.asset_pixels_resident(file_asset));
    assert_eq!(rt.decoded_asset_bytes(), 0);
}

#[cfg(all(feature = "image-png", feature = "renderer-software"))]
#[test]
fn evict_asset_pixels_on_sprite_sheet_auto_redecodes_and_stays_resident() {
    struct SpriteEvictApp;
    struct SpriteEvictState {
        sheet: SpriteSheet,
        visible: bool,
        frame: usize,
        first_evict: bool,
        second_evict: bool,
    }
    #[derive(Clone)]
    enum Msg {
        HideAndEvict,
        ShowFrame(usize),
    }

    impl Application for SpriteEvictApp {
        type State = SpriteEvictState;
        type Message = Msg;

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            // 4x2 sprite sheet: left 2x2 is red (frame 0), right 2x2 is blue (frame 1).
            let mut pixels = Vec::with_capacity(4 * 2 * 4);
            for _y in 0..2 {
                pixels.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255]);
                pixels.extend_from_slice(&[0, 0, 255, 255, 0, 0, 255, 255]);
            }
            let encoded = png::make_rgba_png(4, 2, &pixels);
            let asset = ctx.load_image_bytes(&encoded)?;
            let sheet = SpriteSheet::new(asset).frame_size(2, 2);
            Ok(SpriteEvictState {
                sheet,
                visible: true,
                frame: 0,
                first_evict: false,
                second_evict: true,
            })
        }

        fn update(
            state: &mut Self::State,
            msg: Self::Message,
            ctx: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            match msg {
                Msg::HideAndEvict => {
                    state.visible = false;
                    state.first_evict = ctx.evict_image_pixels(state.sheet.asset);
                    state.second_evict = ctx.evict_image_pixels(state.sheet.asset);
                }
                Msg::ShowFrame(idx) => {
                    state.visible = true;
                    state.frame = idx;
                }
            }
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut surf = Surface::overlay("sprite-evict", Output::primary())
                .size((LogicalPx(8.0), LogicalPx(8.0)))
                .background(Color::TRANSPARENT);
            if state.visible {
                surf = surf.child(
                    Sprite::new(state.sheet.clone())
                        .frame(state.frame)
                        .filter(ImageFilter::Nearest)
                        .size((LogicalPx(8.0), LogicalPx(8.0))),
                );
            }
            let mut out = Surfaces::new();
            out.push(surf);
            out
        }
    }

    let mut rt = HeadlessRuntime::<SpriteEvictApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo::default()],
        RendererPreference::Software,
    )
    .unwrap();

    let asset = rt.state().sheet.asset;
    assert!(rt.asset_pixels_resident(asset));
    assert_eq!(rt.decoded_asset_bytes(), 4 * 2 * 4);
    assert_eq!(
        &rt.rendered_buffer("sprite-evict").unwrap()[0..4],
        &[0, 0, 255, 255]
    );

    // Hide sprite and evict decoded pixels while keeping SpriteSheet handle valid.
    rt.enqueue(Msg::HideAndEvict);
    rt.step().unwrap();
    assert!(rt.state().first_evict);
    assert!(!rt.state().second_evict);
    assert!(!rt.asset_pixels_resident(asset));
    assert_eq!(rt.decoded_asset_bytes(), 0);

    // Show sprite again at frame 1 (blue): auto-redecodes and remains resident (KeepDecoded).
    rt.enqueue(Msg::ShowFrame(1));
    rt.step().unwrap();
    assert!(rt.asset_pixels_resident(asset));
    assert_eq!(rt.decoded_asset_bytes(), 4 * 2 * 4);
    assert_eq!(
        &rt.rendered_buffer("sprite-evict").unwrap()[0..4],
        &[255, 0, 0, 255]
    );
}

#[cfg(all(feature = "image-svg", feature = "renderer-software"))]
#[test]
fn reclaim_after_present_sized_svg_preserves_scaled_dimensions() {
    struct ReclaimSvgApp;
    struct ReclaimSvgState {
        svg: ImageAsset,
    }

    impl Application for ReclaimSvgApp {
        type State = ReclaimSvgState;
        type Message = ();

        fn init(ctx: &mut EffectCtx<'_, Self::Message>) -> Result<Self::State> {
            let raw = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="12"><rect width="16" height="12" fill="rgb(0,0,255)"/></svg>"#;
            let svg = ctx.load_image_bytes_sized(raw, 48)?;
            assert!(ctx.set_image_retention(svg, AssetRetention::ReclaimAfterPresent));
            Ok(ReclaimSvgState { svg })
        }

        fn update(
            _: &mut Self::State,
            _: Self::Message,
            _: &mut EffectCtx<'_, Self::Message>,
        ) -> Result<()> {
            Ok(())
        }

        fn view(state: &Self::State) -> Surfaces<Self::Message> {
            let mut out = Surfaces::new();
            out.push(
                Surface::overlay("svg-reclaim", Output::primary())
                    .layer(Layer::Background)
                    .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
                    .size(Size::fill())
                    .child(Image::new(state.svg).size(Size::fill()).fit(ImageFit::Fill)),
            );
            out
        }
    }

    let mut rt = HeadlessRuntime::<ReclaimSvgApp>::new_with_renderer(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            logical_size: LogicalSize::new(LogicalPx(48.0), LogicalPx(36.0)),
            ..OutputInfo::default()
        }],
        RendererPreference::Software,
    )
    .unwrap();

    let svg = rt.state().svg;
    assert_eq!((svg.width, svg.height), (48, 36));
    assert_eq!(
        &rt.rendered_buffer("svg-reclaim").unwrap()[0..4],
        &[255, 0, 0, 255]
    );
    assert!(!rt.asset_pixels_resident(svg));
    assert_eq!(rt.decoded_asset_bytes(), 0);

    assert!(rt.simulate_surface_configure(
        "svg-reclaim",
        LogicalSize::new(LogicalPx(64.0), LogicalPx(48.0))
    ));
    rt.step().unwrap();
    let buf = rt.rendered_buffer("svg-reclaim").unwrap();
    assert_eq!(buf.len(), 64 * 48 * 4);
    assert_eq!(&buf[0..4], &[255, 0, 0, 255]);
    assert!(!rt.asset_pixels_resident(svg));
    assert_eq!(rt.decoded_asset_bytes(), 0);
}
