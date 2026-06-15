//! Polyline- and polygon-as-ribbon tessellation.
//!
//! A "ribbon" is a stroked open polyline or closed polygon expressed
//! as a triangle [`Mesh`] with per-vertex colour and per-vertex
//! half-width. Drawing happens via
//! [`SceneBuilder::draw_mesh`](crate::scene::SceneBuilder); the Vello
//! backend decomposes the mesh into per-triangle linear-gradient
//! fills, which gives perfect Gouraud-equivalent colour blending
//! along ribbon strips (because the two shoulders at each polyline
//! vertex carry the same colour, so the gradient axis runs cleanly
//! between adjacent segments).
//!
//! # Entry points
//!
//! Open polylines (with end caps):
//! - [`polyline_ribbon`] — constant colour, constant half-width.
//! - [`polyline_gradient`] — per-vertex colour, constant half-width.
//! - [`polyline_ribbon_full`] — per-vertex colour and per-vertex
//!   half-width.
//!
//! Closed polygons (no caps; the loop closes from `points[n - 1]`
//! back to `points[0]` — do not repeat the first vertex):
//! - [`polygon_ribbon`] — constant colour, constant half-width.
//! - [`polygon_gradient`] — per-vertex colour, constant half-width.
//! - [`polygon_ribbon_full`] — per-vertex colour and per-vertex
//!   half-width.
//!
//! Caps: butt / square / round (open only — [`RibbonOptions::cap`]
//! is ignored by the `polygon_*` entry points). Joins: miter (with
//! auto-bevel fallback when the miter exceeds
//! [`RibbonOptions::miter_limit`]), bevel, round.
//!
//! All distances are in **panel pixels**. Callers convert from pt at
//! their own draw sites (`px = pt * dpi / 72.0`).

use kurbo::Vec2;

use crate::color::Color;
use crate::geometry::Point;
use crate::mesh::Mesh;
use crate::stroke::{Cap, Join};

const EPSILON: f64 = 1e-9;
/// Approximation tolerance for round-cap / round-join arcs, in panel
/// pixels. Sub-pixel deviation from the true arc — visually
/// indistinguishable at any reasonable zoom.
const ROUND_TOLERANCE: f64 = 0.5;

/// Maximum angular step per fan triangle, in radians. The chord-error
/// formula `ε = R(1 − cos(Δθ/2))` keeps the *positional* deviation
/// sub-pixel, but at small `R` (typical line half-widths of 1–3 px)
/// it picks angular steps of 60–90°, which read as visible corners
/// even though the chord error itself is sub-pixel. Capping the step
/// at 15° (= π/12 rad) ensures at least 12 segments per semicircle
/// at any radius, eliminating the perceptible faceting on round
/// caps and joins of thin lines.
const ROUND_MAX_ANGULAR_STEP: f64 = std::f64::consts::PI / 12.0;
/// Per-segment seam-bleed in panel pixels. Each interior quad is
/// extended this far past its natural endpoint at both ends along the
/// local segment tangent, so adjacent quads overlap and SrcOver
/// compositing on the overlap region renders fully opaque — hiding
/// the AA seam that would otherwise appear at segment boundaries.
/// `0.75 px` is enough to cover a 1-px AA edge on each side while
/// keeping the gradient-axis distortion below 1.5% for typical
/// segment lengths.
const SEAM_BLEED_PX: f64 = 0.75;

// ── Options ────────────────────────────────────────────────────────────────

/// Tessellation options for [`polyline_ribbon`] / [`polyline_gradient`]
/// / [`polyline_ribbon_full`]. Caps and joins reuse [`crate::stroke::Cap`]
/// and [`crate::stroke::Join`] — same three variants each.
#[derive(Clone, Copy, Debug)]
pub struct RibbonOptions {
    /// Half-width in panel pixels. Used by entry points that don't
    /// take a per-vertex half-width slice; ignored by
    /// [`polyline_ribbon_full`] when `half_widths` is `Some`.
    pub half_width: f64,
    /// End-cap style for open ribbons. Ignored by `polygon_*` entry
    /// points (closed loops have no endpoints to cap).
    pub cap: Cap,
    /// Corner-join style at each interior vertex.
    pub join: Join,
    /// Maximum ratio `1 / cos(turn_angle / 2)` allowed at a mitre
    /// join. Joins exceeding this fall back to bevel for that join
    /// only. Matches the SVG default of `4.0`.
    pub miter_limit: f64,
}

impl Default for RibbonOptions {
    fn default() -> Self {
        Self {
            half_width: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
            miter_limit: 4.0,
        }
    }
}

// ── Public entry points ─────────────────────────────────────────────────────

/// Constant-colour, constant-width ribbon. Equivalent to a uniformly
/// stroked polyline expressed as a mesh.
pub fn polyline_ribbon(points: &[Point], color: Color, opts: &RibbonOptions) -> Mesh {
    ribbon_inner(points, ColorSource::Constant(color), None, opts, false)
}

/// Per-vertex coloured, constant-width ribbon. `colors.len()` must
/// equal `points.len()`.
pub fn polyline_gradient(points: &[Point], colors: &[Color], opts: &RibbonOptions) -> Mesh {
    assert_eq!(
        points.len(),
        colors.len(),
        "polyline_gradient: points.len() ({}) != colors.len() ({})",
        points.len(),
        colors.len(),
    );
    ribbon_inner(points, ColorSource::PerVertex(colors), None, opts, false)
}

