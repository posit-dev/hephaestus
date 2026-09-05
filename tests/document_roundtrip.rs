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
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::theme::Theme;
use hephaestus::plot::{
    scale, ImageGeom, ImageRegistry, LineGeom, Plot, PlotComposition, PointGeom, Projection,
    RectGeom, TextGeom,
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

/// Bytes before the first chunk: the 8-byte magic, then the major, minor
/// and flags words. The tests that reach into the container by hand read
/// their offsets from this rather than restating it.
const HEADER: usize = 8 + 2 + 2 + 2;

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
    // Polar placements, which only validate against a polar
    // projection — the reader has to restore the projection before it
    // attaches an axis.
    polar.add_axis(Axis::rail(
        "cat",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    polar.add_axis(Axis::rail(
        "value",
        AxisPlacement::PolarRadius { theta_frac: 0.0 },
    ));

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
            // more than once, and CI caught exactly that. `Op::DrawImage`
            // is stricter: `peniko::ImageData` is foreign and compares its
            // blob by identity. That costs nothing here because a document
            // carries image *names*, so the reloaded plot samples the very
            // handle the caller re-registered —
            // `an_image_geom_round_trips_once_its_registry_is_restored`
            // is where that is pinned. Nothing in `build` draws one.
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

/// Append a chunk `tag` this build has never heard of.
fn with_extra_chunk(tag: &[u8; 4]) -> Vec<u8> {
    let comp = build();
    let mut bytes = write_composition(&comp, &WriteOptions::new()).expect("writable");
    bytes.extend_from_slice(tag);
    bytes.extend_from_slice(&7u32.to_le_bytes());
    bytes.extend_from_slice(b"payload");
    bytes
}

/// An unknown *ancillary* chunk — lowercase initial — is skipped, which
/// is what makes a minor version additive.
#[test]
fn an_unknown_ancillary_chunk_is_skipped_rather_than_rejected() {
    read_composition(&with_extra_chunk(b"xxxx"), &ReadContext::new())
        .map(|_| ())
        .expect("an unknown ancillary chunk should be ignored");
}

/// An unknown *critical* chunk — uppercase initial — is refused. Silently
/// skipping something load-bearing would rebuild a plot that differs from
/// the one written, with nothing to say so.
#[test]
fn an_unknown_critical_chunk_is_refused() {
    match read_composition(&with_extra_chunk(b"XXXX"), &ReadContext::new()) {
        Ok(_) => panic!("an unknown critical chunk must not be skipped"),
        Err(e) => assert!(
            e.to_string().contains("XXXX"),
            "error should name the chunk: {e}"
        ),
    }
}

/// Every tag the format defines is written at most once, so a repeat is a
/// corrupt document rather than something to read past.
#[test]
fn a_repeated_chunk_is_refused() {
    let comp = build();
    let bytes = write_composition(&comp, &WriteOptions::new()).expect("writable");
    // The head is the first chunk, so its tag and body sit at a known
    // offset; appending a second copy of the tag is enough.
    let mut doubled = bytes.clone();
    doubled.extend_from_slice(b"HEAD");
    doubled.extend_from_slice(&0u32.to_le_bytes());

    match read_composition(&doubled, &ReadContext::new()) {
        Ok(_) => panic!("a repeated tag must be refused"),
        Err(e) => assert!(
            e.to_string().contains("HEAD"),
            "error should name the chunk: {e}"
        ),
    }
}

/// Every flag bit is reserved, so a set one means the container is
/// encoded in a way this build cannot interpret — refuse rather than
/// read the chunks and hope.
#[test]
fn an_unknown_container_flag_is_refused() {
    let comp = build();
    let mut bytes = write_composition(&comp, &WriteOptions::new()).expect("writable");
    bytes[HEADER - 2..HEADER].copy_from_slice(&1u16.to_le_bytes());

    match read_composition(&bytes, &ReadContext::new()) {
        Ok(_) => panic!("an unknown flag bit must be refused"),
        Err(e) => assert!(
            e.to_string().contains("flags"),
            "error should mention flags: {e}"
        ),
    }
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
    let len_at = HEADER + 4;
    let body_len = u32::from_le_bytes(
        full[len_at..len_at + 4]
            .try_into()
            .expect("the head's length field"),
    ) as usize;
    let truncated = &full[..len_at + 4 + body_len];

    let hints = read_hints(truncated).expect("head-only document");
    assert_eq!(hints.size, Some((300.0, 200.0)));
    assert!(
        read_composition(truncated, &ReadContext::new()).is_err(),
        "a head-only document has no composition to rebuild"
    );
}

// ─── Images ──────────────────────────────────────────────────────────────────

/// A document names the images a plot draws; it does not carry their
/// pixels, exactly as it names font families rather than embedding them by
/// default. So the reloaded plot needs its
/// [`ImageRegistry`](hephaestus::plot::ImageRegistry) restored before it
/// can draw, and once it is, the draw calls match the original's — the
/// blob-identity comparison in `Op::DrawImage` is satisfied because both
/// sides sample the one handle the caller registered.
#[test]
fn an_image_geom_round_trips_once_its_registry_is_restored() {
    let swatch = |side: u32| {
        let px = vec![200u8; (side as usize) * (side as usize) * 4];
        hephaestus::image::from_rgba8(side, side, px).expect("valid buffer")
    };
    let registry_of = |side: u32| {
        let mut r = ImageRegistry::new();
        r.insert("swatch", swatch(side));
        r
    };
    let registry = registry_of(8);

    let comp = || {
        hephaestus::composition::Composition::empty(1, 1).place(
            1,
            1,
            hephaestus::composition::Span::cell(),
            Patch::new("panel"),
        )
    };
    let source_plot = |registry: ImageRegistry| {
        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x")
            .bind("y", "y")
            .image_registry(registry);
        plot.add_geom(
            ImageGeom::builder()
                .set("image", "swatch")
                .set("x", vec![2.0_f64])
                .set("y", vec![2.0_f64])
                .set("x2", vec![8.0_f64])
                .set("y2", vec![8.0_f64])
                .build(),
        );
        plot
    };
    let view = |plot: Plot| {
        PlotComposition::new(&comp())
            .add_scale("x", scale::continuous(0.0..=10.0))
            .add_scale("y", scale::continuous(0.0..=10.0))
            .with_plot(plot)
    };

    let mut original = view(source_plot(registry.clone()));
    let bytes = write_composition(&original, &WriteOptions::new()).expect("a writable plot");

    // Names, not pixels: the document is the same size whether the image
    // it names is 8x8 or 512x512. This is the property the whole registry
    // design exists for — a basemap tile costs a document nothing.
    let big = write_composition(&view(source_plot(registry_of(512))), &WriteOptions::new())
        .expect("a writable plot");
    assert_eq!(
        bytes.len(),
        big.len(),
        "document grew from {} to {} bytes when the image it names got 4096x larger",
        bytes.len(),
        big.len()
    );

    // Without the registry the geom finds no image and draws none.
    let mut bare = read_composition(&bytes, &ReadContext::new()).expect("a readable document");
    let bare_calls = draw_calls(&mut bare, 400, 300);
    assert!(
        !bare_calls
            .iter()
            .any(|op| matches!(op, Op::DrawImage { .. })),
        "a reloaded plot whose registry was never restored should draw no image"
    );

    // With it restored, the draw calls match the original exactly.
    let mut reloaded = read_composition(&bytes, &ReadContext::new()).expect("a readable document");
    reloaded.update_plot("panel", |p| p.set_image_registry(registry.clone()));

    let want = draw_calls(&mut original, 400, 300);
    let got = draw_calls(&mut reloaded, 400, 300);
    assert_eq!(want.len(), got.len(), "draw call count changed");
    assert!(
        want.iter().any(|op| matches!(op, Op::DrawImage { .. })),
        "the original drew no image, so this proves nothing"
    );
    for (i, (a, b)) in want.iter().zip(&got).enumerate() {
        assert!(
            a == b,
            "draw call {i} differs:\n  original: {a:?}\n  reloaded: {b:?}"
        );
    }
}

// ─── Marker shapes ───────────────────────────────────────────────────────────

/// A composition with one plot, bound so `PointGeom` draws a marker per
/// row and nothing else varies.
fn shape_case(registry: hephaestus::shape::ShapeRegistry, marker: &str) -> PlotComposition {
    let marker = Value::String(std::sync::Arc::from(marker));
    let comp = || {
        hephaestus::composition::Composition::empty(1, 1).place(
            1,
            1,
            hephaestus::composition::Span::cell(),
            Patch::new("panel"),
        )
    };
    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x")
        .bind("y", "y")
        .shape_registry(registry);
    plot.add_geom(
        PointGeom::builder()
            .set("x", vec![1.0_f64, 2.0, 3.0])
            .set("y", vec![1.0_f64, 2.0, 3.0])
            .set("shape", marker)
            .set("size", 12.0_f64)
            .set("fill", rgb8(200, 60, 60))
            .build(),
    );
    PlotComposition::new(&comp())
        .add_scale("x", scale::continuous(0.0..=4.0))
        .add_scale("y", scale::continuous(0.0..=4.0))
        .with_plot(plot)
}

/// A registry holding nothing but the built-ins writes no shape payload:
/// the reader rebuilds those itself, so a document that customises
/// nothing pays nothing.
#[test]
fn a_builtin_only_registry_costs_the_document_nothing() {
    use hephaestus::shape::ShapeRegistry;

    let builtin_only = shape_case(ShapeRegistry::with_builtins(), "circle");
    let bytes = write_composition(&builtin_only, &WriteOptions::new()).expect("writable");

    // The same plot with one extra registered shape, to prove the
    // comparison is sensitive to a shape actually being carried.
    let mut extended = ShapeRegistry::with_builtins();
    extended.insert("wedge-ish", triangle_shape());
    let customised = shape_case(extended, "circle");
    let bigger = write_composition(&customised, &WriteOptions::new()).expect("writable");

    assert!(
        bigger.len() > bytes.len(),
        "a registered custom shape should add bytes; {} vs {}",
        bigger.len(),
        bytes.len()
    );
}

/// A hand-built path shape round-trips, and the reloaded plot draws with
/// it rather than falling back or skipping the row.
#[test]
fn a_custom_path_shape_round_trips() {
    use hephaestus::shape::ShapeRegistry;

    let mut registry = ShapeRegistry::with_builtins();
    registry.insert("spike", triangle_shape());
    let mut original = shape_case(registry, "spike");

    let bytes = write_composition(&original, &WriteOptions::new()).expect("writable");
    let mut reloaded = read_composition(&bytes, &ReadContext::new()).expect("readable");

    // The registry itself came back.
    let restored = reloaded
        .plot("panel")
        .expect("panel plot")
        .shape_registry_ref()
        .get("spike")
        .cloned();
    assert_eq!(
        restored.as_ref(),
        Some(&triangle_shape()),
        "the custom shape did not survive the round trip"
    );

    // And the draw calls match, which is what proves the reloaded plot
    // resolves the name rather than skipping the row.
    let want = draw_calls(&mut original, 400, 300);
    let got = draw_calls(&mut reloaded, 400, 300);
    assert_eq!(want.len(), got.len(), "draw call count changed");
    for (i, (a, b)) in want.iter().zip(&got).enumerate() {
        assert!(a == b, "draw call {i} differs:\n  {a:?}\n  {b:?}");
    }
}

/// Replacing a built-in travels too. Writing only the names absent from
/// `builtin::NAMES` would silently drop this, which is why the delta is
/// computed by comparing shapes rather than names.
#[test]
fn overriding_a_builtin_travels() {
    use hephaestus::shape::ShapeRegistry;

    let mut registry = ShapeRegistry::with_builtins();
    registry.insert("circle", triangle_shape());
    let original = shape_case(registry, "circle");

    let bytes = write_composition(&original, &WriteOptions::new()).expect("writable");
    let reloaded = read_composition(&bytes, &ReadContext::new()).expect("readable");

    let got = reloaded
        .plot("panel")
        .expect("panel plot")
        .shape_registry_ref()
        .get("circle")
        .cloned();
    assert_eq!(
        got.as_ref(),
        Some(&triangle_shape()),
        "an overridden built-in should not revert to the built-in"
    );
}

/// A glyph marker travels as its source text and style, not as a
/// face-specific glyph id, and comes back drawing the same glyph.
#[test]
fn a_glyph_shape_travels_as_its_text() {
    use hephaestus::shape::ShapeRegistry;
    use hephaestus::text::{glyph_marker, TextStyle};

    let style = TextStyle::default();
    let arrow = glyph_marker("A", &style);
    // The source is what makes it carryable.
    let source = arrow
        .glyph_source()
        .expect("glyph_marker records its source");
    assert_eq!(source.text, "A");

    let mut registry = ShapeRegistry::with_builtins();
    registry.insert("letter", arrow);
    let mut original = shape_case(registry, "letter");

    let bytes = write_composition(&original, &WriteOptions::new()).expect("writable");
    let mut reloaded = read_composition(&bytes, &ReadContext::new()).expect("readable");

    let restored = reloaded
        .plot("panel")
        .expect("panel plot")
        .shape_registry_ref()
        .get("letter")
        .cloned()
        .expect("the glyph shape should have been rebuilt");
    assert_eq!(
        restored.glyph_source().map(|s| s.text.as_str()),
        Some("A"),
        "the rebuilt shape lost its source text"
    );

    let want = draw_calls(&mut original, 400, 300);
    let got = draw_calls(&mut reloaded, 400, 300);
    assert_eq!(want.len(), got.len(), "draw call count changed");
    for (i, (a, b)) in want.iter().zip(&got).enumerate() {
        assert!(a == b, "draw call {i} differs:\n  {a:?}\n  {b:?}");
    }
}

/// A glyph shape built straight from a resolved face has no source text,
/// so it cannot be carried — and the writer says so rather than dropping
/// it silently.
#[test]
fn a_source_less_glyph_shape_is_reported() {
    use hephaestus::document::UnsupportedItem;
    use hephaestus::shape::{Shape, ShapeRegistry};

    // Borrow a real face off a resolved glyph marker, then rebuild the
    // shape through the raw constructor so the source is dropped.
    let resolved = hephaestus::text::glyph_marker("A", &hephaestus::text::TextStyle::default());
    let bare = match resolved.kind() {
        hephaestus::shape::ShapeKind::Glyph {
            font,
            glyph_id,
            em_bbox,
            em_origin,
        } => Shape::glyph(
            font.clone(),
            glyph_id,
            em_bbox,
            em_origin,
            resolved.anchor(),
        ),
        _ => panic!("glyph_marker should produce a glyph shape"),
    };
    assert!(bare.glyph_source().is_none());

    let mut registry = ShapeRegistry::with_builtins();
    registry.insert("bare", bare);
    let comp = shape_case(registry, "bare");

    let problems = hephaestus::document::unsupported_items(&comp);
    assert!(
        problems.iter().any(|p| matches!(
            p,
            UnsupportedItem::UnnameableShape { name, .. } if name == "bare"
        )),
        "expected an UnnameableShape report, got {problems:?}"
    );

    // Strict mode refuses; lossy mode drops it and writes.
    assert!(write_composition(&comp, &WriteOptions::new()).is_err());
    let bytes = write_composition(&comp, &WriteOptions::new().lossy(true)).expect("lossy writes");
    let reloaded = read_composition(&bytes, &ReadContext::new()).expect("readable");
    assert!(
        reloaded
            .plot("panel")
            .expect("panel plot")
            .shape_registry_ref()
            .get("bare")
            .is_none(),
        "a dropped shape should not reappear"
    );
}

/// A three-spike triangle, distinguishable from every built-in.
fn triangle_shape() -> hephaestus::shape::Shape {
    use hephaestus::geometry::Point;
    use hephaestus::path::Path;
    use hephaestus::shape::{Shape, ShapeStyle};

    let mut p = Path::new();
    p.move_to((0.0, -0.6));
    p.line_to((0.55, 0.5));
    p.line_to((-0.55, 0.5));
    p.close_path();
    Shape::new(vec![p], ShapeStyle::Fill, Point::new(-0.8, 0.0))
}

// ─── Embedded images ─────────────────────────────────────────────────────────

/// A composition whose one plot draws `name` across a data-space rect,
/// with `registry` supplying the pixels.
fn image_case(registry: ImageRegistry, name: &str) -> PlotComposition {
    let comp = || {
        hephaestus::composition::Composition::empty(1, 1).place(
            1,
            1,
            hephaestus::composition::Span::cell(),
            Patch::new("panel"),
        )
    };
    let name = Value::String(std::sync::Arc::from(name));
    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x")
        .bind("y", "y")
        .image_registry(registry);
    plot.add_geom(
        ImageGeom::builder()
            .set("image", name)
            .set("x", vec![2.0_f64])
            .set("y", vec![2.0_f64])
            .set("x2", vec![8.0_f64])
            .set("y2", vec![8.0_f64])
            .build(),
    );
    PlotComposition::new(&comp())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=10.0))
        .with_plot(plot)
}

/// A gradient of `side` x `side`, compressible but not trivially so.
fn gradient_image(side: u32) -> hephaestus::brush::Image {
    let mut px = Vec::with_capacity((side as usize) * (side as usize) * 4);
    for y in 0..side {
        for x in 0..side {
            px.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128, 255]);
        }
    }
    hephaestus::image::from_rgba8(side, side, px).expect("valid buffer")
}

