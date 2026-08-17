//! Linetype patterns and the arc-length walk that renders them.
//!
//! A pattern is a sequence of `LinetypeStep` entries (`Dash` / `Marker`
//! / `Gap`) carried by [`Arc<[LinetypeStep]>`](LinetypeStep).
//!
//! Even-length, alternating: even-indexed entries draw something
//! (`Dash` for a stroked segment, `Marker` for a stamped shape),
//! odd-indexed entries are `Gap` (unconditional advance without
//! drawing). Empty array = solid (no dashing, no markers).
//!
//! The pt values resolve to px at draw time using the active dpi (`px
//! = pt * dpi / 72.0`), matching the same convention used for
//! `linewidth` and point `size`, so dash and marker proportions stay
//! stable across resolutions. Markers are sized to the resolved
//! `linewidth` pt of arc length, so they don't eat into the
//! surrounding gaps.
//!
//! ```ignore
//! use hephaestus::linetype::{self, dash, gap, marker, pattern};
//!
//! linetype::solid();    // []
//! linetype::dashed();   // [Dash(8), Gap(4)]
//! linetype::dotted();   // [Dash(2), Gap(3)]
//! linetype::dashdot();  // [Dash(8), Gap(3), Dash(2), Gap(3)]
//!
//! // Mixed marker + dash pattern: 5pt dash, 3pt gap, circle marker,
//! // 5pt gap, repeat.
//! pattern([dash(5.0), gap(3.0), marker("circle"), gap(5.0)]);
//! ```
//!
//! `draw_linetype_with_markers` is the renderer for marker-bearing
//! patterns: it walks arc length along a set of
//! [`crate::primitives::PolylineSampler`]s and stamps
//! shapes at the cursor positions the pattern dictates. Marker-free
//! patterns go through kurbo's dash fast path instead — see
//! [`is_marker_free`] and [`to_kurbo_dashes`].
//!
//! This module sits at the crate root rather than under `plot::geom`
//! because rich-text block borders express their stroke as a linetype
//! too, and the text layer must not depend on the plot layer.

use std::sync::Arc;

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::Affine;
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::primitives::PolylineSampler;
use crate::scene::{Glyph, GlyphRun, SceneBuilder};
use crate::shape::{Shape, ShapeKind, ShapeRegistry, ShapeStyle};
use crate::stroke::Stroke;

// ─── Linetype steps ──────────────────────────────────────────────────────────

pub use crate::style_vocab::LinetypeStep;

/// Arc-length tolerance below which two cursor positions count as
/// coincident.
const MARKER_EPSILON: f64 = 1e-9;

/// Scale-up applied to a glyph-backed marker's font size: the visible
/// ink of a typical glyph fills ~85% of its em-box height, so the boost
/// makes glyph markers read at the same visual extent as the vector
/// shapes they sit alongside.
pub(crate) const MARKER_INK_COVERAGE_BOOST: f64 = 1.0 / 0.85;

/// Convert pt to px at the given dpi. The same convention is used for
/// every absolute graphical size (point diameter, stroke linewidth,
/// dash lengths, dash offset).
#[inline]
fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    pt * dpi / 72.0
}

/// Stroke a segment of `length_pt` pt along the line.
pub fn dash(length_pt: f64) -> LinetypeStep {
    LinetypeStep::Dash(length_pt)
}

/// Advance the cursor by `length_pt` pt without drawing.
pub fn gap(length_pt: f64) -> LinetypeStep {
    LinetypeStep::Gap(length_pt)
}

/// Stamp the named shape at the current cursor (rotated to the local
/// tangent). The marker is sized to the resolved `linewidth` pt of arc
/// length so subsequent gaps measure clear space.
pub fn marker(name: impl Into<Arc<str>>) -> LinetypeStep {
    LinetypeStep::Marker(name.into())
}