/// Full ribbon: optionally per-vertex coloured, optionally per-vertex
/// half-width. Either slice may be `None`; the defaults are taken
/// from `opts`.
pub fn polyline_ribbon_full(
    points: &[Point],
    colors: Option<&[Color]>,
    half_widths: Option<&[f64]>,
    opts: &RibbonOptions,
) -> Mesh {
    if let Some(c) = colors {
        assert_eq!(
            points.len(),
            c.len(),
            "polyline_ribbon_full: colors.len() must match points.len()"
        );
    }
    if let Some(w) = half_widths {
        assert_eq!(
            points.len(),
            w.len(),
            "polyline_ribbon_full: half_widths.len() must match points.len()"
        );
    }
    let color_source = match colors {
        Some(c) => ColorSource::PerVertex(c),
        None => ColorSource::Constant(Color::new([0.0, 0.0, 0.0, 1.0])),
    };
    ribbon_inner(points, color_source, half_widths, opts, false)
}

/// Constant-colour, constant-width closed-polygon ribbon. The loop
/// closes from `points[n - 1]` back to `points[0]` — do **not**
/// repeat the first vertex. [`RibbonOptions::cap`] is ignored (a
/// closed loop has no endpoints to cap). Returns an empty mesh when
/// `points.len() < 3`.
pub fn polygon_ribbon(points: &[Point], color: Color, opts: &RibbonOptions) -> Mesh {
    ribbon_inner(points, ColorSource::Constant(color), None, opts, true)
}

/// Per-vertex coloured, constant-width closed-polygon ribbon.
/// `colors.len()` must equal `points.len()`. The wrap segment
/// interpolates `colors[n - 1] → colors[0]` like any other segment,
/// so the gradient closes seamlessly. See [`polygon_ribbon`] for the
/// closure convention.
pub fn polygon_gradient(points: &[Point], colors: &[Color], opts: &RibbonOptions) -> Mesh {
    assert_eq!(
        points.len(),
        colors.len(),
        "polygon_gradient: points.len() ({}) != colors.len() ({})",
        points.len(),
        colors.len(),
    );
    ribbon_inner(points, ColorSource::PerVertex(colors), None, opts, true)
}

/// Full closed-polygon ribbon: optionally per-vertex coloured,
/// optionally per-vertex half-width. Either slice may be `None`; the
/// defaults are taken from `opts`. See [`polygon_ribbon`] for the
/// closure convention.
pub fn polygon_ribbon_full(
    points: &[Point],
    colors: Option<&[Color]>,
    half_widths: Option<&[f64]>,
    opts: &RibbonOptions,
) -> Mesh {
    if let Some(c) = colors {
        assert_eq!(
            points.len(),
            c.len(),
            "polygon_ribbon_full: colors.len() must match points.len()"
        );
    }
    if let Some(w) = half_widths {
        assert_eq!(
            points.len(),
            w.len(),
            "polygon_ribbon_full: half_widths.len() must match points.len()"
        );
    }
    let color_source = match colors {
        Some(c) => ColorSource::PerVertex(c),
        None => ColorSource::Constant(Color::new([0.0, 0.0, 0.0, 1.0])),
    };
    ribbon_inner(points, color_source, half_widths, opts, true)
}

// ── Inner machinery ─────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum ColorSource<'a> {
    Constant(Color),
    PerVertex(&'a [Color]),
}

impl ColorSource<'_> {
    fn at(&self, i: usize) -> Color {
        match self {
            ColorSource::Constant(c) => *c,
            ColorSource::PerVertex(slice) => slice[i],
        }
    }
}

/// Per-vertex layout info computed in pass 1.
struct VertexLayout {
    /// Inbound shoulder pair `(left, right)` — at the end of the
    /// previous segment. Equal to `out` for mitre / endpoint cases.
    in_left: Point,
    in_right: Point,
    /// Outbound shoulder pair `(left, right)` — at the start of the
    /// next segment.
    out_left: Point,
    out_right: Point,
    /// `true` when the vertex is a bevel join (in_pair ≠ out_pair). A
    /// bevel-fill triangle is emitted on the outside of the turn.
    is_bevel: bool,
    /// Which side bulges out at this bevel join — `true` for "left",
    /// `false` for "right". Ignored when `is_bevel` is false.
    bevel_outside_left: bool,
}

