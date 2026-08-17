//! Measure/draw parity and theme-field consumption for chrome text.
//!
//! Two classes of bug live here, both invisible to a test that only
//! renders the default theme:
//!
//! - A chrome slot that **measures** text with one style and **draws**
//!   it with another reserves the wrong amount of space, so labels
//!   clip or float. Every assertion below compares a themed render
//!   against the default one, since a slot that ignores the theme
//!   produces byte-identical geometry.
//! - A themed text field that no renderer reads is a dead field: the
//!   theme accepts it and nothing changes.

use hephaestus::composition::{beside, Composition, Patch};
use hephaestus::geometry::{Rect, Size};
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{Length, TextElement, Theme};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::recording::{Op, RecordingScene};

const W: f64 = 400.0;
const H: f64 = 300.0;
const DPI: f64 = 96.0;

/// Render a single plot carrying bottom + left axes under `theme`.
fn render(theme: Theme) -> Vec<Op> {
    let template: Composition = beside(Patch::new("p"), Patch::new("__pad"));
    let mut view = PlotComposition::new(&template)
        .theme(theme)
        .add_scale("x", scale::continuous(0.0..=1.0))
        .add_scale("y", scale::continuous(0.0..=1.0));
    let dummy: Composition = beside(Patch::new("p"), Patch::new("__pad"));
    let mut p = Plot::new(&dummy, "p").bind("x", "x").bind("y", "y");
    p.add_geom(
        PointGeom::builder()
            .set("x", vec![0.25_f64, 0.5, 0.75])
            .set("y", vec![0.25_f64, 0.5, 0.75])
            .build(),
    );
    p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
    view.attach_plot(p);
    let mut scene = RecordingScene::default();
    view.render(&mut scene, Size::new(W, H), DPI);
    scene.ops
}

/// Font sizes of every recorded glyph run, largest first. Tick labels
/// are the only text in these renders, so this is their style.
fn glyph_font_sizes(ops: &[Op]) -> Vec<f32> {
    let mut sizes: Vec<f32> = ops
        .iter()
        .filter_map(|op| match op {
            Op::DrawGlyphs(run) => Some(run.font_size),
            _ => None,
        })
        .collect();
    sizes.sort_by(|a, b| b.partial_cmp(a).unwrap());
    sizes
}

/// Bounding box of every drawn glyph origin — a stand-in for "where
/// the tick labels ended up", which is what a mis-measured slot moves.
fn glyph_extent(ops: &[Op]) -> Option<Rect> {
    let mut acc: Option<Rect> = None;
    for op in ops {
        let Op::DrawGlyphs(run) = op else { continue };
        for g in &run.glyphs {
            let p = run.transform * hephaestus::geometry::Point::new(g.x as f64, g.y as f64);
            let r = Rect::new(p.x, p.y, p.x, p.y);
            acc = Some(match acc {
                Some(a) => a.union(r),
                None => r,
            });
        }
    }
    acc
}

/// Bounding box of a recorded path, walked element-wise so the test
/// needs nothing beyond the crate's own re-exports.
fn path_bbox(path: &hephaestus::path::Path) -> Option<Rect> {
    use hephaestus::path::PathEl;
    let mut acc: Option<Rect> = None;
    let mut add = |p: hephaestus::geometry::Point| {
        let r = Rect::new(p.x, p.y, p.x, p.y);
        acc = Some(match acc {
            Some(a) => a.union(r),
            None => r,
        });
    };
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => add(p),
            PathEl::QuadTo(a, b) => {
                add(a);
                add(b)
            }
            PathEl::CurveTo(a, b, c) => {
                add(a);
                add(b);
                add(c)
            }
            PathEl::ClosePath => {}
        }
    }
    acc
}

/// The panel rect, recovered from the panel-background fill — the
/// largest fill that isn't the full canvas.
fn panel_rect(ops: &[Op]) -> Option<Rect> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Fill { path, .. } => path_bbox(path),
            _ => None,
        })
        .filter(|r| r.width() < W - 1.0 && r.height() < H - 1.0)
        .max_by(|a, b| {
            (a.width() * a.height())
                .partial_cmp(&(b.width() * b.height()))
                .unwrap()
        })
}

