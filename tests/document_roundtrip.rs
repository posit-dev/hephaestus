//! The load-bearing test for plot documents.
//!
//! A document doesn't carry pixels or shaped text — it carries a plot's
//! configuration, and the reader re-solves the layout and re-shapes the
//! text for whatever size it's rendering at. The way to test that is to
//! render the original composition and the reloaded one **at sizes the
//! writer never saw** and require the buffers to match byte for byte.
//!
//! Equal output at a novel size is the property that separates this from
//! replaying a baked op stream: a stream could only be scaled, and
//! scaling changes chrome padding, stroke widths and text metrics. If a
//! field failed to round-trip, the two renders diverge at some size.
//!
//! Each size compares a freshly built composition against a freshly
//! loaded one, and the comparison allows a tiny tolerance. Rasterising
//! one unchanged scene is currently nondeterministic — see
//! `examples/aa_nondeterminism.rs`, which reproduces two different images
//! from the same composition with no document involved — so requiring
//! bit-exact equality here would fail on a coin flip. The tolerance is
//! kept far below anything a real round-trip fault could produce.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, stack, Patch};
use hephaestus::document::{read_composition, write_composition, ReadContext, WriteOptions};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::Theme;
use hephaestus::plot::{
    scale, LineGeom, Plot, PlotComposition, PointGeom, Projection, RectGeom, TextGeom,
};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::value::Value;
use hephaestus::Renderer;

/// One unmistakable colour per panel, so
/// `every_panel_of_the_test_composition_actually_draws_something` can
/// look for the marks themselves rather than guessing from colour
/// variety — antialiased text alone would pass a variety check.
const MARK_COLORS: [hephaestus::color::Color; 4] = [
    hephaestus::color::Color::from_rgb8(200, 30, 30),
    hephaestus::color::Color::from_rgb8(30, 140, 60),
    hephaestus::color::Color::from_rgb8(40, 60, 200),
    hephaestus::color::Color::from_rgb8(190, 40, 170),
];

/// Sizes to compare at. None of them is used while writing, and their
/// aspect ratios differ widely, so a layout that only happens to agree
/// at one shape is caught.
const SIZES: [(u32, u32); 4] = [(400, 300), (800, 600), (1200, 500), (500, 900)];