fn ribbon_inner(
    points: &[Point],
    colors: ColorSource<'_>,
    half_widths: Option<&[f64]>,
    opts: &RibbonOptions,
    closed: bool,
) -> Mesh {
    let n = points.len();
    // Open polylines need >= 2 points; closed polygons need >= 3
    // (anything less is degenerate).
    let min_pts = if closed { 3 } else { 2 };
    if n < min_pts {
        return Mesh::new(Vec::new(), Vec::new(), Vec::new());
    }

    // Per-vertex half-widths (panel-px). Falls back to opts.half_width.
    let hw = |i: usize| -> f64 {
        match half_widths {
            Some(w) => w[i],
            None => opts.half_width,
        }
    };

    // Compute unit segment tangents. `seg_tangent[i]` is the tangent
    // of segment `points[i] → points[(i + 1) % n]`. Open polylines
    // have n-1 segments; closed polygons have n (the last one wraps
    // back to vertex 0).
    let n_segs = if closed { n } else { n - 1 };
    let mut seg_tangent: Vec<Vec2> = Vec::with_capacity(n_segs);
    for i in 0..n_segs {
        let delta = points[(i + 1) % n] - points[i];
        let len = delta.hypot();
        if len <= EPSILON {
            // Degenerate segment — re-use last tangent if any,
            // otherwise +x. The resulting ribbon will still be valid;
            // a duplicated polyline vertex just creates a zero-area
            // quad.
            let last = seg_tangent.last().copied().unwrap_or(Vec2::new(1.0, 0.0));
            seg_tangent.push(last);
        } else {
            seg_tangent.push(delta / len);
        }
    }

    // Compute per-vertex layout.
    let mut layouts: Vec<VertexLayout> = Vec::with_capacity(n);
    for i in 0..n {
        // For closed loops vertex 0's inbound segment is the wrap
        // (seg_tangent[n - 1]), and vertex n-1's outbound is the same
        // wrap. For open polylines the endpoint branch below picks
        // whichever tangent it actually needs, so the dummy values
        // here are unused.
        let t_in = if i == 0 {
            if closed {
                seg_tangent[n - 1]
            } else {
                seg_tangent[0]
            }
        } else {
            seg_tangent[i - 1]
        };
        let t_out = if i + 1 == n {
            if closed {
                seg_tangent[n - 1]
            } else {
                seg_tangent[n - 2]
            }
        } else {
            seg_tangent[i]
        };
        let pi = points[i];
        let w = hw(i);

        if !closed && (i == 0 || i + 1 == n) {
            // Endpoint: single perpendicular offset.
            let t = if i == 0 { t_out } else { t_in };
            let n_left = perp_left(t);
            let l = pi + n_left * w;
            let r = pi - n_left * w;
            layouts.push(VertexLayout {
                in_left: l,
                in_right: r,
                out_left: l,
                out_right: r,
                is_bevel: false,
                bevel_outside_left: false,
            });
            continue;
        }

        // Interior vertex.
        let perp_in = perp_left(t_in);
        let perp_out = perp_left(t_out);
        // Determine outside direction: a left turn (cross > 0) bulges
        // on the right side; right turn bulges on the left.
        let cross = t_in.x * t_out.y - t_in.y * t_out.x;
        let dot = t_in.x * t_out.x + t_in.y * t_out.y;
        let bevel_outside_left = cross < 0.0;

        // Try miter: shoulder pair at the bisector position.
        let denom = 1.0 + dot;
        let miter_mag = if denom > EPSILON {
            // 1 / cos(α/2) where α is the turn angle. Equivalent to
            // `(perp_in + perp_out) / denom`'s magnitude divided by 1
            // (the unit perpendicular length). Cheaper to compute via
            // the half-angle identity.
            (2.0 / denom).sqrt()
        } else {
            f64::INFINITY
        };

        let want_miter = match opts.join {
            Join::Miter => miter_mag <= opts.miter_limit && denom > EPSILON,
            // Round and bevel both emit two shoulder pairs; round
            // additionally fills the outside notch with a fan, bevel
            // fills it with one triangle.
            _ => false,
        };

        if want_miter {
            let mitre = (perp_in + perp_out) * (w / denom);
            let l = pi + mitre;
            let r = pi - mitre;
            layouts.push(VertexLayout {
                in_left: l,
                in_right: r,
                out_left: l,
                out_right: r,
                is_bevel: false,
                bevel_outside_left,
            });
        } else {
            // Bevel (or round, handled as bevel + fan in the emit
            // step). Two shoulder pairs perpendicular to each segment.
            let in_l = pi + perp_in * w;
            let in_r = pi - perp_in * w;
            let out_l = pi + perp_out * w;
            let out_r = pi - perp_out * w;
            layouts.push(VertexLayout {
                in_left: in_l,
                in_right: in_r,
                out_left: out_l,
                out_right: out_r,
                is_bevel: true,
                bevel_outside_left,
            });
        }
    }

    // Build the mesh. Output buffers.
    let mut vertices: Vec<Point> = Vec::new();
    let mut vcolors: Vec<Color> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Helper: push a single vertex with colour, return its index.
    let push_vertex =
        |vertices: &mut Vec<Point>, vcolors: &mut Vec<Color>, p: Point, c: Color| -> u32 {
            let idx = vertices.len() as u32;
            vertices.push(p);
            vcolors.push(c);
            idx
        };

    // Emission order: path-order. For a self-intersecting polyline,
    // later geometry draws on top of earlier geometry under SrcOver —
    // so emitting "start cap → segments → joins → end cap" in path
    // order ensures the path's tail correctly occludes its head when
    // they cross. The previous ordering (caps last) caused the start
    // cap to draw OVER segments that happened to pass through it.
    //
    // **Seam-bleed**: each interior segment quad is extended by
    // `SEAM_BLEED_PX` along its local tangent at both ends, so
    // adjacent quads overlap by ~2 × bleed in their shared boundary
    // region. SrcOver compositing on the overlap renders fully
    // opaque, eliminating the AA seam between adjacent fills. The
    // gradient stops are computed against the original (unbled)
    // axis, so the bleed introduces a tiny ε/L colour shift at the
    // original endpoints — invisible for typical segment lengths.
    // Endpoint edges (segment 0's near / last segment's far) are NOT
    // bled, so cap geometry attaches at the natural shoulder
    // positions.

    // 1. Start cap (open polylines only — closed polygons have no
    //    endpoints to cap).
    if !closed {
        emit_cap(
            &mut vertices,
            &mut vcolors,
            &mut indices,
            points[0],
            layouts[0].out_left,
            layouts[0].out_right,
            -seg_tangent[0],
            colors.at(0),
            opts.cap,
            hw(0),
        );
    }

    // 2. Per-segment quads, interleaved with joins at the segment's
    //    *end* vertex (the start vertex of the next segment).
    //
    // Endpoint-edge bleed: for caps with geometry (square / round),
    // bleed the segment's endpoint edge **into the cap region** so
    // the segment overlaps the cap's interior — eliminating the AA
    // seam between the segment quad and the cap polygon. For butt
    // caps there's no cap geometry, so the bleed would just extend
    // the line by ε past its nominal endpoint — skip it. For closed
    // polygons every segment is interior, so the cap-bleed path is
    // unused.
    let cap_bleed_amount = match opts.cap {
        Cap::Butt => 0.0,
        Cap::Square | Cap::Round => SEAM_BLEED_PX,
    };
    for i in 0..n_segs {
        let i_next = (i + 1) % n;
        let ci = colors.at(i);
        let cj = colors.at(i_next);
        let t = seg_tangent[i];
        // For closed loops every boundary is interior; for open
        // polylines the segment's near boundary is the start cap when
        // i == 0, and the far boundary is the end cap when i == n-2.
        let near_bleed_amount = if closed || i > 0 {
            SEAM_BLEED_PX
        } else {
            cap_bleed_amount
        };
        let far_bleed_amount = if closed || i + 1 < n - 1 {
            SEAM_BLEED_PX
        } else {
            cap_bleed_amount
        };
        let near_bleed = t * near_bleed_amount;
        let far_bleed = t * far_bleed_amount;
        let a_pos = layouts[i].out_left - near_bleed;
        let b_pos = layouts[i].out_right - near_bleed;
        let c_pos = layouts[i_next].in_right + far_bleed;
        let d_pos = layouts[i_next].in_left + far_bleed;
        let a = push_vertex(&mut vertices, &mut vcolors, a_pos, ci);
        let b = push_vertex(&mut vertices, &mut vcolors, b_pos, ci);
        let c = push_vertex(&mut vertices, &mut vcolors, c_pos, cj);
        let d = push_vertex(&mut vertices, &mut vcolors, d_pos, cj);
        indices.extend_from_slice(&[a, b, c, a, c, d]);

        // Join at vertex i_next, if it's a bevel/round.
        // Open: only interior vertices (vertex 0 and n-1 are caps).
        // Closed: every vertex is interior, including the wrap-back
        // to vertex 0 emitted by the last segment.
        let is_interior_join = closed || i_next < n - 1;
        if is_interior_join && layouts[i_next].is_bevel {
            emit_join_fill(
                &mut vertices,
                &mut vcolors,
                &mut indices,
                points[i_next],
                &layouts[i_next],
                colors.at(i_next),
                opts.join,
            );
        }
    }

    // 3. End cap (open polylines only).
    if !closed {
        let last = n - 1;
        emit_cap(
            &mut vertices,
            &mut vcolors,
            &mut indices,
            points[last],
            layouts[last].in_right,
            layouts[last].in_left,
            seg_tangent[n - 2],
            colors.at(last),
            opts.cap,
            hw(last),
        );
    }

    Mesh::new(vertices, vcolors, indices)
}