/// Build a linetype pattern from a sequence of steps. Validates the
/// "even-index = Dash|Marker, odd-index = Gap" alternation; panics with
/// a clear message on violation. Empty input → solid.
pub fn pattern(steps: impl IntoIterator<Item = LinetypeStep>) -> Arc<[LinetypeStep]> {
    let v: Vec<LinetypeStep> = steps.into_iter().collect();
    validate_alternation(&v);
    Arc::from(v)
}

/// `true` if `pattern` contains no `LinetypeStep::Marker` entries.
/// Marker-free patterns can be rendered via the kurbo dash fast path.
pub fn is_marker_free(pattern: &[LinetypeStep]) -> bool {
    pattern.iter().all(|s| !s.is_marker())
}

/// Project a marker-free pattern to the flat `[dash, gap, dash, gap,
/// ...]` f64 slice that [`Stroke::with_dashes`](crate::stroke::Stroke) expects. Panics if
/// the pattern contains markers (call [`is_marker_free`] first) or if
/// the alternation is malformed (use [`pattern`] / [`validate_pattern`]
/// to construct).
pub fn to_kurbo_dashes(pattern: &[LinetypeStep]) -> Option<Vec<f64>> {
    pattern
        .iter()
        .map(|step| match step {
            LinetypeStep::Dash(l) | LinetypeStep::Gap(l) => Some(*l),
            // A marker stamps a shape rather than advancing a dash, so
            // the pattern has no stroke-dash equivalent; the caller
            // walks it with `draw_linetype_with_markers` instead.
            LinetypeStep::Marker(_) => None,
        })
        .collect()
}

/// Replace every `Marker(_)` in `pattern` with `Gap(linewidth_pt)`,
/// preserving the marker's arc-length contribution while skipping the
/// stamp. Used by non-LineGeom geoms to render the dashing portion of
/// a marker-bearing linetype while ignoring the markers themselves.
pub fn strip_markers(pattern: &[LinetypeStep], linewidth_pt: f64) -> Arc<[LinetypeStep]> {
    let mapped: Vec<LinetypeStep> = pattern
        .iter()
        .map(|step| match step {
            LinetypeStep::Marker(_) => LinetypeStep::Gap(linewidth_pt),
            other => other.clone(),
        })
        .collect();
    Arc::from(mapped)
}

/// Validate the alternation invariant: even-indexed entries are `Dash`
/// or `Marker`; odd-indexed entries are `Gap`; length is even. Panics
/// with a clear message on violation. Empty input is valid (solid).
pub fn validate_pattern(pattern: &[LinetypeStep]) {
    validate_alternation(pattern);
}

fn validate_alternation(pattern: &[LinetypeStep]) {
    if pattern.is_empty() {
        return;
    }
    if !pattern.len().is_multiple_of(2) {
        panic!(
            "linetype::pattern: must have even length (alternating Dash|Marker / Gap), got {}",
            pattern.len()
        );
    }
    for (i, step) in pattern.iter().enumerate() {
        let is_gap = matches!(step, LinetypeStep::Gap(_));
        let expected_gap = i % 2 == 1;
        if is_gap != expected_gap {
            let kind = match step {
                LinetypeStep::Dash(_) => "Dash",
                LinetypeStep::Marker(_) => "Marker",
                LinetypeStep::Gap(_) => "Gap",
            };
            let expected = if expected_gap {
                "Gap"
            } else {
                "Dash or Marker"
            };
            panic!(
                "linetype::pattern: entry {i} is {kind} but expected {expected} \
                 (patterns must alternate Dash|Marker, Gap, Dash|Marker, Gap, …)"
            );
        }
    }
}

/// No dashing — a continuous solid line.
pub fn solid() -> Arc<[LinetypeStep]> {
    Arc::from(Vec::<LinetypeStep>::new())
}

/// `[Dash(8), Gap(4)]`.
pub fn dashed() -> Arc<[LinetypeStep]> {
    pattern([dash(8.0), gap(4.0)])
}

