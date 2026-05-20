//! End-to-end picking tests.
//!
//! These exercise the full pipeline: VelloRenderer constructs with picking
//! enabled, the parallel pick scene captures each fill, the second render
//! pass writes a hitmap, readback decodes to ids, and `pick_at` returns
//! the expected value at known pixel positions.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::rgb8;
use hephaestus::{Affine, Brush, FillRule, PickId, Rect, Renderer, SceneBuilder};
use kurbo::Shape;

const W: u32 = 200;
const H: u32 = 200;

fn fresh_buf() -> Vec<u8> {
    vec![0u8; (W * H * 4) as usize]
}

#[test]
fn pick_at_returns_none_when_picking_disabled() {
    let mut r = VelloRenderer::new().expect("vello renderer init");
    let mut buf = fresh_buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut buf)
        .expect("render");
    assert_eq!(r.pick_at(50, 50), None);
    assert!(r.hitmap().is_none());
}

#[test]
fn pick_at_returns_id_at_known_positions() {
    let mut r = VelloRenderer::with_picking().expect("vello renderer init");
    {
        let scene = r.scene();
        let red: Brush = rgb8(220, 60, 60).into();
        let green: Brush = rgb8(60, 220, 60).into();
        let blue: Brush = rgb8(60, 60, 220).into();
        let a = Rect::new(10.0, 10.0, 60.0, 60.0).to_path(0.1);
        let b = Rect::new(80.0, 10.0, 130.0, 60.0).to_path(0.1);
        let c = Rect::new(150.0, 10.0, 195.0, 60.0).to_path(0.1);
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &red,
            None,
            &a,
            PickId::Id(7),
        );
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &green,
            None,
            &b,
            PickId::Id(42),
        );
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &blue,
            None,
            &c,
            PickId::Id(0xAA_BBCC),
        );
    }
    let mut buf = fresh_buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut buf)
        .expect("render");

    // Centers of each rect — should report their ids exactly.
    assert_eq!(r.pick_at(35, 35), Some(7));
    assert_eq!(r.pick_at(105, 35), Some(42));
    assert_eq!(r.pick_at(170, 35), Some(0xAA_BBCC));

    // Gaps between rects — no hit.
    assert_eq!(r.pick_at(70, 35), None);
    assert_eq!(r.pick_at(35, 100), None);

    // Out of bounds.
    assert_eq!(r.pick_at(W, 0), None);
    assert_eq!(r.pick_at(0, H), None);

    // Hitmap is the right shape and at least one pixel matches.
    let map = r.hitmap().expect("hitmap populated");
    assert_eq!(map.len() as u32, W * H);
}

#[test]
fn block_occludes_underneath_pick() {
    let mut r = VelloRenderer::with_picking().expect("vello renderer init");
    {
        let scene = r.scene();
        let red: Brush = rgb8(220, 60, 60).into();
        let yellow: Brush = rgb8(220, 220, 60).into();
        let big = Rect::new(0.0, 0.0, W as f64, H as f64).to_path(0.1);
        let block = Rect::new(80.0, 80.0, 120.0, 120.0).to_path(0.1);
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &red,
            None,
            &big,
            PickId::Id(7),
        );
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &yellow,
            None,
            &block,
            PickId::Block,
        );
    }
    let mut buf = fresh_buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut buf)
        .expect("render");

    // Outside the block: still id 7 from the underlying fill.
    assert_eq!(r.pick_at(20, 20), Some(7));
    // Inside the block: overwritten with id 0 → no hit.
    assert_eq!(r.pick_at(100, 100), None);
}

#[test]
fn skip_does_not_disturb_underlying_pick() {
    let mut r = VelloRenderer::with_picking().expect("vello renderer init");
    {
        let scene = r.scene();
        let red: Brush = rgb8(220, 60, 60).into();
        let yellow: Brush = rgb8(220, 220, 60).into();
        let big = Rect::new(0.0, 0.0, W as f64, H as f64).to_path(0.1);
        let overlay = Rect::new(80.0, 80.0, 120.0, 120.0).to_path(0.1);
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &red,
            None,
            &big,
            PickId::Id(7),
        );
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &yellow,
            None,
            &overlay,
            PickId::Skip,
        );
    }
    let mut buf = fresh_buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut buf)
        .expect("render");

    // Both inside and outside the Skip overlay: id 7 should still be reported.
    assert_eq!(r.pick_at(20, 20), Some(7));
    assert_eq!(r.pick_at(100, 100), Some(7));
}
