use waywi::display::{Output, OutputId, OutputInfo, resolve_outputs};
use waywi::geometry::{
    BufferPx, BufferRect, Edges, ElementAnchor, GeoRect, Length, LogicalPx, LogicalPxExt,
    LogicalSize, PercentExt, Point, Position, ScaleFactor, Size, Transform,
};

#[test]
fn logical_and_percent_ergonomics_work() {
    let x = 36.logical_px();
    let y = 12.5.logical_px();
    let p = 80.percent();
    assert_eq!(x, LogicalPx(36.0));
    assert_eq!(y, LogicalPx(12.5));
    assert_eq!(p.get(), 80.0);
    assert_eq!(p.of(200.logical_px()), 160.logical_px());
}

#[test]
fn fractional_scale_buffer_rounding() {
    let logical = LogicalSize::new(LogicalPx(360.0), LogicalPx(65.0));
    for s120 in [120u32, 150, 180, 240] {
        let scale = ScaleFactor::from_120ths(s120);
        let (bw, bh) = logical.to_buffer_ceil(scale);
        let f = scale.as_f32();
        assert_eq!(bw, BufferPx((360.0 * f).ceil() as i32));
        assert_eq!(bh, BufferPx((65.0 * f).ceil() as i32));
    }
    assert_eq!(ScaleFactor::from_120ths(150).integer_ceil(), 2);
}

#[test]
fn anchor_positioning_and_transform_bounds() {
    let anchor_off = ElementAnchor::BottomRight.top_left_offset(40.logical_px(), 20.logical_px());
    assert_eq!(anchor_off, Point::new(40.logical_px(), 20.logical_px()));

    let pos = Position::relative_to_parent(50.percent(), 100.percent())
        .with_offset((-8).logical_px(), (-6).logical_px());
    assert!(matches!(
        pos,
        Position::RelativeToParent { offset_x, offset_y, .. }
            if offset_x == (-8).logical_px() && offset_y == (-6).logical_px()
    ));

    let affine = Transform::identity()
        .scale(2.0)
        .to_affine_around(Point::new(LogicalPx(5.0), LogicalPx(5.0)));
    let p = affine.transform_point(Point::new(LogicalPx(10.0), LogicalPx(15.0)));
    assert_eq!(p, Point::new(LogicalPx(15.0), LogicalPx(25.0)));
}

#[test]
fn buffer_rect_clamp_and_union() {
    let r = GeoRect::new(
        LogicalPx(10.2),
        LogicalPx(20.4),
        LogicalPx(30.0),
        LogicalPx(40.0),
    );
    let br = r.to_buffer_rect(ScaleFactor::from_120ths(150), BufferPx(100), BufferPx(100));
    assert!(br.x >= 12 && br.y >= 25);
    assert!(br.right() <= 100 && br.bottom() <= 100);

    let union = br.union(BufferRect {
        x: 0,
        y: 0,
        width: 10,
        height: 10,
    });
    assert_eq!(union.x, 0);
    assert_eq!(union.y, 0);
    assert_eq!(union.right(), br.right());
    assert_eq!(union.bottom(), br.bottom());
}

#[test]
fn output_selection_policies_are_deterministic() {
    let o1 = OutputInfo {
        id: OutputId(10),
        name: Some("DP-1".into()),
        logical_position: Point::new(LogicalPx(1920.0), LogicalPx(0.0)),
        ..OutputInfo::default()
    };
    let o2 = OutputInfo {
        id: OutputId(2),
        name: Some("eDP-1".into()),
        logical_position: Point::new(LogicalPx(0.0), LogicalPx(0.0)),
        ..OutputInfo::default()
    };
    let list = vec![o1.clone(), o2.clone()];

    let prim = resolve_outputs(&Output::Primary, &list);
    assert_eq!(prim.len(), 1);
    assert_eq!(prim[0].id, OutputId(2));

    let named = resolve_outputs(&Output::named("DP-1"), &list);
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].id, OutputId(10));

    let by_id = resolve_outputs(&Output::id(OutputId(10)), &list);
    assert_eq!(by_id[0].id, OutputId(10));

    let all = resolve_outputs(&Output::All, &list);
    assert_eq!(all.len(), 2);

    let missing = resolve_outputs(&Output::named("HDMI-A-9"), &list);
    assert!(missing.is_empty());

    let sz = Size::new(50.percent(), 40.logical_px());
    let edges = Edges::symmetric(8.logical_px(), 12.logical_px());
    assert!(matches!(sz.width, Length::Percent(_)));
    assert_eq!(edges.left + edges.right, 24.logical_px());
}
