//! Decomposing a [`Mesh`](crate::mesh::Mesh) into fills.
//!
//! Shared by every rasterising backend: no backend has a native indexed-mesh
//! primitive, and the decomposition works purely in this crate's own types.
//!
//! Neither vello nor peniko has an indexed-mesh primitive, so a mesh
//! becomes a sequence of `fill` calls. Emitting one fill per triangle
//! leaves a visible AA seam along every shared edge, so two patterns
//! are detected first and drawn as a single fill each:
//!
//! - a **quad pair** — the `[A, B, C, A, C, D]` shape a ribbon segment
//!   emits — becomes one 4-vertex polygon;
//! - a **uniform fan** — a run of triangles sharing a hub vertex and a
//!   single colour — becomes one polygon over the fan's rim.
//!
//! Anything else falls back to per-triangle fills, which is correct but
//! bands where a triangle carries three distinct colours.
//!
//! Colours become linear-gradient brushes: two vertices sharing a
//! colour define the gradient's perpendicular axis, and a triangle with
//! three distinct colours falls back to the most-separated pair.

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point};
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::scene::SceneBuilder;

/// Build a closed triangle path from three vertices.
pub(crate) fn triangle_path(pts: &[Point; 3]) -> Path {
    let mut p = Path::new();
    p.move_to(pts[0]);
    p.line_to(pts[1]);
    p.line_to(pts[2]);
    p.close_path();
    p
}

/// Pick a brush for a triangle's three vertex colors.
///
/// - All three equal → solid brush.
/// - Exactly two equal (the ribbon-triangle case: two shoulders sharing
///   a color + one tip with a different color) → linear gradient
///   running from the midpoint of the matching pair to the unique tip
///   vertex, with stops `[shared, tip]`. This places both equal-color
///   vertices at gradient fraction 0 (because they project equidistant
///   from the axis's start) and the tip at fraction 1 — so adjacent
///   ribbon segments meet seamlessly.
/// - Three distinct colors (general mesh) → linear gradient between
///   the max-color-distance pair. The third vertex gets an
///   interpolated color at its perpendicular-projection position,
///   which produces a small visible discontinuity along the edge
///   between the picked pair and the third vertex — a documented
///   limitation.
pub(crate) fn triangle_gradient_brush(pts: &[Point; 3], colors: &[Color; 3]) -> Brush {
    let eq01 = colors_eq(&colors[0], &colors[1]);
    let eq12 = colors_eq(&colors[1], &colors[2]);
    let eq20 = colors_eq(&colors[2], &colors[0]);
    if eq01 && eq12 {
        return Brush::Solid(colors[0]);
    }
    // Identify the "tip" vertex when exactly two colors match. For
    // `eq01 && !eq12 && !eq20` the matching pair is (0, 1) and the tip
    // is index 2; similar for the other two cases.
    let tip_idx = if eq01 {
        Some(2)
    } else if eq12 {
        Some(0)
    } else if eq20 {
        Some(1)
    } else {
        None
    };

    if let Some(t) = tip_idx {
        let (a, b) = match t {
            0 => (1, 2),
            1 => (2, 0),
            _ => (0, 1),
        };
        // The gradient axis runs perpendicular to the back-edge AB,
        // through the back-edge midpoint, to the foot of the
        // perpendicular dropped from the tip onto that axis. This
        // places A and B at gradient fraction 0 (pure shared color)
        // and the tip at fraction 1 — Gouraud-exact across the
        // triangle, with no projection error along the AB side.
        let start_x = 0.5 * (pts[a].x + pts[b].x);
        let start_y = 0.5 * (pts[a].y + pts[b].y);
        let abx = pts[b].x - pts[a].x;
        let aby = pts[b].y - pts[a].y;
        let perp_len = (abx * abx + aby * aby).sqrt();
        if perp_len < 1e-12 {
            // Degenerate back-edge: A and B coincide. Fall back to a
            // straight A → tip gradient.
            let gradient = crate::brush::Gradient::new_linear(pts[a], pts[t])
                .with_stops([colors[a], colors[t]]);
            return Brush::Gradient(gradient);
        }
        // Perpendicular to AB (90° CCW). Either sign is fine — we
        // resolve direction by signed projection of (tip - start)
        // onto it.
        let perp_x = -aby / perp_len;
        let perp_y = abx / perp_len;
        let dx = pts[t].x - start_x;
        let dy = pts[t].y - start_y;
        let d_signed = dx * perp_x + dy * perp_y;
        let end_x = start_x + perp_x * d_signed;
        let end_y = start_y + perp_y * d_signed;
        let gradient = crate::brush::Gradient::new_linear(
            Point::new(start_x, start_y),
            Point::new(end_x, end_y),
        )
        .with_stops([colors[a], colors[t]]);
        return Brush::Gradient(gradient);
    }

    // Three distinct colours: fall back to the max-distance pair.
    let d01 = color_distance_sq(&colors[0], &colors[1]);
    let d12 = color_distance_sq(&colors[1], &colors[2]);
    let d20 = color_distance_sq(&colors[2], &colors[0]);
    let (a_idx, b_idx) = if d01 >= d12 && d01 >= d20 {
        (0, 1)
    } else if d12 >= d20 {
        (1, 2)
    } else {
        (2, 0)
    };
    let gradient = crate::brush::Gradient::new_linear(pts[a_idx], pts[b_idx])
        .with_stops([colors[a_idx], colors[b_idx]]);
    Brush::Gradient(gradient)
}

