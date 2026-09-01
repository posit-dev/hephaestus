//! The PDF backend, end to end.
//!
//! Assertions are on emitted *structure*, never on a golden byte
//! baseline — the same reasoning `tests/svg.rs` and
//! `tests/document_roundtrip.rs` give. Most tests turn compression off
//! so the content stream can be searched as text; the two that care
//! about compression check the structure survives it.

#![cfg(feature = "pdf")]

use hephaestus::backend::pdf::{encode_pdf, PdfConfig, PdfScene, PdfWarning};
use hephaestus::brush::{Brush, Gradient};
use hephaestus::color::Color;
use hephaestus::geometry::{Affine, Point, Rect, Shape, Size};
use hephaestus::mesh::Mesh;
use hephaestus::path::{FillRule, Path};
use hephaestus::pick::PickId;
use hephaestus::scene::SceneBuilder;
use hephaestus::stroke::Stroke;
use hephaestus::text::rich::{draw_rich_text, RichAnchor, RichTextRun, RichTextStyleSheet};
use hephaestus::text::{draw_text, TextRun, TextStyle};

const W: f64 = 320.0;
const H: f64 = 200.0;

fn scene() -> PdfScene {
    PdfScene::with_config(Size::new(W, H), 96.0, PdfConfig::new().compress(false))
}

fn rect_path(r: Rect) -> Path {
    Shape::to_path(&r, 0.1)
}

fn black() -> Brush {
    Brush::Solid(Color::BLACK)
}

/// The whole file as text. Only safe on an uncompressed document, and
/// only for `contains`-style checks — the header's binary comment is
/// not UTF-8, so byte offsets do not survive.
fn text(pdf: &[u8]) -> String {
    String::from_utf8_lossy(pdf).into_owned()
}

/// Byte offset of the first occurrence of `needle`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Every occurrence of `needle`, as byte offsets.
fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .map(|(i, _)| i)
        .collect()
}

/// Every uncompressed stream's payload, as text.
fn plain_streams(pdf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for start in find_all(pdf, b">>\nstream\n") {
        let head = &pdf[..start];
        let obj = head.rsplit(|b| *b == b'\n').take(4).collect::<Vec<_>>();
        let dict = obj.concat();
        if find(&dict, b"/Filter").is_some() {
            continue;
        }
        let body = start + b">>\nstream\n".len();
        let Some(end) = find(&pdf[body..], b"\nendstream") else {
            continue;
        };
        out.push(String::from_utf8_lossy(&pdf[body..body + end]).into_owned());
    }
    out
}

// ─── Well-formedness ────────────────────────────────────────────────────────