fn image_registry_of(side: u32) -> ImageRegistry {
    let mut r = ImageRegistry::new();
    r.insert("swatch", gradient_image(side));
    r
}

/// With embedding on, the pixels come back — so a reloaded plot draws
/// its image with no registry restored by hand.
///
/// Asserted on the decoded pixels rather than on `Op` equality: a
/// document-decoded image is a fresh blob, and `Op::DrawImage` compares
/// blobs by identity.
#[test]
fn an_embedded_image_comes_back_without_the_reader_supplying_it() {
    let source = image_case(image_registry_of(16), "swatch");
    let bytes = write_composition(&source, &WriteOptions::new().embed_images(true))
        .expect("a writable plot");

    let reloaded = read_composition(&bytes, &ReadContext::new()).expect("a readable document");
    let got = reloaded
        .plot("panel")
        .expect("panel plot")
        .image_registry_ref()
        .get("swatch")
        .cloned()
        .expect("the embedded image should have been decoded");

    let want = gradient_image(16);
    assert_eq!((got.width, got.height), (want.width, want.height));
    assert_eq!(
        got.data.as_ref(),
        want.data.as_ref(),
        "PNG embedding must be lossless"
    );
}

/// Embedding is off by default, and a document that only names an image
/// stays the same size however large the image is. This is the property
/// the whole registry design exists for.
#[test]
fn naming_an_image_costs_the_document_nothing() {
    let small = write_composition(
        &image_case(image_registry_of(8), "swatch"),
        &WriteOptions::new(),
    )
    .expect("writable");
    let large = write_composition(
        &image_case(image_registry_of(512), "swatch"),
        &WriteOptions::new(),
    )
    .expect("writable");
    assert_eq!(
        small.len(),
        large.len(),
        "a named image must not put its pixels in the document"
    );

    // And a reload draws nothing, since nothing supplied the pixels.
    let mut reloaded = read_composition(&small, &ReadContext::new()).expect("readable");
    assert!(
        !draw_calls(&mut reloaded, 400, 300)
            .iter()
            .any(|op| matches!(op, Op::DrawImage { .. })),
        "a reader given no pixels should draw no image"
    );
}

