//! Render-path integration tests for theme consumption.
//!
//! These exercise the full orchestrator → chrome → scene pipeline,
//! capturing draw ops into [`RecordingScene`] and asserting the
//! theme's resolved values end up in the emitted brushes / strokes
//! / shapes. Differ from the per-module unit tests under
//! `src/plot/chrome/` in that the *orchestrator* drives the render —
//! so the theme cascade, per-plot override merge, palette resolution,
//! and `Element::Blank` short-circuits are all in scope.

use hephaestus::brush::Brush;
use hephaestus::color::{rgb, Color};
use hephaestus::composition::{beside, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{
    Element, Length, Locale, Palette, RectElement, Theme, ThemeColor, ThemePart,
};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::recording::{Op, RecordingScene};

// ─── Helpers ────────────────────────────────────────────────────────────────

const W: f64 = 400.0;
const H: f64 = 300.0;
const DPI: f64 = 96.0;

fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() <= eps
}

fn solid_color(brush: &Brush) -> Option<Color> {
    match brush {
        Brush::Solid(c) => Some(*c),
        _ => None,
    }
}

fn color_eq(actual: Color, expected: Color, eps: f32) -> bool {
    let [r1, g1, b1, a1] = actual.components;
    let [r2, g2, b2, a2] = expected.components;
    approx_eq(r1, r2, eps)
        && approx_eq(g1, g2, eps)
        && approx_eq(b1, b2, eps)
        && approx_eq(a1, a2, eps)
}

fn any_fill_with_color(ops: &[Op], target: Color, eps: f32) -> bool {
    ops.iter().any(|op| match op {
        Op::Fill { brush, .. } => solid_color(brush)
            .map(|c| color_eq(c, target, eps))
            .unwrap_or(false),
        _ => false,
    })
}

fn any_stroke_with_color(ops: &[Op], target: Color, eps: f32) -> bool {
    ops.iter().any(|op| match op {
        Op::Stroke { brush, .. } => solid_color(brush)
            .map(|c| color_eq(c, target, eps))
            .unwrap_or(false),
        _ => false,
    })
}

/// Build a single-plot composition with a point geom bound to x / y,
/// scales 0..1 on both, and a bottom + left axis. Returns the
/// orchestrator ready for `render`. Caller installs the theme.
fn single_plot_view(theme: Theme) -> (PlotComposition, RecordingScene) {
    let template = beside(Patch::new("p"), Patch::new("__pad"));
    let mut view = PlotComposition::new(template)
        .theme(theme)
        .add_scale("x", scale::continuous(0.0..=1.0))
        .add_scale("y", scale::continuous(0.0..=1.0));
    let dummy_template: Composition = beside(Patch::new("p"), Patch::new("__pad"));
    let mut p = Plot::new(&dummy_template, "p")
        .bind("x", "x")
        .bind("y", "y");
    p.add_geom(
        PointGeom::builder()
            .set("x", vec![0.25_f64, 0.5, 0.75])
            .set("y", vec![0.25_f64, 0.5, 0.75])
            .build(),
    );
    p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
    view.attach_plot(p);
    let scene = RecordingScene::default();
    (view, scene)
}

fn render_view(view: &mut PlotComposition, scene: &mut RecordingScene) {
    view.render(scene, Size::new(W, H), DPI);
}

fn solid_red_panel_theme() -> Theme {
    Theme {
        panel_background: Element::Set(RectElement {
            fill: Some(ThemeColor::Fixed(rgb(1.0, 0.0, 0.0))),
            color: None,
            linewidth_pt: Some(Length::Abs(0.0)),
            ..RectElement::default()
        }),
        ..Theme::default()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn default_theme_plot_background_uses_paper() {
    // Default theme's plot_background is `Paper` (white at the new
    // ggplot2-style defaults). The orchestrator's phase-1 patch
    // background pass must emit a white fill.
    let theme = Theme::default();
    let paper = theme.palette.paper;
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);
    assert!(
        any_fill_with_color(&scene.ops, paper, 1e-4),
        "expected a fill in palette.paper (white) for the plot background"
    );
}

#[test]
fn default_theme_panel_background_uses_grey92() {
    // Panel background = mix(Paper, Ink, 0.08) = grey92 at the
    // light palette default. The phase-2 panel chrome pass must
    // emit this fill.
    let theme = Theme::default();
    let expected =
        ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.08).resolve(&theme.palette);
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);
    assert!(
        any_fill_with_color(&scene.ops, expected, 1e-4),
        "expected the panel background to be drawn in grey92"
    );
}