/// Build a composition exercising a wide slice of the plot surface:
/// nested compositions, four geom kinds, continuous / discrete / log
/// scales, a polar projection, a non-default theme, axes, a legend,
/// titles and facet strips.
fn build() -> PlotComposition {
    let comp = || {
        stack(
            beside(Patch::new("scatter"), Patch::new("lines")),
            beside(Patch::new("bars"), Patch::new("polar")),
        )
    };

    // Three contiguous runs of 20, not interleaved: `LineGeom` groups
    // *consecutive* rows sharing a key into one mark, so interleaved keys
    // would give every mark a single point and draw nothing at all.
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let mut groups: Vec<&str> = Vec::new();
    for (k, g) in ["alpha", "beta", "gamma"].iter().enumerate() {
        for i in 0..20 {
            let x = f64::from(i) * 1.5;
            xs.push(x);
            ys.push(10.0 + 8.0 * (x * 0.2 + k as f64).sin());
            groups.push(g);
        }
    }

    let mut scatter = Plot::new(&comp(), "scatter")
        .bind("x", "t")
        .bind("y", "value")
        .title("Scatter")
        .subtitle("with a subtitle")
        .caption("and a caption")
        .strip(AxisSide::Top, "top strip");
    scatter.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", MARK_COLORS[0])
            .set("size", 4.0_f64)
            .build(),
    );
    scatter.add_axis(Axis::rail("t", AxisPlacement::Cartesian(AxisSide::Bottom)).title("t"));
    scatter.add_axis(Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)).title("value"));

    let mut lines = Plot::new(&comp(), "lines")
        .bind("x", "t")
        .bind("y", "logged")
        .title("Lines");
    lines.add_geom(
        LineGeom::builder()
            .keys(groups.clone())
            .set("x", xs.clone())
            .set("y", ys.iter().map(|y| y + 1.0).collect::<Vec<f64>>())
            // An explicit stroke: `LineDefaults::stroke` is `None` by
            // design, so a line with no stroke channel draws nothing.
            .set("stroke", MARK_COLORS[1])
            .set("linewidth", 1.5_f64)
            .set("linetype", Value::Linetype(hephaestus::linetype::dashed()))
            .build(),
    );
    lines.add_axis(Axis::rail(
        "logged",
        AxisPlacement::Cartesian(AxisSide::Left),
    ));

    let mut bars = Plot::new(&comp(), "bars")
        // A *positional* discrete scale. Binding a position channel to
        // `"group"` would hand `resolve_position` a colour and NaN out
        // every row, since that scale's output range is colours.
        .bind("x", "cat")
        .bind("y", "value")
        .title("Bars");
    bars.add_geom(
        RectGeom::builder()
            .set("x", vec!["alpha", "beta", "gamma"])
            .set("y", vec![0.0, 0.0, 0.0])
            .set("x2", vec!["alpha", "beta", "gamma"])
            .set("y2", vec![6.0, 12.0, 9.0])
            .set("fill", MARK_COLORS[2])
            .build(),
    );
    bars.add_geom(
        TextGeom::builder()
            .set("x", vec!["alpha", "beta", "gamma"])
            .set("y", vec![6.0, 12.0, 9.0])
            .set("text", vec!["6", "12", "9"])
            .set("size", 9.0_f64)
            .build(),
    );
    bars.add_axis(Axis::rail(
        "cat",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));

    let mut polar = Plot::new(&comp(), "polar")
        .bind("x", "cat")
        .bind("y", "value")
        .projection(Projection::polar())
        .title("Polar");
    polar.add_geom(
        RectGeom::builder()
            .set("x", vec!["alpha", "beta", "gamma"])
            .set("y", vec![0.0, 0.0, 0.0])
            .set("x2", vec!["alpha", "beta", "gamma"])
            .set("y2", vec![6.0, 12.0, 9.0])
            .set("fill", MARK_COLORS[3])
            .build(),
    );

    PlotComposition::new(&comp())
        .theme(Theme::minimal())
        .title("Document round trip")
        .caption("rendered from a rebuilt composition")
        .with_plot(scatter)
        .with_plot(lines)
        .with_plot(bars)
        .with_plot(polar)
        .add_scale("t", scale::continuous(0.0..=30.0))
        .add_scale("value", scale::continuous(0.0..=20.0))
        .add_scale(
            "logged",
            scale::continuous(1.0..=20.0).with_transform(hephaestus::plot::TransformKind::Log10),
        )
        .add_scale(
            "cat",
            scale::discrete(["alpha", "beta", "gamma"].map(Value::from)),
        )
        .add_scale(
            "group",
            scale::discrete(["alpha", "beta", "gamma"].map(Value::from)).range_colors(vec![
                rgb8(200, 60, 60),
                rgb8(60, 160, 90),
                rgb8(70, 90, 200),
            ]),
        )
}

/// Render `comp` at `(w, h)` into an RGBA8 buffer.
fn render(comp: &mut PlotComposition, w: u32, h: u32) -> Vec<u8> {
    let mut renderer = VelloRenderer::new().expect("a working wgpu adapter");
    comp.render(
        renderer.scene(),
        Size::new(f64::from(w), f64::from(h)),
        96.0,
    );
    let mut buf = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, Color::WHITE, &mut buf)
        .expect("render to buffer");
    buf
}