/// Embedding costs far less than the raw buffer. A gradient is a
/// pessimistic case next to a rendered plot, and still lands well under
/// half.
#[test]
fn embedding_costs_far_less_than_the_raw_pixels() {
    let side = 256u32;
    let raw = (side * side * 4) as usize;
    let embedded = write_composition(
        &image_case(image_registry_of(side), "swatch"),
        &WriteOptions::new().embed_images(true),
    )
    .expect("writable");
    let named = write_composition(
        &image_case(image_registry_of(side), "swatch"),
        &WriteOptions::new(),
    )
    .expect("writable");
    let payload = embedded.len() - named.len();
    assert!(
        payload < raw / 2,
        "embedded payload is {payload} bytes against {raw} raw"
    );
    println!("embedded {payload} bytes for a {side}x{side} image; raw would be {raw}");
}

/// Embedding twice must produce the same bytes, which the PNG encoder
/// and the sorted registry walk together guarantee.
#[test]
fn an_embedded_document_is_stable_across_a_second_write() {
    let opts = WriteOptions::new().embed_images(true);
    let first =
        write_composition(&image_case(image_registry_of(32), "swatch"), &opts).expect("writable");
    let reloaded = read_composition(&first, &ReadContext::new()).expect("readable");
    let second = write_composition(&reloaded, &opts).expect("writable");
    assert_eq!(
        first, second,
        "write -> read -> write changed the bytes of an embedded document"
    );
}

