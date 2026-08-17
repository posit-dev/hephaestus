//! Shared path-building walk for the multi-vertex geoms.
//!
//! Every geom that turns a sequence of source vertices into a panel-pixel
//! polyline runs the same walk: resolve each vertex's channel-space
//! position, drop the ones that don't resolve, densify each edge under a
//! non-linear projection so the path follows the projected geodesic
//! instead of cutting across it as a chord, apply the per-vertex pt
//! offsets, and push. [`project_and_densify`] owns that walk; the caller
//! supplies only the per-vertex resolution and the two policies that
//! genuinely differ between geoms — what an unresolvable vertex does
//! ([`GapPolicy`]) and whether the sequence is a ring ([`PathOptions`]).
//!
//! Geoms drawing a variable-width / variable-colour ribbon co-build
//! per-vertex half-widths and colours alongside the points. Those come
//! from [`PathVertex::attrs`]; a vertex that leaves them `None` costs
//! nothing, and the run's attribute arrays stay empty.

use crate::color::{lerp_color, Color, ColorSpace};
use crate::geometry::{Point, Rect};
use crate::plot::projection::InteriorSample;

use super::GeomContext;

/// Ribbon attributes carried by one vertex.
#[derive(Clone, Copy)]
pub(crate) struct VertexAttrs {
    /// Half the stroke width at this vertex, in px.
    pub half_width_px: f64,
    /// Stroke colour at this vertex.
    pub color: Color,
}

/// One source vertex's resolved inputs.
pub(crate) struct PathVertex {
    /// Channel-space position as `[x, y]` panel fractions. A non-finite
    /// component makes the vertex unresolvable.
    pub frac: [f64; 2],
    /// Pixel offset applied after projection: `x` adds, `y` subtracts.
    pub offset_px: (f64, f64),
    /// Ribbon attributes, or `None` for a plain path.
    pub attrs: Option<VertexAttrs>,
}

/// What an unresolvable vertex does to the path.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GapPolicy {
    /// Drop the vertex; the vertices on either side of it connect. A
    /// closed or smoothed shape has no meaningful gap, so bridging is
    /// the only coherent answer.
    Skip,
    /// End the run in progress and open the next one after the gap, so
    /// missing data reads as a break in the mark.
    Split,
}

/// One contiguous stretch of projected vertices.
#[derive(Default)]
pub(crate) struct VertexRun {
    /// Panel-pixel vertices, projection-densified.
    pub points: Vec<Point>,
    /// Per-vertex half-widths in px. Empty for a plain path.
    pub widths: Vec<f64>,
    /// Per-vertex stroke colours. Empty for a plain path.
    pub colors: Vec<Color>,
}

/// Path-level settings for [`project_and_densify`].
pub(crate) struct PathOptions {
    /// What an unresolvable vertex does to the path.
    pub gap: GapPolicy,
    /// Treat the sequence as a ring: the edge from the last vertex back
    /// to the first is densified through the seam-aware
    /// [`Projection::interpolate_closing_segment_with_t`], so on a
    /// cyclic polar domain the ring closes across the theta seam.
    ///
    /// [`Projection::interpolate_closing_segment_with_t`]: crate::plot::projection::Projection::interpolate_closing_segment_with_t
    pub closing: bool,
    /// Runs shorter than this are dropped.
    pub min_run_len: usize,
    /// Colour space densified vertices blend their bracketing rows'
    /// colours through. Unused for a plain path.
    pub color_space: ColorSpace,
}

impl PathOptions {
    /// An open path whose unresolvable vertices are dropped.
    pub(crate) fn path(min_run_len: usize) -> Self {
        PathOptions {
            gap: GapPolicy::Skip,
            closing: false,
            min_run_len,
            color_space: ColorSpace::default(),
        }
    }

    /// A ring whose unresolvable vertices are dropped.
    pub(crate) fn ring(min_run_len: usize) -> Self {
        PathOptions {
            closing: true,
            ..PathOptions::path(min_run_len)
        }
    }

    /// An open path that breaks into a fresh run at every unresolvable
    /// vertex.
    pub(crate) fn split(min_run_len: usize) -> Self {
        PathOptions {
            gap: GapPolicy::Split,
            ..PathOptions::path(min_run_len)
        }
    }

    /// Set the colour space densified vertices blend through.
    pub(crate) fn with_color_space(mut self, space: ColorSpace) -> Self {
        self.color_space = space;
        self
    }
}