/// Parse enough of `pdf` to prove a viewer would accept it.
///
/// The highest-value check here, for the same reason the SVG backend's
/// tag-stack check is: a broken xref makes the file unusable rather
/// than merely wrong, and nothing about the picture would reveal it.
fn assert_well_formed(pdf: &[u8]) {
    assert!(pdf.starts_with(b"%PDF-"), "no header");
    assert!(pdf.ends_with(b"%%EOF\n"), "no trailer marker");

    // `startxref` names the byte offset of the table.
    let tail = find(pdf, b"startxref\n").expect("startxref") + b"startxref\n".len();
    let end = tail + pdf[tail..].iter().position(|b| *b == b'\n').unwrap();
    let xref: usize = std::str::from_utf8(&pdf[tail..end])
        .unwrap()
        .parse()
        .expect("a startxref offset");
    assert!(
        pdf[xref..].starts_with(b"xref\n"),
        "startxref does not point at the table"
    );

    // `N M` header, then exactly-20-byte entries, then the trailer.
    let header = xref + b"xref\n".len();
    let header_end = header + pdf[header..].iter().position(|b| *b == b'\n').unwrap();
    let header_line = std::str::from_utf8(&pdf[header..header_end]).unwrap();
    let count: usize = header_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let first = header_end + 1;
    let mut defined = vec![false; count];
    for i in 0..count {
        let entry = &pdf[first + i * 20..first + (i + 1) * 20];
        assert_eq!(entry[10], b' ', "entry {i} is not 20 bytes");
        assert_eq!(entry[16], b' ');
        assert_eq!(entry[18], b' ');
        assert_eq!(entry[19], b'\n');
        let kind = entry[17];
        if kind == b'f' {
            continue;
        }
        assert_eq!(kind, b'n', "entry {i} has an unknown type");
        let offset: usize = std::str::from_utf8(&entry[..10]).unwrap().parse().unwrap();
        let expected = format!("{i} 0 obj");
        assert!(
            pdf[offset..].starts_with(expected.as_bytes()),
            "object {i} does not start at {offset}"
        );
        defined[i] = true;
    }
    let trailer = first + count * 20;
    assert!(
        pdf[trailer..].starts_with(b"trailer\n"),
        "the entry count and the table disagree"
    );
    let size_at = find(&pdf[trailer..], b"/Size ").expect("/Size") + trailer + b"/Size ".len();
    let size_end = size_at
        + pdf[size_at..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .unwrap();
    let size: usize = std::str::from_utf8(&pdf[size_at..size_end])
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(size, count, "/Size disagrees with the table");

    // Every declared stream length matches its payload.
    for start in find_all(pdf, b">>\nstream\n") {
        let length_at = find(&pdf[..start], b"/Length ").map(|_| {
            // The *last* `/Length ` before the stream keyword is this
            // stream's, since a dictionary may name another object.
            find_all(&pdf[..start], b"/Length ")
                .last()
                .copied()
                .unwrap()
        });
        let Some(at) = length_at else { continue };
        let digits = at + b"/Length ".len();
        let digits_end = digits
            + pdf[digits..]
                .iter()
                .position(|b| !b.is_ascii_digit())
                .unwrap();
        let declared: usize = std::str::from_utf8(&pdf[digits..digits_end])
            .unwrap()
            .parse()
            .unwrap();
        let body = start + b">>\nstream\n".len();
        assert_eq!(
            &pdf[body + declared..body + declared + b"\nendstream".len()],
            b"\nendstream",
            "a stream's /Length {declared} does not reach its endstream"
        );
    }

    // Every `N 0 R` names an object the table defines. The trailing
    // delimiter matters: `0 0 0 RG` contains the same four characters.
    let text = text(pdf);
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(hit) = text[at..].find(" 0 R") {
        let i = at + hit;
        at = i + 4;
        let after = bytes.get(i + 4).copied().unwrap_or(b' ');
        if !matches!(after, b' ' | b'\n' | b']' | b'>' | b'/' | b'\r') {
            continue;
        }
        let digits_end = i;
        let mut digits_start = i;
        while digits_start > 0 && bytes[digits_start - 1].is_ascii_digit() {
            digits_start -= 1;
        }
        if digits_start == digits_end {
            continue;
        }
        let n: usize = text[digits_start..digits_end].parse().unwrap();
        assert!(
            n < count && defined[n],
            "object {n} is referenced but never defined"
        );
    }

    // `q` and `Q` balance in every readable content stream.
    for stream in plain_streams(pdf) {
        let mut depth = 0i32;
        for token in stream.split_whitespace() {
            match token {
                "q" => depth += 1,
                "Q" => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "a stream popped more than it pushed");
        }
        assert_eq!(depth, 0, "a stream left {depth} levels open");
    }

    // Every resource name a stream selects is one the file defines.
    let mut used: Vec<String> = Vec::new();
    for stream in plain_streams(pdf) {
        let tokens: Vec<&str> = stream.split_whitespace().collect();
        for (i, t) in tokens.iter().enumerate() {
            if !matches!(*t, "gs" | "Do" | "sh" | "Tf" | "scn" | "SCN") {
                continue;
            }
            for back in 1..=2 {
                if i >= back {
                    if let Some(name) = tokens[i - back].strip_prefix('/') {
                        if name.starts_with("GS")
                            || name.starts_with('P')
                            || name.starts_with("Sh")
                            || name.starts_with('X')
                            || name.starts_with('F')
                        {
                            used.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    for name in used {
        assert!(
            text.contains(&format!("/{name} ")),
            "the content stream selects /{name}, which nothing defines"
        );
    }
}

// ─── Structure ──────────────────────────────────────────────────────────────

#[test]
fn a_scene_of_every_primitive_produces_a_well_formed_file() {
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 100.0, 80.0));
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    s.stroke(
        &Stroke::new(2.0),
        Affine::translate((5.0, 5.0)),
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    s.push_layer(Default::default(), 0.5, Affine::IDENTITY, &r);
    s.fill(
        FillRule::EvenOdd,
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    s.push_layer(Default::default(), 1.0, Affine::IDENTITY, &Path::new());
    s.draw_mesh(&triangle(), Affine::IDENTITY, PickId::Skip);
    s.pop_layer();
    s.pop_layer();
    let style = TextStyle::new(12.0);
    let run = TextRun::new("hello", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        190.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );

    let pdf = encode_pdf(&s);
    assert_well_formed(&pdf);
}

#[test]
fn the_media_box_is_the_requested_size_in_points() {
    // 800x600 at 96 dpi is 600x450 pt.
    let mut s = PdfScene::with_config(
        Size::new(800.0, 600.0),
        96.0,
        PdfConfig::new().compress(false),
    );
    s.clear();
    let pdf = encode_pdf(&s);
    assert!(
        text(&pdf).contains("/MediaBox [0 0 600 450]"),
        "{}",
        text(&pdf)
    );
}

#[test]
fn two_encodes_of_one_scene_are_byte_identical() {
    let mut s = scene();
    for i in 0..8 {
        let g = Gradient::new_linear((0.0, 0.0), (10.0, 0.0))
            .with_stops([Color::from_rgba8(i * 8, 0, 0, 255), Color::WHITE]);
        s.fill(
            FillRule::NonZero,
            Affine::translate((f64::from(i), 0.0)),
            &Brush::Gradient(g),
            None,
            &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
            PickId::Skip,
        );
    }
    let style = TextStyle::new(12.0);
    let run = TextRun::new("determinism", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        90.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    assert_eq!(encode_pdf(&s), encode_pdf(&s));
}

#[test]
fn clear_really_resets_the_scene() {
    let mut a = scene();
    let fresh = encode_pdf(&a);
    a.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &rect_path(Rect::new(0.0, 0.0, 5.0, 5.0)),
        PickId::Skip,
    );
    let style = TextStyle::new(12.0);
    let run = TextRun::new("gone", &style, 96.0);
    draw_text(
        &mut a,
        &run,
        5.0,
        90.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    a.clear();
    assert_eq!(encode_pdf(&a), fresh);
}

#[test]
fn no_coordinate_is_written_in_scientific_notation_or_as_a_non_number() {
    let mut s = scene();
    let mut p = Path::new();
    p.move_to(Point::new(1e-9, 1e18));
    p.line_to(Point::new(f64::NAN, f64::INFINITY));
    p.line_to(Point::new(-1e-30, 5.0));
    p.close_path();
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &p,
        PickId::Skip,
    );
    let pdf = encode_pdf(&s);
    let body = text(&pdf);
    assert!(!body.contains("NaN") && !body.contains("inf"), "{body}");
    for (i, c) in body.char_indices() {
        if c != 'e' && c != 'E' {
            continue;
        }
        let before = body[..i].chars().next_back();
        let after = body[i + 1..].chars().next();
        let exponent = matches!(before, Some(b) if b.is_ascii_digit())
            && matches!(after, Some(a) if a.is_ascii_digit() || a == '-' || a == '+');
        assert!(!exponent, "exponent notation near byte {i}");
    }
    assert!(s.warnings().contains(&PdfWarning::NonFiniteCoordinate));
}

#[test]
fn unbalanced_layers_are_closed_rather_than_left_open() {
    let mut s = scene();
    s.push_layer(
        Default::default(),
        1.0,
        Affine::IDENTITY,
        &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
    );
    s.push_layer(Default::default(), 0.5, Affine::IDENTITY, &Path::new());
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &rect_path(Rect::new(1.0, 1.0, 5.0, 5.0)),
        PickId::Skip,
    );
    assert_well_formed(&encode_pdf(&s));
}

#[test]
fn popping_more_layers_than_were_pushed_is_reported_not_fatal() {
    let mut s = scene();
    s.pop_layer();
    assert!(s.warnings().contains(&PdfWarning::UnbalancedLayers));
    assert_well_formed(&encode_pdf(&s));
}

// ─── Paths and paint ────────────────────────────────────────────────────────

#[test]
fn a_filled_and_stroked_path_is_one_painting_operator() {
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 40.0, 40.0));
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    s.stroke(
        &Stroke::new(1.0),
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("\nB\n"), "a merged fill and stroke: {pdf}");
    assert!(!pdf.contains("\nf\n"), "the fill was not written twice");
}

#[test]
fn a_stroke_of_a_different_path_does_not_merge() {
    let mut s = scene();
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &rect_path(Rect::new(10.0, 10.0, 40.0, 40.0)),
        PickId::Skip,
    );
    s.stroke(
        &Stroke::new(1.0),
        Affine::IDENTITY,
        &black(),
        None,
        &rect_path(Rect::new(50.0, 50.0, 80.0, 80.0)),
        PickId::Skip,
    );
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("\nf\n"), "{pdf}");
    assert!(pdf.contains("\nS\n"), "{pdf}");
    assert!(!pdf.contains("\nB\n"), "{pdf}");
}

#[test]
fn an_even_odd_fill_uses_the_star_operator() {
    let mut s = scene();
    s.fill(
        FillRule::EvenOdd,
        Affine::IDENTITY,
        &black(),
        None,
        &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
        PickId::Skip,
    );
    assert!(text(&encode_pdf(&s)).contains("\nf*\n"));
}

#[test]
fn asymmetric_caps_are_reported() {
    use hephaestus::stroke::Cap;
    let mut s = scene();
    s.stroke(
        &Stroke::new(1.0)
            .with_start_cap(Cap::Round)
            .with_end_cap(Cap::Butt),
        Affine::IDENTITY,
        &black(),
        None,
        &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
        PickId::Skip,
    );
    assert!(s.warnings().contains(&PdfWarning::AsymmetricCaps));
}

#[test]
fn a_gradient_becomes_a_shading_pattern() {
    let mut s = scene();
    let g = Gradient::new_linear((0.0, 0.0), (10.0, 0.0)).with_stops([Color::BLACK, Color::WHITE]);
    for t in [Affine::IDENTITY, Affine::translate((20.0, 0.0))] {
        s.fill(
            FillRule::NonZero,
            t,
            &Brush::Gradient(g.clone()),
            None,
            &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
            PickId::Skip,
        );
    }
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("/PatternType 2"), "{pdf}");
    assert!(pdf.contains("/ShadingType 2"), "{pdf}");
    assert_eq!(
        pdf.matches("/PatternType 2").count(),
        2,
        "a gradient under two transforms is two patterns"
    );
    assert_well_formed(&encode_pdf(&s));
}

#[test]
fn a_radial_gradient_keeps_its_focal_radius() {
    let mut s = scene();
    let g = Gradient::new_two_point_radial((5.0, 5.0), 2.0, (5.0, 5.0), 10.0)
        .with_stops([Color::BLACK, Color::WHITE]);
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(g),
        None,
        &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
        PickId::Skip,
    );
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("/ShadingType 3"), "{pdf}");
    assert!(
        pdf.contains("/Coords [5 5 2 5 5 10"),
        "the start radius survives natively: {pdf}"
    );
}

