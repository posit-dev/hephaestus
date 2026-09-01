//! The SVG backend, end to end.
//!
//! Assertions are on emitted *structure*, never on a golden byte
//! baseline. A baseline would need regenerating on every cosmetic
//! change, which teaches everyone to regenerate it without reading the
//! diff — and it is the same reasoning `document_roundtrip.rs` gives for
//! asserting on draw calls rather than pixels.

#![cfg(feature = "svg")]

use hephaestus::backend::svg::{encode_svg, SvgConfig, SvgScene, SvgUnits, SvgWarning};
use hephaestus::brush::{Brush, Gradient};
use hephaestus::color::Color;
use hephaestus::geometry::{Affine, Point, Rect, Shape, Size};
use hephaestus::path::{FillRule, Path};
use hephaestus::pick::PickId;
use hephaestus::scene::SceneBuilder;
use hephaestus::stroke::Stroke;
use hephaestus::style_vocab::{FontFeatureSetting, FontVariationSetting, HAlign, Palette};
use hephaestus::text::rich::{draw_rich_text, RichAnchor, RichTextRun, RichTextStyleSheet};
use hephaestus::text::{draw_text, TextRun, TextStyle};

const W: f64 = 320.0;
const H: f64 = 200.0;

fn scene() -> SvgScene {
    SvgScene::new(Size::new(W, H), 96.0)
}

fn rect_path(r: Rect) -> Path {
    Shape::to_path(&r, 0.1)
}

fn black() -> Brush {
    Brush::Solid(Color::BLACK)
}

// ─── Well-formedness ────────────────────────────────────────────────────────

/// Every tag opened is closed, in order, and the document ends balanced.
///
/// The highest-value check here: an unbalanced `</g>` is the
/// characteristic failure of a streaming emitter, and it makes the file
/// unusable rather than merely wrong.
fn assert_well_formed(svg: &str) {
    let mut stack: Vec<&str> = Vec::new();
    let bytes = svg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Skip CDATA wholesale — it is opaque text, not markup.
        if svg[i..].starts_with("<![CDATA[") {
            match svg[i..].find("]]>") {
                Some(j) => {
                    i += j + 3;
                    continue;
                }
                None => panic!("unterminated CDATA"),
            }
        }
        let end = svg[i..].find('>').expect("unterminated tag") + i;
        let inner = &svg[i + 1..end];
        if let Some(name) = inner.strip_prefix('/') {
            let open = stack
                .pop()
                .unwrap_or_else(|| panic!("</{name}> with nothing open"));
            assert_eq!(open, name.trim(), "mismatched close tag");
        } else if !inner.ends_with('/') && !inner.starts_with('?') && !inner.starts_with('!') {
            let name = inner.split_whitespace().next().unwrap_or(inner);
            stack.push(name);
        }
        i = end + 1;
    }
    assert!(stack.is_empty(), "unclosed tags: {stack:?}");
}

#[test]
fn a_scene_of_every_primitive_produces_a_well_formed_document() {
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Id(3),
    );
    s.stroke(
        &Stroke::new(2.0),
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Id(3),
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
    s.pop_layer();
    let style = TextStyle::new(12.0);
    let run = TextRun::new("hello", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        90.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );

    let svg = encode_svg(&s);
    assert_well_formed(&svg);
    assert!(svg.starts_with("<svg "));
    assert!(svg.ends_with("</svg>"));
}

#[test]
fn unbalanced_layers_are_closed_rather_than_left_open() {
    let mut s = scene();
    let r = rect_path(Rect::new(0.0, 0.0, 10.0, 10.0));
    s.push_layer(Default::default(), 1.0, Affine::IDENTITY, &r);
    s.push_layer(Default::default(), 1.0, Affine::IDENTITY, &r);
    // Never popped.
    let svg = encode_svg(&s);
    assert_well_formed(&svg);
}

#[test]
fn popping_more_layers_than_were_pushed_is_reported_not_fatal() {
    let mut s = scene();
    s.pop_layer();
    assert!(s.warnings().contains(&SvgWarning::UnbalancedLayers));
    assert_well_formed(&encode_svg(&s));
}

// ─── Document invariants ────────────────────────────────────────────────────