/// Subpixel bytes allowed to differ, and by how much.
///
/// Zero would be right if rasterising a scene were deterministic. It
/// isn't: the same composition rendered twice, unchanged, with a fresh
/// renderer each time, returns one of two images differing by one unit
/// in a single antialiased pixel — roughly an even split, in debug and
/// release alike. `examples/aa_nondeterminism.rs` reproduces it with no
/// document involved, and the recorded op stream is byte-identical
/// across runs, so the draw calls are deterministic and only the
/// rasterisation is not.
///
/// The bound stays far tighter than any real round-trip fault: a dropped
/// field moves geometry, a colour, or a whole mark, changing thousands of
/// bytes or one byte by a lot.
const MAX_DIFFERING_BYTES: usize = 8;
const MAX_BYTE_DELTA: u8 = 1;

#[test]
fn a_reloaded_composition_renders_identically_at_sizes_the_writer_never_saw() {
    let source = build();
    let bytes = write_composition(&source, &WriteOptions::new()).expect("a writable plot");

    for (w, h) in SIZES {
        // A fresh pair per size: rendering mutates a composition's own
        // caches, so reusing one across sizes would compare a warm side
        // against a cold one.
        let mut original = build();
        let mut reloaded =
            read_composition(&bytes, &ReadContext::new()).expect("a readable document");

        let want = render(&mut original, w, h);
        let got = render(&mut reloaded, w, h);

        let mut differing = 0usize;
        let mut worst = 0u8;
        let mut first = None;
        for (i, (a, b)) in want.iter().zip(&got).enumerate() {
            if a != b {
                differing += 1;
                worst = worst.max(a.abs_diff(*b));
                if first.is_none() {
                    first = Some((i / 4, i % 4, *a, *b));
                }
            }
        }

        assert!(
            differing <= MAX_DIFFERING_BYTES && worst <= MAX_BYTE_DELTA,
            "reloaded composition diverges at {w}x{h}: {differing} bytes differ, \
             worst delta {worst}; first at pixel ({}, {}) channel {} ({} vs {})",
            first.map_or(0, |(p, _, _, _)| p % w as usize),
            first.map_or(0, |(p, _, _, _)| p / w as usize),
            first.map_or(0, |(_, c, _, _)| c),
            first.map_or(0, |(_, _, a, _)| a),
            first.map_or(0, |(_, _, _, b)| b),
        );
    }
}

/// Writing what was just read must produce the same bytes. A field that
/// decodes into something subtly different — a default substituted for a
/// missing value, a collection reordered — shows up here even when it
/// happens not to change any pixels.
#[test]
fn a_document_is_stable_across_a_second_write() {
    let original = build();
    let first = write_composition(&original, &WriteOptions::new()).expect("a writable plot");
    let reloaded = read_composition(&first, &ReadContext::new()).expect("a readable document");
    let second = write_composition(&reloaded, &WriteOptions::new()).expect("a writable plot");
    assert_eq!(
        first.len(),
        second.len(),
        "second write produced a different length"
    );
    assert!(first == second, "second write produced different bytes");
}

/// A document should be a small fraction of the raster it replaces, or
/// there's no reason to prefer it.
#[test]
fn a_document_is_smaller_than_the_image_it_replaces() {
    let comp = build();
    let bytes = write_composition(&comp, &WriteOptions::new()).expect("a writable plot");
    let raster = 800 * 600 * 4;
    assert!(
        bytes.len() < raster / 4,
        "document is {} bytes against {raster} for one 800x600 frame",
        bytes.len()
    );
    println!(
        "document: {} bytes; one 800x600 RGBA frame: {raster} bytes",
        bytes.len()
    );
}