/// `[Dash(2), Gap(3)]`.
pub fn dotted() -> Arc<[LinetypeStep]> {
    pattern([dash(2.0), gap(3.0)])
}

/// `[Dash(8), Gap(3), Dash(2), Gap(3)]`.
pub fn dashdot() -> Arc<[LinetypeStep]> {
    pattern([dash(8.0), gap(3.0), dash(2.0), gap(3.0)])
}

/// Walk one or more [`PolylineSampler`]s through a linetype pattern,
/// emitting `scene.stroke` for each `Dash` segment and
/// `scene.fill` / `scene.stroke` for each `Marker` stamp. Advances the
/// arc-length cursor by Dash / Marker (= `linewidth_px`) / Gap as the
/// pattern dictates; the pattern loops when the cursor wraps.
///
/// **Mode**:
/// - `distribute = false` — open polyline. Pattern starts at cursor
///   `-dash_offset_px` and runs until the cursor reaches the end of
///   each sampler. The trailing partial pattern run is silently
///   truncated.
/// - `distribute = true` — closed perimeter. Scale every `Gap` in the
///   pattern by a uniform factor so an integer number of pattern runs
///   exactly fits the sampler's total length. Dashes and marker widths
///   are left untouched. The seam at distance 0 == total_length is
///   invisible: the pattern wraps continuously. A pattern with zero
///   total Gap length cannot stretch — the call falls back to the
///   non-distribute walk.
///
/// `marker_fill` and `marker_stroke` are passed to filled / stroked
/// subpaths of each marker shape respectively (mirroring PointGeom's
/// emission convention). `solid_stroke_spec` is reused for every Dash
/// sub-stroke and for any marker shape whose style is
/// [`ShapeStyle::Stroke`] — the geom is responsible for setting
/// `width / cap / join` correctly.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_linetype_with_markers(
    scene: &mut dyn SceneBuilder,
    samplers: &[PolylineSampler],
    pattern_pt: &[LinetypeStep],
    dash_offset_px: f64,
    linewidth_px: f64,
    marker_fill: Color,
    marker_stroke: Color,
    marker_outline_pt: f64,
    solid_stroke_spec: &Stroke,
    xform: Affine,
    shapes: &ShapeRegistry,
    dpi: f64,
    pick: PickId,
    distribute: bool,
) {
    debug_assert!(
        !pattern_pt.is_empty(),
        "draw_linetype_with_markers: empty pattern"
    );

    for sampler in samplers {
        let total = sampler.total_length();
        if total <= 0.0 {
            continue;
        }

        // Resolve a per-sampler pattern: when `distribute` is set,
        // scale gaps to fit `total` exactly; otherwise use the pattern
        // as-is.
        let pattern_px = resolve_pattern_px(pattern_pt, linewidth_px, dpi, total, distribute);

        let n_steps = pattern_px.len();
        let mut cursor = if distribute { 0.0 } else { -dash_offset_px };
        let mut step_idx = 0usize;
        let mut safety = 0usize;
        // Safety cap proportional to total / linewidth to catch
        // malformed zero-advance patterns.
        let max_iters: usize = (total / linewidth_px.max(1e-3))
            .ceil()
            .clamp(64.0, 1_000_000.0) as usize
            * n_steps
            + n_steps * 4;
        while cursor < total - MARKER_EPSILON && safety < max_iters {
            safety += 1;
            let step = &pattern_px[step_idx];
            step_idx = (step_idx + 1) % n_steps;
            match step {
                ResolvedStep::Dash(len_px) => {
                    let len = *len_px;
                    if len <= 0.0 {
                        continue;
                    }
                    let start = cursor.max(0.0);
                    let end = (cursor + len).min(total);
                    if end > start + MARKER_EPSILON {
                        let path = build_sub_polyline(sampler, start, end);
                        if path.elements().len() >= 2 {
                            scene.stroke(
                                solid_stroke_spec,
                                xform,
                                &Brush::Solid(marker_stroke),
                                None,
                                &path,
                                pick,
                            );
                        }
                    }
                    cursor += len;
                }
                ResolvedStep::Marker(name) => {
                    let mid = cursor + 0.5 * linewidth_px;
                    if mid >= 0.0 && mid <= total + MARKER_EPSILON {
                        if let Some(shape) = shapes.get(name.as_ref()) {
                            if let Some(sample) = sampler.sample_at(mid) {
                                let bbox = shape.bounding_box();
                                let local_h = bbox.height();
                                let scale_factor = if local_h > 0.0 {
                                    linewidth_px / local_h
                                } else {
                                    linewidth_px
                                };
                                let marker_xform_unscaled = xform
                                    * Affine::translate(sample.point.to_vec2())
                                    * Affine::rotate(sample.tangent.atan2());
                                emit_marker_shape(
                                    scene,
                                    shape,
                                    marker_xform_unscaled,
                                    scale_factor,
                                    marker_fill,
                                    marker_stroke,
                                    pt_to_px(marker_outline_pt, dpi),
                                    pick,
                                );
                            }
                        }
                    }
                    cursor += linewidth_px;
                }
                ResolvedStep::Gap(len_px) => {
                    cursor += *len_px;
                }
            }
        }
    }
}

