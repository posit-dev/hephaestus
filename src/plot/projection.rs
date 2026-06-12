//! Coordinate projection — converts per-channel panel-fractions into
//! pixel positions inside the panel rect.
//!
//! Ships [`Projection::Cartesian`] (default rectilinear) and
//! [`Projection::Polar`] (with configurable angular range — see
//! [`PolarProjection`]). The signature is N-channel
//! (`project_to_panel_px(panel, &[f64])`) so a future Ternary variant
//! (deferred) can drop in without touching geom code.
//!
//! ## Why this exists
//!
//! Before this module, every geom did the panel-rect conversion inline:
//!
//! ```ignore
//! let px = panel.x0 + x_frac * (panel.x1 - panel.x0);
//! let py = panel.y1 - y_frac * (panel.y1 - panel.y0);  // y flips
//! ```
//!
//! That math is **Cartesian-specific**. A polar projection needs to map
//! `(theta_frac, r_frac)` onto a centred inscribed disk; ternary needs
//! `(a, b, c)` → barycentric coords on a triangle. By routing through a
//! projection method, the geom's hot loop stays the same shape and the
//! coordinate math lives in one place per projection.
//!
//! For Cartesian the projection collapses to exactly the inlined math
//! above — the match arm is monomorphic and the compiler optimises it
//! back to a direct multiply. Polar runs the polar math; under polar
//! `is_linear() == false` so connected geoms densify their edges via
//! [`Projection::interpolate_segment`].
//!
//! ## Spatial projection is one instance of a broader pattern
//!
//! `Projection` is the *spatial* case of a more general **scale
//! combiner**: scale-map several channels independently, then combine
//! the scaled values into a single higher-level aesthetic. The same
//! pattern applies to other aesthetics:
//!
//! | Combiner            | Channels (typical) | Output       |
//! |---------------------|--------------------|--------------|
//! | Position (this file) | `x`, `y` (Cartesian); `theta`, `radius` (Polar); `a`, `b`, `c` (Ternary) | `(px, py)` |
//! | Color (future)       | `hue`, `lightness`, `saturation`; or `r`, `g`, `b`; or `color` + `alpha` | `Color` |
//! | Size (future)        | `width`, `height`; or uniform `size` | `f64` or `(w, h)` |
//! | Stroke spec (future) | `linewidth`, `cap`, `join`, `linetype`     | `Stroke` |
//!
//! Hephaestus already has a degenerate two-channel color combiner
//! hard-wired into every geom that supports `fill_opacity`:
//! `resolve_color_channel(fill, …) + override_alpha(color, opacity)`
//! is exactly `(Color, alpha: f64) → Color`. A future `ColorProjection`
//! would make that user-configurable — bind `hue` to one scale,
//! `lightness` to another, `alpha` to a third, and the projection
//! combines them.
//!
//! **Why these live as separate concrete types instead of one generic
//! `Combiner<I, O>` trait:**
//!
//! - Output types differ (`(f64, f64)`, `Color`, `f64`, `Stroke`, …).
//!   A generic trait would force per-row dynamic dispatch or
//!   monomorphisation across geom code paths.
//! - The "is the mapping linear and does it need densification?"
//!   question only applies to *spatial* output — colors and sizes
//!   have no connectivity to densify between.
//! - The hot-loop integration into geoms is different per output kind
//!   (position threads through `project_to_panel_px`; color would
//!   replace `resolve_color_channel`; etc.).
//!
//! When non-spatial combiners land, they live in sibling modules
//! (`color_projection.rs`, `size_projection.rs`, …) with their own
//! enums and their own integration into the per-row resolution helpers.
//! What's shared is the **design pattern**, not a trait:
//!
//! - Channel-name declaration (`consume_channels() -> &[&str]`).
//! - Enum-tag + match dispatch (no `Arc<dyn>`), matching the
//!   [`scales`](crate::scales) crate's style.
//! - User-facing binding (`plot.bind("hue", "category_scale")`).
//!
//! The `is_linear` / `interpolate_segment` methods on this enum are
//! **spatial-only concerns** (geodesic-vs-chord rendering of connected
//! shapes). Future color / size / stroke combiners won't have them.

use crate::geometry::Rect;

/// Where this projection wants its axis chrome drawn. Cartesian uses
/// the standard patch axis slots (`AxisLeft`, `AxisBottom`, etc.).
/// Polar / Ternary draw circular / triangular axes inside the panel
/// rect because there's no rectilinear edge to align to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromeStrategy {
    /// Use the patch's anatomical axis slots. The standard rectilinear
    /// layout.
    PatchSlots,
    /// Draw axes inside the panel rect; leave the axis slots empty.
    /// Used by Polar (concentric arcs + radial spokes) and a future
    /// Ternary (triangular tick rails).
    ///
    /// **Known limitation:** tick / break *labels* may extend outside
    /// the panel rect (e.g. polar theta labels sit just beyond the
    /// outermost circle). Today they're drawn unclipped into whatever
    /// space the panel has around the inscribed bbox. The proper fix
    /// — populate the four axis slots with polar-specific bleed
    /// `Measure`s so the layout solver reserves space, then bleed
    /// labels into those strips — is a follow-up. The bleed amount
    /// is exactly calculable from the max label dimension projected
    /// along each cardinal direction (partial-arc configurations
    /// only contribute on the sides covered by active spokes).
    InsidePanel,
}

/// Coordinate projection.
///
/// The N-channel `project_to_panel_px(panel, &[f64])` signature is
/// designed so future variants that consume more than two channels
/// (Ternary's three barycentric coords) can drop in without changing
/// the geom call sites.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Projection {
    /// Identity over `(x, y)`. The fraction-to-pixel map is the
    /// canonical `panel.x0 + x_frac * panel_w`, `panel.y1 - y_frac *
    /// panel_h` (y flips so positive y maps "up" visually).
    #[default]
    Cartesian,
    /// Polar coordinates with a configurable angular range. Reads two
    /// channels — `angle_channel` (theta) and `radius_channel`
    /// (radius) — and projects them onto a centred inscribed disk
    /// (or annular ring if `inner_radius_frac > 0`). Supports
    /// partial-arc layouts (gauges, half-disks) via `theta_start` /
    /// `theta_end`. See [`PolarProjection`].
    Polar(PolarProjection),
    // Ternary(TernaryProjection) — deferred (design accommodated via
    // the N-channel signature).
}

/// How edges between data points are interpreted under a polar
/// projection. The point-to-pixel math is identical either way; the
/// difference is whether connecting lines follow the projected arc
/// (geodesic) or are straight pixel-space chords between consecutive
/// theta-break positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PolarEdgeStyle {
    /// Edges follow the projected polar geodesic — arcs for theta
    /// variation, straight radial lines for radius variation. The
    /// natural interpretation for continuous theta domains (time
    /// series in polar coords, scatter plots, polar histograms).
    /// Densification is chord-error driven (see
    /// [`Projection::interpolate_segment`]).
    #[default]
    Geodesic,
    /// Edges are straight pixel-space chords between consecutive
    /// theta-break positions — the classic radar / spider chart look.
    /// The categories live at the [`PolarProjection::theta_break_fracs`]
    /// positions; a polyline crossing K breaks bends K times, with
    /// each bend at the radius linearly interpolated from the
    /// surrounding data vertices.
    ///
    /// `is_linear()` is still **false** under `Chord` — a polyline
    /// crossing one or more breaks isn't a straight line in panel
    /// space. The difference from `Geodesic` is that interior samples
    /// land at the **break crossings** rather than at chord-error-driven
    /// arc-length intervals.
    ///
    /// Chrome adjustments under `Chord`:
    /// - Concentric "rings" become polygons connecting the theta
    ///   scale's break positions at each radius break — the classic
    ///   radar grid look.
    /// - Side caps (for partial-arc configurations) are skipped —
    ///   the outermost polygon ring already closes the figure.
    Chord,
}