#[test]
fn a_sweep_gradient_degrades_to_a_flat_fill_and_reports_it() {
    let mut s = scene();
    let g = Gradient::new_sweep((5.0, 5.0), 0.0, std::f32::consts::TAU)
        .with_stops([Color::BLACK, Color::WHITE]);
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(g),
        None,
        &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
        PickId::Skip,
    );
    let pdf = text(&encode_pdf(&s));
    assert!(!pdf.contains("/PatternType"), "{pdf}");
    assert!(s.warnings().contains(&PdfWarning::SweepGradient));
}

#[test]
fn a_repeating_gradient_is_padded_and_reported() {
    use hephaestus::brush::Extend;
    let mut s = scene();
    let g = Gradient::new_linear((0.0, 0.0), (10.0, 0.0))
        .with_stops([Color::BLACK, Color::WHITE])
        .with_extend(Extend::Repeat);
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(g),
        None,
        &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
        PickId::Skip,
    );
    assert!(text(&encode_pdf(&s)).contains("/Extend [true true]"));
    assert!(s.warnings().contains(&PdfWarning::UnsupportedExtend));
}

#[test]
fn warnings_are_reported_once_however_often_they_recur() {
    let mut s = scene();
    for i in 0..50 {
        let g = Gradient::new_sweep((5.0, 5.0), 0.0, std::f32::consts::TAU)
            .with_stops([Color::from_rgba8(i, 0, 0, 255), Color::WHITE]);
        s.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Gradient(g),
            None,
            &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
            PickId::Skip,
        );
    }
    assert_eq!(
        s.warnings()
            .iter()
            .filter(|w| **w == PdfWarning::SweepGradient)
            .count(),
        1
    );
}