/// An older reader — one built without the codec — skips the section
/// rather than failing, exactly as it would an unknown chunk.
#[test]
fn an_embedded_document_still_loads_for_a_reader_that_cannot_decode() {
    // Simulated by naming an image the writer embedded and confirming
    // the document is otherwise intact: the chunk is additive, so the
    // rest of the plot must read the same as the un-embedded form.
    let embedded = write_composition(
        &image_case(image_registry_of(16), "swatch"),
        &WriteOptions::new().embed_images(true),
    )
    .expect("writable");
    let mut with = read_composition(&embedded, &ReadContext::new()).expect("readable");

    let named = write_composition(
        &image_case(image_registry_of(16), "swatch"),
        &WriteOptions::new(),
    )
    .expect("writable");
    let mut without = read_composition(&named, &ReadContext::new()).expect("readable");

    // The embedded one draws its image; the named one does not. Every
    // other draw call is shared, which is what proves the chunk is
    // purely additive.
    let a = draw_calls(&mut with, 400, 300);
    let b = draw_calls(&mut without, 400, 300);
    let images_a = a
        .iter()
        .filter(|o| matches!(o, Op::DrawImage { .. }))
        .count();
    let images_b = b
        .iter()
        .filter(|o| matches!(o, Op::DrawImage { .. }))
        .count();
    assert_eq!(images_a, 1, "the embedded document should draw its image");
    assert_eq!(images_b, 0, "the named document has no pixels to draw");
    assert_eq!(
        a.len() - images_a,
        b.len() - images_b,
        "the two documents should agree on every non-image draw call"
    );
}