/// Configurable polar projection. Maps two channel-space fractions
/// onto a centred inscribed disk inside the panel rect:
///
/// - **`angle_channel`** (default `"x"`) — the theta channel. Frac 0
///   maps to `theta_start`, frac 1 to `theta_end`.
/// - **`radius_channel`** (default `"y"`) — the radius channel. Frac
///   0 maps to `inner_radius_frac * r_outer`, frac 1 to `r_outer`
///   (the inscribed disk radius).
/// - **`theta_start` / `theta_end`** — angular span in radians, math
///   convention (0 = 3 o'clock, π/2 = 12 o'clock). Span sign sets
///   sweep direction: `end - start < 0` is clockwise, `> 0` is
///   counter-clockwise.
/// - **`inner_radius_frac`** — fraction of `r_outer` for the hole at
///   the centre. `0.0` = filled disk; `> 0` = ring (donut, gauge).
/// - **`edge_style`** — [`PolarEdgeStyle::Geodesic`] (default; arcs
///   between data points) or [`PolarEdgeStyle::Chord`] (straight
///   chords — the radar / spider chart look).
///
/// Constructed via [`Projection::polar`] (full clockwise circle from
/// 12 o'clock), [`Projection::gauge`] (half-disk arc with a hole),
/// or [`Projection::radar`] (full circle with chord-style edges and
/// polygon grid).
#[derive(Debug, Clone, PartialEq)]
pub struct PolarProjection {
    /// Channel name read as theta.
    pub angle_channel: String,
    /// Channel name read as radius.
    pub radius_channel: String,
    /// Angle at `theta_frac = 0`, in radians (math convention).
    pub theta_start: f64,
    /// Angle at `theta_frac = 1`, in radians (math convention). The
    /// sign of `theta_end - theta_start` determines sweep direction:
    /// negative = clockwise (the visual default), positive =
    /// counter-clockwise.
    pub theta_end: f64,
    /// Inner radius as a fraction of `r_outer`. `0.0` = filled disk.
    pub inner_radius_frac: f64,
    /// How edges between data points are interpreted. Defaults to
    /// [`PolarEdgeStyle::Geodesic`]; set to
    /// [`PolarEdgeStyle::Chord`] for radar / spider charts.
    pub edge_style: PolarEdgeStyle,
    /// Theta-break positions (channel-space fractions in `[0, 1]`)
    /// used as polygon vertices under `Chord` edge style. Empty for
    /// `Geodesic` (the field is ignored there). Typically `[0.0,
    /// 1.0/N, 2.0/N, …, (N-1)/N]` for N evenly-spaced categories;
    /// for a non-closed radar partial use the partial range.
    ///
    /// Affects two things under `Chord`:
    /// - Chrome polygon rings draw their vertices at these
    ///   theta positions.
    /// - [`Projection::interpolate_segment`] emits one interior
    ///   sample per break that the segment crosses, so a polyline
    ///   correctly bends at each category boundary instead of
    ///   cutting diagonally across them.
    pub theta_break_fracs: Vec<f64>,
    /// Use the projected bbox to size + offset the inscribed disk
    /// inside the panel rect. `true` by default — partial-arc
    /// projections (gauges, half-disks) get their swept area
    /// centred in the panel rather than the full-circle's centre.
    ///
    /// Set to `false` when two or more polar projections share the
    /// same panel (concentric nesting, multiple partial arcs at
    /// different theta_start offsets, etc.) so they share a common
    /// centre + max radius regardless of their individual sweeps.
    pub fit_to_bbox: bool,
    /// Optional outer-radius override, as a fraction of the
    /// projection's natural maximum radius (the inscribed-disk
    /// radius for full-circle, or the bbox-fitted radius for
    /// partial arcs when `fit_to_bbox` is on). `None` = fill the
    /// available space.
    ///
    /// Pairs with [`Self::inner_radius_frac`] for concentric
    /// nesting: an outer projection with
    /// `outer_radius_frac = None, inner_radius_frac = 0.5` and an
    /// inner projection with `outer_radius_frac = Some(0.5)`
    /// (both with `fit_to_bbox = false`) draw on disjoint annular
    /// regions of the same panel.
    pub outer_radius_frac: Option<f64>,
}

impl PolarProjection {
    /// Full clockwise circle from 12 o'clock. Default for
    /// [`Projection::polar`].
    pub fn full_circle() -> Self {
        PolarProjection {
            angle_channel: "x".into(),
            radius_channel: "y".into(),
            theta_start: std::f64::consts::FRAC_PI_2,
            theta_end: std::f64::consts::FRAC_PI_2 - std::f64::consts::TAU,
            inner_radius_frac: 0.0,
            edge_style: PolarEdgeStyle::Geodesic,
            theta_break_fracs: Vec::new(),
            fit_to_bbox: true,
            outer_radius_frac: None,
        }
    }

    /// Half-disk gauge: 9 o'clock → 12 o'clock → 3 o'clock, with a
    /// 40 %-of-radius hole at the centre. Default for
    /// [`Projection::gauge`].
    pub fn gauge() -> Self {
        PolarProjection {
            angle_channel: "x".into(),
            radius_channel: "y".into(),
            theta_start: std::f64::consts::PI,
            theta_end: 0.0,
            inner_radius_frac: 0.4,
            edge_style: PolarEdgeStyle::Geodesic,
            theta_break_fracs: Vec::new(),
            fit_to_bbox: true,
            outer_radius_frac: None,
        }
    }

    /// Radar / spider chart with `n_categories` evenly-spaced
    /// vertices around a full CW circle from 12 o'clock. Edges
    /// between data points are chord-style; polylines that span
    /// multiple categories bend at each crossed category boundary.
    ///
    /// Sweep direction is **CW** (negative span) — matching
    /// [`PolarProjection::full_circle`] and the typical decoding
    /// direction a viewer reads angular position in (clockwise from
    /// 12 o'clock, like a clock face). Override `theta_start` /
    /// `theta_end` for a CCW radar or partial-arc radar.
    ///
    /// `theta_break_fracs` is set to **band-centre** positions
    /// `(i + 0.5) / N` — these match what `Scale::map` returns for a
    /// discrete scale with `N` entries, so the natural pairing
    /// `scale::discrete([N category names])` + `Projection::radar(N)`
    /// aligns the polygon vertices, axis spokes, and data positions
    /// without further configuration.
    pub fn radar(n_categories: usize) -> Self {
        let n = n_categories.max(2);
        let theta_break_fracs: Vec<f64> = (0..n).map(|i| (i as f64 + 0.5) / n as f64).collect();
        PolarProjection {
            angle_channel: "x".into(),
            radius_channel: "y".into(),
            theta_start: std::f64::consts::FRAC_PI_2,
            theta_end: std::f64::consts::FRAC_PI_2 - std::f64::consts::TAU,
            inner_radius_frac: 0.0,
            edge_style: PolarEdgeStyle::Chord,
            theta_break_fracs,
            fit_to_bbox: true,
            outer_radius_frac: None,
        }
    }

