//! Chord diagram built from `RibbonBSplineGeom` under a polar
//! projection. Each chord is one mark — a four-control-point cubic
//! B-spline ribbon whose endpoints sit on the outer ring (at the
//! source and destination category arcs) and whose two interior
//! control points sit deep inside the disc, pulling the band into a
//! classic dip-toward-the-centre arc.
//!
//! Demonstrates several `RibbonBSplineGeom` features at once:
//!
//! - Polar projection with `"domain"` interpolation: each spline
//!   sample is projected individually, so the chord follows the polar
//!   metric faithfully.
//! - Per-mark control polygons grouped by `keys` (one mark = one
//!   chord; multiple chords share a single geom).
//! - Per-mark `fill` colour from the source category.
//! - Variable chord *width* expressed as the gap between curve A and
//!   curve B at the outer-ring endpoints — the natural way to encode
//!   per-link flow magnitude.
//! - Outer-ring arc tickmarks drawn as a second
//!   `RibbonBSplineGeom` overlay with degree-1 control polygons (the
//!   polyline fallback) so the arc segments sit cleanly on the ring.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::projection::Projection;
use hephaestus::plot::{scale, Plot, PlotComposition, RibbonBSplineGeom};
use hephaestus::Renderer;

fn cell_comp() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"))
}

/// One outgoing flow from a category. The chord lands on `dest` and
/// occupies a `width` slice (in fractions of the full theta range) at
/// each end of the outer ring.
struct Flow {
    dest: &'static str,
    width: f64,
}