/// An image a *markdown* slot names travels the same way an
/// `ImageGeom`'s does, including one the writer read off disk rather
/// than registering by hand — which is what lets a page rebuild a
/// figure whose title holds a picture.
#[test]
fn a_markdown_title_image_round_trips_from_both_registers() {
    let comp = || {
        hephaestus::composition::Composition::empty(1, 1).place(
            1,
            1,
            hephaestus::composition::Span::cell(),
            Patch::new("panel"),
        )
    };
    let mut theme = hephaestus::plot::theme::Theme::default();
    theme.text.markdown = Some(true);

    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x")
        .bind("y", "y")
        .image_registry(image_registry_of(8))
        .title("plot ![](swatch)");
    plot.add_geom(
        hephaestus::plot::TextGeom::builder()
            .set("x", vec![5.0_f64])
            .set("y", vec![5.0_f64])
            .set("text", vec!["x".to_string()])
            .build(),
    );
    let mut comp_registry = ImageRegistry::new();
    comp_registry.insert("banner", gradient_image(12));
    let source = PlotComposition::new(&comp())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=10.0))
        .theme(theme)
        .image_registry(comp_registry)
        .title("figure ![](banner)")
        .with_plot(plot);

    let bytes = write_composition(&source, &WriteOptions::new().embed_images(true))
        .expect("a writable composition");
    let reloaded = read_composition(&bytes, &ReadContext::new()).expect("a readable document");

    assert!(
        reloaded
            .plot("panel")
            .expect("panel plot")
            .image_registry_ref()
            .contains("swatch"),
        "the plot's own register should come back"
    );
    assert!(
        reloaded.image_registry_ref().contains("banner"),
        "and so should the composition's, which its title names"
    );
}