#[test]
fn every_referenced_id_is_defined_exactly_once() {
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
    for i in 0..3 {
        let clip = rect_path(Rect::new(i as f64, 0.0, 50.0, 50.0));
        s.push_layer(Default::default(), 1.0, Affine::IDENTITY, &clip);
        let g = Gradient::new_linear((0.0, 0.0), (10.0 * i as f64 + 1.0, 0.0))
            .with_stops([Color::BLACK, Color::WHITE]);
        s.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Gradient(g),
            None,
            &r,
            PickId::Skip,
        );
        s.pop_layer();
    }
    let svg = encode_svg(&s);

    let defined: Vec<&str> = svg
        .match_indices(" id=\"")
        .map(|(i, _)| {
            let rest = &svg[i + 5..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect();
    let mut sorted = defined.clone();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "an id was defined twice: {defined:?}");

    for (i, _) in svg.match_indices("url(#") {
        let rest = &svg[i + 5..];
        let id = &rest[..rest.find(')').unwrap()];
        assert!(defined.contains(&id), "url(#{id}) resolves to nothing");
    }
}

#[test]
fn two_renders_of_one_scene_are_byte_identical() {
    // Catches hash iteration order leaking into id allocation, which is
    // the usual way a hand-rolled emitter stops being reproducible.
    let build = || {
        let mut s = scene();
        let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
        for i in 0..8 {
            let g = Gradient::new_linear((0.0, 0.0), (i as f64 + 1.0, 0.0))
                .with_stops([Color::BLACK, Color::WHITE]);
            s.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &Brush::Gradient(g),
                None,
                &r,
                PickId::Skip,
            );
            let clip = rect_path(Rect::new(0.0, i as f64, 50.0, 50.0));
            s.push_layer(Default::default(), 1.0, Affine::IDENTITY, &clip);
            s.pop_layer();
        }
        encode_svg(&s)
    };
    assert_eq!(build(), build());
}

#[test]
fn clear_really_resets_the_scene() {
    let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));

    let mut used = scene();
    let g = Gradient::new_linear((0.0, 0.0), (5.0, 0.0)).with_stops([Color::BLACK, Color::WHITE]);
    used.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(g),
        None,
        &r,
        PickId::Skip,
    );
    used.push_layer(Default::default(), 1.0, Affine::IDENTITY, &r);
    let style = TextStyle::new(12.0);
    let run = TextRun::new("discarded", &style, 96.0);
    draw_text(
        &mut used,
        &run,
        1.0,
        2.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    used.clear();
    used.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );

    let mut fresh = scene();
    fresh.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );

    assert_eq!(encode_svg(&used), encode_svg(&fresh));
}

#[test]
fn no_coordinate_is_written_in_scientific_notation_or_as_a_non_number() {
    let mut s = scene();
    let mut p = Path::new();
    p.move_to(Point::new(1e-9, 3.0));
    p.line_to(Point::new(1e18, f64::NAN));
    p.line_to(Point::new(f64::INFINITY, 2.0));
    p.close_path();
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &p,
        PickId::Skip,
    );
    let svg = encode_svg(&s);

    for bad in ["NaN", "nan", "inf", "Infinity"] {
        assert!(!svg.contains(bad), "{bad:?} reached the document: {svg}");
    }
    let b = svg.as_bytes();
    for i in 1..b.len().saturating_sub(1) {
        if (b[i] == b'e' || b[i] == b'E') && b[i - 1].is_ascii_digit() {
            let next = b[i + 1];
            assert!(
                !(next == b'+' || next == b'-' || next.is_ascii_digit()),
                "exponent notation at byte {i}: {}",
                &svg[i.saturating_sub(12)..(i + 8).min(svg.len())]
            );
        }
    }
    assert_well_formed(&svg);
}

#[test]
fn root_attributes_describe_the_requested_size() {
    let s = scene();
    let svg = encode_svg(&s);
    assert!(svg.contains("width=\"320\""), "{svg}");
    assert!(svg.contains("height=\"200\""), "{svg}");
    assert!(svg.contains("viewBox=\"0 0 320 200\""), "{svg}");

    // Points: the physical size changes, the coordinate system does not.
    let s = SvgScene::with_config(Size::new(W, H), 192.0, SvgConfig::new().units(SvgUnits::Pt));
    let svg = encode_svg(&s);
    assert!(svg.contains("width=\"120pt\""), "{svg}");
    assert!(svg.contains("viewBox=\"0 0 320 200\""), "{svg}");
}

#[test]
fn an_id_prefix_keeps_two_documents_from_colliding_on_one_page() {
    let mut s = SvgScene::with_config(Size::new(W, H), 96.0, SvgConfig::new().id_prefix("plot2-"));
    let clip = rect_path(Rect::new(0.0, 0.0, 10.0, 10.0));
    s.push_layer(Default::default(), 1.0, Affine::IDENTITY, &clip);
    s.pop_layer();
    let svg = encode_svg(&s);
    assert!(svg.contains("id=\"plot2-c0\""), "{svg}");
    assert!(svg.contains("url(#plot2-c0)"), "{svg}");
}

// ─── One object per thing ───────────────────────────────────────────────────