/// Resolved pattern entry — pt converted to px and (for distribute
/// mode) gaps scaled to fit the polyline total length.
enum ResolvedStep {
    Dash(f64),
    Marker(Arc<str>),
    Gap(f64),
}

/// Convert a pattern from pt → px, optionally distributing gaps so an
/// integer number of pattern runs fits `total_px` exactly.
fn resolve_pattern_px(
    pattern_pt: &[LinetypeStep],
    linewidth_px: f64,
    dpi: f64,
    total_px: f64,
    distribute: bool,
) -> Vec<ResolvedStep> {
    // Pre-compute fixed and gap contributions per pattern run.
    let mut fixed_px = 0.0;
    let mut gap_px = 0.0;
    for step in pattern_pt {
        match step {
            LinetypeStep::Dash(p) => fixed_px += pt_to_px(*p, dpi),
            LinetypeStep::Marker(_) => fixed_px += linewidth_px,
            LinetypeStep::Gap(p) => gap_px += pt_to_px(*p, dpi),
        }
    }
    let period_px = fixed_px + gap_px;

    let gap_scale = if distribute && gap_px > MARKER_EPSILON && period_px > MARKER_EPSILON {
        let n = (total_px / period_px).round().max(1.0);
        let target_gap = (total_px - n * fixed_px) / n;
        // Disallow negative scale (happens when fixed >> total: the
        // pattern's non-gap content alone already exceeds the
        // perimeter). Fall back to 1.0 — the pattern will overflow
        // visually, which matches what an unfittable closed pattern
        // does anyway.
        (target_gap / gap_px).max(0.0)
    } else {
        1.0
    };

    pattern_pt
        .iter()
        .map(|step| match step {
            LinetypeStep::Dash(p) => ResolvedStep::Dash(pt_to_px(*p, dpi)),
            LinetypeStep::Marker(name) => ResolvedStep::Marker(name.clone()),
            LinetypeStep::Gap(p) => ResolvedStep::Gap(pt_to_px(*p, dpi) * gap_scale),
        })
        .collect()
}