/// A gradient fading from transparent to opaque, which is what a
/// confidence band whose opacity encodes something resolves to.
fn fading() -> Gradient {
    Gradient::new_linear((0.0, 0.0), (100.0, 0.0)).with_stops([
        Color::from_rgba8(200, 40, 40, 0),
        Color::from_rgba8(200, 40, 40, 255),
    ])
}

#[test]
fn a_gradient_whose_stops_disagree_about_alpha_gets_a_soft_mask() {
    let mut s = scene();
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(fading()),
        None,
        &rect_path(Rect::new(0.0, 0.0, 100.0, 50.0)),
        PickId::Skip,
    );
    let pdf = encode_pdf(&s);
    let body = text(&pdf);
    assert!(body.contains("/S /Luminosity"), "{body}");
    assert!(body.contains("/BC [0]"), "{body}");
    assert!(body.contains("/CS /DeviceGray"), "{body}");
    // The ramp is the alpha channel, one gray component per stop.
    assert!(body.contains("/C0 [0] /C1 [1]"), "{body}");
    // Nothing was flattened, so nothing is reported.
    assert!(s.warnings().is_empty(), "{:?}", s.warnings());
    assert_well_formed(&pdf);
}

#[test]
fn a_gradient_whose_stops_agree_needs_no_soft_mask() {
    let mut s = scene();
    let g = Gradient::new_linear((0.0, 0.0), (100.0, 0.0)).with_stops([
        Color::from_rgba8(200, 40, 40, 128),
        Color::from_rgba8(40, 40, 200, 128),
    ]);
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(g),
        None,
        &rect_path(Rect::new(0.0, 0.0, 100.0, 50.0)),
        PickId::Skip,
    );
    let body = text(&encode_pdf(&s));
    assert!(
        !body.contains("/SMask"),
        "a constant alpha is a /ca: {body}"
    );
    assert!(body.contains("/ca 0.5"), "{body}");
}