/// Emit the bevel / round fill triangle(s) at a single interior
/// vertex. For mitre joins that didn't fall back to bevel, `is_bevel`
/// is false and the caller skips this entirely.
fn emit_join_fill(
    vertices: &mut Vec<Point>,
    vcolors: &mut Vec<Color>,
    indices: &mut Vec<u32>,
    pi: Point,
    layout: &VertexLayout,
    color: Color,
    join: Join,
) {
    let (outside_in, outside_out) = if layout.bevel_outside_left {
        (layout.in_left, layout.out_left)
    } else {
        (layout.in_right, layout.out_right)
    };
    match join {
        Join::Bevel | Join::Miter => {
            let i_p = vertices.len() as u32;
            vertices.push(pi);
            vcolors.push(color);
            let i_oi = vertices.len() as u32;
            vertices.push(outside_in);
            vcolors.push(color);
            let i_oo = vertices.len() as u32;
            vertices.push(outside_out);
            vcolors.push(color);
            indices.extend_from_slice(&[i_p, i_oi, i_oo]);
        }
        Join::Round => {
            emit_round_fan(
                vertices,
                vcolors,
                indices,
                pi,
                outside_in,
                outside_out,
                color,
            );
        }
    }
}

/// Emit cap geometry at one endpoint. `outward` is the unit vector
/// pointing away from the polyline at this endpoint (start cap:
/// `-tangent_of_first_segment`; end cap: `+tangent_of_last_segment`).
/// `(a, b)` are the two shoulder vertices already placed at the
/// endpoint, ordered so that a→b crosses outward to the right of the
/// outward direction (i.e., `a = left_relative_to_outward,
/// b = right_relative_to_outward`).
#[allow(clippy::too_many_arguments, clippy::ptr_arg)]
fn emit_cap(
    vertices: &mut Vec<Point>,
    vcolors: &mut Vec<Color>,
    indices: &mut Vec<u32>,
    endpoint: Point,
    a: Point,
    b: Point,
    outward: Vec2,
    color: Color,
    cap: Cap,
    half_width: f64,
) {
    match cap {
        Cap::Butt => {} // No cap geometry.
        Cap::Square => {
            // Extrude (a, b) by `half_width` along `outward`, emit a
            // quad.
            let a_ext = a + outward * half_width;
            let b_ext = b + outward * half_width;
            let i_a = vertices.len() as u32;
            vertices.push(a);
            vcolors.push(color);
            let i_b = vertices.len() as u32;
            vertices.push(b);
            vcolors.push(color);
            let i_be = vertices.len() as u32;
            vertices.push(b_ext);
            vcolors.push(color);
            let i_ae = vertices.len() as u32;
            vertices.push(a_ext);
            vcolors.push(color);
            indices.extend_from_slice(&[i_a, i_b, i_be, i_a, i_be, i_ae]);
        }
        Cap::Round => {
            emit_round_cap_fan(
                vertices, vcolors, indices, endpoint, a, b, color, half_width,
            );
        }
    }
}