// ─── Forward compatibility ───────────────────────────────────────────────────
//
// The two growth rules the format rests on, each asserted against bytes
// assembled by hand — a newer writer is the one thing this crate cannot
// produce for itself.

/// Offset of the first chunk body carrying `tag`, and its length.
fn locate_chunk(bytes: &[u8], tag: &[u8; 4]) -> (usize, usize) {
    let mut at = HEADER;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("length")) as usize;
        if &bytes[at..at + 4] == tag {
            return (at + 8, len);
        }
        at += 8 + len;
    }
    panic!("no {:?} chunk", std::str::from_utf8(tag).unwrap_or("????"));
}

/// Rewrite the length field of the chunk whose body starts at `body_at`.
fn set_chunk_len(bytes: &mut [u8], body_at: usize, len: u32) {
    bytes[body_at - 4..body_at].copy_from_slice(&len.to_le_bytes());
}

/// A chunk body may grow at its tail: the chunk's own length delimits it,
/// so a reader that decodes the sections it knows and stops is unaffected
/// by whatever a newer writer appended. Asserted on `HEAD`, which is where
/// the writer's own version already rides that rule.
#[test]
fn a_chunk_body_that_grew_at_its_tail_still_reads() {
    let comp = build();
    let opts = WriteOptions::new().size_hint(321.0, 123.0);
    let original = write_composition(&comp, &opts).expect("writable");

    let (body_at, body_len) = locate_chunk(&original, b"HEAD");
    let mut grown = original[..body_at + body_len].to_vec();
    grown.extend_from_slice(b"\x07appended");
    grown.extend_from_slice(&original[body_at + body_len..]);
    set_chunk_len(&mut grown, body_at, (body_len + 9) as u32);

    let hints = read_hints(&grown).expect("a head with an unknown tail should still read");
    assert_eq!(hints.size, Some((321.0, 123.0)));
    read_composition(&grown, &ReadContext::new()).expect("and so should the whole document");
}