    /// Bounding box in unit-radius math-convention coordinates — the
    /// rect that tightly encloses the swept area at full outer
    /// radius. Used to size and centre the projection inside the
    /// panel.
    ///
    /// For a full circle this is `(-1, -1, 1, 1)`. For a partial arc
    /// the bbox tracks just the swept region: a half-disk gauge
    /// (theta_start = π → theta_end = 0) returns `(-1, 0, 1, 1)`
    /// (no swept area in the bottom half).
    pub fn bounding_box_units(&self) -> (f64, f64, f64, f64) {
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        let mut accumulate = |x: f64, y: f64| {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        };

        let use_polygon =
            matches!(self.edge_style, PolarEdgeStyle::Chord) && !self.theta_break_fracs.is_empty();

        if use_polygon {
            // Chord-style with explicit categories: the outer boundary
            // is the polygon connecting each category vertex on the
            // unit circle. Smaller than the inscribing arc, so we
            // size to the polygon's actual extent.
            for &frac in &self.theta_break_fracs {
                let theta = self.theta_for_frac(frac);
                accumulate(theta.cos(), theta.sin());
            }
            // Also include the endpoints if this is a partial-arc
            // radar — the user can draw between theta_start and the
            // first break, or the last break and theta_end.
            accumulate(self.theta_start.cos(), self.theta_start.sin());
            accumulate(self.theta_end.cos(), self.theta_end.sin());
        } else {
            // Outer endpoints.
            accumulate(self.theta_start.cos(), self.theta_start.sin());
            accumulate(self.theta_end.cos(), self.theta_end.sin());

            // Cardinal direction unit vectors reached by the sweep
            // contribute bbox extrema (the arc passes through them).
            // Check 0, ±π/2, ±π, ±3π/2 since the sweep can be up to ±TAU.
            for k in -2..=2 {
                let target = k as f64 * std::f64::consts::FRAC_PI_2;
                if angle_in_sweep(target, self.theta_start, self.theta_end) {
                    accumulate(target.cos(), target.sin());
                }
            }
        }

        if self.inner_radius_frac > 0.0 {
            let inner = self.inner_radius_frac;
            if use_polygon {
                // Inner polygon at the same theta_break_fracs.
                for &frac in &self.theta_break_fracs {
                    let theta = self.theta_for_frac(frac);
                    accumulate(inner * theta.cos(), inner * theta.sin());
                }
            } else {
                // Ring layout: include inner-arc endpoints. The inner
                // arc shares the same angular range; for cardinals it
                // lies INSIDE the outer arc's bbox so doesn't expand
                // it. Endpoints contribute when they project to bbox
                // extrema (typically for partial arcs).
                accumulate(
                    inner * self.theta_start.cos(),
                    inner * self.theta_start.sin(),
                );
                accumulate(inner * self.theta_end.cos(), inner * self.theta_end.sin());
            }
        } else {
            // Filled pie: the polar centre (0, 0) is part of the
            // swept area.
            accumulate(0.0, 0.0);
        }

        (min_x, min_y, max_x, max_y)
    }

    /// Geometry of the inscribed disk for this panel. Uses
    /// [`Self::bounding_box_units`] to scale and position the
    /// projection so partial-arc layouts don't waste the unused half
    /// of the panel.
    pub(crate) fn geometry(&self, panel: Rect) -> PolarGeometry {
        let panel_w = (panel.x1 - panel.x0).max(0.0);
        let panel_h = (panel.y1 - panel.y0).max(0.0);
        if panel_w <= 0.0 || panel_h <= 0.0 {
            return PolarGeometry {
                cx: panel.x0,
                cy: panel.y0,
                r_outer: 0.0,
                r_inner: 0.0,
            };
        }

        let (cx, cy, max_radius) = if self.fit_to_bbox {
            // Fit the projected bbox into the panel preserving
            // aspect. Asymmetric sweeps land their centre off-panel-
            // centre so the swept region fills the panel.
            let (min_x, min_y, max_x, max_y) = self.bounding_box_units();
            let bbox_w = (max_x - min_x).max(f64::EPSILON);
            let bbox_h = (max_y - min_y).max(f64::EPSILON);
            let scale = (panel_w / bbox_w).min(panel_h / bbox_h);
            let scaled_bbox_w = bbox_w * scale;
            let scaled_bbox_h = bbox_h * scale;
            let bbox_x0_px = panel.x0 + (panel_w - scaled_bbox_w) * 0.5;
            let bbox_y0_px = panel.y0 + (panel_h - scaled_bbox_h) * 0.5;
            let centre_rel_x = -min_x / bbox_w;
            let centre_rel_y = -min_y / bbox_h;
            // Screen y flips (math y up → screen y down).
            let cx = bbox_x0_px + centre_rel_x * scaled_bbox_w;
            let cy = bbox_y0_px + (1.0 - centre_rel_y) * scaled_bbox_h;
            (cx, cy, scale)
        } else {
            // Fit-to-bbox disabled: centre on the panel's geometric
            // centre with the largest inscribed disk. Lets multiple
            // polar projections share a panel (concentric nesting,
            // overlapping partial arcs).
            let cx = panel.x0 + panel_w * 0.5;
            let cy = panel.y0 + panel_h * 0.5;
            let max_radius = panel_w.min(panel_h) * 0.5;
            (cx, cy, max_radius)
        };

        let r_outer = max_radius * self.outer_radius_frac.unwrap_or(1.0);
        let r_inner = r_outer * self.inner_radius_frac;

        PolarGeometry {
            cx,
            cy,
            r_outer,
            r_inner,
        }
    }

    /// Map a theta fraction to the radians angle.
    pub(crate) fn theta_for_frac(&self, frac: f64) -> f64 {
        self.theta_start + frac * (self.theta_end - self.theta_start)
    }

    /// Project a (theta_frac, radius_frac) pair to pixel space.
    /// For chord-style projections with categories the (theta_frac,
    /// r_frac=1) image is the polygon — not the inscribed circle —
    /// so a point between two adjacent breaks lands on the polygon
    /// edge connecting them. See [`Self::unit_position`].
    pub(crate) fn project_frac(&self, panel: Rect, theta_frac: f64, r_frac: f64) -> (f64, f64) {
        let g = self.geometry(panel);
        let (ux, uy) = self.unit_position(theta_frac);
        let r = g.r_inner + r_frac * (g.r_outer - g.r_inner);
        // Math-convention angles + screen-y-down: `+sin` lifts visually,
        // hence the `cy -` (not `cy +`).
        (g.cx + r * ux, g.cy - r * uy)
    }

    /// Map a theta fraction to a unit-radius (cos θ, sin θ) position
    /// on the projection's outer boundary. Geodesic edge style uses
    /// the standard circle math; chord edge style returns a position
    /// on the polygon defined by [`Self::theta_break_fracs`] (and the
    /// sweep endpoints for partial arcs), interpolated linearly in
    /// cartesian space between adjacent polygon vertices.
    ///
    /// Returned y is math convention (positive up); the caller flips
    /// it to screen convention.
    pub(crate) fn unit_position(&self, theta_frac: f64) -> (f64, f64) {
        if matches!(self.edge_style, PolarEdgeStyle::Chord) && !self.theta_break_fracs.is_empty() {
            self.chord_unit_position(theta_frac)
        } else {
            let theta = self.theta_for_frac(theta_frac);
            (theta.cos(), theta.sin())
        }
    }