/// Round-cap fan: triangles fanning out from the polyline endpoint,
/// approximating a semicircle from shoulder `a` around to shoulder `b`.
/// The fan rotates from `a` (relative to the endpoint) to `b` along
/// the outward side.
#[allow(clippy::too_many_arguments)]
fn emit_round_cap_fan(
    vertices: &mut Vec<Point>,
    vcolors: &mut Vec<Color>,
    indices: &mut Vec<u32>,
    endpoint: Point,
    a: Point,
    b: Point,
    color: Color,
    half_width: f64,
) {
    // Two complementary bounds on the angular step:
    //   1. Chord error: ε = R · (1 - cos(Δθ/2)) → Δθ ≈ 2·acos(1 - ε/R).
    //      Sub-pixel chord deviation at any radius.
    //   2. Max angular step: capped at ROUND_MAX_ANGULAR_STEP so even
    //      thin lines (small R) get enough segments to read as smooth.
    // Take the smaller (= denser) of the two.
    let r = half_width.max(EPSILON);
    let chord_step = (1.0 - (ROUND_TOLERANCE / r).clamp(0.0, 1.0)).acos() * 2.0;
    let theta_step = chord_step.clamp(1e-3, ROUND_MAX_ANGULAR_STEP);
    let segments = (std::f64::consts::PI / theta_step).ceil() as usize;
    let segments = segments.clamp(4, 64);

    // From endpoint, the angle of vector (a - endpoint) and (b - endpoint).
    let va = a - endpoint;
    let vb = b - endpoint;
    let theta_a = va.y.atan2(va.x);
    let theta_b = vb.y.atan2(vb.x);
    // Sweep from a → b on the outward side. Pick the shorter signed
    // sweep that crosses the outward direction (which is at the
    // half-angle bisector of va and vb on the convex side). We
    // achieve "go round the outside" by sweeping in the direction
    // that takes us past the cross-product sign-flip point.
    let mut delta = theta_b - theta_a;
    // Normalise into (-π, π].
    while delta > std::f64::consts::PI {
        delta -= std::f64::consts::TAU;
    }
    while delta <= -std::f64::consts::PI {
        delta += std::f64::consts::TAU;
    }
    // We want the sweep that's a semicircle (≈ ±π). If the natural
    // (-π, π] delta has magnitude < π, the cap should go the OTHER
    // way (over the top) to make a semicircle. Otherwise it's the
    // right direction.
    if delta.abs() < std::f64::consts::PI - 1e-6 {
        delta = if delta >= 0.0 {
            delta - std::f64::consts::TAU
        } else {
            delta + std::f64::consts::TAU
        };
    }
    let n_steps = segments.max(2);
    let step = delta / n_steps as f64;

    let i_center = vertices.len() as u32;
    vertices.push(endpoint);
    vcolors.push(color);
    let i_a = vertices.len() as u32;
    vertices.push(a);
    vcolors.push(color);
    let mut prev = i_a;
    for k in 1..=n_steps {
        let theta = theta_a + step * k as f64;
        let p = Point::new(endpoint.x + r * theta.cos(), endpoint.y + r * theta.sin());
        let idx = vertices.len() as u32;
        vertices.push(p);
        vcolors.push(color);
        indices.extend_from_slice(&[i_center, prev, idx]);
        prev = idx;
    }
}

/// Round-join fan: triangles fanning from the polyline vertex out to
/// the outside arc connecting the two outside shoulders.
fn emit_round_fan(
    vertices: &mut Vec<Point>,
    vcolors: &mut Vec<Color>,
    indices: &mut Vec<u32>,
    pivot: Point,
    a: Point,
    b: Point,
    color: Color,
) {
    let va = a - pivot;
    let vb = b - pivot;
    let r = va.hypot();
    let theta_a = va.y.atan2(va.x);
    let theta_b = vb.y.atan2(vb.x);

    let mut delta = theta_b - theta_a;
    while delta > std::f64::consts::PI {
        delta -= std::f64::consts::TAU;
    }
    while delta <= -std::f64::consts::PI {
        delta += std::f64::consts::TAU;
    }
    // The bevel direction is the SHORTER of the two sweeps around
    // pivot. Stick with the (-π, π] delta directly.
    //
    // Angular step is the smaller of (a) chord-error driven and
    // (b) the absolute cap — see `emit_round_cap_fan` for the
    // rationale.
    let chord_step = (1.0 - (ROUND_TOLERANCE / r.max(EPSILON)).clamp(0.0, 1.0)).acos() * 2.0;
    let theta_step = chord_step.clamp(1e-3, ROUND_MAX_ANGULAR_STEP);
    let segments = (delta.abs() / theta_step).ceil() as usize;
    let n_steps = segments.clamp(2, 32);
    let step = delta / n_steps as f64;

    let i_pivot = vertices.len() as u32;
    vertices.push(pivot);
    vcolors.push(color);
    let i_a = vertices.len() as u32;
    vertices.push(a);
    vcolors.push(color);
    let mut prev = i_a;
    for k in 1..=n_steps {
        let theta = theta_a + step * k as f64;
        let p = Point::new(pivot.x + r * theta.cos(), pivot.y + r * theta.sin());
        let idx = vertices.len() as u32;
        vertices.push(p);
        vcolors.push(color);
        indices.extend_from_slice(&[i_pivot, prev, idx]);
        prev = idx;
    }
}