/// The bug this cost an afternoon to find: a soft-mask group is
/// evaluated in the coordinate system in force when `gs` runs, and a
/// renderer composites it into a buffer it sizes from that system. Set
/// the mask under the page flip and the mask is silently clipped part
/// way across the page — the shape fades correctly and then stops.
#[test]
fn a_soft_mask_is_set_with_the_page_transform_undone() {
    let mut s = scene();
    s.fill(
        FillRule::NonZero,
        Affine::translate((40.0, 10.0)),
        &Brush::Gradient(fading()),
        None,
        &rect_path(Rect::new(0.0, 0.0, 100.0, 50.0)),
        PickId::Skip,
    );
    let pdf = encode_pdf(&s);
    let stream = plain_streams(&pdf)
        .into_iter()
        .find(|s| s.contains(" gs\n") && s.contains(" scn\n"))
        .expect("the page content stream");
    let tokens: Vec<&str> = stream.split_whitespace().collect();
    let gs = tokens
        .iter()
        .position(|t| *t == "gs")
        .expect("a gs operator");
    // The six operands before `<name> gs` are the `cm` that resets to
    // default user space.
    assert_eq!(
        tokens[gs - 2],
        "cm",
        "no cm before the mask is set: {stream}"
    );
    let m: Vec<f64> = tokens[gs - 8..gs - 2]
        .iter()
        .map(|t| t.parse().expect("a matrix operand"))
        .collect();
    // Composing it with the page flip must give the identity — that is
    // exactly the claim "the CTM is the identity when `gs` runs".
    let s_px = 72.0 / 96.0;
    let flip = Affine::new([s_px, 0.0, 0.0, -s_px, 0.0, H * s_px]);
    let net = (flip * Affine::new([m[0], m[1], m[2], m[3], m[4], m[5]])).as_coeffs();
    for (got, want) in net.iter().zip(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]) {
        assert!(
            (got - want).abs() < 1e-6,
            "the CTM is not the identity when the mask is set: {net:?}"
        );
    }
}

/// One graphics state carries one `/SMask`, which would mask the stroke
/// as well as the fill.
#[test]
fn a_masked_fill_does_not_merge_with_its_stroke() {
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 60.0, 40.0));
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(fading()),
        None,
        &r,
        PickId::Skip,
    );
    s.stroke(
        &Stroke::new(1.0),
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    let body = text(&encode_pdf(&s));
    assert!(!body.contains("\nB\n"), "the merge is broken: {body}");
    assert!(body.contains("\nf\n") && body.contains("\nS\n"), "{body}");
    assert_well_formed(&encode_pdf(&s));
}

#[test]
fn a_mesh_whose_vertices_disagree_about_alpha_gets_a_gray_type_4_mask() {
    let mut s = scene();
    let m = Mesh::new(
        vec![
            Point::new(0.0, 0.0),
            Point::new(80.0, 0.0),
            Point::new(0.0, 80.0),
        ],
        vec![
            Color::from_rgba8(200, 40, 40, 0),
            Color::from_rgba8(200, 40, 40, 255),
            Color::from_rgba8(200, 40, 40, 255),
        ],
        vec![0, 1, 2],
    );
    s.draw_mesh(&m, Affine::IDENTITY, PickId::Skip);
    let pdf = encode_pdf(&s);
    let body = text(&pdf);
    assert!(body.contains("/S /Luminosity"), "{body}");
    assert_eq!(
        body.matches("/ShadingType 4").count(),
        2,
        "one shading for colour, one for alpha: {body}"
    );
    assert!(body.contains("/ColorSpace /DeviceGray"), "{body}");
    assert!(s.warnings().is_empty(), "{:?}", s.warnings());
    assert_well_formed(&pdf);
}

/// Two fills of one gradient share a mask; two gradients do not.
#[test]
fn soft_masks_are_interned_like_every_other_resource() {
    let mut s = scene();
    for i in 0..3 {
        s.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Gradient(fading()),
            None,
            &rect_path(Rect::new(0.0, f64::from(i) * 60.0, 100.0, 50.0)),
            PickId::Skip,
        );
    }
    let body = text(&encode_pdf(&s));
    assert_eq!(body.matches("/S /Luminosity").count(), 1, "{body}");
}

// ─── Layers ─────────────────────────────────────────────────────────────────

#[test]
fn a_translucent_layer_becomes_a_transparency_group() {
    let mut s = scene();
    s.push_layer(Default::default(), 0.5, Affine::IDENTITY, &Path::new());
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
        PickId::Skip,
    );
    s.pop_layer();
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("/S /Transparency"), "{pdf}");
    assert!(pdf.contains("/ca 0.5"), "{pdf}");
    assert_well_formed(&encode_pdf(&s));
}

#[test]
fn an_opaque_layer_is_a_plain_clip() {
    let mut s = scene();
    s.push_layer(
        Default::default(),
        1.0,
        Affine::IDENTITY,
        &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
    );
    s.pop_layer();
    let pdf = text(&encode_pdf(&s));
    assert!(!pdf.contains("/S /Transparency"), "{pdf}");
    assert!(pdf.contains("W n"), "{pdf}");
}

#[test]
fn a_mix_mode_becomes_a_blend_mode_in_an_extgstate() {
    use hephaestus::blend::{BlendMode, Compose, Mix};
    let mut s = scene();
    s.push_layer(
        BlendMode::new(Mix::Multiply, Compose::SrcOver),
        1.0,
        Affine::IDENTITY,
        &Path::new(),
    );
    s.pop_layer();
    assert!(text(&encode_pdf(&s)).contains("/BM /Multiply"));
}