/// Stamp one shape at `xform`. Mirrors PointGeom's emission loop.
///
/// - Path-backed shapes: fill subpaths take `marker_fill`; stroke subpaths
///   take `marker_stroke`.
/// - Glyph-backed shapes: a single `GlyphRun` is emitted with
///   `brush = marker_fill`; `marker_stroke` is ignored (glyph markers
///   are fill-only). The em-space shift `em_origin - em_bbox.center()` is
///   composed inside the caller's `xform` so the glyph's visual centre
///   lands at the placement point. The caller's `xform` is expected to
///   already carry the desired translate/rotate/scale; the scale factor
///   becomes the glyph's effective font size in pixels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_marker_shape(
    scene: &mut dyn SceneBuilder,
    shape: &Shape,
    xform_unscaled: Affine,
    scale_factor: f64,
    marker_fill: Color,
    marker_stroke: Color,
    outline_px: f64,
    pick: PickId,
) {
    match shape.kind() {
        ShapeKind::Paths { paths, style } => {
            if !(scale_factor.is_finite() && scale_factor > 0.0) {
                return;
            }
            let xform = xform_unscaled * Affine::scale(scale_factor);
            // The subpaths are stroked under `Affine::scale(scale_factor)`,
            // so the width is divided by the same factor to land at
            // `outline_px` in output pixels — the inversion `PointGeom`
            // applies for the identical transform.
            let outline_spec = Stroke::new(outline_px / scale_factor);
            for sub in paths {
                match style {
                    ShapeStyle::Fill => {
                        scene.fill(
                            FillRule::NonZero,
                            xform,
                            &Brush::Solid(marker_fill),
                            None,
                            sub,
                            pick,
                        );
                    }
                    ShapeStyle::Stroke => {
                        scene.stroke(
                            &outline_spec,
                            xform,
                            &Brush::Solid(marker_stroke),
                            None,
                            sub,
                            pick,
                        );
                    }
                }
            }
        }
        ShapeKind::Glyph {
            font,
            glyph_id,
            em_bbox,
            em_origin,
        } => {
            // Glyph linetype markers / arrow terminators: scale up so
            // the visible ink approximately fills the surrounding
            // linewidth track. The bbox height is the typographic
            // ascender, but the visible ink of most glyphs only fills
            // ~85% of that height (typical emoji padding within their
            // em-square; Latin cap-to-baseline within ascender).
            // PointGeom does *not* apply this boost — its sizing is
            // anchored to the GLYPH_BBOX_REFERENCE convention, which
            // already targets the vector-shape visual extent.
            //
            // The effective scale (incl. INK_COVERAGE_BOOST) is baked
            // into `font_size` rather than the transform so vello
            // picks the matching bitmap strike for colour-emoji
            // fonts — `font_size: 1.0` with a transform scale would
            // pick the smallest strike and upscale (= fuzzy).
            let effective_font_size = scale_factor * MARKER_INK_COVERAGE_BOOST;
            // Centring is in em-space; convert to pixel space at the
            // effective font size, then apply the unscaled outer
            // transform (which carries the rotation + translation).
            let centring_em = em_origin.to_vec2() - em_bbox.center().to_vec2();
            let glyphs = [Glyph {
                id: glyph_id,
                x: 0.0,
                y: 0.0,
            }];
            let brush = Brush::Solid(marker_fill);
            let run = GlyphRun {
                font,
                font_size: effective_font_size as f32,
                transform: xform_unscaled * Affine::translate(centring_em * effective_font_size),
                glyph_transform: None,
                brush: &brush,
                brush_alpha: 1.0,
                hint: false,
                glyphs: &glyphs,
                style: None,
            };
            scene.draw_glyphs(&run, pick);
        }
    }
}