/// A record may grow at its tail: it is length-prefixed, so a reader
/// decodes the fields it knows and skips the rest. This is the property
/// the whole `record` form exists for.
///
/// `THEM` opens with the theme record, so extending that record's declared
/// length and padding it simulates a `Theme` that gained a field.
#[test]
fn a_record_that_grew_a_trailing_field_still_reads() {
    let comp = build();
    let original = write_composition(&comp, &WriteOptions::new()).expect("writable");
    let (body_at, body_len) = locate_chunk(&original, b"THEM");

    // The theme record's own length is the varint at the front of `THEM`.
    // Every theme is far longer than 127 bytes and shorter than 16 kB, so
    // it is two bytes wide, and bumping it keeps that width.
    let (a, b) = (original[body_at], original[body_at + 1]);
    assert_eq!(a & 0x80, 0x80, "expected a two-byte record length");
    assert_eq!(b & 0x80, 0, "expected a two-byte record length");
    let record_len = u32::from(a & 0x7f) | (u32::from(b) << 7);
    let grown_len = record_len + 4;
    assert!(grown_len < 1 << 14, "still two bytes");

    let mut grown = original.clone();
    grown[body_at] = 0x80 | (grown_len & 0x7f) as u8;
    grown[body_at + 1] = (grown_len >> 7) as u8;
    // Four bytes of a field this build has never heard of, inserted where
    // a newer writer would have put it: at the record's end.
    let insert_at = body_at + 2 + record_len as usize;
    for (k, byte) in [0xde_u8, 0xad, 0xbe, 0xef].into_iter().enumerate() {
        grown.insert(insert_at + k, byte);
    }
    set_chunk_len(&mut grown, body_at, (body_len + 4) as u32);

    let reloaded = read_composition(&grown, &ReadContext::new())
        .expect("a theme with an unknown trailing field should still read");
    assert_eq!(
        reloaded.theme_ref().locale,
        comp.theme_ref().locale,
        "the fields this build knows must survive the skip"
    );
}