/// Project `count` source vertices to panel pixels, densifying every
/// edge under a non-linear projection and splitting into runs per the
/// path's [`GapPolicy`].
///
/// `vertex` resolves one source vertex by index — its channel-space
/// position, its pt offsets, and (for ribbon-mode callers) its
/// per-vertex half-width and colour. Vertices whose position doesn't
/// resolve to a finite panel pixel are handled by the gap policy.
///
/// Per-vertex offsets apply to the source vertices only — interior
/// densified points sit on the un-offset geodesic. Interior points
/// carry attributes lerped between their two bracketing vertices at the
/// channel-space `t` the projection reports, so points, widths and
/// colours stay length-aligned.
pub(crate) fn project_and_densify<F>(
    ctx: &GeomContext<'_>,
    count: usize,
    opts: &PathOptions,
    mut vertex: F,
) -> Vec<VertexRun>
where
    F: FnMut(usize) -> PathVertex,
{
    let panel = ctx.panel_rect;
    let is_linear = ctx.projection.is_linear();
    let mut runs: Vec<VertexRun> = Vec::new();
    let mut run = VertexRun::default();
    let mut samples: Vec<InteriorSample> = Vec::new();
    let mut prev: Option<([f64; 2], Option<VertexAttrs>)> = None;
    let mut first: Option<([f64; 2], Option<VertexAttrs>)> = None;

    for k in 0..count {
        let v = vertex(k);
        let Some(pt) = project_vertex(ctx, panel, &v) else {
            if opts.gap == GapPolicy::Split {
                flush_run(&mut runs, &mut run, opts.min_run_len);
                prev = None;
            }
            continue;
        };
        if !is_linear {
            if let Some((prev_frac, prev_attrs)) = prev {
                samples.clear();
                ctx.projection
                    .interpolate_segment_with_t(panel, &prev_frac, &v.frac, &mut samples);
                push_interior(&mut run, &samples, prev_attrs, v.attrs, opts.color_space);
            }
        }
        run.points.push(pt);
        if let Some(a) = v.attrs {
            run.widths.push(a.half_width_px);
            run.colors.push(a.color);
        }
        if first.is_none() {
            first = Some((v.frac, v.attrs));
        }
        prev = Some((v.frac, v.attrs));
    }

    if opts.closing && !is_linear {
        if let (Some((prev_frac, prev_attrs)), Some((first_frac, first_attrs))) = (prev, first) {
            if prev_frac != first_frac {
                samples.clear();
                ctx.projection.interpolate_closing_segment_with_t(
                    panel,
                    &prev_frac,
                    &first_frac,
                    &mut samples,
                );
                push_interior(
                    &mut run,
                    &samples,
                    prev_attrs,
                    first_attrs,
                    opts.color_space,
                );
            }
        }
    }
    flush_run(&mut runs, &mut run, opts.min_run_len);
    runs
}

/// [`project_and_densify`] for a path that can't split: the single run,
/// empty when fewer than `min_run_len` vertices resolved.
pub(crate) fn project_and_densify_one<F>(
    ctx: &GeomContext<'_>,
    count: usize,
    opts: &PathOptions,
    vertex: F,
) -> VertexRun
where
    F: FnMut(usize) -> PathVertex,
{
    project_and_densify(ctx, count, opts, vertex)
        .into_iter()
        .next()
        .unwrap_or_default()
}

// Panel pixels for one source vertex, or `None` when either the
// channel-space position or the projected pixel is non-finite.
fn project_vertex(ctx: &GeomContext<'_>, panel: Rect, v: &PathVertex) -> Option<Point> {
    if !v.frac[0].is_finite() || !v.frac[1].is_finite() {
        return None;
    }
    let (px, py) = ctx.projection.project_to_panel_px(panel, &v.frac);
    let pt = Point::new(px + v.offset_px.0, py - v.offset_px.1);
    (pt.x.is_finite() && pt.y.is_finite()).then_some(pt)
}

// Append the interior samples of one densified edge, lerping the
// bracketing vertices' attributes when both carry them.
fn push_interior(
    run: &mut VertexRun,
    samples: &[InteriorSample],
    from: Option<VertexAttrs>,
    to: Option<VertexAttrs>,
    space: ColorSpace,
) {
    match (from, to) {
        (Some(a), Some(b)) => {
            for s in samples {
                run.points.push(Point::new(s.px, s.py));
                run.widths
                    .push(a.half_width_px + s.t * (b.half_width_px - a.half_width_px));
                run.colors.push(lerp_color(a.color, b.color, s.t, space));
            }
        }
        _ => {
            for s in samples {
                run.points.push(Point::new(s.px, s.py));
            }
        }
    }
}

// Close off the vertices accumulated so far, keeping them only if they
// form something drawable, and leave the buffers ready for the next run.
fn flush_run(runs: &mut Vec<VertexRun>, run: &mut VertexRun, min_run_len: usize) {
    if run.points.len() >= min_run_len {
        runs.push(std::mem::take(run));
    } else {
        run.points.clear();
        run.widths.clear();
        run.colors.clear();
    }
}