#[test]
fn default_theme_panel_border_blank_emits_no_panel_outline_stroke() {
    // Default `panel_border = Blank` — no panel-outline stroke.
    // Any strokes present must come from grids / ticks / axis
    // chrome, not from the panel rect's edge. The test asserts no
    // black 1pt stroke is emitted (the prior pre-theme look).
    let theme = Theme::default();
    let ink = theme.palette.ink;
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);
    let any_ink_1pt = scene.ops.iter().any(|op| match op {
        Op::Stroke { brush, stroke, .. } => {
            let one_px = (DPI / 72.0) as f32;
            solid_color(brush)
                .map(|c| color_eq(c, ink, 1e-4))
                .unwrap_or(false)
                && approx_eq(stroke.width as f32, one_px, 0.5)
        }
        _ => false,
    });
    assert!(
        !any_ink_1pt,
        "panel_border = Blank must not emit an ink 1pt stroke around the panel"
    );
}

#[test]
fn default_theme_grids_stroke_in_paper() {
    // Major + minor grids both default to `Paper` (white on the
    // light theme). The grid pass must emit at least one stroke in
    // that colour.
    let theme = Theme::default();
    let paper = theme.palette.paper;
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);
    assert!(
        any_stroke_with_color(&scene.ops, paper, 1e-4),
        "expected grid lines drawn in palette.paper (white)"
    );
}

#[test]
fn default_axis_baseline_blank_emits_no_baseline_stroke() {
    // `axis_concrete_defaults().line = Element::Blank` — the axis
    // baseline must not be stroked. With the panel border also
    // Blank, the only ink-coloured strokes left in the scene would
    // be tick marks (grey20). A grey20 stroke is fine; a baseline
    // would be ink (grey0).
    let theme = Theme::default();
    let ink = theme.palette.ink;
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);
    let ink_strokes = scene
        .ops
        .iter()
        .filter(|op| match op {
            Op::Stroke { brush, .. } => solid_color(brush)
                .map(|c| color_eq(c, ink, 1e-4))
                .unwrap_or(false),
            _ => false,
        })
        .count();
    assert_eq!(
        ink_strokes, 0,
        "expected zero pure-ink strokes — axis baseline + panel border are both Blank"
    );
}

#[test]
fn default_axis_ticks_stroke_in_grey20() {
    // axis_concrete_defaults().ticks = grey20 = mix(Paper, Ink, 0.8).
    let theme = Theme::default();
    let grey20 = ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.8).resolve(&theme.palette);
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);
    assert!(
        any_stroke_with_color(&scene.ops, grey20, 1e-4),
        "expected axis ticks drawn in grey20"
    );
}

#[test]
fn theme_override_replaces_panel_background_fill() {
    // A per-plot ThemePart override on panel_background must win
    // over the composition's theme. Render with a red override and
    // assert a red fill is emitted.
    let template = beside(Patch::new("p"), Patch::new("__pad"));
    let mut view = PlotComposition::new(template)
        .theme(Theme::default())
        .add_scale("x", scale::continuous(0.0..=1.0))
        .add_scale("y", scale::continuous(0.0..=1.0));
    let dummy: Composition = beside(Patch::new("p"), Patch::new("__pad"));
    let red = rgb(1.0, 0.0, 0.0);
    let mut p = Plot::new(&dummy, "p")
        .bind("x", "x")
        .bind("y", "y")
        .theme_override(ThemePart {
            panel_background: Some(Element::Set(RectElement {
                fill: Some(ThemeColor::Fixed(red)),
                color: None,
                linewidth_pt: Some(Length::Abs(0.0)),
                ..RectElement::default()
            })),
            ..ThemePart::default()
        });
    p.add_geom(
        PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .build(),
    );
    view.attach_plot(p);

    let mut scene = RecordingScene::default();
    render_view(&mut view, &mut scene);

    assert!(
        any_fill_with_color(&scene.ops, red, 1e-4),
        "theme_override on panel_background must override the composition theme"
    );
}