#[test]
fn a_filled_and_stroked_path_is_one_element_carrying_both() {
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
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
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    let svg = encode_svg(&s);

    assert_eq!(svg.matches("<path").count(), 1, "two stacked paths: {svg}");
    assert!(svg.contains("fill=\"#000000\""));
    assert!(svg.contains("stroke=\"#000000\""));
    assert_eq!(
        svg.matches("d=\"").count(),
        1,
        "the geometry is written once"
    );
}

#[test]
fn a_stroke_of_a_different_path_does_not_merge() {
    let mut s = scene();
    let a = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
    let b = rect_path(Rect::new(11.0, 10.0, 90.0, 60.0));
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &a,
        PickId::Skip,
    );
    s.stroke(
        &Stroke::new(2.0),
        Affine::IDENTITY,
        &black(),
        None,
        &b,
        PickId::Skip,
    );
    assert_eq!(encode_svg(&s).matches("<path").count(), 2);
}

#[test]
fn a_wrapped_label_is_one_text_element_with_a_tspan_per_line() {
    let mut s = scene();
    let style = TextStyle::new(12.0);
    let run = TextRun::new(
        "The quick brown fox jumps over the lazy dog near the riverbank",
        &style,
        96.0,
    );
    run.set_max_width(120.0, HAlign::Start);
    draw_text(
        &mut s,
        &run,
        8.0,
        20.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let svg = encode_svg(&s);

    assert_eq!(
        svg.matches("<text").count(),
        1,
        "a wrapped block is one editable object, not one per line: {svg}"
    );
    assert!(
        svg.matches("<tspan").count() > 1,
        "the lines should be tspans: {svg}"
    );
    assert_well_formed(&svg);
}

#[test]
fn outlined_text_is_one_element_with_a_stroke_and_a_fill() {
    use hephaestus::text::draw_text_outline;
    let mut s = scene();
    let style = TextStyle::new(14.0);
    let run = TextRun::new("haloed", &style, 96.0);
    let stroke = Stroke::new(2.0);
    draw_text_outline(
        &mut s,
        &run,
        10.0,
        30.0,
        &black(),
        &stroke,
        Affine::IDENTITY,
        PickId::Skip,
    );
    draw_text(
        &mut s,
        &run,
        10.0,
        30.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let svg = encode_svg(&s);

    assert_eq!(
        svg.matches(">haloed<").count(),
        1,
        "the string must appear once — two stacked copies means editing \
         one leaves the other spelling the old text: {svg}"
    );
    assert_eq!(svg.matches("<text").count(), 1, "{svg}");
    assert!(svg.contains("paint-order=\"stroke fill\""), "{svg}");
    assert!(svg.contains("stroke-width="), "{svg}");
}

// ─── Text is text ───────────────────────────────────────────────────────────

#[test]
fn labels_arrive_as_editable_text_naming_their_font() {
    let mut s = scene();
    let style = TextStyle::new(11.0).family("Inter");
    for (i, label) in ["Median revenue", "2024", "north-west"].iter().enumerate() {
        let run = TextRun::new(label, &style, 96.0);
        draw_text(
            &mut s,
            &run,
            10.0,
            20.0 + 20.0 * i as f64,
            &black(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }
    let svg = encode_svg(&s);
    for label in ["Median revenue", "2024", "north-west"] {
        assert!(
            svg.contains(&format!(">{label}</tspan>")),
            "{label:?} is not present as text: {svg}"
        );
    }
    assert!(svg.contains("font-family=\"Inter, sans-serif\""), "{svg}");
    assert!(svg.contains("textLength=\""), "{svg}");
    assert!(svg.contains("lengthAdjust=\"spacingAndGlyphs\""), "{svg}");
    // Alignment is already baked into the shaped positions.
    assert!(!svg.contains("text-anchor"), "{svg}");
}

/// The case that ruled out recovering characters by inverting the
/// font's cmap: shaping substitutes and reorders here, so glyph ids do
/// not map back to the source.
#[test]
fn non_latin_labels_survive_as_their_source_text() {
    for label in ["日本語のラベル", "العربية", "हिन्दी", "naïve café"] {
        let mut s = scene();
        let style = TextStyle::new(12.0);
        let run = TextRun::new(label, &style, 96.0);
        draw_text(
            &mut s,
            &run,
            5.0,
            20.0,
            &black(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let svg = encode_svg(&s);
        let text: String = svg
            .match_indices('>')
            .filter_map(|(i, _)| {
                let rest = &svg[i + 1..];
                rest.find("</tspan>").map(|j| &rest[..j])
            })
            .filter(|t| !t.contains('<'))
            .collect();
        assert_eq!(text, label, "in {svg}");
    }
}

#[test]
fn xml_reserved_characters_in_a_label_are_escaped() {
    let mut s = scene();
    let style = TextStyle::new(12.0);
    let run = TextRun::new("a<b & c>d", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        20.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    assert!(svg.contains("a&lt;b &amp; c&gt;d"), "{svg}");
    assert_well_formed(&svg);
}

// ─── Rich text ──────────────────────────────────────────────────────────────

fn rich(source: &str) -> String {
    let mut s = scene();
    let run = RichTextRun::new(
        source,
        &TextStyle::new(12.0).family("Inter"),
        Color::BLACK,
        &RichTextStyleSheet::default(),
        &Palette::default(),
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
    encode_svg(&s)
}

#[test]
fn markdown_spans_become_styled_tspans_in_one_text_element() {
    let svg = rich("plain **bold** and *italic*");
    assert_eq!(
        svg.matches("<text").count(),
        1,
        "a paragraph is one editable object: {svg}"
    );
    assert!(svg.contains(">bold</tspan>"), "{svg}");
    assert!(svg.contains("font-weight=\"700\""), "{svg}");
    assert!(svg.contains("font-style=\"italic\""), "{svg}");
    assert_well_formed(&svg);
}

#[test]
fn an_underlined_span_is_semantic_and_leaves_no_rule_behind() {
    let svg = rich("some _underlined_ words");
    assert!(
        svg.contains("text-decoration-line=\"underline\""),
        "the rule should be semantic: {svg}"
    );
    assert!(
        !svg.contains("<path"),
        "the decoration rect should have been suppressed: {svg}"
    );
    assert_eq!(
        svg.matches("<text").count(),
        1,
        "and suppressing it must not split the block: {svg}"
    );
}

/// A span background arrives as an ordinary fill *between* two glyph
/// runs of one paragraph. Letting it through would end the `<text>` and
/// start another, so a `code` span would split its own sentence into two
/// objects. Instead a block's own chrome is held and written ahead of
/// the text it belongs to.
#[test]
fn a_span_background_does_not_split_the_text_it_sits_behind() {
    let svg = rich("plain `code` plain");
    assert_eq!(
        svg.matches("<text").count(),
        1,
        "the sentence must stay one editable object: {svg}"
    );
    let text: String = svg
        .match_indices("<tspan")
        .filter_map(|(i, _)| {
            let rest = &svg[i..];
            let open = rest.find('>')? + 1;
            let close = rest.find("</tspan>")?;
            Some(&rest[open..close])
        })
        .collect();
    assert_eq!(text, "plain code plain", "{svg}");
    // The background is still drawn, and behind rather than over.
    let bg = svg.find("<path").expect("a background path");
    let txt = svg.find("<text").expect("the text");
    assert!(bg < txt, "the background must precede the text: {svg}");
    assert_well_formed(&svg);
}

#[test]
fn a_rect_that_no_run_predicted_is_still_drawn() {
    // The suppression works by predicting the decoration rect, so an
    // ordinary rect near a label can never be swallowed.
    let mut s = scene();
    let style = TextStyle::new(12.0);
    let run = TextRun::new("label", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        10.0,
        30.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let r = rect_path(Rect::new(10.0, 32.0, 60.0, 33.0));
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    assert_eq!(svg.matches("<path").count(), 1, "{svg}");
}

#[test]
fn a_markdown_link_becomes_an_anchor_that_cannot_navigate_the_host_page() {
    let svg = rich("see [the docs](https://example.com/a?x=1&y=2) here");
    assert!(
        svg.contains("<a href=\"https://example.com/a?x=1&amp;y=2\""),
        "{svg}"
    );
    assert!(svg.contains("target=\"_blank\""), "{svg}");
    assert!(svg.contains("rel=\"noopener noreferrer\""), "{svg}");
    assert!(svg.contains(">the docs</tspan>"), "{svg}");
    assert_well_formed(&svg);
}

#[test]
fn a_dangerous_link_scheme_is_dropped_but_its_text_survives() {
    let svg = rich("click [here](javascript:alert(1)) now");
    assert!(
        !svg.contains("<a href"),
        "no anchor should be emitted: {svg}"
    );
    assert!(!svg.contains("javascript:"), "{svg}");
    assert!(
        svg.contains(">here</tspan>"),
        "the text still renders: {svg}"
    );
}

// ─── Degradations ───────────────────────────────────────────────────────────

#[test]
fn a_sweep_gradient_degrades_to_a_flat_fill_and_reports_it() {
    let mut s = scene();
    let g = Gradient::new_sweep((50.0, 50.0), 0.0, std::f32::consts::TAU)
        .with_stops([Color::BLACK, Color::WHITE]);
    let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(g),
        None,
        &r,
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    assert!(s.warnings().contains(&SvgWarning::SweepGradient));
    assert!(!svg.contains("url(#"), "no paint server was made: {svg}");
    assert_well_formed(&svg);
}

#[test]
fn a_porter_duff_composite_is_reported_and_the_group_still_renders() {
    use hephaestus::blend::{BlendMode, Compose, Mix};
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
    s.push_layer(
        BlendMode::new(Mix::Normal, Compose::Xor),
        1.0,
        Affine::IDENTITY,
        &r,
    );
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &r,
        PickId::Skip,
    );
    s.pop_layer();
    assert!(s.warnings().contains(&SvgWarning::UnsupportedCompose));
    assert_well_formed(&encode_svg(&s));
}

#[test]
fn a_mix_mode_maps_to_its_css_keyword() {
    use hephaestus::blend::{BlendMode, Compose, Mix};
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
    s.push_layer(
        BlendMode::new(Mix::Multiply, Compose::SrcOver),
        1.0,
        Affine::IDENTITY,
        &r,
    );
    s.pop_layer();
    let svg = encode_svg(&s);
    assert!(svg.contains("mix-blend-mode:multiply"), "{svg}");
    // Scopes the blend to this document rather than the host page.
    assert!(svg.contains("isolation:isolate"), "{svg}");
}

#[test]
fn asymmetric_caps_are_reported_since_svg_has_only_one() {
    let mut s = scene();
    let mut p = Path::new();
    p.move_to(Point::new(0.0, 0.0));
    p.line_to(Point::new(10.0, 0.0));
    let stroke = Stroke::new(2.0)
        .with_start_cap(hephaestus::stroke::Cap::Butt)
        .with_end_cap(hephaestus::stroke::Cap::Round);
    s.stroke(&stroke, Affine::IDENTITY, &black(), None, &p, PickId::Skip);
    assert!(s.warnings().contains(&SvgWarning::AsymmetricCaps));
}

#[test]
fn warnings_are_reported_once_however_often_they_recur() {
    let mut s = scene();
    let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
    for _ in 0..50 {
        let g = Gradient::new_sweep((0.0, 0.0), 0.0, 1.0).with_stops([Color::BLACK, Color::WHITE]);
        s.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Gradient(g),
            None,
            &r,
            PickId::Skip,
        );
    }
    assert_eq!(
        s.warnings()
            .iter()
            .filter(|w| **w == SvgWarning::SweepGradient)
            .count(),
        1
    );
}

// ─── Picking ────────────────────────────────────────────────────────────────

#[test]
fn picking_attributes_are_off_by_default_and_complete_when_on() {
    let draw = |s: &mut SvgScene| {
        let r = rect_path(Rect::new(10.0, 10.0, 90.0, 60.0));
        s.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &black(),
            None,
            &r,
            PickId::Id(42),
        );
        let r2 = rect_path(Rect::new(20.0, 20.0, 30.0, 30.0));
        s.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &black(),
            None,
            &r2,
            PickId::Skip,
        );
        let r3 = rect_path(Rect::new(40.0, 20.0, 50.0, 30.0));
        s.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &black(),
            None,
            &r3,
            PickId::Block,
        );
    };

    let mut off = scene();
    draw(&mut off);
    let svg = encode_svg(&off);
    assert!(!svg.contains("data-pick-id"), "{svg}");

    let mut on = SvgScene::with_config(Size::new(W, H), 96.0, SvgConfig::new().pick_ids(true));
    draw(&mut on);
    let svg = encode_svg(&on);
    assert!(svg.contains("data-pick-id=\"42\""), "{svg}");
    assert!(
        svg.contains("data-pick-id=\"0\""),
        "Block records as 0: {svg}"
    );
    // Without this a skipped gridline over a mark swallows the hit.
    assert!(svg.contains("pointer-events=\"none\""), "{svg}");
}

// ─── Images and meshes ──────────────────────────────────────────────────────

#[cfg(feature = "png")]
fn checkerboard() -> hephaestus::brush::Image {
    let (w, h) = (4u32, 4u32);
    let mut px: Vec<u8> = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let v: u8 = if (x + y) % 2 == 0 { 255 } else { 0 };
            px.extend_from_slice(&[v, v, v, 255]);
        }
    }
    hephaestus::image::from_rgba8(w, h, px).expect("image")
}

#[test]
#[cfg(feature = "png")]
fn an_image_is_embedded_once_and_referenced_per_draw() {
    let mut s = scene();
    let img = checkerboard();
    for i in 0..5 {
        s.draw_image(
            &img,
            Affine::translate((i as f64 * 10.0, 0.0)),
            hephaestus::brush::Sampling::Nearest,
            1.0,
            PickId::Skip,
        );
    }
    let svg = encode_svg(&s);
    assert_eq!(
        svg.matches("data:image/png;base64,").count(),
        1,
        "the payload must be written once, not once per draw"
    );
    assert_eq!(svg.matches("<use ").count(), 5, "{svg}");
    assert!(svg.contains("image-rendering=\"pixelated\""), "{svg}");
    assert!(s.warnings().is_empty(), "{:?}", s.warnings());
    assert_well_formed(&svg);
}

/// Without an encoder there is nothing to embed, so an image reports
/// rather than drawing something wrong. Constructed by hand because the
/// `image` module is itself gated on having a codec.
#[test]
#[cfg(not(feature = "png"))]
fn without_a_png_encoder_an_image_degrades_and_says_so() {
    use hephaestus::brush::{Blob, Image, ImageAlphaType, ImageFormat, Sampling};
    let mut s = scene();
    let img = Image {
        data: Blob::from(vec![255u8; 4 * 4 * 4]),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 4,
        height: 4,
    };
    s.draw_image(
        &img,
        Affine::IDENTITY,
        Sampling::Bilinear,
        1.0,
        PickId::Skip,
    );
    assert!(s.warnings().contains(&SvgWarning::MissingPngFeature));
    assert_well_formed(&encode_svg(&s));
}

#[test]
fn a_mesh_decomposes_into_ordinary_fills() {
    use hephaestus::mesh::Mesh;
    let mut s = scene();
    let mesh = Mesh {
        vertices: vec![
            Point::new(0.0, 0.0),
            Point::new(20.0, 0.0),
            Point::new(10.0, 20.0),
        ],
        colors: vec![Color::BLACK, Color::WHITE, Color::BLACK],
        indices: vec![0, 1, 2],
    };
    s.draw_mesh(&mesh, Affine::IDENTITY, PickId::Skip);
    let svg = encode_svg(&s);
    assert!(svg.contains("<path"), "the triangle should be drawn: {svg}");
    assert_well_formed(&svg);
}

// ─── Outlines ───────────────────────────────────────────────────────────────

#[test]
fn a_run_with_no_source_text_is_drawn_as_outlines_not_dropped() {
    use hephaestus::scene::{Font, Glyph, GlyphRun};
    // What a caller positioning glyphs itself produces — a marker
    // shape, say. There is no string to emit, but the glyphs must
    // still appear.
    let style = TextStyle::new(24.0);
    let probe = TextRun::new("A", &style, 96.0);
    // Reach a real face through a shaped run so the ids are valid.
    let mut probe_scene = scene();
    draw_text(
        &mut probe_scene,
        &probe,
        0.0,
        0.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let with_text = encode_svg(&probe_scene);
    assert!(with_text.contains("<text"), "baseline: {with_text}");

    // The same glyphs with the source stripped.
    let mut recorded = hephaestus::scene::recording::RecordingScene::new();
    draw_text(
        &mut recorded,
        &probe,
        10.0,
        40.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let op = recorded
        .ops
        .iter()
        .find_map(|o| match o {
            hephaestus::scene::recording::Op::DrawGlyphs(r) => Some(r),
            _ => None,
        })
        .expect("a glyph run");
    let glyphs: Vec<Glyph> = op.glyphs.clone();
    let font: Font = op.font.clone();
    let brush = black();
    let mut s = scene();
    s.draw_glyphs(
        &GlyphRun {
            font: &font,
            font_size: op.font_size,
            transform: Affine::IDENTITY,
            glyph_transform: None,
            brush: &brush,
            brush_alpha: 1.0,
            hint: false,
            glyphs: &glyphs,
            style: None,
            source: None,
        },
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    assert!(!svg.contains("<text"), "no text to emit: {svg}");
    assert!(
        svg.contains("<path d=\"M"),
        "the glyphs must still be drawn, as outlines: {svg}"
    );
    assert_well_formed(&svg);
}

#[test]
fn outline_mode_turns_labels_into_paths() {
    use hephaestus::backend::svg::TextMode;
    let mut s = SvgScene::with_config(
        Size::new(W, H),
        96.0,
        SvgConfig::new().text(TextMode::Outline),
    );
    let style = TextStyle::new(18.0);
    let run = TextRun::new("outlined", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        10.0,
        40.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    assert!(!svg.contains("<text"), "{svg}");
    assert!(svg.contains("<path d=\"M"), "{svg}");
    assert_well_formed(&svg);
}

// ─── Fonts ──────────────────────────────────────────────────────────────────

/// Embedding either inlines the face or says why it could not.
///
/// Which of the two happens is a property of the machine: a face that
/// is a *collection* — which is what macOS resolves `sans-serif` to —
/// cannot be inlined at all, because `@font-face` has no way to name a
/// member of one. So the invariant is the disjunction, not either
/// branch.
#[test]
fn embedding_either_inlines_the_face_or_reports_that_it_could_not() {
    let style = TextStyle::new(12.0);
    let draw = |s: &mut SvgScene| {
        let run = TextRun::new("embedded", &style, 96.0);
        draw_text(s, &run, 5.0, 20.0, &black(), Affine::IDENTITY, PickId::Skip);
    };

    let mut plain = scene();
    draw(&mut plain);
    let small = encode_svg(&plain);
    assert!(!small.contains("@font-face"), "{small}");
    assert!(
        small.len() < 2_000,
        "nothing should be inlined by default: {} bytes",
        small.len()
    );

    let mut embedded =
        SvgScene::with_config(Size::new(W, H), 96.0, SvgConfig::new().embed_fonts(true));
    draw(&mut embedded);
    let big = encode_svg(&embedded);
    assert_well_formed(&big);

    if embedded.warnings().contains(&SvgWarning::FontNotEmbeddable) {
        assert!(!big.contains("@font-face"), "refused, so nothing inlined");
    } else {
        assert!(big.contains("@font-face"), "{}", &big[..big.len().min(400)]);
        assert!(big.contains("data:font/"), "the bytes should be inlined");
        // CDATA is what keeps a base64 payload and CSS punctuation from
        // needing any escaping at all.
        assert!(big.contains("<![CDATA["), "{}", &big[..big.len().min(400)]);
        assert!(
            big.len() > small.len() * 4,
            "embedding should dominate the file: {} vs {}",
            big.len(),
            small.len()
        );
    }
}

/// Whatever happens with delivery, the element names the face — an
/// `@font-face` that nothing references would do nothing at all.
#[test]
fn embedding_names_the_resolved_face_on_the_element() {
    let mut s = SvgScene::with_config(Size::new(W, H), 96.0, SvgConfig::new().embed_fonts(true));
    let style = TextStyle::new(12.0);
    let run = TextRun::new("named", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        20.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    let i = svg.find("font-family=\"").expect("a family is named");
    let chain = &svg[i + 13..i + 13 + svg[i + 13..].find('"').unwrap()];
    assert!(
        chain != "sans-serif",
        "the resolved face should lead the chain, not just a generic: {chain}"
    );
    assert!(
        chain.ends_with("sans-serif"),
        "and a generic still trails: {chain}"
    );
}

#[test]
fn a_family_nothing_could_deliver_is_still_named_on_the_element() {
    let mut s = scene();
    let style = TextStyle::new(12.0).family("Nonexistent Face");
    let run = TextRun::new("fallback", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        20.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    assert!(
        svg.contains("font-family=\"'Nonexistent Face', sans-serif\""),
        "the chain plus a generic tail, and the apostrophes unescaped: {svg}"
    );
    // Nothing was fetched from Google, so nothing is imported.
    assert!(!svg.contains("@import"), "{svg}");
}

// ─── Hoisted to the root ────────────────────────────────────────────────────

/// Draw `text` twice in the same style, at two places.
fn two_labels_in_one_style() -> String {
    let mut s = scene();
    let style = TextStyle::new(12.0);
    for (i, label) in ["first", "second"].iter().enumerate() {
        let run = TextRun::new(label, &style, 96.0);
        draw_text(
            &mut s,
            &run,
            5.0,
            20.0 + i as f64 * 20.0,
            &black(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }
    encode_svg(&s)
}

#[test]
fn text_agreeing_on_a_font_inherits_it_from_the_root() {
    let svg = two_labels_in_one_style();
    let root = &svg[..svg.find('>').expect("a root tag")];
    assert!(root.contains("font-family=\""), "{svg}");
    assert!(root.contains("font-size=\""), "{svg}");
    assert_eq!(
        svg.matches("font-family=\"").count(),
        1,
        "one document, one place naming the family: {svg}"
    );
    assert_eq!(svg.matches("font-size=\"").count(), 1, "{svg}");
    assert_well_formed(&svg);
}

#[test]
fn a_run_disagreeing_with_the_root_still_names_its_own_font() {
    let svg = rich("plain `code`");
    let root = &svg[..svg.find('>').expect("a root tag")];
    assert!(root.contains("font-family=\"Inter"), "{svg}");
    assert!(
        svg.contains("font-family=\"monospace"),
        "the code span names the family it needs: {svg}"
    );
    // Named on the span, not on the root: an inherited default the
    // element overrides, rather than a rule that would beat it.
    assert!(
        svg[root.len()..].contains("font-family=\"monospace"),
        "{svg}"
    );
    assert_well_formed(&svg);
}

#[test]
fn white_space_handling_is_declared_once_for_the_document() {
    let svg = two_labels_in_one_style();
    assert_eq!(svg.matches("xml:space=\"preserve\"").count(), 1, "{svg}");
    assert_eq!(svg.matches("white-space:pre").count(), 1, "{svg}");
    // Both live on the root, which is the only element that can carry
    // them for every `<text>` at once.
    let root = &svg[..svg.find('>').expect("a root tag")];
    assert!(root.contains("xml:space=\"preserve\""), "{svg}");
    assert!(root.contains("white-space:pre"), "{svg}");
}

#[test]
fn a_document_with_no_text_says_nothing_about_fonts() {
    let mut s = scene();
    s.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &black(),
        None,
        &rect_path(Rect::new(1.0, 1.0, 9.0, 9.0)),
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    assert!(!svg.contains("font-"), "{svg}");
    assert!(!svg.contains("xml:space"), "{svg}");
    assert!(!svg.contains("white-space"), "{svg}");
}

#[test]
fn a_block_still_open_at_write_time_reaches_the_root() {
    // Nothing follows the text to flush it, so the block is written
    // during serialization — after the point the root was assembled.
    let mut s = scene();
    let run = TextRun::new("late", &TextStyle::new(12.0), 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        20.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let svg = encode_svg(&s);
    let root = &svg[..svg.find('>').expect("a root tag")];
    assert!(
        root.contains("font-family=\""),
        "the font the open block named is on the root: {svg}"
    );
    assert!(svg.contains(">late</tspan>"), "{svg}");
}

#[test]
fn serializing_twice_gives_the_same_bytes() {
    let mut s = scene();
    let run = TextRun::new("held", &TextStyle::new(12.0), 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        20.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    // The open block is emitted at write time against a copy of what
    // the root carries, so writing cannot consume the claim.
    assert_eq!(encode_svg(&s), encode_svg(&s));
}

// ─── The whole face request survives ────────────────────────────────────────

/// One label in `style`, as the emitted document.
fn styled(style: TextStyle) -> String {
    let mut s = scene();
    let run = TextRun::new("Sample", &style, 96.0);
    draw_text(
        &mut s,
        &run,
        5.0,
        20.0,
        &black(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    encode_svg(&s)
}

#[test]
fn a_condensed_face_is_named_condensed() {
    let svg = styled(TextStyle::new(12.0).width(0.75));
    // Without this the viewer resolves the normal-width face and
    // `textLength` squeezes it into the condensed measurement — a
    // mechanical scale of the wrong face.
    assert!(svg.contains("font-stretch=\"condensed\""), "{svg}");
    assert_well_formed(&svg);
}

#[test]
fn a_width_between_the_keywords_is_a_percentage() {
    let svg = styled(TextStyle::new(12.0).width(0.8));
    assert!(svg.contains("font-stretch=\"80%\""), "{svg}");
    assert_well_formed(&svg);
}

#[test]
fn a_normal_width_says_nothing() {
    let svg = styled(TextStyle::new(12.0));
    assert!(!svg.contains("font-stretch"), "{svg}");
}

#[test]
fn opentype_features_reach_the_element() {
    let svg = styled(TextStyle::new(12.0).features([
        FontFeatureSetting {
            tag: *b"tnum",
            value: 1,
        },
        FontFeatureSetting {
            tag: *b"liga",
            value: 0,
        },
    ]));
    // Tabular figures are the case that matters: without them the
    // viewer shapes proportional figures and `textLength` squeezes
    // them to the tabular measurement.
    assert!(
        svg.contains("font-feature-settings:'tnum' 1,'liga' 0"),
        "{svg}"
    );
    assert_well_formed(&svg);
}

#[test]
fn variable_font_axes_reach_the_element() {
    let svg = styled(TextStyle::new(12.0).variations([FontVariationSetting {
        tag: *b"wght",
        value: 550.0,
    }]));
    assert!(svg.contains("font-variation-settings:'wght' 550"), "{svg}");
    assert_well_formed(&svg);
}

#[test]
fn features_and_axes_share_one_style_attribute() {
    let svg = styled(
        TextStyle::new(12.0)
            .features([FontFeatureSetting {
                tag: *b"tnum",
                value: 1,
            }])
            .variations([FontVariationSetting {
                tag: *b"wdth",
                value: 75.0,
            }]),
    );
    let text = &svg[svg.find("<text").expect("a text element")..];
    let el = &text[..text.find('>').expect("a closed tag")];
    assert_eq!(
        el.matches("style=\"").count(),
        1,
        "two style attributes would mean one silently losing: {el}"
    );
    assert!(el.contains("font-feature-settings:'tnum' 1;"), "{el}");
    assert!(el.contains("font-variation-settings:'wdth' 75"), "{el}");
    assert_well_formed(&svg);
}

#[test]
fn a_tag_that_would_break_the_css_string_is_dropped() {
    let svg = styled(TextStyle::new(12.0).features([FontFeatureSetting {
        tag: *b"tn'm",
        value: 1,
    }]));
    assert!(!svg.contains("font-feature-settings"), "{svg}");
    assert_well_formed(&svg);
}