    fn chord_unit_position(&self, theta_frac: f64) -> (f64, f64) {
        let span = self.theta_end - self.theta_start;
        let is_full_circle = (span.abs() - std::f64::consts::TAU).abs() < 1e-6;

        // Build the polygon vertex list, sorted by frac. For partial
        // arcs include 0.0 and 1.0 as sweep endpoints so points
        // between theta_start/end and the first/last break also land
        // on the polygon.
        let mut verts: Vec<(f64, (f64, f64))> =
            Vec::with_capacity(self.theta_break_fracs.len() + 2);
        if !is_full_circle {
            let th = self.theta_for_frac(0.0);
            verts.push((0.0, (th.cos(), th.sin())));
        }
        for &b in &self.theta_break_fracs {
            let th = self.theta_for_frac(b);
            verts.push((b, (th.cos(), th.sin())));
        }
        if !is_full_circle {
            let th = self.theta_for_frac(1.0);
            verts.push((1.0, (th.cos(), th.sin())));
        }
        verts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        if verts.is_empty() {
            let th = self.theta_for_frac(theta_frac);
            return (th.cos(), th.sin());
        }

        let t = theta_frac;

        // Try each interior edge first.
        for w in verts.windows(2) {
            let (f_lo, p_lo) = w[0];
            let (f_hi, p_hi) = w[1];
            if t >= f_lo && t <= f_hi && f_hi > f_lo {
                let u = (t - f_lo) / (f_hi - f_lo);
                return (
                    p_lo.0 * (1.0 - u) + p_hi.0 * u,
                    p_lo.1 * (1.0 - u) + p_hi.1 * u,
                );
            }
        }

        if is_full_circle {
            // Wrap edge: from the last vertex (highest frac) cyclically
            // back to the first (lowest frac). t is in [0, verts[0].0)
            // or (verts.last().0, 1].
            let n = verts.len();
            let (f_lo, p_lo) = verts[n - 1];
            let (f_hi, p_hi) = verts[0];
            let total = (1.0 - f_lo) + f_hi;
            let u = if t >= f_lo {
                (t - f_lo) / total
            } else {
                (1.0 - f_lo + t) / total
            };
            (
                p_lo.0 * (1.0 - u) + p_hi.0 * u,
                p_lo.1 * (1.0 - u) + p_hi.1 * u,
            )
        } else {
            // Outside the polygon's frac range — clamp to the nearest
            // endpoint vertex.
            if t < verts[0].0 {
                verts[0].1
            } else {
                verts.last().unwrap().1
            }
        }
    }
}

/// Computed inscribed-disk geometry for a [`PolarProjection`] on a
/// specific panel rect. Exposed at `pub(crate)` so the chrome renderer
/// (`crate::plot::chrome::polar`) can reuse it without recomputing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PolarGeometry {
    pub cx: f64,
    pub cy: f64,
    pub r_outer: f64,
    pub r_inner: f64,
}

impl Projection {
    /// Convenience: the default Cartesian projection.
    pub const fn cartesian() -> Self {
        Projection::Cartesian
    }

    /// Full clockwise polar projection from 12 o'clock. See
    /// [`PolarProjection::full_circle`].
    pub fn polar() -> Self {
        Projection::Polar(PolarProjection::full_circle())
    }

    /// Half-disk gauge projection. See [`PolarProjection::gauge`].
    pub fn gauge() -> Self {
        Projection::Polar(PolarProjection::gauge())
    }

    /// Radar / spider chart projection — chord-style edges + polygon
    /// grid. `n_categories` is the number of evenly-spaced theta
    /// vertices. See [`PolarProjection::radar`].
    pub fn radar(n_categories: usize) -> Self {
        Projection::Polar(PolarProjection::radar(n_categories))
    }

    /// Channel names this projection reads, in argument order for
    /// [`Self::project_to_panel_px`]. The returned slice borrows from
    /// `self` so configured channel names (Polar's angle/radius
    /// names) flow through without allocation by the projection. The
    /// `Vec` itself is a small per-call allocation; this is metadata
    /// (rare) and not called in the per-row hot loop.
    pub fn consume_channels(&self) -> Vec<&str> {
        match self {
            Projection::Cartesian => vec!["x", "y"],
            Projection::Polar(p) => vec![p.angle_channel.as_str(), p.radius_channel.as_str()],
        }
    }

    /// Map per-channel panel-fractions to a pixel position inside
    /// `panel`. `channels` is parallel to [`Self::consume_channels`];
    /// position 0 = theta-or-x fraction, position 1 = radius-or-y
    /// fraction.
    ///
    /// Missing channels (slice shorter than `consume_channels().len()`)
    /// default to `0.0`. Extra channels are ignored.
    pub fn project_to_panel_px(&self, panel: Rect, channels: &[f64]) -> (f64, f64) {
        match self {
            Projection::Cartesian => {
                let x_frac = channels.first().copied().unwrap_or(0.0);
                let y_frac = channels.get(1).copied().unwrap_or(0.0);
                let panel_w = panel.x1 - panel.x0;
                let panel_h = panel.y1 - panel.y0;
                // y flips: panel_rect.y0 is the TOP edge of the panel
                // (smaller pixel value); y_frac=0 should map to the
                // BOTTOM (panel_rect.y1).
                (panel.x0 + x_frac * panel_w, panel.y1 - y_frac * panel_h)
            }
            Projection::Polar(p) => {
                let theta_frac = channels.first().copied().unwrap_or(0.0);
                let r_frac = channels.get(1).copied().unwrap_or(0.0);
                p.project_frac(panel, theta_frac, r_frac)
            }
        }
    }

    /// Where this projection wants its axis chrome drawn.
    pub const fn chrome_strategy(&self) -> ChromeStrategy {
        match self {
            Projection::Cartesian => ChromeStrategy::PatchSlots,
            Projection::Polar(_) => ChromeStrategy::InsidePanel,
        }
    }

    /// `true` when channel-space straight lines map to panel-space
    /// straight lines. Cartesian is linear; Polar (and future
    /// Ternary) are not.
    ///
    /// Geoms that draw connected shapes (LineGeom, PolygonGeom,
    /// SegmentGeom, RectGeom, TextPathGeom) consult this to decide
    /// whether to densify their edges before stroking. For linear
    /// projections they take the fast path (project the endpoints and
    /// stroke a straight segment); for non-linear projections they
    /// insert interior sample points via [`Self::interpolate_segment`]
    /// so the rendered polyline follows the projected geodesic instead
    /// of cutting across it as a chord.
    pub const fn is_linear(&self) -> bool {
        matches!(self, Projection::Cartesian)
    }

    /// Borrow the polar projection's config, if this is one.
    pub fn as_polar(&self) -> Option<&PolarProjection> {
        match self {
            Projection::Polar(p) => Some(p),
            _ => None,
        }
    }

    /// For a channel-space line segment from `start` to `end`, append
    /// **interior** sample points (in panel pixels) to `out`. Does NOT
    /// include either endpoint — the caller projects those directly
    /// via [`Self::project_to_panel_px`] and pushes them.
    ///
    /// For [linear](Self::is_linear) projections this is a no-op
    /// (interior of a straight segment needs no extra samples). For
    /// non-linear projections the implementation chooses an appropriate
    /// sample count to approximate the geodesic to within reasonable
    /// visual error.
    ///
    /// **Recommended geom usage** (LineGeom, PolygonGeom, etc.):
    ///
    /// ```ignore
    /// let is_linear = ctx.projection.is_linear();
    /// let mut interior = Vec::new();
    /// let mut prev_channels: Option<[f64; 2]> = None;
    /// for vertex in row_iter {
    ///     let curr = [vertex.x_frac, vertex.y_frac];
    ///     if !is_linear {
    ///         if let Some(prev) = prev_channels {
    ///             interior.clear();
    ///             ctx.projection.interpolate_segment(panel, &prev, &curr, &mut interior);
    ///             for (px, py) in &interior {
    ///                 polyline.push(Point::new(*px, *py));
    ///             }
    ///         }
    ///     }
    ///     let (px, py) = ctx.projection.project_to_panel_px(panel, &curr);
    ///     polyline.push(Point::new(px, py));
    ///     prev_channels = Some(curr);
    /// }
    /// ```
    ///
    /// **Offsets and the densification path.** Per-row pixel offsets
    /// (`x_offset` / `y_offset`) apply to the vertex points only;
    /// interior densified points sit on the un-offset geodesic. This
    /// produces correct visuals when offsets are zero (the common
    /// case) and is "close enough" for small offsets. Large offsets
    /// combined with non-linear projections would visibly kink at
    /// each vertex — out of scope for v1.
    pub fn interpolate_segment(
        &self,
        panel: Rect,
        start_channels: &[f64],
        end_channels: &[f64],
        out: &mut Vec<(f64, f64)>,
    ) {
        // Delegate to the t-aware variant and drop the fractions —
        // single implementation, one chord-error calculation.
        let mut samples: Vec<InteriorSample> = Vec::new();
        self.interpolate_segment_with_t(panel, start_channels, end_channels, &mut samples);
        for s in samples {
            out.push((s.px, s.py));
        }
    }