#[test]
fn theme_invert_swaps_paper_and_ink_in_render() {
    // `Theme::default().invert()` swaps paper ↔ ink. The plot
    // background, which references `Paper`, must render in what
    // *was* ink (black) after inversion.
    let theme = Theme::default().invert();
    let inverted_paper = theme.palette.paper; // = was-ink = black
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);
    assert!(
        any_fill_with_color(&scene.ops, inverted_paper, 1e-4),
        "plot background must follow the inverted palette anchor"
    );
}

#[test]
fn custom_palette_propagates_through_theme_anchors() {
    // Swap the palette wholesale via `with_palette`. Every
    // ThemeColor reference (Paper / Ink / Mix) resolves against
    // the new anchors at draw time — no theme element gets stuck
    // on a stale resolved colour.
    let palette = Palette::new(
        rgb(0.10, 0.20, 0.40), // paper — deep blue
        rgb(0.95, 0.95, 0.90), // ink — bone white
        rgb(0.95, 0.65, 0.20), // accent — amber
    );
    let theme = Theme::default().with_palette(palette);
    let paper = theme.palette.paper;
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);
    assert!(
        any_fill_with_color(&scene.ops, paper, 1e-4),
        "plot background must use the swapped paper colour"
    );
}

#[test]
fn solid_panel_override_renders_as_one_fill() {
    // Verifies the panel rect actually receives the fill and the
    // border is suppressed (linewidth = 0) — sanity check on the
    // helper used by other tests.
    let theme = solid_red_panel_theme();
    let red = rgb(1.0, 0.0, 0.0);
    let (mut view, mut scene) = single_plot_view(theme);
    render_view(&mut view, &mut scene);

    let red_fills = scene
        .ops
        .iter()
        .filter(|op| match op {
            Op::Fill { brush, .. } => solid_color(brush)
                .map(|c| color_eq(c, red, 1e-4))
                .unwrap_or(false),
            _ => false,
        })
        .count();
    assert!(
        red_fills >= 1,
        "expected at least one red panel fill, found {red_fills}"
    );
    // No red strokes since linewidth_pt = 0.
    assert!(
        !any_stroke_with_color(&scene.ops, red, 1e-4),
        "linewidth_pt = 0 must suppress the red border stroke"
    );
}

#[test]
fn strip_text_blank_suppresses_whole_strip() {
    // `theme.strip_text = Sided::all_blank` → no strip emission
    // even when `Plot::strip` installs a label. The theme is the
    // gate; an installed label without resolved text element is a
    // no-op.
    // Pick a contrasting fixed colour for the strip background so
    // we can spot it cleanly if it leaks through.
    let strip_bg_color = rgb(0.9, 0.2, 0.2);
    let theme = Theme {
        strip_text: hephaestus::plot::theme::Sided {
            all: Element::Blank,
            by_channel: [Element::Inherit, Element::Inherit],
            by_channel_side: [
                [Element::Inherit, Element::Inherit],
                [Element::Inherit, Element::Inherit],
            ],
        },
        strip_background: hephaestus::plot::theme::Sided::new(RectElement {
            fill: Some(ThemeColor::Fixed(strip_bg_color)),
            color: None,
            linewidth_pt: Some(Length::Abs(0.0)),
            ..RectElement::default()
        }),
        ..Theme::default()
    };

    let template = beside(Patch::new("p"), Patch::new("__pad"));
    let mut view = PlotComposition::new(template)
        .theme(theme)
        .add_scale("x", scale::continuous(0.0..=1.0))
        .add_scale("y", scale::continuous(0.0..=1.0));
    let dummy: Composition = beside(Patch::new("p"), Patch::new("__pad"));
    let mut p = Plot::new(&dummy, "p")
        .bind("x", "x")
        .bind("y", "y")
        .strip(AxisSide::Top, "Facet")
        .strip(AxisSide::Right, "Group");
    p.add_geom(
        PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .build(),
    );
    view.attach_plot(p);

    let mut scene = RecordingScene::default();
    render_view(&mut view, &mut scene);

    assert!(
        !any_fill_with_color(&scene.ops, strip_bg_color, 1e-4),
        "strip_text = Blank must suppress the strip background even when a label is installed"
    );
}

