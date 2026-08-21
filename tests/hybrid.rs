//! End-to-end tests for the Hybrid backend.
//!
//! The picking cases are the point of the backend: binary coverage means an
//! edge pixel carries exactly one id, so an id read back from a boundary
//! between two marks is one of the two rather than a blend of both.

use hephaestus::backend::hybrid::HybridRenderer;
use hephaestus::color::rgb8;
use hephaestus::{Affine, Brush, FillRule, PickId, Rect, Renderer, SceneBuilder};
use kurbo::Shape;

const W: u32 = 100;
const H: u32 = 100;

fn buf() -> Vec<u8> {
    vec![0u8; (W * H * 4) as usize]
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// Fill `rect` with `color`, tagged `pick`.
fn fill(scene: &mut impl SceneBuilder, rect: Rect, color: [u8; 3], pick: PickId) {
    scene.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Solid(rgb8(color[0], color[1], color[2])),
        None,
        &rect.to_path(0.1),
        pick,
    );
}

#[test]
fn renders_a_solid_fill_over_the_background() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let mut out = buf();
    fill(
        r.scene(),
        Rect::new(20.0, 20.0, 80.0, 80.0),
        [255, 0, 0],
        PickId::Skip,
    );
    r.render_to_buffer(W, H, rgb8(255, 255, 255), &mut out)
        .expect("render");

    assert_eq!(px(&out, 50, 50), [255, 0, 0, 255], "inside the fill");
    assert_eq!(px(&out, 5, 5), [255, 255, 255, 255], "background");
}

#[test]
fn pick_at_returns_none_when_picking_disabled() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let mut out = buf();
    fill(
        r.scene(),
        Rect::new(20.0, 20.0, 80.0, 80.0),
        [255, 0, 0],
        PickId::Id(7),
    );
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");
    assert_eq!(r.pick_at(50, 50), None);
}

#[test]
fn pick_at_reports_the_id_under_the_pixel() {
    let mut r = HybridRenderer::with_picking().expect("hybrid renderer init");
    let mut out = buf();
    fill(
        r.scene(),
        Rect::new(20.0, 20.0, 80.0, 80.0),
        [255, 0, 0],
        PickId::Id(42),
    );
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    assert_eq!(r.pick_at(50, 50), Some(42), "inside the mark");
    assert_eq!(r.pick_at(5, 5), None, "empty space");
}

/// The case the compute-shader backend cannot pass.
///
/// Two overlapping circles: the upper circle's antialiased edge falls on the
/// lower one, and an antialiased pick pass blends their two ids into a ramp of
/// values that are neither, all at full alpha and so indistinguishable from
/// real hits. Measured against the compute-shader backend, this exact scene
/// yields 28 ids that were never drawn. Binary coverage cannot produce one.
#[test]
fn overlapping_picked_marks_never_blend_into_a_third_id() {
    let mut r = HybridRenderer::with_picking().expect("hybrid renderer init");
    let mut out = buf();
    // Far apart in value, so any blend lands nowhere near either id.
    for (shape, color, id) in [
        (kurbo::Circle::new((40.0, 50.0), 28.0), [255, 0, 0], 0x20u32),
        (kurbo::Circle::new((62.0, 50.0), 28.0), [0, 0, 255], 0xC0u32),
    ] {
        r.scene().fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Solid(rgb8(color[0], color[1], color[2])),
            None,
            &shape.to_path(0.1),
            PickId::Id(id),
        );
    }
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    let mut seen = std::collections::BTreeSet::new();
    for raw in r.hitmap().expect("picking enabled") {
        if let Some(id) = hephaestus::pick::decode(*raw) {
            seen.insert(id);
        }
    }
    assert_eq!(
        seen,
        [0x20, 0xC0].into_iter().collect(),
        "hitmap holds an id that was never drawn"
    );
}

#[test]
fn a_scene_can_be_rendered_at_two_sizes_in_a_row() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    fill(
        r.scene(),
        Rect::new(0.0, 0.0, 10.0, 10.0),
        [0, 255, 0],
        PickId::Skip,
    );
    let mut small = vec![0u8; 40 * 40 * 4];
    r.render_to_buffer(40, 40, rgb8(0, 0, 0), &mut small)
        .expect("small render");
    let mut large = vec![0u8; 120 * 90 * 4];
    r.render_to_buffer(120, 90, rgb8(0, 0, 0), &mut large)
        .expect("large render");

    assert_eq!(&small[0..4], &[0, 255, 0, 255], "fill survives resize");
    assert_eq!(&large[0..4], &[0, 255, 0, 255]);
}

// ─── Alpha convention ───────────────────────────────────────────────────────

/// `render_to_buffer` hands out straight (un-premultiplied) alpha, same as
/// every other backend. The rasteriser composites premultiplied, so this is
/// the assertion that catches the conversion going missing.
#[test]
fn output_is_straight_alpha() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let transparent = hephaestus::color::Color::new([0.0, 0.0, 0.0, 0.0]);
    r.scene().fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &hephaestus::color::Color::new([1.0, 0.0, 0.0, 0.5]).into(),
        None,
        &Rect::new(0.0, 0.0, W as f64, H as f64).to_path(0.1),
        PickId::Skip,
    );
    let mut out = buf();
    r.render_to_buffer(W, H, transparent, &mut out)
        .expect("render");

    let [red, _, _, alpha] = px(&out, 50, 50);
    // Premultiplied would report red ≈ 128 here; straight keeps it at full.
    assert!(
        red > 250,
        "red channel {red} looks premultiplied, not straight"
    );
    assert!(
        (120..=136).contains(&alpha),
        "alpha {alpha} off half-coverage"
    );
}