#[inline]
fn perp_left(v: Vec2) -> Vec2 {
    Vec2::new(-v.y, v.x)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }
    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }
    fn green() -> Color {
        Color::new([0.0, 1.0, 0.0, 1.0])
    }
    fn blue() -> Color {
        Color::new([0.0, 0.0, 1.0, 1.0])
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn polyline_ribbon_two_point_butt() {
        // Straight line along +x, half_width 1. Two segments end up
        // sharing shoulders — total 4 vertices (the two shoulder
        // pairs), 2 triangles.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let opts = RibbonOptions {
            half_width: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
            miter_limit: 4.0,
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        assert_eq!(mesh.vertex_count(), 4);
        assert_eq!(mesh.triangle_count(), 2);
        // Shoulders sit at (0, ±1) and (10, ±1).
        let mut ys: Vec<f64> = mesh.vertices.iter().map(|p| p.y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(approx(ys[0], -1.0));
        assert!(approx(ys[1], -1.0));
        assert!(approx(ys[2], 1.0));
        assert!(approx(ys[3], 1.0));
    }

    #[test]
    fn polyline_ribbon_constant_color_all_vertices_match() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let mesh = polyline_ribbon(&pts, red(), &RibbonOptions::default());
        for c in &mesh.colors {
            assert_eq!(*c, red());
        }
    }

    #[test]
    fn polyline_gradient_endpoint_colors_preserved() {
        // 2-point polyline; vertex 0 gets red, vertex 1 gets blue.
        // Both shoulders at vertex 0 carry red; both at vertex 1
        // carry blue. With butt caps + miter (no joins), there are
        // exactly 4 vertices.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let cols = [red(), blue()];
        let mesh = polyline_gradient(&pts, &cols, &RibbonOptions::default());
        assert_eq!(mesh.vertex_count(), 4);
        // The two left-most x vertices (x ≈ 0) carry red; the two
        // right-most (x ≈ 10) carry blue.
        for (p, c) in mesh.vertices.iter().zip(mesh.colors.iter()) {
            if approx(p.x, 0.0) {
                assert_eq!(*c, red());
            } else if approx(p.x, 10.0) {
                assert_eq!(*c, blue());
            }
        }
    }

    #[test]
    fn polyline_gradient_interior_color_shared_across_segments() {
        // 3-vertex polyline; interior vertex's shoulders carry green.
        // Miter join → single shoulder pair at interior. Each segment
        // quad gets a small bleed (SEAM_BLEED_PX) along the local
        // tangent to eliminate the AA seam between adjacent fills, so
        // shoulders near the interior vertex are emitted at slightly
        // staggered x-coordinates. All such shoulders should still
        // carry the green colour.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(20.0, 0.0)];
        let cols = [red(), green(), blue()];
        let mesh = polyline_gradient(&pts, &cols, &RibbonOptions::default());
        // Anything within ±2 × bleed of x=10 (the interior vertex) is
        // an interior-shoulder emission; all should be green.
        let interior_greens = mesh
            .vertices
            .iter()
            .zip(mesh.colors.iter())
            .filter(|(p, _)| (p.x - 10.0).abs() < 2.0)
            .map(|(_, c)| *c)
            .collect::<Vec<_>>();
        assert!(!interior_greens.is_empty());
        for c in &interior_greens {
            assert_eq!(*c, green(), "interior shoulder should be green");
        }
    }

    #[test]
    fn polyline_ribbon_full_variable_width_shoulder_offsets() {
        // Straight line along +x with widths [1, 2, 1]. Shoulder
        // y-coords should be ±1, ±2, ±1 at the (approximate) x
        // positions 0, 5, 10. Seam-bleed splits the interior x=5
        // shoulders into a stagger around x ≈ 4.25 and x ≈ 5.75, but
        // the y-coords remain unchanged.
        let pts = [pt(0.0, 0.0), pt(5.0, 0.0), pt(10.0, 0.0)];
        let widths = [1.0_f64, 2.0, 1.0];
        let mesh = polyline_ribbon_full(&pts, None, Some(&widths), &RibbonOptions::default());
        // Bucket shoulders by approximate x (within ±1 of the
        // expected polyline-vertex x).
        let mut shoulders_at_x: Vec<(f64, Vec<f64>)> =
            vec![(0.0, Vec::new()), (5.0, Vec::new()), (10.0, Vec::new())];
        for p in &mesh.vertices {
            for (x, ys) in shoulders_at_x.iter_mut() {
                if (p.x - *x).abs() < 1.0 {
                    ys.push(p.y);
                }
            }
        }
        for (x, ys) in shoulders_at_x {
            let expected: Vec<f64> = if approx(x, 5.0) {
                vec![-2.0, 2.0]
            } else {
                vec![-1.0, 1.0]
            };
            let mut sorted = ys.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            sorted.dedup_by(|a, b| approx(*a, *b));
            assert_eq!(
                sorted.len(),
                expected.len(),
                "at x={x}, unique shoulder ys = {sorted:?}"
            );
            for (s, e) in sorted.iter().zip(expected.iter()) {
                assert!(approx(*s, *e), "at x={x}, got {s}, expected {e}");
            }
        }
    }

    #[test]
    fn polyline_ribbon_90_corner_mitre() {
        // Three points forming a right-turn 90° corner at (10, 0).
        // miter_mag = 1/cos(45°) ≈ 1.4142, within default miter_limit
        // of 4 → miter join, single shoulder pair at the corner.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0)];
        let opts = RibbonOptions {
            half_width: 1.0,
            join: Join::Miter,
            ..RibbonOptions::default()
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        // No bevel triangle → 2 segments × 2 tris = 4 triangles.
        assert_eq!(mesh.triangle_count(), 4);
        // The outer-corner mitre sits at (11, -1) in the layout, but
        // segment 0's far-end shoulders are bled forward by
        // SEAM_BLEED_PX (= 0.75) along seg_tangent[0] = (1, 0). So
        // the emitted vertex lands at (11.75, -1). Segment 1's
        // near-end shoulders are bled backward by SEAM_BLEED_PX along
        // -seg_tangent[1] = (0, -1), landing at (11, -0.75). Both
        // are valid bled-mitre emissions; test for *either*.
        let near_mitre = mesh.vertices.iter().find(|p| {
            (approx(p.x, 11.75) && approx(p.y, -1.0)) || (approx(p.x, 11.0) && approx(p.y, -0.25))
        });
        assert!(
            near_mitre.is_some(),
            "expected bled outer-mitre near (11, -1); got vertices = {:?}",
            mesh.vertices
        );
    }

    #[test]
    fn polyline_ribbon_sharp_corner_clamps_to_bevel() {
        // Near-U-turn — mitre would extend far beyond miter_limit, so
        // the miter-join setting falls back to a bevel at this vertex.
        // The bevel emits an extra fill triangle.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(0.0, 0.1)];
        let opts = RibbonOptions {
            half_width: 1.0,
            join: Join::Miter,
            miter_limit: 2.0,
            ..RibbonOptions::default()
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        // 2 segments × 2 tris = 4, plus 1 bevel-fill = 5.
        assert_eq!(mesh.triangle_count(), 5);
    }

    #[test]
    fn polyline_ribbon_bevel_join_emits_extra_triangle() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0)];
        let opts = RibbonOptions {
            half_width: 1.0,
            join: Join::Bevel,
            ..RibbonOptions::default()
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        // 4 segment triangles + 1 bevel fill.
        assert_eq!(mesh.triangle_count(), 5);
    }

    #[test]
    fn polyline_ribbon_round_join_emits_fan() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0)];
        let opts = RibbonOptions {
            half_width: 5.0, // larger radius → more fan segments
            join: Join::Round,
            ..RibbonOptions::default()
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        // 4 segment triangles + N fan triangles (N >= 2).
        assert!(
            mesh.triangle_count() >= 6,
            "got {} triangles",
            mesh.triangle_count()
        );
    }

    #[test]
    fn polyline_ribbon_square_cap_extends_endpoint() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let opts = RibbonOptions {
            half_width: 1.0,
            cap: Cap::Square,
            ..RibbonOptions::default()
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        // Square caps add 2 triangles per cap.
        // 2 segment + 2 (start cap) + 2 (end cap) = 6 triangles.
        assert_eq!(mesh.triangle_count(), 6);
        // Bounding box should now extend past x ∈ [-1, 11] (one
        // half-width beyond each endpoint).
        let bb = mesh.bounding_box();
        assert!(approx(bb.x0, -1.0));
        assert!(approx(bb.x1, 11.0));
    }

    #[test]
    fn polyline_ribbon_round_cap_emits_fan() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let opts = RibbonOptions {
            half_width: 5.0,
            cap: Cap::Round,
            ..RibbonOptions::default()
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        // 2 segment + ≥4 fan triangles per round cap.
        assert!(mesh.triangle_count() >= 2 + 2 * 4);
    }

    #[test]
    fn polyline_ribbon_butt_cap_emits_no_cap_triangles() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let opts = RibbonOptions {
            half_width: 1.0,
            cap: Cap::Butt,
            ..RibbonOptions::default()
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        assert_eq!(mesh.triangle_count(), 2);
    }

    #[test]
    fn polyline_ribbon_bounding_box_straight_butt() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let opts = RibbonOptions {
            half_width: 1.0,
            cap: Cap::Butt,
            ..RibbonOptions::default()
        };
        let mesh = polyline_ribbon(&pts, red(), &opts);
        let bb = mesh.bounding_box();
        assert!(approx(bb.x0, 0.0));
        assert!(approx(bb.x1, 10.0));
        assert!(approx(bb.y0, -1.0));
        assert!(approx(bb.y1, 1.0));
    }

    #[test]
    fn polyline_ribbon_under_two_points_returns_empty() {
        let pts = [pt(0.0, 0.0)];
        let mesh = polyline_ribbon(&pts, red(), &RibbonOptions::default());
        assert!(mesh.is_empty());
    }

    #[test]
    #[should_panic(expected = "colors.len()")]
    fn polyline_gradient_panics_on_length_mismatch() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let cols = [red(), green(), blue()];
        let _ = polyline_gradient(&pts, &cols, &RibbonOptions::default());
    }

    // ── Closed-polygon ribbon ──────────────────────────────────────

    #[test]
    fn polygon_ribbon_equilateral_triangle_segment_count() {
        // Interior angle 60°, turn angle 120°, miter_mag = 2 < 4 →
        // mitre at every vertex, no bevel fills. 3 wrap segments × 2
        // tris per segment = 6.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(5.0, 8.66)];
        let opts = RibbonOptions {
            half_width: 1.0,
            join: Join::Miter,
            ..RibbonOptions::default()
        };
        let mesh = polygon_ribbon(&pts, red(), &opts);
        assert_eq!(mesh.triangle_count(), 6);
    }

    #[test]
    fn polygon_ribbon_square_bevel_emits_four_extra_triangles() {
        // 4 segments × 2 + one bevel fill at each of 4 corners
        // (including the wrap-back to vertex 0).
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let opts = RibbonOptions {
            half_width: 1.0,
            join: Join::Bevel,
            ..RibbonOptions::default()
        };
        let mesh = polygon_ribbon(&pts, red(), &opts);
        assert_eq!(mesh.triangle_count(), 12);
    }

    #[test]
    fn polygon_ribbon_too_few_points_returns_empty() {
        // < 3 points → empty mesh; a 2-point closed loop is degenerate.
        for pts in [&[][..], &[pt(0.0, 0.0)], &[pt(0.0, 0.0), pt(10.0, 0.0)]] {
            let mesh = polygon_ribbon(pts, red(), &RibbonOptions::default());
            assert!(
                mesh.is_empty(),
                "expected empty mesh for {} points",
                pts.len()
            );
        }
    }

    #[test]
    fn polygon_ribbon_cap_setting_is_ignored() {
        // Closed polygon has no endpoints — varying `cap` must not
        // change the mesh.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(5.0, 8.66)];
        let make = |cap| {
            let opts = RibbonOptions {
                half_width: 1.0,
                cap,
                join: Join::Miter,
                ..RibbonOptions::default()
            };
            polygon_ribbon(&pts, red(), &opts).triangle_count()
        };
        let butt = make(Cap::Butt);
        assert_eq!(butt, make(Cap::Square));
        assert_eq!(butt, make(Cap::Round));
    }

    #[test]
    fn polygon_gradient_wrap_segment_closes_color_loop() {
        // Triangle with vertex colours [red, green, blue]. The wrap
        // segment (vertex 2 → vertex 0) must emit red shoulders at
        // its far end, otherwise the loop wouldn't actually close.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(5.0, 8.66)];
        let cols = [red(), green(), blue()];
        let opts = RibbonOptions {
            half_width: 1.0,
            join: Join::Miter,
            ..RibbonOptions::default()
        };
        let mesh = polygon_gradient(&pts, &cols, &opts);
        let mut counts = [0_usize; 3];
        for c in &mesh.colors {
            if *c == red() {
                counts[0] += 1;
            } else if *c == green() {
                counts[1] += 1;
            } else if *c == blue() {
                counts[2] += 1;
            }
        }
        // Each colour should appear at multiple shoulder emissions
        // (incoming AND outgoing segment at its vertex).
        assert!(counts[0] >= 2, "expected red shoulders, got {counts:?}");
        assert!(counts[1] >= 2, "expected green shoulders, got {counts:?}");
        assert!(counts[2] >= 2, "expected blue shoulders, got {counts:?}");
    }

    #[test]
    fn polygon_ribbon_full_variable_width_widens_with_width() {
        // Square loop at two width settings: the bounding box should
        // grow as the width grows. Exact equality is hard because the
        // seam-bleed shifts shoulders along the local tangent (which
        // for a square is the same axis as the bounding-box edge),
        // but the *outward* extent must still scale with width.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let opts = RibbonOptions {
            half_width: 1.0,
            join: Join::Miter,
            ..RibbonOptions::default()
        };
        let m_thin = polygon_ribbon_full(&pts, None, Some(&[1.0_f64; 4]), &opts);
        let m_thick = polygon_ribbon_full(&pts, None, Some(&[5.0_f64; 4]), &opts);
        let bb_thin = m_thin.bounding_box();
        let bb_thick = m_thick.bounding_box();
        // Outer extent grows by ~4 px on each side as w goes 1 → 5.
        assert!(
            bb_thick.x0 < bb_thin.x0 - 3.0,
            "expected thicker x0 ({}) at least 3 px outside thin x0 ({})",
            bb_thick.x0,
            bb_thin.x0,
        );
        assert!(
            bb_thick.x1 > bb_thin.x1 + 3.0,
            "expected thicker x1 ({}) at least 3 px outside thin x1 ({})",
            bb_thick.x1,
            bb_thin.x1,
        );
        assert!(bb_thick.y0 < bb_thin.y0 - 3.0);
        assert!(bb_thick.y1 > bb_thin.y1 + 3.0);
    }

    #[test]
    fn polygon_ribbon_full_per_vertex_width_changes_shoulder_offsets() {
        // Triangle with widths [1, 4, 1]. Vertex 1 (the wide one)
        // should produce shoulder pairs farther from the polyline
        // than vertex 0 or 2.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(5.0, 8.66)];
        let widths = [1.0_f64, 4.0, 1.0];
        let opts = RibbonOptions {
            half_width: 1.0,
            join: Join::Miter,
            ..RibbonOptions::default()
        };
        let mesh = polygon_ribbon_full(&pts, None, Some(&widths), &opts);
        // Find max shoulder offset from each vertex (taking shoulder
        // as "any mesh vertex within ~3 px of the polyline vertex
        // along the polyline" is too fragile, so just measure
        // shoulder-vertex distance from polyline vertex and bucket
        // by closest polyline vertex).
        let mut max_offset = [0.0_f64; 3];
        for v in &mesh.vertices {
            let d = [
                (*v - pts[0]).hypot(),
                (*v - pts[1]).hypot(),
                (*v - pts[2]).hypot(),
            ];
            let (idx, dist) = d
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .unwrap();
            if *dist > max_offset[idx] {
                max_offset[idx] = *dist;
            }
        }
        // Vertex 1 (width 4) should sit further from its vertex than
        // vertices 0/2 (width 1).
        assert!(
            max_offset[1] > max_offset[0] + 2.0,
            "max shoulder offsets per vertex: {max_offset:?}",
        );
        assert!(max_offset[1] > max_offset[2] + 2.0);
    }

    #[test]
    #[should_panic(expected = "colors.len()")]
    fn polygon_gradient_panics_on_length_mismatch() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(5.0, 8.66)];
        let cols = [red(), green()];
        let _ = polygon_gradient(&pts, &cols, &RibbonOptions::default());
    }
}