/// Detect a fan of ≥ 2 triangles all sharing the first vertex and a
/// uniform colour: pattern `[A, B₀, B₁], [A, B₁, B₂], [A, B₂, B₃], …`.
/// All referenced vertices must have the same colour. Returns the
/// polygon boundary `[A, B₀, B₁, …, Bₖ]` (in cyclic order so the
/// polygon closes via `Bₖ → A`) along with the number of mesh-index
/// entries consumed.
///
/// Used to collapse round-cap and round-join fans into a single
/// closed-polygon fill so the internal "wedge" seams between
/// adjacent fan triangles disappear.
pub(crate) fn detect_uniform_fan(
    indices: &[u32],
    start: usize,
    colors: &[Color],
) -> Option<(Vec<u32>, usize)> {
    if start + 6 > indices.len() {
        return None;
    }
    let t0 = &indices[start..start + 3];
    let t1 = &indices[start + 3..start + 6];
    let a = t0[0];
    if t1[0] != a || t1[1] != t0[2] {
        return None;
    }
    let target = colors[a as usize];
    if !colors_eq(&colors[t0[1] as usize], &target)
        || !colors_eq(&colors[t0[2] as usize], &target)
        || !colors_eq(&colors[t1[2] as usize], &target)
    {
        return None;
    }
    let mut boundary = vec![a, t0[1], t0[2], t1[2]];
    let mut consumed = 6;
    while start + consumed + 3 <= indices.len() {
        let tk = &indices[start + consumed..start + consumed + 3];
        if tk[0] == a && tk[1] == *boundary.last().unwrap() {
            if !colors_eq(&colors[tk[2] as usize], &target) {
                break;
            }
            boundary.push(tk[2]);
            consumed += 3;
        } else {
            break;
        }
    }
    Some((boundary, consumed))
}

/// Build a closed polygon path from N vertices in cyclic order.
pub(crate) fn polygon_path(pts: &[Point]) -> Path {
    let mut p = Path::new();
    if pts.is_empty() {
        return p;
    }
    p.move_to(pts[0]);
    for v in &pts[1..] {
        p.line_to(*v);
    }
    p.close_path();
    p
}

/// Detect adjacent triangle pairs that form a quad shaped
/// `[A, B, C, A, C, D]` — the canonical ribbon-strip emission. Returns
/// the quad indices `[A, B, C, D]` in CCW cyclic order when matched.
pub(crate) fn detect_quad_pair(six: &[u32]) -> Option<[u32; 4]> {
    debug_assert_eq!(six.len(), 6);
    let a = six[0];
    let b = six[1];
    let c = six[2];
    let d2 = six[3];
    let e2 = six[4];
    let f2 = six[5];
    // Canonical ribbon emission: triangle 1 = (a, b, c), triangle 2 =
    // (a, c, d). Check shared vertices are a and c in that order.
    if d2 == a && e2 == c {
        Some([a, b, c, f2])
    } else {
        None
    }
}