    /// Like [`Self::interpolate_segment`] but also emits each interior
    /// sample's channel-space `t` fraction (`0 < t < 1`, exclusive of
    /// both endpoints).
    ///
    /// Geoms that carry **per-vertex auxiliary channels** that need
    /// to be interpolated across densified interior points use the
    /// `t` to lerp their channels:
    ///
    /// - per-vertex linewidth (variable-width strokes, ribbons)
    /// - per-vertex colour (gradient strokes, ribbon meshes)
    /// - per-vertex alpha / opacity
    ///
    /// ```ignore
    /// // Per-vertex ribbon usage:
    /// let mut samples = Vec::new();
    /// ctx.projection.interpolate_segment_with_t(
    ///     panel, &prev_ch, &curr_ch, &mut samples,
    /// );
    /// for s in &samples {
    ///     points.push(Point::new(s.px, s.py));
    ///     colors.push(lerp_color(prev_color, curr_color, s.t));
    ///     widths.push(prev_width + s.t * (curr_width - prev_width));
    /// }
    /// ```
    ///
    /// Geoms with **per-mark** auxiliary channels (all current geoms —
    /// `LineGeom` resolves stroke/linewidth at `i0` for the whole
    /// mark) don't need the `t`; use the simpler
    /// [`Self::interpolate_segment`].
    pub fn interpolate_segment_with_t(
        &self,
        panel: Rect,
        start_channels: &[f64],
        end_channels: &[f64],
        out: &mut Vec<InteriorSample>,
    ) {
        match self {
            Projection::Cartesian => {
                // No-op: straight segments need no interior samples.
            }
            Projection::Polar(p) => {
                let theta_a_frac = start_channels.first().copied().unwrap_or(0.0);
                let r_a_frac = start_channels.get(1).copied().unwrap_or(0.0);
                let theta_b_frac = end_channels.first().copied().unwrap_or(0.0);
                let r_b_frac = end_channels.get(1).copied().unwrap_or(0.0);

                match p.edge_style {
                    PolarEdgeStyle::Geodesic => {
                        polar_geodesic_samples(
                            p,
                            panel,
                            theta_a_frac,
                            r_a_frac,
                            theta_b_frac,
                            r_b_frac,
                            out,
                        );
                    }
                    PolarEdgeStyle::Chord => {
                        polar_chord_samples(
                            p,
                            panel,
                            theta_a_frac,
                            r_a_frac,
                            theta_b_frac,
                            r_b_frac,
                            out,
                        );
                    }
                }
            }
        }
    }
}

/// Geodesic densification: insert interior samples along the projected
/// arc so chord error stays below ~1 pixel.
fn polar_geodesic_samples(
    p: &PolarProjection,
    panel: Rect,
    theta_a_frac: f64,
    r_a_frac: f64,
    theta_b_frac: f64,
    r_b_frac: f64,
    out: &mut Vec<InteriorSample>,
) {
    let theta_a = p.theta_for_frac(theta_a_frac);
    let theta_b = p.theta_for_frac(theta_b_frac);
    let theta_delta = (theta_b - theta_a).abs();

    // Radial line (constant theta) projects to a straight pixel-space
    // line, needs no densification.
    if theta_delta < 1e-9 {
        return;
    }

    // Chord-error heuristic: for a circular arc of radius R and
    // angular extent θ, midpoint deviates from the chord by
    // R·(1 − cos(θ/2)) ≈ R·θ²/8 pixels. Pick the per-step angle so
    // chord error stays below ~1 pixel.
    let g = p.geometry(panel);
    let avg_r_frac = (r_a_frac + r_b_frac) * 0.5;
    let avg_r_px = (g.r_inner + avg_r_frac * (g.r_outer - g.r_inner)).max(1.0);
    let theta_step_max = (CHORD_ERROR_PX * 8.0 / avg_r_px).sqrt();
    let n_steps =
        ((theta_delta / theta_step_max).ceil() as usize).clamp(1, MAX_INTERPOLATION_STEPS);

    for i in 1..n_steps {
        let t = i as f64 / n_steps as f64;
        let theta_frac_i = theta_a_frac + t * (theta_b_frac - theta_a_frac);
        let r_frac_i = r_a_frac + t * (r_b_frac - r_a_frac);
        let (px, py) = p.project_frac(panel, theta_frac_i, r_frac_i);
        out.push(InteriorSample { px, py, t });
    }
}

/// Chord densification: emit one interior sample per break the segment
/// crosses, so the polyline bends at each category boundary instead of
/// cutting diagonally across spokes. Within a single between-break
/// span the chord is straight in pixel space; the non-linearity lives
/// purely at the break crossings.
///
/// When `theta_break_fracs` is empty (no categories configured), no
/// interior samples are emitted — the segment becomes a single
/// straight chord between the two projected endpoints. This degrades
/// gracefully to "naïve chord" rendering for callers that don't set
/// up breaks.
fn polar_chord_samples(
    p: &PolarProjection,
    panel: Rect,
    theta_a_frac: f64,
    r_a_frac: f64,
    theta_b_frac: f64,
    r_b_frac: f64,
    out: &mut Vec<InteriorSample>,
) {
    let theta_delta = theta_b_frac - theta_a_frac;
    // Same-theta segments: radial line, no break crossings possible.
    if theta_delta.abs() < 1e-12 {
        return;
    }

    // Collect t values of break crossings, strictly in (0, 1).
    let mut crossings: Vec<f64> = Vec::new();
    for &break_frac in &p.theta_break_fracs {
        let t = (break_frac - theta_a_frac) / theta_delta;
        if t > 1e-9 && t < 1.0 - 1e-9 {
            crossings.push(t);
        }
    }
    // Sweep direction may be either sign — sort ascending so the
    // emitted samples are in segment order.
    crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    for t in crossings {
        let theta_frac_i = theta_a_frac + t * theta_delta;
        let r_frac_i = r_a_frac + t * (r_b_frac - r_a_frac);
        let (px, py) = p.project_frac(panel, theta_frac_i, r_frac_i);
        out.push(InteriorSample { px, py, t });
    }
}

/// One interior sample emitted by
/// [`Projection::interpolate_segment_with_t`]. Carries the projected
/// pixel position plus the channel-space `t` fraction so callers can
/// interpolate per-vertex auxiliary channels (linewidth, colour,
/// alpha, …) between the segment's two endpoint values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InteriorSample {
    /// X pixel position.
    pub px: f64,
    /// Y pixel position.
    pub py: f64,
    /// Channel-space fraction along the segment, exclusive of both
    /// endpoints. For a segment A → B, `t = 0.5` means
    /// `0.5 * A + 0.5 * B` in channel space.
    pub t: f64,
}