/// Guard against the equality test passing on two blank frames.
///
/// `a_reloaded_composition_renders_identically_at_sizes_the_writer_never_saw`
/// only proves the two renders agree — it would be just as happy if both
/// drew nothing, which is exactly what happened while this suite was
/// being written: a `LineGeom` with interleaved keys produced
/// single-point marks and drew nothing, and the equality test passed.
///
/// So this looks for each panel's own mark colour rather than inferring
/// from colour variety, which antialiased text alone would satisfy.
#[test]
fn every_panel_of_the_test_composition_actually_draws_something() {
    let (w, h) = (1200u32, 500u32);
    let mut comp = build();
    let buf = render(&mut comp, w, h);

    // Quadrant of the 2x2 layout -> the colour its geom is drawn in.
    let quadrants = [
        ("scatter", 0..w / 2, 0..h / 2, MARK_COLORS[0]),
        ("lines", w / 2..w, 0..h / 2, MARK_COLORS[1]),
        ("bars", 0..w / 2, h / 2..h, MARK_COLORS[2]),
        ("polar", w / 2..w, h / 2..h, MARK_COLORS[3]),
    ];

    for (name, xs, ys, color) in quadrants {
        let [r, g, b, _] = color.to_rgba8().to_u8_array();
        let mut hits = 0usize;
        for y in ys {
            for x in xs.clone() {
                let i = ((y * w + x) * 4) as usize;
                // Exact match on the fill: antialiased edges blend, but a
                // mark of any size has interior pixels at full coverage.
                if buf[i] == r && buf[i + 1] == g && buf[i + 2] == b {
                    hits += 1;
                }
            }
        }
        assert!(
            hits > 20,
            "the {name} panel has only {hits} pixels of its mark colour \
             ({r}, {g}, {b}), so its geom is drawing (almost) nothing"
        );
    }
}

// ─── Refusals ────────────────────────────────────────────────────────────────

/// An anonymous formatter closure can't be named, so it can't be
/// reproduced. The write says so rather than quietly dropping it.
#[test]
fn an_anonymous_formatter_is_refused_and_names_the_scale() {
    let comp = || Patch::new("p");
    let mut view = PlotComposition::new(&stack(comp(), Patch::new("q")))
        .add_scale("t", scale::continuous(0.0..=1.0));
    view.update_scale("t", |s| s.set_format(|_, _| "custom".to_string()));

    match write_composition(&view, &WriteOptions::new()) {
        Err(e) => {
            let msg = e.to_string();
            assert!(msg.contains("\"t\""), "error should name the scale: {msg}");
            assert!(
                msg.contains("with_named_format"),
                "error should say how to fix it: {msg}"
            );
        }
        Ok(_) => panic!("an anonymous formatter should be refused"),
    }
}

/// The same plot writes once `lossy` is set, and the scale falls back to
/// default labels rather than the closure's output.
#[test]
fn lossy_mode_writes_an_anonymous_formatter_as_default_labels() {
    use hephaestus::plot::FormatSpec;
    use hephaestus::scales::locale::Locale;
    use hephaestus::scales::value::Value;

    let comp = || stack(Patch::new("p"), Patch::new("q"));
    let mut view = PlotComposition::new(&comp()).add_scale("t", scale::continuous(0.0..=100.0));
    view.update_scale("t", |s| s.set_format(|_, _| "custom".to_string()));

    let bytes = write_composition(&view, &WriteOptions::new().lossy(true))
        .expect("lossy mode should write it");
    let reloaded = read_composition(&bytes, &ReadContext::new()).expect("readable");
    let scale = reloaded.scale("t").expect("the scale survives");
    assert_eq!(scale.format_spec(), FormatSpec::Default);
    assert_eq!(scale.format(&Value::Number(50.0), &Locale::EN_US), "50");
}

/// A named formatter round-trips when the reader is told what the name
/// means — the path a host is expected to use.
#[test]
fn a_named_formatter_round_trips_through_the_read_context() {
    use hephaestus::plot::FormatSpec;
    use hephaestus::scales::locale::Locale;
    use hephaestus::scales::value::Value;

    let comp = || stack(Patch::new("p"), Patch::new("q"));
    let mut view = PlotComposition::new(&comp()).add_scale("t", scale::continuous(0.0..=100.0));
    view.update_scale("t", |s| {
        s.set_named_format("pct", |v, _| format!("{}%", v.as_number().unwrap_or(0.0)));
    });

    let bytes = write_composition(&view, &WriteOptions::new()).expect("a named formatter is fine");
    let ctx = ReadContext::new()
        .with_formatter("pct", |v, _| format!("{}%", v.as_number().unwrap_or(0.0)));
    let reloaded = read_composition(&bytes, &ctx).expect("readable");
    let scale = reloaded.scale("t").expect("the scale survives");
    assert_eq!(scale.format_spec(), FormatSpec::Named("pct".into()));
    assert_eq!(scale.format(&Value::Number(50.0), &Locale::EN_US), "50%");
}