/// Build a closed quad path from four vertices, in cyclic order.
pub(crate) fn quad_path(pts: &[Point; 4]) -> Path {
    let mut p = Path::new();
    p.move_to(pts[0]);
    p.line_to(pts[1]);
    p.line_to(pts[2]);
    p.line_to(pts[3]);
    p.close_path();
    p
}

/// Pick a brush for a quad's four vertex colours. The expected ribbon
/// pattern is `(ci, ci, cj, cj)` where vertices 0-1 share the start
/// colour and 2-3 share the end colour — the gradient axis then runs
/// from `midpoint(p0, p1)` to `midpoint(p2, p3)` (the segment
/// centerline) with stops `[ci, cj]`. For uniform colours the brush
/// collapses to solid. For other colour patterns, fall back to
/// emitting per-triangle (re-decomposes via the caller's loop) — but
/// since the caller already chose to merge, we use a reasonable
/// default of max-distance pair across all four vertices.
pub(crate) fn quad_gradient_brush(pts: &[Point; 4], colors: &[Color; 4]) -> Brush {
    let all_same = colors_eq(&colors[0], &colors[1])
        && colors_eq(&colors[1], &colors[2])
        && colors_eq(&colors[2], &colors[3]);
    if all_same {
        return Brush::Solid(colors[0]);
    }
    // Ribbon-canonical: indices 0-1 share colour ci, indices 2-3
    // share colour cj. Gradient axis = midpoint(p0,p1) → midpoint(p2,p3).
    let pair01 = colors_eq(&colors[0], &colors[1]);
    let pair23 = colors_eq(&colors[2], &colors[3]);
    if pair01 && pair23 {
        let start = Point::new(0.5 * (pts[0].x + pts[1].x), 0.5 * (pts[0].y + pts[1].y));
        let end = Point::new(0.5 * (pts[2].x + pts[3].x), 0.5 * (pts[2].y + pts[3].y));
        let gradient =
            crate::brush::Gradient::new_linear(start, end).with_stops([colors[0], colors[2]]);
        return Brush::Gradient(gradient);
    }
    // Other pairings (12, 30): rotate the gradient axis accordingly.
    let pair12 = colors_eq(&colors[1], &colors[2]);
    let pair30 = colors_eq(&colors[3], &colors[0]);
    if pair12 && pair30 {
        let start = Point::new(0.5 * (pts[1].x + pts[2].x), 0.5 * (pts[1].y + pts[2].y));
        let end = Point::new(0.5 * (pts[3].x + pts[0].x), 0.5 * (pts[3].y + pts[0].y));
        let gradient =
            crate::brush::Gradient::new_linear(start, end).with_stops([colors[1], colors[3]]);
        return Brush::Gradient(gradient);
    }
    // Fallback: pick the max-distance pair across the four vertices.
    let mut best = (0usize, 1usize, 0.0f32);
    for i in 0..4 {
        for j in (i + 1)..4 {
            let d = color_distance_sq(&colors[i], &colors[j]);
            if d > best.2 {
                best = (i, j, d);
            }
        }
    }
    let gradient = crate::brush::Gradient::new_linear(pts[best.0], pts[best.1])
        .with_stops([colors[best.0], colors[best.1]]);
    Brush::Gradient(gradient)
}

fn colors_eq(a: &Color, b: &Color) -> bool {
    a.components == b.components
}

fn color_distance_sq(a: &Color, b: &Color) -> f32 {
    let [ar, ag, ab, _] = a.components;
    let [br, bg, bb, _] = b.components;
    let dr = ar - br;
    let dg = ag - bg;
    let db = ab - bb;
    dr * dr + dg * dg + db * db
}

