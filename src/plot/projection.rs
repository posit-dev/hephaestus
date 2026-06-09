//! Coordinate projection — converts per-channel panel-fractions into
//! pixel positions inside the panel rect.
//!
//! v1 ships [`Projection::Cartesian`] only. The signature is N-channel
//! (`project_to_panel_px(panel, &[f64])`) so future variants — Polar
//! (E.3b) and Ternary (deferred) — drop in without touching geom code.
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
//! For Cartesian — the only variant in v1 — the projection collapses to
//! exactly the inlined math above. The match arm is monomorphic and the
//! compiler optimises it back to a direct multiply.
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
    /// **Note for the E.3b implementation:** even with `InsidePanel`,
    /// tick / break *labels* typically need to bleed *outside* the
    /// panel rect (e.g. polar theta labels sit just beyond the
    /// outermost circle). The polar bleed amount is exactly
    /// calculable from the max label dimension projected along each
    /// cardinal direction — partial-arc configurations only contribute
    /// bleed on the sides covered by active spokes. The plan: have
    /// `Plot::wire` populate the four axis slots with polar-specific
    /// `Measure`s whose thickness comes from this calculation, so the
    /// layout solver reserves the strips and `draw_chrome_into` can
    /// bleed labels into them. Reuses the existing slot machinery
    /// instead of inventing a new "chrome bleed" concept.
    InsidePanel,
}

/// Coordinate projection. v1 ships [`Self::Cartesian`] only.
///
/// The N-channel `project_to_panel_px(panel, &[f64])` signature is
/// designed so future variants that consume more than two channels
/// (Ternary's three barycentric coords) can drop in without changing
/// the geom call sites.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Projection {
    /// Identity over `(x, y)`. The fraction-to-pixel map is the
    /// canonical `panel.x0 + x_frac * panel_w`, `panel.y1 - y_frac *
    /// panel_h` (y flips so positive y maps "up" visually). The only
    /// variant in v1.
    #[default]
    Cartesian,
    // Polar(PolarProjection) — added in E.3b.
    // Ternary(TernaryProjection) — deferred (design accommodated via
    // the N-channel signature).
}

impl Projection {
    /// Convenience: the default Cartesian projection.
    pub const fn cartesian() -> Self {
        Projection::Cartesian
    }

    /// Channel names this projection reads, in argument order for
    /// [`Self::project_to_panel_px`]. Geoms that want to be
    /// projection-aware (future polar / ternary geoms) consult this to
    /// know which scales to look up; Cartesian-only geoms just hardcode
    /// `"x"` / `"y"` and pass `&[x_frac, y_frac]`.
    pub const fn consume_channels(&self) -> &'static [&'static str] {
        match self {
            Projection::Cartesian => &["x", "y"],
        }
    }

    /// Map per-channel panel-fractions to a pixel position inside
    /// `panel`. `channels` is parallel to [`Self::consume_channels`];
    /// Cartesian reads positions 0 and 1.
    ///
    /// Missing channels (slice shorter than `consume_channels().len()`)
    /// default to `0.0`. Extra channels are ignored. Both behaviours
    /// keep this function tolerant of geoms passing 2-element slices
    /// even when the projection theoretically reads more.
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
        }
    }

    /// Where this projection wants its axis chrome drawn.
    pub const fn chrome_strategy(&self) -> ChromeStrategy {
        match self {
            Projection::Cartesian => ChromeStrategy::PatchSlots,
        }
    }

    /// `true` when channel-space straight lines map to panel-space
    /// straight lines. Cartesian is linear; Polar / future Ternary are
    /// not.
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
        match self {
            Projection::Cartesian => true,
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
        _panel: Rect,
        _start_channels: &[f64],
        _end_channels: &[f64],
        _out: &mut Vec<(f64, f64)>,
    ) {
        match self {
            Projection::Cartesian => {
                // No-op: straight segments need no interior samples.
            }
        }
    }
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
}