/// Maximum chord-error tolerance (pixels) for [`Projection::interpolate_segment`].
/// Keeps polar arcs visually smooth on standard DPIs without exploding
/// the sample count on small radii.
const CHORD_ERROR_PX: f64 = 1.0;

/// Hard upper bound on interior samples per segment. Protects against
/// degenerate inputs (huge angular extents, tiny radii) producing
/// unbounded work.
const MAX_INTERPOLATION_STEPS: usize = 720;

/// True when `target` is in the sweep `[theta_start → theta_end]`
/// (going either CW or CCW depending on sign of the span). Accounts
/// for cyclic angle equivalence — `target ± k·2π` for small `k` is
/// considered the same physical angle.
fn angle_in_sweep(target: f64, theta_start: f64, theta_end: f64) -> bool {
    let span = theta_end - theta_start;
    if span.abs() < 1e-12 {
        return (target - theta_start).abs() < 1e-9;
    }
    for k in -2..=2 {
        let t_target = target + k as f64 * std::f64::consts::TAU;
        let t = (t_target - theta_start) / span;
        if (0.0..=1.0).contains(&t) {
            return true;
        }
    }
    false
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, msg: &str) {
        assert!((a - b).abs() < 1e-9, "{msg}: {a} ≠ {b}");
    }

    fn panel_400_300() -> Rect {
        Rect::new(50.0, 30.0, 450.0, 330.0)
    }

    #[test]
    fn cartesian_is_default() {
        let p = Projection::default();
        assert_eq!(p, Projection::Cartesian);
    }

    #[test]
    fn cartesian_consume_channels() {
        let p = Projection::Cartesian;
        assert_eq!(p.consume_channels(), &["x", "y"]);
    }

    #[test]
    fn cartesian_chrome_strategy_is_patch_slots() {
        assert_eq!(
            Projection::Cartesian.chrome_strategy(),
            ChromeStrategy::PatchSlots
        );
    }

    #[test]
    fn cartesian_origin_maps_to_bottom_left() {
        // x_frac=0, y_frac=0 → (panel.x0, panel.y1).
        let panel = panel_400_300();
        let (px, py) = Projection::Cartesian.project_to_panel_px(panel, &[0.0, 0.0]);
        approx(px, panel.x0, "x");
        approx(py, panel.y1, "y (bottom)");
    }

    #[test]
    fn cartesian_corner_maps_to_top_right() {
        // x_frac=1, y_frac=1 → (panel.x1, panel.y0).
        let panel = panel_400_300();
        let (px, py) = Projection::Cartesian.project_to_panel_px(panel, &[1.0, 1.0]);
        approx(px, panel.x1, "x");
        approx(py, panel.y0, "y (top)");
    }

    #[test]
    fn cartesian_centre_maps_to_panel_centre() {
        let panel = panel_400_300();
        let (px, py) = Projection::Cartesian.project_to_panel_px(panel, &[0.5, 0.5]);
        approx(px, (panel.x0 + panel.x1) * 0.5, "x");
        approx(py, (panel.y0 + panel.y1) * 0.5, "y");
    }

    #[test]
    fn cartesian_matches_legacy_inline_math() {
        // For a sweep of (x_frac, y_frac) values, the projection's
        // output must equal the pre-refactor inline math byte-for-byte.
        let panel = panel_400_300();
        let panel_w = panel.x1 - panel.x0;
        let panel_h = panel.y1 - panel.y0;
        for x_frac in [-0.5, 0.0, 0.25, 0.5, 0.75, 1.0, 1.5] {
            for y_frac in [-0.5, 0.0, 0.5, 1.0, 1.5] {
                let (px, py) = Projection::Cartesian.project_to_panel_px(panel, &[x_frac, y_frac]);
                let expected_px = panel.x0 + x_frac * panel_w;
                let expected_py = panel.y1 - y_frac * panel_h;
                approx(px, expected_px, "px");
                approx(py, expected_py, "py");
            }
        }
    }

    #[test]
    fn cartesian_short_slice_defaults_to_zero() {
        let panel = panel_400_300();
        let (px, py) = Projection::Cartesian.project_to_panel_px(panel, &[]);
        approx(px, panel.x0, "px");
        approx(py, panel.y1, "py");
    }

    #[test]
    fn cartesian_extra_channels_are_ignored() {
        let panel = panel_400_300();
        let (px, py) = Projection::Cartesian.project_to_panel_px(panel, &[0.25, 0.5, 999.0, 999.0]);
        let panel_w = panel.x1 - panel.x0;
        let panel_h = panel.y1 - panel.y0;
        approx(px, panel.x0 + 0.25 * panel_w, "px");
        approx(py, panel.y1 - 0.5 * panel_h, "py");
    }

    #[test]
    fn cartesian_is_linear() {
        assert!(Projection::Cartesian.is_linear());
    }

    #[test]
    fn cartesian_interpolate_segment_is_noop() {
        let panel = panel_400_300();
        // Pre-populated `out` should be untouched — Cartesian appends
        // zero interior points.
        let mut out = vec![(999.0, 999.0)];
        Projection::Cartesian.interpolate_segment(panel, &[0.0, 0.0], &[1.0, 1.0], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], (999.0, 999.0));
    }

    // ── Polar ──

    fn square_panel() -> Rect {
        Rect::new(0.0, 0.0, 400.0, 400.0)
    }

    fn approx_pt(actual: (f64, f64), expected: (f64, f64), tol: f64, msg: &str) {
        assert!(
            (actual.0 - expected.0).abs() < tol && (actual.1 - expected.1).abs() < tol,
            "{msg}: actual={actual:?}, expected={expected:?}"
        );
    }

    #[test]
    fn polar_is_not_linear() {
        assert!(!Projection::polar().is_linear());
        assert!(!Projection::gauge().is_linear());
    }

    #[test]
    fn polar_chrome_strategy_is_inside_panel() {
        assert_eq!(
            Projection::polar().chrome_strategy(),
            ChromeStrategy::InsidePanel
        );
        assert_eq!(
            Projection::gauge().chrome_strategy(),
            ChromeStrategy::InsidePanel
        );
    }

    #[test]
    fn polar_default_consume_channels() {
        let p = Projection::polar();
        let chans = p.consume_channels();
        assert_eq!(chans, vec!["x", "y"]);
    }

    #[test]
    fn polar_zero_radius_maps_to_panel_centre() {
        // r_frac=0 with inner_radius_frac=0 → centre of inscribed disk.
        let panel = square_panel();
        let proj = Projection::polar();
        for theta_frac in [0.0, 0.1, 0.25, 0.5, 0.75, 1.0] {
            let pt = proj.project_to_panel_px(panel, &[theta_frac, 0.0]);
            approx_pt(pt, (200.0, 200.0), 1e-9, "centre");
        }
    }

    #[test]
    fn polar_default_full_radius_at_zero_theta_is_top() {
        // Default starts at theta = π/2 = 12 o'clock visually. r=1 →
        // top of inscribed disk.
        let panel = square_panel();
        let pt = Projection::polar().project_to_panel_px(panel, &[0.0, 1.0]);
        approx_pt(pt, (200.0, 0.0), 1e-9, "12 o'clock top");
    }

    #[test]
    fn polar_default_clockwise_sweep_at_quarters() {
        // Default sweep is CW from 12 o'clock through 3, 6, 9 back to 12.
        let panel = square_panel();
        let proj = Projection::polar();
        // theta_frac=0.25 → 3 o'clock (right) at full radius.
        approx_pt(
            proj.project_to_panel_px(panel, &[0.25, 1.0]),
            (400.0, 200.0),
            1e-9,
            "3 o'clock",
        );
        // theta_frac=0.5 → 6 o'clock (bottom).
        approx_pt(
            proj.project_to_panel_px(panel, &[0.5, 1.0]),
            (200.0, 400.0),
            1e-9,
            "6 o'clock",
        );
        // theta_frac=0.75 → 9 o'clock (left).
        approx_pt(
            proj.project_to_panel_px(panel, &[0.75, 1.0]),
            (0.0, 200.0),
            1e-9,
            "9 o'clock",
        );
    }

    #[test]
    fn polar_non_square_panel_uses_inscribed_square() {
        // Wide panel: 600 × 300 → inscribed square is 300 × 300
        // centred at (300, 150). r=1 at 12 o'clock → (300, 0).
        let panel = Rect::new(0.0, 0.0, 600.0, 300.0);
        let proj = Projection::polar();
        approx_pt(
            proj.project_to_panel_px(panel, &[0.0, 1.0]),
            (300.0, 0.0),
            1e-9,
            "12 o'clock on wide panel",
        );
        // 3 o'clock at full radius → (300 + 150, 150) = (450, 150).
        approx_pt(
            proj.project_to_panel_px(panel, &[0.25, 1.0]),
            (450.0, 150.0),
            1e-9,
            "3 o'clock on wide panel",
        );
    }

    #[test]
    fn polar_inner_radius_frac_offsets_origin() {
        // Gauge: theta_start = π (9 o'clock), theta_end = 0 (3 o'clock).
        // bbox = (-1, 0, 1, 1) → aspect 2:1. On a 400×400 panel
        // (aspect 1), fit-to-width gives scale = 200, scaled-bbox =
        // 400×200, vertically centred (bbox_y0_px = 100). Polar
        // centre (math 0,0) lives at bottom of bbox = panel.y0 + 300.
        // Inner radius (frac 0.4) = 80 px.
        let panel = square_panel();
        let proj = Projection::gauge();
        let pt = proj.project_to_panel_px(panel, &[0.0, 0.0]);
        // theta_frac=0 → 9 o'clock at inner radius → (200 - 80, 300).
        approx_pt(pt, (120.0, 300.0), 1e-9, "gauge inner @ 9 o'clock");
        // r=1 → full radius along 9 o'clock spoke → (0, 300).
        let pt = proj.project_to_panel_px(panel, &[0.0, 1.0]);
        approx_pt(pt, (0.0, 300.0), 1e-9, "gauge outer @ 9 o'clock");
    }

    #[test]
    fn polar_gauge_partial_arc_endpoints() {
        // Gauge on 400×400 panel: bbox-aware geometry centres the
        // half-disk so it spans the bbox's full width and height.
        // Centre lands at (200, 300); r_outer = 200.
        let panel = square_panel();
        let proj = Projection::gauge();
        // theta_frac=0 → 9 o'clock at full radius → (0, 300).
        approx_pt(
            proj.project_to_panel_px(panel, &[0.0, 1.0]),
            (0.0, 300.0),
            1e-9,
            "gauge start (9 o'clock)",
        );
        // theta_frac=1 → 3 o'clock at full radius → (400, 300).
        approx_pt(
            proj.project_to_panel_px(panel, &[1.0, 1.0]),
            (400.0, 300.0),
            1e-9,
            "gauge end (3 o'clock)",
        );
        // theta_frac=0.5 → 12 o'clock = top of bbox → (200, 100).
        approx_pt(
            proj.project_to_panel_px(panel, &[0.5, 1.0]),
            (200.0, 100.0),
            1e-9,
            "gauge middle (12 o'clock)",
        );
    }

    #[test]
    fn polar_full_circle_bounding_box_is_unit_square() {
        let (mn_x, mn_y, mx_x, mx_y) = PolarProjection::full_circle().bounding_box_units();
        approx_pt((mn_x, mn_y), (-1.0, -1.0), 1e-9, "full-circle min");
        approx_pt((mx_x, mx_y), (1.0, 1.0), 1e-9, "full-circle max");
    }

    #[test]
    fn polar_gauge_bounding_box_is_top_half() {
        // theta_start = π, theta_end = 0, inner_radius_frac = 0.4.
        // Sweep covers angles 0 ≤ θ ≤ π → x ∈ [-1, 1], y ∈ [0, 1].
        let (mn_x, mn_y, mx_x, mx_y) = PolarProjection::gauge().bounding_box_units();
        approx_pt((mn_x, mn_y), (-1.0, 0.0), 1e-9, "gauge min");
        approx_pt((mx_x, mx_y), (1.0, 1.0), 1e-9, "gauge max");
    }

    #[test]
    fn polar_quarter_pie_bounding_box() {
        // theta_start = 0, theta_end = π/2 (first quadrant), filled
        // pie (no inner radius). Sweep includes angles in [0, π/2]
        // and the origin → x ∈ [0, 1], y ∈ [0, 1].
        let pie = PolarProjection {
            theta_start: 0.0,
            theta_end: std::f64::consts::FRAC_PI_2,
            ..PolarProjection::full_circle()
        };
        let (mn_x, mn_y, mx_x, mx_y) = pie.bounding_box_units();
        approx_pt((mn_x, mn_y), (0.0, 0.0), 1e-9, "quarter-pie min");
        approx_pt((mx_x, mx_y), (1.0, 1.0), 1e-9, "quarter-pie max");
    }

    #[test]
    fn polar_gauge_uses_full_panel_width_on_tall_panel() {
        // 200×400 panel: panel aspect 0.5, gauge bbox aspect 2.
        // Bbox > panel → fit to width: scale = 100, scaled bbox 200×100.
        // Vertically centred at y_mid_px = 150.
        let panel = Rect::new(0.0, 0.0, 200.0, 400.0);
        let proj = Projection::gauge();
        // 9 o'clock at full radius → (0, 250).
        approx_pt(
            proj.project_to_panel_px(panel, &[0.0, 1.0]),
            (0.0, 250.0),
            1e-9,
            "gauge 9 o'clock on tall panel",
        );
        // 3 o'clock at full radius → (200, 250).
        approx_pt(
            proj.project_to_panel_px(panel, &[1.0, 1.0]),
            (200.0, 250.0),
            1e-9,
            "gauge 3 o'clock on tall panel",
        );
    }

    #[test]
    fn polar_interpolate_segment_radial_line_emits_nothing() {
        // Same theta, different radius → straight pixel-space line.
        let panel = square_panel();
        let proj = Projection::polar();
        let mut out = Vec::new();
        proj.interpolate_segment(panel, &[0.25, 0.0], &[0.25, 1.0], &mut out);
        assert!(out.is_empty(), "expected no interior points: {out:?}");
    }

    #[test]
    fn polar_interpolate_segment_arc_emits_samples() {
        // Quarter-arc at full radius — should produce a fair number of
        // samples to keep chord error below 1 px on a 200-pixel radius.
        let panel = square_panel();
        let proj = Projection::polar();
        let mut out = Vec::new();
        proj.interpolate_segment(panel, &[0.0, 1.0], &[0.25, 1.0], &mut out);
        // At R=200, theta_step_max = sqrt(8/200) ≈ 0.2 rad. Quarter
        // arc = π/2 ≈ 1.57 rad → n_steps ≈ 8.
        assert!(
            out.len() >= 5,
            "expected ≥5 interior samples, got {}",
            out.len()
        );
        assert!(out.len() < 20, "too many samples: {}", out.len());
        // All samples should sit on the unit-radius circle centred at
        // (200, 200) — within chord-error tolerance.
        for (px, py) in &out {
            let d = ((*px - 200.0).powi(2) + (*py - 200.0).powi(2)).sqrt();
            assert!(
                (d - 200.0).abs() < 2.0,
                "sample {:?} not on circle (d={d})",
                (px, py)
            );
        }
    }

    #[test]
    fn cartesian_interpolate_segment_with_t_is_noop() {
        let panel = panel_400_300();
        let mut out: Vec<InteriorSample> = Vec::new();
        Projection::Cartesian.interpolate_segment_with_t(panel, &[0.0, 0.0], &[1.0, 1.0], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn polar_interpolate_segment_with_t_yields_evenly_spaced_t() {
        // Quarter-arc at full radius. Interior samples should have
        // their `t` strictly increasing in (0, 1).
        let panel = square_panel();
        let proj = Projection::polar();
        let mut out: Vec<InteriorSample> = Vec::new();
        proj.interpolate_segment_with_t(panel, &[0.0, 1.0], &[0.25, 1.0], &mut out);
        assert!(!out.is_empty());
        // First sample's t > 0; last sample's t < 1.
        assert!(out.first().unwrap().t > 0.0);
        assert!(out.last().unwrap().t < 1.0);
        // Monotonic.
        for w in out.windows(2) {
            assert!(w[1].t > w[0].t, "t not monotonic: {:?}", out);
        }
        // Position matches `project_frac(t)` at each step.
        if let Projection::Polar(p) = &proj {
            for s in &out {
                let theta_frac = 0.0 + s.t * (0.25 - 0.0);
                let r_frac = 1.0;
                let (px, py) = p.project_frac(panel, theta_frac, r_frac);
                approx_pt((s.px, s.py), (px, py), 1e-9, "sample position");
            }
        }
    }

    #[test]
    fn polar_interpolate_segment_and_with_t_agree_on_positions() {
        // The two variants must emit the same interior pixel
        // positions — only the second one additionally yields t.
        let panel = square_panel();
        let proj = Projection::polar();
        let mut a: Vec<(f64, f64)> = Vec::new();
        let mut b: Vec<InteriorSample> = Vec::new();
        proj.interpolate_segment(panel, &[0.0, 1.0], &[0.25, 1.0], &mut a);
        proj.interpolate_segment_with_t(panel, &[0.0, 1.0], &[0.25, 1.0], &mut b);
        assert_eq!(a.len(), b.len());
        for (ap, bs) in a.iter().zip(b.iter()) {
            approx_pt(*ap, (bs.px, bs.py), 1e-9, "agree");
        }
    }

    // ── Radar (Polar with Chord edge style) ──

    #[test]
    fn radar_is_still_non_linear() {
        // A polyline crossing one or more category breaks bends at
        // each break — that's a non-linearity in pixel space, even
        // though each *between-break* segment is a straight chord.
        assert!(!Projection::radar(6).is_linear());
    }

    #[test]
    fn radar_interpolate_segment_emits_one_sample_per_break_crossing() {
        // 6-category radar with band-centre breaks at
        // [1/12, 3/12, 5/12, 7/12, 9/12, 11/12]. A segment from
        // 0.05 → 0.45 crosses 1/12 (≈0.0833) and 3/12 (0.25) and
        // 5/12 (≈0.4167) → expect 3 interior samples.
        let panel = square_panel();
        let proj = Projection::radar(6);
        let mut out = Vec::new();
        proj.interpolate_segment_with_t(panel, &[0.05, 0.5], &[0.45, 0.5], &mut out);
        assert_eq!(out.len(), 3, "expected 3 break crossings: {out:?}");
        // Crossings at t = (break - 0.05) / (0.45 - 0.05).
        let span = 0.45 - 0.05;
        let t0_expected = (1.0 / 12.0 - 0.05) / span;
        let t1_expected = (3.0 / 12.0 - 0.05) / span;
        let t2_expected = (5.0 / 12.0 - 0.05) / span;
        assert!((out[0].t - t0_expected).abs() < 1e-9);
        assert!((out[1].t - t1_expected).abs() < 1e-9);
        assert!((out[2].t - t2_expected).abs() < 1e-9);
    }

    #[test]
    fn radar_segment_inside_one_break_span_emits_nothing() {
        // Segment entirely between two adjacent band-centre breaks
        // → straight chord, no bend, no interior samples.
        let panel = square_panel();
        let proj = Projection::radar(6);
        let mut out = Vec::new();
        // From t=0.10 to t=0.20 — both in the (1/12, 3/12) span.
        proj.interpolate_segment(panel, &[0.10, 0.3], &[0.20, 0.7], &mut out);
        assert!(out.is_empty(), "expected no break crossings: {out:?}");
    }

    #[test]
    fn radar_segment_with_no_configured_breaks_emits_nothing() {
        // Degenerate radar (no theta_break_fracs) → naïve chord,
        // no break-awareness — even though the projection is still
        // chord-style.
        let panel = square_panel();
        let radar_no_breaks = Projection::Polar(PolarProjection {
            edge_style: PolarEdgeStyle::Chord,
            theta_break_fracs: Vec::new(),
            ..PolarProjection::full_circle()
        });
        let mut out = Vec::new();
        radar_no_breaks.interpolate_segment(panel, &[0.0, 1.0], &[0.5, 1.0], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn radar_point_projection_matches_polar_at_same_angle_and_radius() {
        // The point-to-pixel math is identical between Geodesic and
        // Chord — only the edge interpretation differs.
        let panel = square_panel();
        let radar_cw = Projection::Polar(PolarProjection {
            edge_style: PolarEdgeStyle::Chord,
            theta_break_fracs: Vec::new(),
            ..PolarProjection::full_circle()
        });
        let polar = Projection::polar();
        for (theta_frac, r_frac) in [(0.0, 1.0), (0.25, 1.0), (0.5, 0.5), (0.75, 0.0)] {
            let a = polar.project_to_panel_px(panel, &[theta_frac, r_frac]);
            let b = radar_cw.project_to_panel_px(panel, &[theta_frac, r_frac]);
            approx_pt(a, b, 1e-9, "polar vs radar (cw)");
        }
    }

    #[test]
    fn radar_chrome_strategy_is_inside_panel() {
        assert_eq!(
            Projection::radar(6).chrome_strategy(),
            ChromeStrategy::InsidePanel
        );
    }

    #[test]
    fn radar_default_has_n_band_centre_break_fracs() {
        if let Projection::Polar(p) = Projection::radar(6) {
            assert_eq!(p.theta_break_fracs.len(), 6);
            // Band centres: (i + 0.5) / N for i in 0..N — matches
            // `scale::discrete(N entries).map(entry_i)`.
            for (i, frac) in p.theta_break_fracs.iter().enumerate() {
                assert!((frac - (i as f64 + 0.5) / 6.0).abs() < 1e-9);
            }
        } else {
            panic!("expected Polar");
        }
    }

    #[test]
    fn polar_interpolate_segment_smaller_radius_needs_fewer_samples() {
        // Same angular extent but smaller radius → smaller chord error
        // per step → fewer steps needed.
        let panel = square_panel();
        let proj = Projection::polar();
        let mut small_r = Vec::new();
        let mut large_r = Vec::new();
        // Quarter-arc at r=0.1 (≈ 20 px) vs r=1.0 (≈ 200 px).
        proj.interpolate_segment(panel, &[0.0, 0.1], &[0.25, 0.1], &mut small_r);
        proj.interpolate_segment(panel, &[0.0, 1.0], &[0.25, 1.0], &mut large_r);
        assert!(
            small_r.len() < large_r.len(),
            "small_r={} large_r={}",
            small_r.len(),
            large_r.len()
        );
    }
}