#[test]
fn a_porter_duff_operator_other_than_source_over_is_reported() {
    use hephaestus::blend::{BlendMode, Compose, Mix};
    let mut s = scene();
    s.push_layer(
        BlendMode::new(Mix::Normal, Compose::Xor),
        1.0,
        Affine::IDENTITY,
        &Path::new(),
    );
    s.pop_layer();
    assert!(s.warnings().contains(&PdfWarning::UnsupportedCompose));
}

// ─── Images ─────────────────────────────────────────────────────────────────

fn checkerboard(alpha: u8) -> hephaestus::brush::Image {
    let mut data = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4 {
        for x in 0..4 {
            let v = if (x + y) % 2 == 0 { 255 } else { 0 };
            data.extend_from_slice(&[v, v, v, alpha]);
        }
    }
    hephaestus::brush::Image {
        data: hephaestus::brush::Blob::from(data),
        format: hephaestus::brush::ImageFormat::Rgba8,
        alpha_type: hephaestus::brush::ImageAlphaType::Alpha,
        width: 4,
        height: 4,
    }
}

#[test]
fn an_image_is_embedded_once_however_often_it_is_drawn() {
    use hephaestus::brush::Sampling;
    let mut s = scene();
    let image = checkerboard(255);
    for i in 0..5 {
        s.draw_image(
            &image,
            Affine::translate((f64::from(i) * 10.0, 0.0)),
            Sampling::Nearest,
            1.0,
            PickId::Skip,
        );
    }
    let pdf = text(&encode_pdf(&s));
    assert_eq!(pdf.matches("/Subtype /Image").count(), 1, "{pdf}");
    assert_eq!(pdf.matches(" Do\n").count(), 5, "{pdf}");
    assert!(
        !pdf.contains("/SMask"),
        "an opaque image needs no soft mask"
    );
    assert_well_formed(&encode_pdf(&s));
}

#[test]
fn a_translucent_image_gains_a_soft_mask() {
    use hephaestus::brush::Sampling;
    let mut s = scene();
    s.draw_image(
        &checkerboard(128),
        Affine::IDENTITY,
        Sampling::Bilinear,
        1.0,
        PickId::Skip,
    );
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("/SMask"), "{pdf}");
    assert!(pdf.contains("/ColorSpace /DeviceGray"), "{pdf}");
    assert!(pdf.contains("/Interpolate true"), "{pdf}");
    assert_well_formed(&encode_pdf(&s));
}

// ─── Meshes ─────────────────────────────────────────────────────────────────

fn triangle() -> Mesh {
    Mesh::new(
        vec![
            Point::new(0.0, 0.0),
            Point::new(50.0, 0.0),
            Point::new(0.0, 50.0),
        ],
        vec![
            Color::from_rgba8(255, 0, 0, 255),
            Color::from_rgba8(0, 255, 0, 255),
            Color::from_rgba8(0, 0, 255, 255),
        ],
        vec![0, 1, 2],
    )
}