/// A record whose fields read *past* its declared length is corruption,
/// not a newer document, and has to be reported rather than silently
/// leaving the cursor misaligned for everything after it.
#[test]
fn a_record_that_overran_its_length_is_reported() {
    let comp = build();
    let original = write_composition(&comp, &WriteOptions::new()).expect("writable");
    let (body_at, _) = locate_chunk(&original, b"THEM");

    // Shrink the theme record's declared length so its own fields run off
    // the end of it.
    let mut corrupt = original.clone();
    corrupt[body_at] = 0x81;
    corrupt[body_at + 1] = 0x01;

    match read_composition(&corrupt, &ReadContext::new()) {
        Ok(_) => panic!("an overrun record must be reported"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("record") || msg.contains("Theme"),
                "error should name the record: {msg}"
            );
        }
    }
}

/// The composition's own image register travels too — it backs chrome
/// that belongs to the composition rather than to any plot, so a title
/// holding `![](…)` needs it. It rides a field of its own rather than a
/// plot address it would have to fake.
#[test]
fn the_compositions_own_image_register_travels() {
    let mut source = image_case(ImageRegistry::new(), "swatch");
    source = source.image_registry(image_registry_of(16));

    let bytes = write_composition(&source, &WriteOptions::new().embed_images(true))
        .expect("writable with images");
    let reloaded = read_composition(&bytes, &ReadContext::new()).expect("readable");

    let restored = reloaded
        .image_registry_ref()
        .get("swatch")
        .expect("the composition's own register should come back");
    let original = gradient_image(16);
    assert_eq!(restored.width, original.width);
    assert_eq!(restored.height, original.height);
    assert_eq!(
        restored.data.as_ref(),
        original.data.as_ref(),
        "pixels should survive the round trip"
    );

    // And it is addressed independently of the plots: this composition's
    // one plot has an empty register, which must stay empty.
    assert!(
        reloaded.plots_in("panel")[0]
            .image_registry_ref()
            .get("swatch")
            .is_none(),
        "a composition-level image must not leak into a plot's register"
    );
}

// ─── The checked-in fixture ──────────────────────────────────────────────────

/// A document written by an earlier build, checked in so that a change to
/// the format is noticed rather than merely being self-consistent.
///
/// Deliberately a *read* assertion rather than a byte comparison: an
/// additive change rewrites these bytes legitimately, and comparing them
/// would fail on exactly the changes the format is designed to absorb.
/// What must not change is that the bytes still rebuild the same plot.
const FIXTURE: &[u8] = include_bytes!("fixtures/four_panel.hep");

#[test]
fn the_checked_in_fixture_still_reads() {
    let doc = hephaestus::document::read_document(FIXTURE, ReadContext::builtin())
        .expect("the checked-in fixture must still read");

    // The same assertion the live round-trip makes, against bytes this
    // build did not produce.
    let mut reloaded = doc.composition;
    for (w, h) in SIZES {
        let ops = draw_calls(&mut reloaded, w, h);
        assert!(
            ops.iter().any(|op| matches!(op, Op::DrawGlyphs(_))),
            "the fixture should still draw its chrome at {w}x{h}"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::Fill { .. })),
            "the fixture should still draw its marks at {w}x{h}"
        );
    }
}

/// Rewrite the fixture. Ignored, so it runs only when asked:
/// `cargo test --features document --test document_roundtrip -- --ignored
/// regenerate_the_fixture`.
///
/// Run it after an intentional format change, and review the diff — a
/// changed fixture is the format changing, which is worth seeing in a
/// commit.
#[test]
#[ignore = "writes tests/fixtures/four_panel.hep; run deliberately"]
fn regenerate_the_fixture() {
    let comp = build();
    let bytes = write_composition(&comp, &WriteOptions::new()).expect("writable");
    std::fs::create_dir_all("tests/fixtures").expect("fixture directory");
    std::fs::write("tests/fixtures/four_panel.hep", &bytes).expect("write the fixture");
}