/// Emit `mesh` into `sink` as a sequence of fills.
///
/// Two patterns are merged before falling back to one fill per triangle, so
/// that adjacent triangles do not leave a seam along their shared edge:
///
/// - a **uniform fan** — a run of triangles sharing a hub vertex and one
///   colour (round caps and joins) — becomes a single polygon over its rim;
/// - a **quad pair** — the `[A, B, C, A, C, D]` a ribbon segment emits —
///   becomes one 4-vertex polygon.
///
/// Every fill carries the mesh's `pick_id`, so whatever `sink` does about
/// picking happens once per emitted fill rather than being duplicated here.
pub(crate) fn decompose(
    mesh: &Mesh,
    transform: Affine,
    pick_id: PickId,
    sink: &mut dyn SceneBuilder,
) {
    let indices = &mesh.indices;
    let mut i = 0;
    while i + 3 <= indices.len() {
        // A fan of two or more triangles sharing a hub vertex and a colour.
        if let Some((boundary, advance)) = detect_uniform_fan(indices, i, &mesh.colors) {
            let pts: Vec<Point> = boundary
                .iter()
                .map(|&idx| mesh.vertices[idx as usize])
                .collect();
            let path = polygon_path(&pts);
            let brush = Brush::Solid(mesh.colors[boundary[0] as usize]);
            sink.fill(FillRule::NonZero, transform, &brush, None, &path, pick_id);
            i += advance;
            continue;
        }
        let merged = if i + 6 <= indices.len() {
            detect_quad_pair(&indices[i..i + 6])
        } else {
            None
        };
        if let Some([a, b, c, d]) = merged {
            let pts = [
                mesh.vertices[a as usize],
                mesh.vertices[b as usize],
                mesh.vertices[c as usize],
                mesh.vertices[d as usize],
            ];
            let colors = [
                mesh.colors[a as usize],
                mesh.colors[b as usize],
                mesh.colors[c as usize],
                mesh.colors[d as usize],
            ];
            let path = quad_path(&pts);
            let brush = quad_gradient_brush(&pts, &colors);
            sink.fill(FillRule::NonZero, transform, &brush, None, &path, pick_id);
            i += 6;
        } else {
            let pts = [
                mesh.vertices[indices[i] as usize],
                mesh.vertices[indices[i + 1] as usize],
                mesh.vertices[indices[i + 2] as usize],
            ];
            let colors = [
                mesh.colors[indices[i] as usize],
                mesh.colors[indices[i + 1] as usize],
                mesh.colors[indices[i + 2] as usize],
            ];
            let path = triangle_path(&pts);
            let brush = triangle_gradient_brush(&pts, &colors);
            sink.fill(FillRule::NonZero, transform, &brush, None, &path, pick_id);
            i += 3;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{Gradient, GradientKind};

    const RED: Color = Color::new([1.0, 0.0, 0.0, 1.0]);
    const GREEN: Color = Color::new([0.0, 1.0, 0.0, 1.0]);
    const BLUE: Color = Color::new([0.0, 0.0, 1.0, 1.0]);
    /// Barely distinguishable from [`RED`] — near enough that any other
    /// pair in a triangle wins the max-distance contest.
    const NEAR_RED: Color = Color::new([0.99, 0.0, 0.0, 1.0]);

    /// The gradient axis and stop colours of a gradient brush.
    #[track_caller]
    fn linear_parts(brush: &Brush) -> (Point, Point, Vec<Color>) {
        match brush {
            Brush::Gradient(Gradient {
                kind: GradientKind::Linear(pos),
                stops,
                ..
            }) => (
                pos.start,
                pos.end,
                stops
                    .iter()
                    .map(|s| s.color.to_alpha_color::<peniko::color::Srgb>())
                    .collect(),
            ),
            other => panic!("expected a linear gradient brush, got {other:?}"),
        }
    }

    #[track_caller]
    fn assert_point(got: Point, want: (f64, f64)) {
        assert!(
            (got.x - want.0).abs() < 1e-9 && (got.y - want.1).abs() < 1e-9,
            "got {got:?}, want {want:?}"
        );
    }

    #[track_caller]
    fn assert_stops(got: &[Color], want: [Color; 2]) {
        assert_eq!(got.len(), want.len(), "got {got:?}, want {want:?}");
        for (g, w) in got.iter().zip(want.iter()) {
            let close = g
                .components
                .iter()
                .zip(w.components.iter())
                .all(|(a, b)| (a - b).abs() < 1e-6);
            assert!(close, "got {got:?}, want {want:?}");
        }
    }

    // ─── triangle_gradient_brush ────────────────────────────────────────

    #[test]
    fn a_uniformly_coloured_triangle_gets_a_solid_brush() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(2.0, 0.0),
            Point::new(1.0, 3.0),
        ];
        let brush = triangle_gradient_brush(&pts, &[RED, RED, RED]);
        assert_eq!(brush, Brush::Solid(RED));
    }

    #[test]
    fn a_shared_edge_puts_both_matching_vertices_at_gradient_zero() {
        // Vertices 0 and 1 share a colour, so the axis runs from the
        // midpoint of that back-edge to the tip, perpendicular to the
        // edge — both shoulders sit at fraction 0.
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(2.0, 0.0),
            Point::new(1.0, 3.0),
        ];
        let (start, end, stops) = linear_parts(&triangle_gradient_brush(&pts, &[RED, RED, BLUE]));
        assert_point(start, (1.0, 0.0));
        assert_point(end, (1.0, 3.0));
        assert_stops(&stops, [RED, BLUE]);
    }

    #[test]
    fn the_axis_drops_a_perpendicular_from_an_off_centre_tip() {
        // The tip is skewed along the back-edge; the axis end is its
        // perpendicular foot, not the tip itself, so the two shoulders
        // stay equidistant from the start.
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(4.0, 0.0),
            Point::new(3.5, 2.0),
        ];
        let (start, end, _) = linear_parts(&triangle_gradient_brush(&pts, &[RED, RED, BLUE]));
        assert_point(start, (2.0, 0.0));
        assert_point(end, (2.0, 2.0));
    }

    #[test]
    fn each_matching_pair_selects_its_own_opposing_tip() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(2.0, 0.0),
            Point::new(0.0, 2.0),
        ];
        // Vertices 1 and 2 match — vertex 0 is the tip, so the axis
        // starts on the midpoint of the edge joining 1 and 2.
        let (start, end, stops) = linear_parts(&triangle_gradient_brush(&pts, &[BLUE, RED, RED]));
        assert_point(start, (1.0, 1.0));
        assert_point(end, (0.0, 0.0));
        assert_stops(&stops, [RED, BLUE]);
        // Vertices 2 and 0 match — vertex 1 is the tip.
        let (start, end, stops) = linear_parts(&triangle_gradient_brush(&pts, &[RED, BLUE, RED]));
        assert_point(start, (0.0, 1.0));
        assert_point(end, (2.0, 1.0));
        assert_stops(&stops, [RED, BLUE]);
    }

    #[test]
    fn a_degenerate_back_edge_falls_back_to_a_straight_axis() {
        // The two shared-colour vertices coincide, so there's no edge to
        // run perpendicular to — the axis runs from the collapsed pair
        // straight to the tip.
        let pts = [
            Point::new(1.0, 1.0),
            Point::new(1.0, 1.0),
            Point::new(4.0, 5.0),
        ];
        let (start, end, stops) = linear_parts(&triangle_gradient_brush(&pts, &[RED, RED, BLUE]));
        assert_point(start, (1.0, 1.0));
        assert_point(end, (4.0, 5.0));
        assert_stops(&stops, [RED, BLUE]);
    }

    #[test]
    fn three_distinct_colours_span_the_furthest_apart_pair() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(0.0, 1.0),
        ];
        // Vertices 1 and 2 are the widest-apart pair here …
        let (start, end, stops) =
            linear_parts(&triangle_gradient_brush(&pts, &[NEAR_RED, RED, BLUE]));
        assert_point(start, (1.0, 0.0));
        assert_point(end, (0.0, 1.0));
        assert_stops(&stops, [RED, BLUE]);

        // … and rotating the colours rotates the picked axis with them.
        let (start, end, stops) =
            linear_parts(&triangle_gradient_brush(&pts, &[RED, NEAR_RED, BLUE]));
        assert_point(start, (0.0, 1.0));
        assert_point(end, (0.0, 0.0));
        assert_stops(&stops, [BLUE, RED]);

        let (start, end, stops) =
            linear_parts(&triangle_gradient_brush(&pts, &[RED, BLUE, NEAR_RED]));
        assert_point(start, (0.0, 0.0));
        assert_point(end, (1.0, 0.0));
        assert_stops(&stops, [RED, BLUE]);
    }

    #[test]
    fn colours_differing_only_in_alpha_tie_and_settle_on_the_first_pair() {
        // Three colours that differ only in alpha are all at distance 0,
        // so the fallback settles on the first pair.
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(0.0, 1.0),
        ];
        let a = Color::new([1.0, 0.0, 0.0, 1.0]);
        let b = Color::new([1.0, 0.0, 0.0, 0.5]);
        let c = Color::new([1.0, 0.0, 0.0, 0.25]);
        let (start, end, _) = linear_parts(&triangle_gradient_brush(&pts, &[a, b, c]));
        assert_point(start, (0.0, 0.0));
        assert_point(end, (1.0, 0.0));
    }

    // ─── detect_uniform_fan ─────────────────────────────────────────────

    #[test]
    fn a_fan_needs_at_least_two_triangles() {
        let colors = vec![RED; 4];
        assert!(detect_uniform_fan(&[0, 1, 2], 0, &colors).is_none());
        // Only three indices remain past the start offset.
        assert!(detect_uniform_fan(&[0, 1, 2, 0, 2, 3], 3, &colors).is_none());
    }

    #[test]
    fn two_fan_triangles_collapse_to_a_four_vertex_boundary() {
        let colors = vec![RED; 4];
        let (boundary, consumed) = detect_uniform_fan(&[0, 1, 2, 0, 2, 3], 0, &colors).unwrap();
        assert_eq!(boundary, vec![0, 1, 2, 3]);
        assert_eq!(consumed, 6);
    }

    #[test]
    fn a_fan_keeps_absorbing_triangles_that_continue_it() {
        let colors = vec![RED; 5];
        let indices = [0, 1, 2, 0, 2, 3, 0, 3, 4];
        let (boundary, consumed) = detect_uniform_fan(&indices, 0, &colors).unwrap();
        assert_eq!(boundary, vec![0, 1, 2, 3, 4]);
        assert_eq!(consumed, 9);
    }

    #[test]
    fn a_fan_stops_where_the_hub_or_the_seam_vertex_changes() {
        let colors = vec![RED; 6];
        // Third triangle hangs off a different hub.
        let other_hub = [0, 1, 2, 0, 2, 3, 5, 3, 4];
        let (boundary, consumed) = detect_uniform_fan(&other_hub, 0, &colors).unwrap();
        assert_eq!(boundary, vec![0, 1, 2, 3]);
        assert_eq!(consumed, 6);
        // Third triangle doesn't start from the previous seam vertex.
        let broken_seam = [0, 1, 2, 0, 2, 3, 0, 1, 4];
        let (_, consumed) = detect_uniform_fan(&broken_seam, 0, &colors).unwrap();
        assert_eq!(consumed, 6);
    }

    #[test]
    fn a_second_triangle_that_does_not_continue_the_fan_is_not_a_fan() {
        let colors = vec![RED; 4];
        // Shares the hub but not the seam vertex.
        assert!(detect_uniform_fan(&[0, 1, 2, 0, 3, 1], 0, &colors).is_none());
        // Shares the seam vertex but not the hub.
        assert!(detect_uniform_fan(&[0, 1, 2, 3, 2, 1], 0, &colors).is_none());
    }

    #[test]
    fn a_fan_requires_one_colour_across_every_vertex_it_touches() {
        // Any of the four vertices of the first two triangles differing
        // disqualifies the fan outright.
        for odd in 0..4 {
            let mut colors = vec![RED; 4];
            colors[odd] = BLUE;
            assert!(
                detect_uniform_fan(&[0, 1, 2, 0, 2, 3], 0, &colors).is_none(),
                "vertex {odd} differing should disqualify the fan"
            );
        }
    }

    #[test]
    fn a_fan_stops_before_a_triangle_that_breaks_the_colour() {
        let mut colors = vec![RED; 5];
        colors[4] = GREEN;
        let indices = [0, 1, 2, 0, 2, 3, 0, 3, 4];
        let (boundary, consumed) = detect_uniform_fan(&indices, 0, &colors).unwrap();
        assert_eq!(boundary, vec![0, 1, 2, 3]);
        assert_eq!(consumed, 6);
    }

    #[test]
    fn a_fan_can_start_partway_into_the_index_list() {
        let colors = vec![RED; 6];
        let indices = [5, 5, 5, 1, 2, 3, 1, 3, 4];
        let (boundary, consumed) = detect_uniform_fan(&indices, 3, &colors).unwrap();
        assert_eq!(boundary, vec![1, 2, 3, 4]);
        assert_eq!(consumed, 6);
    }

    // ─── detect_quad_pair ───────────────────────────────────────────────

    #[test]
    fn the_canonical_ribbon_pair_reads_as_a_quad() {
        assert_eq!(detect_quad_pair(&[0, 1, 2, 0, 2, 3]), Some([0, 1, 2, 3]));
        assert_eq!(detect_quad_pair(&[7, 4, 9, 7, 9, 2]), Some([7, 4, 9, 2]));
    }

    #[test]
    fn triangle_pairs_outside_the_canonical_order_are_not_quads() {
        // Second triangle starts from the wrong shared vertex.
        assert_eq!(detect_quad_pair(&[0, 1, 2, 2, 0, 3]), None);
        // Second triangle reuses the hub but not the seam vertex.
        assert_eq!(detect_quad_pair(&[0, 1, 2, 0, 1, 3]), None);
        // Two triangles that share no edge at all.
        assert_eq!(detect_quad_pair(&[0, 1, 2, 3, 4, 5]), None);
    }

    // ─── quad_gradient_brush ────────────────────────────────────────────

    #[test]
    fn a_uniformly_coloured_quad_gets_a_solid_brush() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(0.0, 2.0),
            Point::new(4.0, 2.0),
            Point::new(4.0, 0.0),
        ];
        assert_eq!(quad_gradient_brush(&pts, &[GREEN; 4]), Brush::Solid(GREEN));
    }

    #[test]
    fn a_ribbon_quad_runs_its_gradient_down_the_segment_centerline() {
        // Vertices 0-1 are the start edge, 2-3 the end edge.
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(0.0, 2.0),
            Point::new(4.0, 2.0),
            Point::new(4.0, 0.0),
        ];
        let (start, end, stops) = linear_parts(&quad_gradient_brush(&pts, &[RED, RED, BLUE, BLUE]));
        assert_point(start, (0.0, 1.0));
        assert_point(end, (4.0, 1.0));
        assert_stops(&stops, [RED, BLUE]);
    }

    #[test]
    fn a_quad_paired_across_its_other_edges_rotates_the_axis() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(0.0, 2.0),
            Point::new(4.0, 2.0),
            Point::new(4.0, 0.0),
        ];
        let (start, end, stops) = linear_parts(&quad_gradient_brush(&pts, &[RED, BLUE, BLUE, RED]));
        assert_point(start, (2.0, 2.0));
        assert_point(end, (2.0, 0.0));
        assert_stops(&stops, [BLUE, RED]);
    }

    #[test]
    fn an_unpaired_quad_spans_the_furthest_apart_vertex_pair() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(0.0, 2.0),
            Point::new(4.0, 2.0),
            Point::new(4.0, 0.0),
        ];
        // Vertices 1 and 2 share a colour, but their opposite edge
        // (3, 0) doesn't — so neither edge pairing applies and the
        // widest pair, vertices 0 and 3, carries the axis.
        let (start, end, stops) =
            linear_parts(&quad_gradient_brush(&pts, &[RED, NEAR_RED, NEAR_RED, BLUE]));
        assert_point(start, (0.0, 0.0));
        assert_point(end, (4.0, 0.0));
        assert_stops(&stops, [RED, BLUE]);
    }

    #[test]
    fn a_half_matched_quad_does_not_take_the_ribbon_path() {
        // Vertices 0-1 match but 2-3 don't, so the canonical ribbon
        // pairing is out and the fallback picks the widest pair.
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(0.0, 2.0),
            Point::new(4.0, 2.0),
            Point::new(4.0, 0.0),
        ];
        let (start, end, stops) = linear_parts(&quad_gradient_brush(&pts, &[RED, RED, RED, BLUE]));
        assert_point(start, (0.0, 0.0));
        assert_point(end, (4.0, 0.0));
        assert_stops(&stops, [RED, BLUE]);
    }
}
