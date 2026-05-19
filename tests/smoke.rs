//! Smoke test: build a Vello renderer, draw nothing, render a solid-background
//! frame, assert the center pixel matches the background. Validates wgpu init,
//! Vello pipeline, readback alignment, and PNG-shaped buffer layout.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::rgb8;
use hephaestus::Renderer;

#[test]
fn solid_background_64x64() {
    let mut r = VelloRenderer::new().expect("vello renderer init");
    let w = 64u32;
    let h = 64u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let bg = rgb8(255, 64, 32);
    r.render_to_buffer(w, h, bg, &mut buf).expect("render");

    // Sample the center pixel.
    let cx = w / 2;
    let cy = h / 2;
    let i = ((cy * w + cx) * 4) as usize;
    let (r8, g8, b8, a8) = (buf[i], buf[i + 1], buf[i + 2], buf[i + 3]);

    // Allow ±2 for any minor color-conversion drift.
    let approx = |v: u8, target: u8| (v as i16 - target as i16).abs() <= 2;
    assert!(approx(r8, 255), "red channel: {r8}");
    assert!(approx(g8, 64), "green channel: {g8}");
    assert!(approx(b8, 32), "blue channel: {b8}");
    assert_eq!(a8, 255, "alpha");
}
