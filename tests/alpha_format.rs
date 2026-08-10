//! Pins the alpha convention of `render_to_buffer`: straight
//! (un-premultiplied) RGBA8. Every case renders over a fully transparent
//! background, the only condition under which the two conventions differ —
//! at alpha 255 they coincide, which is why the rest of the suite is blind
//! to this. Vello owns the behavior, so a version bump that flips it should
//! fail here rather than silently corrupt PNGs with translucent content.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::Color;
use hephaestus::geometry::{Affine, Rect};
use hephaestus::path::FillRule;
use hephaestus::pick::PickId;
use hephaestus::primitives;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

const W: u32 = 8;
const H: u32 = 8;
const TRANSPARENT: Color = Color::new([0.0, 0.0, 0.0, 0.0]);

/// Fill `rect` with `fill` over a transparent background and return the
/// pixel at `(x, y)`.
fn render_pixel(r: &mut VelloRenderer, fill: Color, rect: Rect, x: u32, y: u32) -> [u8; 4] {
    r.scene().clear();
    r.scene().fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &fill.into(),
        None,
        &primitives::rect(rect),
        PickId::Skip,
    );
    let mut buf = vec![0u8; (W * H * 4) as usize];
    r.render_to_buffer(W, H, TRANSPARENT, &mut buf)
        .expect("render");
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

#[test]
fn output_is_straight_alpha() {
    let mut r = VelloRenderer::new().expect("vello renderer init");
    let full = Rect::new(0.0, 0.0, W as f64, H as f64);

    // Allow ±2 for color-conversion drift; the premultiplied answers below
    // are far outside that band.
    let approx = |v: u8, target: u8| (v as i16 - target as i16).abs() <= 2;

    // Translucent fill: red stays saturated. Premultiplied would give ~128.
    let [red, _, _, a] = render_pixel(&mut r, Color::new([1.0, 0.0, 0.0, 0.5]), full, 4, 4);
    assert!(approx(a, 128), "alpha 0.5 -> {a}");
    assert!(
        approx(red, 255),
        "red at alpha 0.5 is {red}; premultiplied would be ~128"
    );

    // Lower alpha widens the gap: premultiplied would give ~64.
    let [red, _, _, a] = render_pixel(&mut r, Color::new([1.0, 0.0, 0.0, 0.25]), full, 4, 4);
    assert!(approx(a, 64), "alpha 0.25 -> {a}");
    assert!(
        approx(red, 255),
        "red at alpha 0.25 is {red}; premultiplied would be ~64"
    );

    // Partial coverage from antialiasing follows the same convention: an
    // opaque fill ending mid-pixel yields half alpha, full-strength color.
    let half_covered = Rect::new(0.0, 0.0, 4.5, H as f64);
    let [red, _, _, a] = render_pixel(&mut r, Color::new([1.0, 0.0, 0.0, 1.0]), half_covered, 4, 4);
    assert!(approx(a, 127), "AA coverage 0.5 -> alpha {a}");
    assert!(
        approx(red, 255),
        "red under AA coverage 0.5 is {red}; premultiplied would be ~128"
    );
}