fn main() {
    let (w, h) = (900u32, 900u32);
    let dpi = 96.0;
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // Four categories around the ring. Flow widths are in
    // theta-fractions (so 0.04 = 4% of the ring's circumference).
    let categories: &[(&str, Color, &[Flow])] = &[
        (
            "A",
            rgb8(220, 80, 80),
            &[
                Flow {
                    dest: "C",
                    width: 0.08,
                },
                Flow {
                    dest: "D",
                    width: 0.05,
                },
            ],
        ),
        (
            "B",
            rgb8(220, 160, 60),
            &[Flow {
                dest: "D",
                width: 0.10,
            }],
        ),
        (
            "C",
            rgb8(80, 180, 100),
            &[Flow {
                dest: "B",
                width: 0.06,
            }],
        ),
        ("D", rgb8(60, 130, 220), &[]),
    ];

    // Lay out the categories around the ring: each occupies an arc of
    // `arc_width` = (1 / n_cats) − inter-arc gap.
    let n_cats = categories.len();
    let gap = 0.03; // 3% of the ring between categories
    let arc_width = 1.0 / n_cats as f64 - gap;
    let arc_start: std::collections::HashMap<&str, f64> = categories
        .iter()
        .enumerate()
        .map(|(i, (name, _, _))| (*name, i as f64 / n_cats as f64 + 0.5 * gap))
        .collect();
    let arc_end: std::collections::HashMap<&str, f64> = categories
        .iter()
        .map(|(name, _, _)| (*name, arc_start[*name] + arc_width))
        .collect();

    // Build chord control polygons. Each chord uses 4 control points
    // per side (cubic B-spline), with the two interior control points
    // sitting at the polar centre (r = 0). In `"panel"` mode all
    // four "centre-side" control points project to the same pixel
    // (the polar origin) before the spline is evaluated, so the cubic
    // is the D3-style chord bezier: source-outer → centre →
    // destination-outer. Both edges of the ribbon converge at the
    // centre, so they touch but don't cross — the ribbon tapers
    // smoothly toward a single coincident point rather than two
    // parallel passes that would otherwise visibly invert as the
    // bands sweep around.
    let inner_r = 0.0;
    let outer_r = 0.92;

    // Look up each category's colour by name for the source→destination
    // fill gradient.
    let cat_color: std::collections::HashMap<&str, Color> = categories
        .iter()
        .map(|(name, color, _)| (*name, *color))
        .collect();

    let mut keys: Vec<String> = Vec::new();
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let mut x2s: Vec<f64> = Vec::new();
    let mut y2s: Vec<f64> = Vec::new();
    let mut fills: Vec<Color> = Vec::new();

    // Track per-category cursor — flows out of one category consume
    // theta from its arc in order, so consecutive flows don't overlap
    // at the source. Same logic mirrored at the destination.
    let mut out_cursor: std::collections::HashMap<&str, f64> = categories
        .iter()
        .map(|(name, _, _)| (*name, arc_start[*name]))
        .collect();
    let mut in_cursor: std::collections::HashMap<&str, f64> = categories
        .iter()
        .map(|(name, _, _)| (*name, arc_end[*name]))
        .collect();

    for (src, _color, flows) in categories.iter() {
        for flow in flows.iter() {
            let dest = flow.dest;
            let src_color = cat_color[src];
            let dest_color = cat_color[dest];
            // Source-side arc slice [src_start, src_start + width].
            let src_start = out_cursor[src];
            let src_end = src_start + flow.width;
            *out_cursor.get_mut(src).unwrap() = src_end;
            // Destination-side arc slice [dest_start, dest_start +
            // width]. Consume inward from the right end of the
            // destination arc so chords from different sources stack
            // without overlapping.
            let dest_end = in_cursor[dest];
            let dest_start = dest_end - flow.width;
            *in_cursor.get_mut(dest).unwrap() = dest_start;

            // 4 control points per curve for a cubic B-spline. D3-style
            // edge pairing: curve A connects source-start to
            // destination-end and curve B connects source-end to
            // destination-start. The chord ribbon physically rotates
            // 180° as it sweeps through the polar centre, so the
            // "lower-theta" end of the source arc lands next to the
            // "higher-theta" end of the destination arc — the
            // criss-cross pairing is what makes the ribbon edges stay
            // parallel through the sweep and not visibly cross in the
            // middle.
            let key = format!("{}->{}", src, dest);
            let ctrl_a_theta = [src_start, src_start, dest_end, dest_end];
            let ctrl_a_r = [outer_r, inner_r, inner_r, outer_r];
            let ctrl_b_theta = [src_end, src_end, dest_start, dest_start];
            let ctrl_b_r = [outer_r, inner_r, inner_r, outer_r];

            // Per-row fill interpolates from source colour at the
            // chord's first two control points (the source-arc end) to
            // destination colour at the last two (the destination-arc
            // end). Triggers RibbonBSplineGeom's mesh dispatch and the
            // mesh-grid colour lerp lands a smooth gradient along the
            // chord's spline parameter.
            let per_row_fill = [src_color, src_color, dest_color, dest_color];
            for i in 0..4 {
                keys.push(key.clone());
                xs.push(ctrl_a_theta[i]);
                ys.push(ctrl_a_r[i]);
                x2s.push(ctrl_b_theta[i]);
                y2s.push(ctrl_b_r[i]);
                fills.push(per_row_fill[i]);
            }
        }
    }

    // Outer-ring arc segments: one tick-bar per category, drawn as a
    // skinny ribbon between two concentric arcs. Each is a degree-1
    // B-spline (polyline fallback) so the arc follows polar
    // densification cleanly.
    let mut ring_keys: Vec<String> = Vec::new();
    let mut ring_xs: Vec<f64> = Vec::new();
    let mut ring_ys: Vec<f64> = Vec::new();
    let mut ring_y2s: Vec<f64> = Vec::new();
    let mut ring_fills: Vec<Color> = Vec::new();
    for (name, color, _) in categories.iter() {
        // 8 control points along the arc so polar densification kicks
        // in nicely between them.
        let steps = 8;
        for i in 0..steps {
            let t = i as f64 / (steps - 1) as f64;
            let theta = arc_start[name] + t * arc_width;
            ring_keys.push(format!("ring:{}", name));
            ring_xs.push(theta);
            ring_ys.push(0.97);
            ring_y2s.push(1.0);
            ring_fills.push(*color);
        }
    }

    // Build the plot.
    let mut plot = Plot::new(&cell_comp(), "panel")
        .title("Chord diagram via RibbonBSplineGeom")
        .subtitle("Cubic B-spline ribbons under polar projection")
        .bind("x", "theta")
        .bind("y", "radius")
        .bind("x2", "theta")
        .bind("y2", "radius")
        .projection(Projection::polar());

    // Chords first (so the ring sits on top).
    //
    // `interpolation = "panel"` is load-bearing: the cubic spline is
    // built in pixel space between projected control points, so the
    // two interior pull points (at small `r` near the polar centre)
    // drag the chord through the centre as a smooth bezier-style arc.
    // Under the default `"domain"` mode the spline would instead be
    // built in `(theta, radius)` data space — its samples projected
    // one-by-one — so the chord would trace a constant-`inner_r`
    // circular arc between source and destination instead of dipping
    // toward the centre. The terminal connections at each end of the
    // chord (the segments along the source / destination arc) are
    // densified independently of the interpolation mode, so they
    // follow the outer ring as polar geodesics in both cases.
    plot.add_geom(
        RibbonBSplineGeom::builder()
            .keys(keys)
            .set("x", xs)
            .set("y", ys)
            .set("x2", x2s)
            .set("y2", y2s)
            .set("interpolation", "panel")
            .set("fill", fills)
            .set("alpha", 0.65_f64)
            .build(),
    );
    // Outer ring tickbars.
    plot.add_geom(
        RibbonBSplineGeom::builder()
            .keys(ring_keys)
            .set("x", ring_xs)
            .set("y", ring_ys)
            .set("y2", ring_y2s)
            .set("degree", 1.0_f64)
            .set("fill", ring_fills)
            .build(),
    );

    let mut view = PlotComposition::new(cell_comp())
        .add_scale("theta", scale::continuous(0.0..=1.0))
        .add_scale("radius", scale::continuous(0.0..=1.0))
        .with_plot(plot);
    panic_on_issues(view.validate());
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/chord_diagram.png",
    );
}

fn panic_on_issues<T: std::fmt::Debug>(issues: Vec<T>) {
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
    }
}

fn render_to(
    renderer: &mut VelloRenderer,
    view: &mut PlotComposition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
    out_relative: &str,
) {
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir().unwrap().join(out_relative);
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
