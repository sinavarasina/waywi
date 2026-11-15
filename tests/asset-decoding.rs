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

    fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
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

    fn update(state: &mut Self::State, msg: Self::Message, ctx: &mut EffectCtx<'_>) -> Result<()> {
        match msg {
            AssetMsg::LoadSvg(svg_bytes) => {
                state.svg_asset = Some(ctx.load_image_bytes(&svg_bytes)?);
            }
            #[cfg(feature = "image-png")]
            AssetMsg::Reload(path) => {
                if let Some(a) = state.asset {
                    ctx.reload_asset(a, path)?;
                    state.reloaded = true;
                }
            }
            #[cfg(feature = "image-png")]
            AssetMsg::Unload => {
                if let Some(a) = state.asset {
                    state.unloaded = ctx.unload_asset(a);
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
            fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
                let bytes = png::make_rgba_png(96, 48, &vec![255u8; 96 * 48 * 4]);
                Ok(SheetState(Some(ctx.load_image_bytes(&bytes)?)))
            }
            fn update(_: &mut Self::State, _: Self::Message, _: &mut EffectCtx<'_>) -> Result<()> {
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
            .frame_size(32.logical_px(), 24.logical_px())
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
        fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
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
        fn update(_: &mut Self::State, _: Self::Message, _: &mut EffectCtx<'_>) -> Result<()> {
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
        fn init(ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
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
        fn update(_: &mut Self::State, _: Self::Message, _: &mut EffectCtx<'_>) -> Result<()> {
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