fn axis_text_theme(edit: impl FnOnce(&mut TextElement)) -> Theme {
    let mut theme = Theme::default();
    let mut el = TextElement::default();
    edit(&mut el);
    theme.axis.all.text = hephaestus::plot::theme::Element::Set(el);
    theme
}

#[test]
fn root_text_size_scales_the_axis_tick_labels() {
    // Axis tick labels default to `Rel(0.8)`, so they resolve against
    // `theme.text.size_pt`. A slot that resolved against the bare
    // crate constant instead would pin them at 8.8pt no matter what
    // the theme says.
    let base = glyph_font_sizes(&render(Theme::default()));
    let mut big = Theme::default();
    big.text.size_pt = Some(Length::Abs(22.0));
    let scaled = glyph_font_sizes(&render(big));

    assert!(
        !base.is_empty(),
        "expected tick labels in the default render"
    );
    assert_eq!(base.len(), scaled.len(), "same labels, different size");
    assert!(
        scaled[0] > base[0] * 1.5,
        "doubling theme.text.size_pt should scale tick labels: {:?} vs {:?}",
        base[0],
        scaled[0]
    );
}

#[test]
fn tick_labels_stay_outside_the_panel_they_were_measured_against() {
    // The parity property, stated directly: whatever style the axis
    // measures its labels in has to be the style it draws them in,
    // otherwise the reserved band is the wrong size and the labels
    // land on top of the panel. Checked under themes that move the
    // label size in both directions, since the default theme passes
    // even when measure ignores the theme entirely.
    for size in [6.0_f64, 11.0, 22.0] {
        let mut theme = Theme::default();
        theme.text.size_pt = Some(Length::Abs(size));
        let ops = render(theme);
        let panel = panel_rect(&ops).expect("panel background fill");

        for op in &ops {
            let Op::DrawGlyphs(run) = op else { continue };
            for g in &run.glyphs {
                let p = run.transform * hephaestus::geometry::Point::new(g.x as f64, g.y as f64);
                // A glyph origin inside the panel means the axis band
                // reserved less than it drew. 1px of slack absorbs
                // baseline rounding at the shared edge.
                let inside = p.x > panel.x0 + 1.0
                    && p.x < panel.x1 - 1.0
                    && p.y > panel.y0 + 1.0
                    && p.y < panel.y1 - 1.0;
                assert!(
                    !inside,
                    "at {size}pt a tick label glyph landed inside the panel: \
                     glyph {p:?} in panel {panel:?}"
                );
            }
        }
    }
}

#[test]
fn root_text_size_rebudgets_the_axis_bands() {
    // The panel is what is left after the axis bands take their share,
    // so a larger tick-label size has to shrink it. A draw-only fix
    // would grow the glyphs and leave the panel untouched.
    let small = {
        let mut t = Theme::default();
        t.text.size_pt = Some(Length::Abs(6.0));
        panel_rect(&render(t)).expect("panel at 6pt")
    };
    let large = {
        let mut t = Theme::default();
        t.text.size_pt = Some(Length::Abs(22.0));
        panel_rect(&render(t)).expect("panel at 22pt")
    };
    assert!(
        large.width() < small.width() - 1.0,
        "larger tick labels should take width from the panel: {small:?} vs {large:?}"
    );
    assert!(
        large.height() < small.height() - 1.0,
        "larger tick labels should take height from the panel: {small:?} vs {large:?}"
    );
}

#[test]
fn axis_text_line_height_reaches_the_tick_labels() {
    // Line height is one of the fields a bare `TextStyle::new(size)`
    // drops. It changes a single line's reported height, which the
    // bottom-axis band reserves, so the panel above it moves.
    let tight = glyph_extent(&render(axis_text_theme(|el| {
        el.size_pt = Some(Length::Abs(18.0));
        el.lineheight = Some(Length::Rel(0.8));
    })))
    .expect("labels in tight render");
    let loose = glyph_extent(&render(axis_text_theme(|el| {
        el.size_pt = Some(Length::Abs(18.0));
        el.lineheight = Some(Length::Rel(2.5));
    })))
    .expect("labels in loose render");

    assert!(
        (loose.y1 - tight.y1).abs() > 0.5,
        "tick-label line height should change the bottom band: {tight:?} vs {loose:?}"
    );
}