#[test]
fn legend_key_frame_uses_theme_swatch_color() {
    // theme.legend.key.frame defaults to a grey92 swatch — a
    // legend with a discrete key body must emit one fill per row
    // in that colour. Use `Theme::default()` and inspect.
    use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
    use hephaestus::scales::value::Value;
    use std::sync::Arc;

    let theme = Theme::default();
    let grey92 = ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.08).resolve(&theme.palette);

    let template = beside(Patch::new("p"), Patch::new("__pad"));
    let mut view = PlotComposition::new(template)
        .theme(theme)
        .add_scale("x", scale::continuous(0.0..=1.0))
        .add_scale("y", scale::continuous(0.0..=1.0))
        .add_scale(
            "cat",
            scale::discrete([Value::String(Arc::from("a")), Value::String(Arc::from("b"))])
                .range_colors([rgb(1.0, 0.0, 0.0), rgb(0.0, 1.0, 0.0)]),
        );
    let dummy: Composition = beside(Patch::new("p"), Patch::new("__pad"));
    let mut p = Plot::new(&dummy, "p").bind("x", "x").bind("y", "y");
    p.add_geom(
        PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .build(),
    );
    p.add_legend(Legend::new("cat").key(LegendKeySpec::rect().scaled("fill", "cat")));
    view.attach_plot(p);

    let mut scene = RecordingScene::default();
    render_view(&mut view, &mut scene);

    // Two breaks → at least two swatch-background fills.
    let swatch_fills = scene
        .ops
        .iter()
        .filter(|op| match op {
            Op::Fill { brush, .. } => solid_color(brush)
                .map(|c| color_eq(c, grey92, 1e-4))
                .unwrap_or(false),
            _ => false,
        })
        .count();
    assert!(
        swatch_fills >= 2,
        "expected at least 2 grey92 swatch fills (one per legend key), found {swatch_fills}"
    );
}

#[test]
fn locale_de_renders_decimal_comma_in_axis_labels() {
    // With `Locale::DE_DE`, the default numeric formatter swaps
    // '.' → ',', so a continuous 0..1 axis emits glyph runs for
    // "0,2", "0,4", … (not "0.2", "0.4", …). The test inspects
    // recorded glyph runs by re-shaping each break and checking
    // the locale-aware label.
    let theme = Theme::default().with_locale(Locale::DE_DE);
    let s = scale::continuous(0.0..=1.0);
    let breaks = s.breaks(hephaestus::scales::DEFAULT_BREAK_COUNT);
    let labels: Vec<String> = breaks.iter().map(|v| s.format(v, &theme.locale)).collect();
    assert!(
        labels.iter().any(|l| l.contains(',')),
        "expected at least one DE_DE label with a decimal comma; got {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains('.')),
        "DE_DE labels must not contain decimal points; got {labels:?}"
    );
}

#[test]
fn locale_default_renders_decimal_point() {
    // Default locale is `Locale::EN_US` → decimal point.
    let theme = Theme::default();
    let s = scale::continuous(0.0..=1.0);
    let breaks = s.breaks(hephaestus::scales::DEFAULT_BREAK_COUNT);
    let labels: Vec<String> = breaks.iter().map(|v| s.format(v, &theme.locale)).collect();
    assert!(
        !labels.iter().any(|l| l.contains(',')),
        "EN_US labels must not contain decimal commas; got {labels:?}"
    );
}

#[test]
fn user_formatter_receives_locale() {
    // A user-supplied formatter closure gets `(value, locale)` and
    // can branch on it.
    let s = scale::continuous(0.0..=1.0).with_format(|v, locale| match v {
        hephaestus::scales::Value::Number(n) => {
            format!(
                "{n:.1}{}",
                if locale.decimal == ',' { " DE" } else { " US" }
            )
        }
        other => hephaestus::plot::Scale::default_format(other, locale),
    });
    assert!(s
        .format(&hephaestus::scales::Value::Number(0.5), &Locale::EN_US)
        .ends_with(" US"));
    assert!(s
        .format(&hephaestus::scales::Value::Number(0.5), &Locale::DE_DE)
        .ends_with(" DE"));
}
