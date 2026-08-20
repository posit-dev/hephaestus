//! The load-bearing test for plot documents.
//!
//! A document doesn't carry pixels or shaped text — it carries a plot's
//! configuration, and the reader re-solves the layout and re-shapes the
//! text for whatever size it's rendering at. So the test renders the
//! original composition and the reloaded one **at sizes the writer never
//! saw** and requires them to agree.
//!
//! Agreeing at a novel size is what separates this from replaying a baked
//! op stream: a stream could only be scaled, and scaling changes chrome
//! padding, stroke widths and text metrics. A field that failed to
//! round-trip makes the two diverge at some size.
//!
//! **Agreement is asserted on draw calls, not pixels.** Rasterising one
//! unchanged scene is currently nondeterministic — see
//! `examples/aa_nondeterminism.rs` — and the magnitude depends on the
//! backend: one pixel via Metal, fifteen subpixels via Mesa's software
//! rasteriser, always by one unit. A pixel tolerance would therefore be
//! tuned to whichever machine set it, and would leave a hole a real
//! regression could hide in. Draw calls are deterministic, and they are
//! the whole of what a document is responsible for. A separate test
//! checks that the reloaded composition still rasterises, asserting only
//! that no channel moves by more than one — the signature of a coverage
//! flip, and backend-independent.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, stack, Patch};
use hephaestus::document::{
    read_composition, read_hints, write_composition, ReadContext, WriteOptions,
};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::Theme;
use hephaestus::plot::{
    scale, LineGeom, Plot, PlotComposition, PointGeom, Projection, RectGeom, TextGeom,
};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::value::Value;
use hephaestus::scene::recording::{Op, RecordingScene};
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

/// Record the draw calls `comp` emits at `(w, h)`.
///
/// No GPU involved: `RecordingScene` is a `SceneBuilder` that keeps the
/// calls instead of rasterising them.
fn draw_calls(comp: &mut PlotComposition, w: u32, h: u32) -> Vec<Op> {
    let mut scene = RecordingScene::new();
    comp.render(&mut scene, Size::new(f64::from(w), f64::from(h)), 96.0);
    scene.ops
}

/// The load-bearing test: a reloaded composition must emit **exactly**
/// the same draw calls, at sizes the writer never saw.
///
/// Asserted on draw calls rather than pixels, and exactly rather than
/// within a tolerance. Rasterising one unchanged scene is currently
/// nondeterministic — `examples/aa_nondeterminism.rs` gets two different
/// images from one composition — and the magnitude is
/// backend-dependent: a single pixel on Metal, fifteen subpixels on
/// Mesa's software rasteriser. Any pixel tolerance would therefore be
/// tuned to whichever machine set it, and would be a hole a real
/// regression could hide in.
///
/// Draw calls are deterministic, and they are also the whole of what a
/// document is responsible for: everything downstream of them is the
/// backend's business. So this is both the stricter assertion and the
/// more honest one.
#[test]
fn a_reloaded_composition_emits_the_same_draw_calls_at_sizes_the_writer_never_saw() {
    let source = build();
    let bytes = write_composition(&source, &WriteOptions::new()).expect("a writable plot");

    for (w, h) in SIZES {
        // A fresh pair per size: rendering mutates a composition's own
        // caches, so reusing one across sizes would compare a warm side
        // against a cold one.
        let mut original = build();
        let mut reloaded =
            read_composition(&bytes, &ReadContext::new()).expect("a readable document");

        let want = draw_calls(&mut original, w, h);
        let got = draw_calls(&mut reloaded, w, h);

        assert_eq!(
            want.len(),
            got.len(),
            "at {w}x{h} the reloaded composition emitted {} draw calls, not {}",
            got.len(),
            want.len()
        );
        for (i, (a, b)) in want.iter().zip(&got).enumerate() {
            // `Op: PartialEq` compares fonts by the face they name, not by
            // which blob handed it over — font resolution loads one file
            // more than once, and CI caught exactly that. The same is not
            // true of `Op::DrawImage`: `peniko::ImageData` is foreign and
            // compares its blob by identity, so a plot that draws images
            // would need that handled before this comparison means
            // anything. Nothing in `build` draws one.
            assert!(
                a == b,
                "at {w}x{h} draw call {i} differs:\n  original: {a:?}\n  reloaded: {b:?}"
            );
        }
    }
}