/// A truncated document reports where it ran out rather than panicking.
#[test]
fn a_truncated_document_is_reported_not_panicked_on() {
    let comp = build();
    let bytes = write_composition(&comp, &WriteOptions::new()).expect("writable");
    for cut in [4, 12, 40, bytes.len() / 2, bytes.len() - 1] {
        // Any diagnosis of a short read is legitimate; what matters is
        // that it's an error and not a panic.
        match read_composition(&bytes[..cut], &ReadContext::new()) {
            Err(e) => assert!(!e.to_string().is_empty()),
            Ok(_) => panic!("a document truncated at {cut} should not read"),
        }
    }
}

/// Bytes that aren't a plot document at all are rejected on the magic,
/// before anything is interpreted.
#[test]
fn arbitrary_bytes_are_rejected_on_the_magic() {
    match read_composition(b"not a plot document at all", &ReadContext::new()) {
        Err(e) => assert!(e.to_string().contains("magic"), "{e}"),
        Ok(_) => panic!("arbitrary bytes should not read as a document"),
    }
}

// ─── Fonts ───────────────────────────────────────────────────────────────────

/// Fonts are off by default, because a system family dwarfs the plot.
#[test]
fn fonts_are_not_embedded_unless_asked_for() {
    let comp = build();
    let lean = write_composition(&comp, &WriteOptions::new()).expect("writable");
    let fat = write_composition(&comp, &WriteOptions::new().embed_fonts(true)).expect("writable");
    assert!(
        fat.len() > lean.len(),
        "embedding should add the font files: {} vs {}",
        fat.len(),
        lean.len()
    );
    // The default has to stay small enough to be worth preferring to an
    // image; the whole argument for the format is size plus reflow.
    assert!(
        lean.len() < 64 * 1024,
        "a font-free document should be small, got {} bytes",
        lean.len()
    );
}

/// With fonts embedded, the document still reads back and renders the
/// same. Registration is process-global, so this mainly pins that the
/// extra chunk parses and that reinstating the generic mapping doesn't
/// change what this machine already resolves.
#[test]
fn a_document_with_embedded_fonts_still_round_trips() {
    let source = build();
    let bytes =
        write_composition(&source, &WriteOptions::new().embed_fonts(true)).expect("writable");

    let (w, h) = (800u32, 600u32);
    let mut original = build();
    let mut reloaded = read_composition(&bytes, &ReadContext::new()).expect("readable");
    let want = render(&mut original, w, h);
    let got = render(&mut reloaded, w, h);

    let differing = want.iter().zip(&got).filter(|(a, b)| a != b).count();
    assert!(
        differing <= MAX_DIFFERING_BYTES,
        "{differing} bytes differ with fonts embedded"
    );
}

/// An unknown chunk is skipped, which is what makes a minor version
/// additive. Simulated by appending one a reader has never heard of.
#[test]
fn an_unknown_chunk_is_skipped_rather_than_rejected() {
    let comp = build();
    let mut bytes = write_composition(&comp, &WriteOptions::new()).expect("writable");
    bytes.extend_from_slice(b"XXXX");
    bytes.extend_from_slice(&7u32.to_le_bytes());
    bytes.extend_from_slice(b"payload");

    read_composition(&bytes, &ReadContext::new())
        .map(|_| ())
        .expect("an unknown trailing chunk should be ignored");
}