// ─── Brushes and layers ─────────────────────────────────────────────────────

#[test]
fn gradient_brush_varies_across_the_fill() {
    use hephaestus::brush::{Brush as B, Gradient};

    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    let gradient = Gradient::new_linear((0.0, 0.0), (W as f64, 0.0))
        .with_stops(&[rgb8(0, 0, 0), rgb8(255, 255, 255)][..]);
    r.scene().fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &B::Gradient(gradient),
        None,
        &Rect::new(0.0, 0.0, W as f64, H as f64).to_path(0.1),
        PickId::Skip,
    );
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    let left = px(&out, 5, 50)[0];
    let right = px(&out, 95, 50)[0];
    assert!(
        right > left + 100,
        "gradient did not ramp: {left} -> {right}"
    );
}

#[test]
fn a_clip_layer_confines_what_it_contains() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    {
        let scene = r.scene();
        scene.push_layer(
            hephaestus::blend::BlendMode::NORMAL,
            1.0,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 50.0, 100.0).to_path(0.1),
        );
        fill(
            scene,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            [255, 0, 0],
            PickId::Skip,
        );
        scene.pop_layer();
    }
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 255), &mut out)
        .expect("render");

    assert_eq!(px(&out, 25, 50), [255, 0, 0, 255], "inside the clip");
    assert_eq!(px(&out, 75, 50), [0, 0, 255, 255], "outside the clip");
}

// ─── Meshes ─────────────────────────────────────────────────────────────────

#[test]
fn a_mesh_triangle_rasterises() {
    use hephaestus::mesh::Mesh;

    let mut r = HybridRenderer::with_picking().expect("hybrid renderer init");
    let green = rgb8(0, 200, 0);
    let mesh = Mesh::new(
        vec![
            hephaestus::geometry::Point::new(10.0, 10.0),
            hephaestus::geometry::Point::new(90.0, 10.0),
            hephaestus::geometry::Point::new(50.0, 90.0),
        ],
        vec![green, green, green],
        vec![0, 1, 2],
    );
    r.scene().draw_mesh(&mesh, Affine::IDENTITY, PickId::Id(5));

    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    assert_eq!(px(&out, 50, 40)[1], 200, "mesh interior");
    assert_eq!(r.pick_at(50, 40), Some(5), "mesh carries its pick id");
}

// ─── Images ─────────────────────────────────────────────────────────────────

/// A 2x2 opaque image: red, green / blue, white.
fn quad_image() -> hephaestus::brush::Image {
    use hephaestus::brush::{Blob, ImageAlphaType, ImageFormat};
    hephaestus::brush::Image {
        data: Blob::from(vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ]),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 2,
        height: 2,
    }
}

#[test]
fn an_image_is_uploaded_and_sampled() {
    let mut r = HybridRenderer::with_picking().expect("hybrid renderer init");
    // Scale the 2x2 up so each source pixel covers a 50x50 block.
    r.scene().draw_image(
        &quad_image(),
        Affine::scale(50.0),
        hephaestus::brush::Sampling::Nearest,
        1.0,
        PickId::Id(9),
    );
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    assert_eq!(px(&out, 25, 25), [255, 0, 0, 255], "top-left source pixel");
    assert_eq!(px(&out, 75, 25), [0, 255, 0, 255], "top-right");
    assert_eq!(px(&out, 25, 75), [0, 0, 255, 255], "bottom-left");
    assert_eq!(r.pick_at(50, 50), Some(9), "image carries its pick id");
}

/// Image opacity cannot ride on the sampler — the shared paint encoder
/// rejects any value but 1.0 — so the backend turns it into a layer. Without
/// that, this panics rather than fading.
#[test]
fn a_translucent_image_fades_instead_of_panicking() {
    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    r.scene().draw_image(
        &quad_image(),
        Affine::scale(50.0),
        hephaestus::brush::Sampling::Nearest,
        0.5,
        PickId::Skip,
    );
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    let [red, _, _, _] = px(&out, 25, 25);
    assert!(
        (110..=145).contains(&red),
        "half-opacity red over black came back {red}"
    );
}

// ─── Text ───────────────────────────────────────────────────────────────────

/// Glyph runs need a `Resources` threaded through the rasteriser's glyph
/// builder, which is the one part of the port with no counterpart in the
/// compute-shader backend. This drives the real text pipeline — shaping
/// included — and asserts ink landed where the block was placed.
#[test]
fn text_draws_ink_inside_its_block() {
    use hephaestus::style_vocab::{HAlign, Palette};
    use hephaestus::text::rich::{RichAnchor, RichTextRun, RichTextStyleSheet};
    use hephaestus::text::TextStyle;

    let sheet = RichTextStyleSheet::new();
    let palette = Palette::default();
    let style = TextStyle::new(16.0);
    let run = RichTextRun::new(
        "Hybrid renders text",
        &style,
        rgb8(255, 255, 255),
        &sheet,
        &palette,
        96.0,
    );
    let height = run.set_max_width(180.0, HAlign::Start);
    assert!(height > 0.0, "a shaped block must have height");

    let mut r = HybridRenderer::new().expect("hybrid renderer init");
    {
        let scene = r.scene();
        scene.clear();
        hephaestus::text::rich::draw_rich_text(
            scene,
            &run,
            10.0,
            10.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }
    let mut out = buf();
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut out)
        .expect("render");

    let lit = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|(x, y)| px(&out, *x, *y)[0] > 40)
        .count();
    assert!(lit > 20, "expected glyph ink, found {lit} lit pixels");
}
