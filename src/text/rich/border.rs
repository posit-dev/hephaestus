//! Block backgrounds and borders.
//!
//! A uniform-width border strokes as one rectangle so it cooperates
//! with `border_radius`; mixed per-side widths group into contiguous
//! same-width polyline chains (T → R → B → L) so a shared corner is
//! one join rather than two abutting endpoints.

use super::block::BlockPaint;
use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::Affine;
use crate::pick::PickId;
use crate::scene::SceneBuilder;

pub(crate) fn emit_block_paint(
    scene: &mut dyn SceneBuilder,
    paint: &BlockPaint,
    offsets: super::anchor::AnchorOffsets,
    outer: Affine,
    dpi: f64,
    pick_id: PickId,
) {
    let rect = kurbo::Rect::new(
        paint.outer_rect.x0 - offsets.ref_x as f64,
        paint.outer_rect.y0 - offsets.ref_y as f64,
        paint.outer_rect.x1 - offsets.ref_x as f64,
        paint.outer_rect.y1 - offsets.ref_y as f64,
    );
    let path = if paint.corner_radius > 0.0 {
        crate::primitives::rounded_rect(rect, paint.corner_radius as f64)
    } else {
        crate::primitives::rect(rect)
    };
    if let Some(color) = paint.background {
        scene.fill(
            crate::path::FillRule::NonZero,
            outer,
            &Brush::Solid(color),
            None,
            &path,
            pick_id,
        );
    }
    if let Some(border) = paint.border.as_ref() {
        // Borders use Butt caps and Miter joins — square ends, sharp
        // corners. This matches typographic convention: block bars
        // (blockquote left rule, hr top rule) shouldn't have visible
        // rounded end-caps peeking out past the block edge.
        //
        // Marker-free patterns take kurbo's `with_dashes` fast path
        // (one stroke call per polyline chain). Marker-bearing
        // patterns route through the crate-wide
        // `draw_linetype_with_markers`, which walks the polyline in
        // arc length and stamps shape markers along it — the same
        // primitive `LineGeom` uses.
        let has_markers = border
            .linetype_pt
            .as_ref()
            .map(|p| !crate::linetype::is_marker_free(p))
            .unwrap_or(false);
        // Kurbo's `with_dashes` fast path only accepts flat pt-length
        // slices — no `Marker` steps. Compute the flat slice only for
        // marker-free patterns; markered patterns route through the
        // shared `draw_linetype_with_markers` primitive below.
        let dashes_px: Option<Vec<f64>> =
            border
                .linetype_pt
                .as_ref()
                .filter(|_| !has_markers)
                .map(|pattern| {
                    crate::linetype::to_kurbo_dashes(pattern)
                        .into_iter()
                        .map(|pt| pt * dpi / 72.0)
                        .collect()
                });
        let border_stroke = |w_px: f32| {
            let s = crate::stroke::Stroke::new(w_px as f64)
                .with_caps(crate::stroke::Cap::Butt)
                .with_join(crate::stroke::Join::Miter);
            if let (Some(pattern_px), false) = (dashes_px.as_ref(), has_markers) {
                s.with_dashes(0.0_f64, pattern_px.clone())
            } else {
                s
            }
        };
        if border.is_uniform() {
            let w = border.widths_px[0];
            if w > 0.0 {
                if has_markers {
                    stroke_markered_perimeter(
                        scene,
                        pick_id,
                        &rect,
                        w,
                        border.color,
                        border
                            .linetype_pt
                            .as_ref()
                            .expect("has_markers implies Some"),
                        outer,
                        dpi,
                    );
                } else {
                    scene.stroke(
                        &border_stroke(w),
                        outer,
                        &Brush::Solid(border.color),
                        None,
                        &path,
                        pick_id,
                    );
                }
            }
        } else {
            // Per-side widths — collapse contiguous same-width sides
            // (in CW cyclic order T → R → B → L) into single
            // polylines so a corner where two present sides meet is
            // stroked as one continuous path (mitred at the join)
            // rather than two independent segments (which would show
            // a visible seam at the corner). Sides with mismatched
            // widths still emit as separate polylines. `corner_radius`
            // is intentionally ignored on the mixed path (square
            // corners; documented on `StyleDelta::border_width`).
            let brush = Brush::Solid(border.color);
            let widths = border.widths_px;
            let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
            let corners = [
                kurbo::Point::new(x0, y0),
                kurbo::Point::new(x1, y0),
                kurbo::Point::new(x1, y1),
                kurbo::Point::new(x0, y1),
            ];
            for chain in group_border_sides_cw(widths, corners) {
                if has_markers {
                    let sampler = crate::primitives::PolylineSampler::from_polyline(&chain.points);
                    let color = border.color;
                    let solid = crate::stroke::Stroke::new(chain.width as f64)
                        .with_caps(crate::stroke::Cap::Butt)
                        .with_join(crate::stroke::Join::Miter);
                    let shapes = crate::shape::ShapeRegistry::shared_builtins();
                    crate::linetype::draw_linetype_with_markers(
                        scene,
                        std::slice::from_ref(&sampler),
                        border
                            .linetype_pt
                            .as_ref()
                            .expect("has_markers implies Some"),
                        0.0,
                        chain.width as f64,
                        color,
                        color,
                        0.0,
                        &solid,
                        outer,
                        shapes,
                        dpi,
                        pick_id,
                        false,
                    );
                    continue;
                }
                let mut path = kurbo::BezPath::new();
                let mut pts = chain.points.iter();
                if let Some(&p) = pts.next() {
                    path.move_to(p);
                    for &p in pts {
                        path.line_to(p);
                    }
                }
                scene.stroke(
                    &border_stroke(chain.width),
                    outer,
                    &brush,
                    None,
                    &path,
                    pick_id,
                );
            }
        }
    }
}