/// Build a sub-polyline from `sampler` spanning arc-length `[start,
/// end]`. Includes interior original vertices that fall strictly
/// between start and end, so straight runs stay one LineTo each.
fn build_sub_polyline(sampler: &PolylineSampler, start: f64, end: f64) -> Path {
    let mut path = Path::new();
    let start = start.max(0.0);
    let end = end.min(sampler.total_length());
    if end <= start + MARKER_EPSILON {
        return path;
    }
    let head = match sampler.sample_at(start) {
        Some(s) => s.point,
        None => return path,
    };
    path.move_to(head);
    for d in sampler.segment_boundaries_between(start, end) {
        if let Some(s) = sampler.sample_at(d) {
            path.line_to(s.point);
        }
    }
    if let Some(tail) = sampler.sample_at(end) {
        path.line_to(tail.point);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_is_empty() {
        assert_eq!(solid().len(), 0);
    }

    #[test]
    fn named_patterns_alternate_and_are_marker_free() {
        for p in [dashed(), dotted(), dashdot()] {
            assert!(p.len().is_multiple_of(2));
            assert!(!p.is_empty());
            assert!(is_marker_free(&p));
            validate_pattern(&p);
        }
    }

    #[test]
    fn dashed_canonical_values() {
        let p = dashed();
        assert!(matches!(p[0], LinetypeStep::Dash(d) if (d - 8.0).abs() < 1e-12));
        assert!(matches!(p[1], LinetypeStep::Gap(g) if (g - 4.0).abs() < 1e-12));
    }

    #[test]
    fn dashdot_canonical_values() {
        let p = dashdot();
        assert_eq!(p.len(), 4);
        assert!(matches!(p[0], LinetypeStep::Dash(d) if (d - 8.0).abs() < 1e-12));
        assert!(matches!(p[1], LinetypeStep::Gap(g) if (g - 3.0).abs() < 1e-12));
        assert!(matches!(p[2], LinetypeStep::Dash(d) if (d - 2.0).abs() < 1e-12));
        assert!(matches!(p[3], LinetypeStep::Gap(g) if (g - 3.0).abs() < 1e-12));
    }

    #[test]
    fn pattern_accepts_markers() {
        let p = pattern([marker("circle"), gap(5.0)]);
        assert_eq!(p.len(), 2);
        assert!(p[0].is_marker());
        assert!(!is_marker_free(&p));
    }

    #[test]
    fn pattern_mixed_dash_and_marker() {
        let p = pattern([dash(6.0), gap(2.0), marker("square"), gap(4.0)]);
        assert_eq!(p.len(), 4);
        assert!(!is_marker_free(&p));
    }

    #[test]
    #[should_panic(expected = "must have even length")]
    fn pattern_panics_on_odd_length() {
        let _ = pattern([dash(1.0), gap(2.0), dash(3.0)]);
    }

    #[test]
    #[should_panic(expected = "expected Gap")]
    fn pattern_panics_on_dash_in_gap_slot() {
        let _ = pattern([dash(1.0), dash(2.0)]);
    }

    #[test]
    #[should_panic(expected = "expected Dash or Marker")]
    fn pattern_panics_on_gap_in_dash_slot() {
        let _ = pattern([gap(2.0), gap(1.0)]);
    }

    #[test]
    fn to_kurbo_dashes_round_trip() {
        let p = pattern([dash(5.0), gap(3.0)]);
        assert_eq!(to_kurbo_dashes(&p), Some(vec![5.0, 3.0]));
    }

    #[test]
    fn to_kurbo_dashes_declines_markered_patterns() {
        // A marker has no stroke-dash equivalent, so the caller has to
        // fall back to the arc-length walk rather than get a pattern
        // that silently drops the stamp.
        let p = pattern([marker("circle"), gap(5.0)]);
        assert_eq!(to_kurbo_dashes(&p), None);
    }

    #[test]
    fn strip_markers_replaces_with_gap_of_linewidth() {
        let p = pattern([dash(6.0), gap(2.0), marker("circle"), gap(5.0)]);
        let stripped = strip_markers(&p, 4.0);
        assert_eq!(stripped.len(), 4);
        assert!(matches!(stripped[0], LinetypeStep::Dash(d) if (d - 6.0).abs() < 1e-12));
        assert!(matches!(stripped[1], LinetypeStep::Gap(g) if (g - 2.0).abs() < 1e-12));
        assert!(matches!(stripped[2], LinetypeStep::Gap(g) if (g - 4.0).abs() < 1e-12));
        assert!(matches!(stripped[3], LinetypeStep::Gap(g) if (g - 5.0).abs() < 1e-12));
        assert!(is_marker_free(&stripped));
    }
}