#[test]
fn a_mesh_becomes_a_type_4_shading() {
    let mut s = scene();
    s.draw_mesh(&triangle(), Affine::IDENTITY, PickId::Skip);
    let pdf = encode_pdf(&s);
    let body = text(&pdf);
    assert!(body.contains("/ShadingType 4"), "{body}");
    assert!(
        !body.contains("/PatternType"),
        "no per-triangle gradient patterns: {body}"
    );
    assert!(body.contains(" sh\n"), "{body}");
    // Twelve bytes per vertex — a flag, two 32-bit coordinates and
    // three color bytes — and three vertices per triangle.
    let at = find(&pdf, b"/BitsPerFlag 8").expect("the shading dictionary");
    let length_at = at + find(&pdf[at..], b"/Length ").expect("the shading's own length");
    let digits = length_at + b"/Length ".len();
    let digits_end = digits
        + pdf[digits..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .unwrap();
    let declared: usize = std::str::from_utf8(&pdf[digits..digits_end])
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(declared, 3 * 12);
    assert_well_formed(&pdf);
}

#[test]
fn an_empty_mesh_paints_nothing() {
    let mut s = scene();
    s.draw_mesh(
        &Mesh::new(Vec::new(), Vec::new(), Vec::new()),
        Affine::IDENTITY,
        PickId::Skip,
    );
    assert!(!text(&encode_pdf(&s)).contains("/ShadingType 4"));
}

// ─── Text ───────────────────────────────────────────────────────────────────

fn with_text(label: &str) -> PdfScene {
    let mut s = scene();
    let style = TextStyle::new(12.0);
    let run = TextRun::new(label, &style, 96.0);
    draw_text(
        &mut s,
        &run,
        10.0,
        100.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    s
}

#[test]
fn text_embeds_a_cid_font() {
    let s = with_text("Embedded");
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("/Subtype /Type0"), "{pdf}");
    assert!(pdf.contains("/Encoding /Identity-H"), "{pdf}");
    assert!(pdf.contains("/Subtype /CIDFontType2"), "{pdf}");
    assert!(pdf.contains("/CIDToGIDMap /Identity"), "{pdf}");
    assert!(pdf.contains("/FontFile2"), "{pdf}");
    assert!(pdf.contains(" TJ\n") || pdf.contains(" Tj\n"), "{pdf}");
    assert_well_formed(&encode_pdf(&s));
}

/// The size claim the whole font design rests on.
#[test]
fn the_embedded_subset_is_far_smaller_than_the_face_it_came_from() {
    let s = with_text("A short axis label");
    let pdf = encode_pdf(&s);
    let body = text(&pdf);
    let at = body.find("/Length1 ").expect("a /FontFile2 stream");
    let rest = &body[at + "/Length1 ".len()..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap();
    let embedded: usize = rest[..end].parse().unwrap();
    assert!(
        embedded > 0 && embedded < 20_000,
        "a label's subset should be a few kB, not {embedded} bytes"
    );
}

/// Validates the sfnt builder end to end against an independent
/// reader. Nothing else catches a checksum, `loca` or flags bug.
#[test]
fn the_embedded_font_parses_back_and_draws_every_glyph() {
    use std::io::Read;
    // Compressed, which is the default a caller gets, so this also
    // proves the `FlateDecode` payload is a zlib stream.
    let mut s = PdfScene::new(Size::new(W, H), 96.0);
    let style = TextStyle::new(12.0);
    let run = TextRun::new("Wavy", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        10.0,
        100.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let pdf = encode_pdf(&s);
    let body = text(&pdf);
    let at = find(&pdf, b"/Length1 ").expect("a /FontFile2 stream");
    let start = find(&pdf[at..], b">>\nstream\n").unwrap() + at + b">>\nstream\n".len();
    let end = find(&pdf[start..], b"\nendstream").unwrap() + start;
    let mut program = Vec::new();
    flate2::read::ZlibDecoder::new(&pdf[start..end])
        .read_to_end(&mut program)
        .expect("the font program inflates");

    use skrifa::instance::{LocationRef, Size as SkSize};
    use skrifa::outline::DrawSettings;
    use skrifa::{FontRef, MetadataProvider};
    let font = FontRef::from_index(&program, 0).expect("a parseable font");
    let outlines = font.outline_glyphs();
    let metrics = font.glyph_metrics(SkSize::unscaled(), LocationRef::default());

    // The `/W` array names one advance per subset glyph, from CID 1.
    let w_at = body.find("/W [ 1 [").expect("a /W array");
    let w_end = body[w_at..].find("] ]").unwrap() + w_at;
    let widths: Vec<f64> = body[w_at + "/W [ 1 [".len()..w_end]
        .split_whitespace()
        .map(|t| t.parse().unwrap())
        .collect();
    assert!(!widths.is_empty(), "the subset has glyphs");

    let upem = f64::from(
        font.metrics(SkSize::unscaled(), LocationRef::default())
            .units_per_em,
    );
    let mut drew = 0;
    for (i, w) in widths.iter().enumerate() {
        let gid = skrifa::GlyphId::new(i as u32 + 1);
        let advance = metrics.advance_width(gid).expect("an advance");
        assert_eq!(
            (f64::from(advance) * 1000.0 / upem).round(),
            *w,
            "glyph {} advance disagrees with its /W entry",
            i + 1
        );
        if let Some(g) = outlines.get(gid) {
            let mut pen = CountingPen::default();
            g.draw(
                DrawSettings::unhinted(SkSize::unscaled(), LocationRef::default()),
                &mut pen,
            )
            .expect("draws");
            drew += usize::from(pen.moves > 0);
        }
    }
    assert!(drew > 0, "at least one subset glyph has contours");
}

#[derive(Default)]
struct CountingPen {
    moves: usize,
}

impl skrifa::outline::OutlinePen for CountingPen {
    fn move_to(&mut self, _x: f32, _y: f32) {
        self.moves += 1;
    }
    fn line_to(&mut self, _x: f32, _y: f32) {}
    fn quad_to(&mut self, _cx: f32, _cy: f32, _x: f32, _y: f32) {}
    fn curve_to(&mut self, _a: f32, _b: f32, _c: f32, _d: f32, _x: f32, _y: f32) {}
    fn close(&mut self) {}
}

#[test]
fn to_unicode_maps_a_glyph_back_to_its_character() {
    let s = with_text("A");
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("beginbfchar"), "{pdf}");
    assert!(pdf.contains("<0041>"), "the letter A is recoverable: {pdf}");
}

#[test]
fn a_variable_font_instance_embeds_its_own_outlines() {
    use hephaestus::style_vocab::FontVariationSetting;
    let mut s = scene();
    for weight in [300.0f32, 800.0] {
        let style = TextStyle::new(12.0)
            .family("Inter")
            .variations(vec![FontVariationSetting {
                tag: *b"wght",
                value: weight,
            }]);
        let run = TextRun::new("Weight", &style, 96.0);
        draw_text(
            &mut s,
            &run,
            10.0,
            f64::from(weight) / 10.0,
            &black(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }
    let pdf = text(&encode_pdf(&s));
    // Two instances of one family are two embedded programs. A face
    // with no `wght` axis resolves both requests to one instance, so
    // this only asserts the file is coherent either way.
    assert!(pdf.contains("/FontFile2"), "{pdf}");
    assert_well_formed(&encode_pdf(&s));
}

#[test]
fn haloed_text_stacks_the_stroke_pass_behind_the_fill() {
    use hephaestus::text::draw_text_outline;
    let mut s = scene();
    let style = TextStyle::new(14.0);
    let run = TextRun::new("halo", &style, 96.0);
    draw_text_outline(
        &mut s,
        &run,
        10.0,
        100.0,
        &Brush::Solid(Color::WHITE),
        &Stroke::new(3.0),
        Affine::IDENTITY,
        PickId::Skip,
    );
    draw_text(
        &mut s,
        &run,
        10.0,
        100.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let pdf = text(&encode_pdf(&s));
    let stroke_at = pdf.find("1 Tr").expect("a stroking text object");
    let fill_at = pdf.find("0 Tr").expect("a filling text object");
    assert!(stroke_at < fill_at, "the halo is painted first");
    assert_well_formed(&encode_pdf(&s));
}

// ─── Links ──────────────────────────────────────────────────────────────────

fn rich(source: &str, config: PdfConfig) -> PdfScene {
    let mut s = PdfScene::with_config(Size::new(W, H), 96.0, config);
    let run = RichTextRun::new(
        source,
        &TextStyle::new(12.0).family("Inter"),
        Color::BLACK,
        &RichTextStyleSheet::default(),
        &hephaestus::style_vocab::Palette::default(),
        96.0,
    );
    draw_rich_text(
        &mut s,
        &run,
        10.0,
        30.0,
        RichAnchor::default(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    s
}

#[test]
fn a_markdown_link_becomes_a_link_annotation() {
    let s = rich(
        "see [the docs](https://example.com/a?b=1)",
        PdfConfig::new().compress(false),
    );
    let pdf = text(&encode_pdf(&s));
    assert!(pdf.contains("/Subtype /Link"), "{pdf}");
    assert!(pdf.contains("/URI (https://example.com/a?b=1)"), "{pdf}");
    assert!(pdf.contains("/Border [0 0 0]"), "{pdf}");
    assert_well_formed(&encode_pdf(&s));
}

#[test]
fn a_dangerous_link_scheme_is_dropped_but_its_text_survives() {
    let s = rich(
        "see [the docs](javascript:alert(1))",
        PdfConfig::new().compress(false),
    );
    let pdf = text(&encode_pdf(&s));
    assert!(!pdf.contains("/Subtype /Link"), "{pdf}");
    assert!(!pdf.contains("javascript"), "{pdf}");
    assert!(
        pdf.contains(" TJ\n") || pdf.contains(" Tj\n"),
        "the text is still drawn: {pdf}"
    );
}

#[test]
fn links_can_be_switched_off() {
    let s = rich(
        "see [the docs](https://example.com)",
        PdfConfig::new().compress(false).links(false),
    );
    assert!(!text(&encode_pdf(&s)).contains("/Subtype /Link"));
}

// ─── Compression ────────────────────────────────────────────────────────────

#[test]
fn compression_changes_the_bytes_but_not_the_structure() {
    let build = |compress: bool| {
        let mut s = PdfScene::with_config(
            Size::new(W, H),
            96.0,
            PdfConfig::new()
                .compress(compress)
                .background(Some(Color::WHITE)),
        );
        s.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &black(),
            None,
            &rect_path(Rect::new(10.0, 10.0, 100.0, 80.0)),
            PickId::Skip,
        );
        let style = TextStyle::new(12.0);
        let run = TextRun::new("compressed", &style, 96.0);
        draw_text(
            &mut s,
            &run,
            5.0,
            190.0,
            &black(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        encode_pdf(&s)
    };
    let plain = build(false);
    let packed = build(true);
    assert_well_formed(&plain);
    assert_well_formed(&packed);
    assert_ne!(plain, packed);
    assert!(text(&packed).contains("/Filter /FlateDecode"));
}

// ─── Color glyphs ───────────────────────────────────────────────────────────

/// Skips itself when the resolved face carries no color glyph, which is
/// the common case on a machine with no emoji font — a system font
/// would otherwise make this suite machine-dependent.
#[test]
fn an_emoji_renders_as_graphics_not_text() {
    let mut s = scene();
    let style = TextStyle::new(24.0);
    let run = TextRun::new("\u{1F600}", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        10.0,
        100.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let pdf = text(&encode_pdf(&s));
    if !pdf.contains("/ActualText") {
        println!("no color font resolved for U+1F600; skipping");
        return;
    }
    assert!(pdf.contains("BDC"), "{pdf}");
    assert!(pdf.contains("EMC"), "{pdf}");
    assert_well_formed(&encode_pdf(&s));
}