/// Stamp a marker-bearing linetype around the full perimeter of a
/// uniform-width block border. Used when [`BlockBorder::linetype_pt`]
/// contains at least one [`crate::scales::value::LinetypeStep::Marker`]
/// step. Builds one closed [`crate::primitives::PolylineSampler`] over
/// the four corners (wrapping the seam back to the top-left) and
/// delegates to the crate-wide dash+marker primitive.
#[allow(clippy::too_many_arguments)]
fn stroke_markered_perimeter(
    scene: &mut dyn SceneBuilder,
    pick_id: PickId,
    rect: &kurbo::Rect,
    width_px: f32,
    color: Color,
    pattern_pt: &[crate::scales::value::LinetypeStep],
    outer: Affine,
    dpi: f64,
) {
    let corners = [
        kurbo::Point::new(rect.x0, rect.y0),
        kurbo::Point::new(rect.x1, rect.y0),
        kurbo::Point::new(rect.x1, rect.y1),
        kurbo::Point::new(rect.x0, rect.y1),
        kurbo::Point::new(rect.x0, rect.y0),
    ];
    let sampler = crate::primitives::PolylineSampler::from_polyline(&corners);
    let solid = crate::stroke::Stroke::new(width_px as f64)
        .with_caps(crate::stroke::Cap::Butt)
        .with_join(crate::stroke::Join::Miter);
    let shapes = crate::shape::ShapeRegistry::shared_builtins();
    crate::linetype::draw_linetype_with_markers(
        scene,
        std::slice::from_ref(&sampler),
        pattern_pt,
        0.0,
        width_px as f64,
        color,
        color,
        0.0,
        &solid,
        outer,
        shapes,
        dpi,
        pick_id,
        false,
    );
}

/// One polyline chunk of a block's mixed-width border. Contiguous
/// same-width sides (in CW cyclic order T → R → B → L) share one
/// chain, so their shared corner is a single join rather than two
/// abutting endpoints.
struct BorderChain {
    /// Chain vertices in traversal order. `len >= 2`.
    points: Vec<kurbo::Point>,
    /// Uniform stroke width for the chain (px).
    width: f32,
}

/// Group non-zero sides (in CW cyclic order T → R → B → L, indices
/// 0..4) into polyline chains: contiguous same-width sides join into
/// one chain sharing their meeting corner; a zero-width or
/// mismatched-width side breaks the chain. Cyclic — a chain may wrap
/// around from L to T when both are present with the same width.
///
/// Returns each chain's ordered vertex list plus its shared width.
/// The uniform-width path in `emit_block_paint` handles the
/// all-four-sides-same case; this helper is only reached when at
/// least one side is zero or widths differ across sides.
fn group_border_sides_cw(widths: [f32; 4], corners: [kurbo::Point; 4]) -> Vec<BorderChain> {
    // Sides in cyclic CW order:
    //   0=T: corner[0] → corner[1]
    //   1=R: corner[1] → corner[2]
    //   2=B: corner[2] → corner[3]
    //   3=L: corner[3] → corner[0]
    let side = |i: usize| -> (kurbo::Point, kurbo::Point) { (corners[i], corners[(i + 1) % 4]) };
    let present = |i: usize| widths[i] > 0.0;
    let same_width = |i: usize, j: usize| (widths[i] - widths[j]).abs() < 1e-3;
    // Pick a start index whose predecessor either isn't present or
    // has a different width — that's a chain boundary. If no such
    // break exists (all four present, all same width), the caller
    // is in the uniform-stroke branch and never enters this helper.
    let start = (0..4).find(|&i| {
        let prev = (i + 3) % 4;
        present(i) && !(present(prev) && same_width(prev, i))
    });
    let Some(start) = start else {
        // All four present with same width. Emit as a closed loop.
        let mut points: Vec<kurbo::Point> = corners.to_vec();
        points.push(corners[0]);
        return vec![BorderChain {
            points,
            width: widths[0],
        }];
    };
    let mut chains: Vec<BorderChain> = Vec::new();
    let mut cur: Vec<kurbo::Point> = Vec::new();
    let mut cur_w = 0.0f32;
    for step in 0..4 {
        let idx = (start + step) % 4;
        let w = widths[idx];
        let (a, b) = side(idx);
        if w <= 0.0 {
            if !cur.is_empty() {
                chains.push(BorderChain {
                    points: std::mem::take(&mut cur),
                    width: cur_w,
                });
            }
            continue;
        }
        if cur.is_empty() {
            cur.push(a);
            cur.push(b);
            cur_w = w;
        } else if (w - cur_w).abs() < 1e-3 {
            cur.push(b);
        } else {
            chains.push(BorderChain {
                points: std::mem::take(&mut cur),
                width: cur_w,
            });
            cur.push(a);
            cur.push(b);
            cur_w = w;
        }
    }
    if !cur.is_empty() {
        chains.push(BorderChain {
            points: cur,
            width: cur_w,
        });
    }
    chains
}