/// The reloaded composition also rasterises, and to the same image up to
/// the backend's own nondeterminism.
///
/// The invariant is **worst delta ≤ 1**, with no bound on how many
/// subpixels are affected. That is the signature of an antialiasing
/// coverage flip, and it holds whatever the backend; a real round-trip
/// fault moves a mark, a colour or a coordinate, which changes bytes by
/// far more than one. Counting affected bytes instead would only measure
/// which rasteriser is running.
#[test]
fn a_reloaded_composition_rasterises_to_the_same_image() {
    let source = build();
    let bytes = write_composition(&source, &WriteOptions::new()).expect("a writable plot");
    let (w, h) = (800u32, 600u32);

    let mut original = build();
    let mut reloaded = read_composition(&bytes, &ReadContext::new()).expect("a readable document");
    let want = render(&mut original, w, h);
    let got = render(&mut reloaded, w, h);

    let mut worst = 0u8;
    let mut at = None;
    for (i, (a, b)) in want.iter().zip(&got).enumerate() {
        let d = a.abs_diff(*b);
        if d > worst {
            worst = d;
            at = Some((i / 4, i % 4, *a, *b));
        }
    }

    assert!(
        worst <= 1,
        "reloaded composition rasterises differently at {w}x{h}: worst delta {worst} \
         at pixel ({}, {}) channel {} ({} vs {})",
        at.map_or(0, |(p, _, _, _)| p % w as usize),
        at.map_or(0, |(p, _, _, _)| p / w as usize),
        at.map_or(0, |(_, c, _, _)| c),
        at.map_or(0, |(_, _, a, _)| a),
        at.map_or(0, |(_, _, _, b)| b),
    );
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

/// With fonts embedded, the document still reads back to the same draw
/// calls. Registration is process-global, so this mainly pins that the
/// extra chunk parses and that reinstating the generic mapping doesn't
/// change which faces this machine resolves.
#[test]
fn a_document_with_embedded_fonts_still_round_trips() {
    let source = build();
    let bytes =
        write_composition(&source, &WriteOptions::new().embed_fonts(true)).expect("writable");

    let (w, h) = (800u32, 600u32);
    let mut original = build();
    let mut reloaded = read_composition(&bytes, &ReadContext::new()).expect("readable");
    assert_eq!(
        draw_calls(&mut original, w, h),
        draw_calls(&mut reloaded, w, h),
        "embedding fonts changed the draw calls"
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

/// The head's hints are advisory, but a consumer that has to pick a size
/// before it lays anything out needs them, so they have to survive the
/// trip rather than being written and dropped.
#[test]
fn the_render_hints_a_writer_records_come_back() {
    let comp = build();
    let opts = WriteOptions::new()
        .background(rgb8(12, 34, 56))
        .size_hint(640.0, 480.0)
        .dpi_hint(144.0);
    let bytes = write_composition(&comp, &opts).expect("writable");

    let hints = read_hints(&bytes).expect("readable head");
    assert_eq!(hints.background, Some(rgb8(12, 34, 56)));
    assert_eq!(hints.size, Some((640.0, 480.0)));
    assert_eq!(hints.dpi, Some(144.0));
}

/// Hints are optional, and a writer that sets none is the common case —
/// `WriteOptions::new()` records nothing.
#[test]
fn a_document_written_without_hints_reports_none_of_them() {
    let comp = build();
    let bytes = write_composition(&comp, &WriteOptions::new()).expect("writable");

    let hints = read_hints(&bytes).expect("readable head");
    assert_eq!(hints.background, None);
    assert_eq!(hints.size, None);
    assert_eq!(hints.dpi, None);
}

/// Reading the hints must not depend on anything after the head, so that
/// it stays cheap enough to call before deciding on a size.
#[test]
fn hints_read_from_the_head_alone_without_the_chunks_behind_it() {
    let comp = build();
    let opts = WriteOptions::new().size_hint(300.0, 200.0);
    let full = write_composition(&comp, &opts).expect("writable");

    // Truncating to the head plus its own body leaves a document that
    // `read_composition` must reject and `read_hints` must still answer.
    let head_end =
        12 + 4 + 4 + u32::from_le_bytes(full[16..20].try_into().expect("length field")) as usize;
    let truncated = &full[..head_end];

    let hints = read_hints(truncated).expect("head-only document");
    assert_eq!(hints.size, Some((300.0, 200.0)));
    assert!(
        read_composition(truncated, &ReadContext::new()).is_err(),
        "a head-only document has no composition to rebuild"
    );
}
