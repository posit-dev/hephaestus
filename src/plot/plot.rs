//! `Plot` — a per-patch unit of plotting state.
//!
//! A `Plot` is bound to a named patch in a user-supplied
//! [`Composition`](crate::composition::Composition) and stores:
//!
//! - Channel → scale-name bindings (the orchestrator's scale registry
//!   carries the actual scales).
//! - The geom list (heterogeneous `Box<dyn Geom>`).
//! - Title / subtitle / caption / axis-title text.
//! - The shape registry.
//!
//! Plot is the lower-level surface; the canonical user-facing surface
//! is the (Phase 7) `PlotComposition` orchestrator that owns a
//! [`ScaleRegistry`] and a `HashMap<String, Plot>` and drives the full
//! `wire → solve → draw_chrome → draw_panel` flow with dirty tracking.
//! Stand-alone Plot use is supported for tests and one-off renders.
//!
//! See `we-are-approaching-the-binary-kitten.md` for the full design.

use std::collections::HashMap;
use std::sync::Arc;

use crate::composition::{Composition, Slot};
use crate::geometry::Rect;
use crate::scene::SceneBuilder;
use crate::shape::ShapeRegistry;

use super::geom::{Geom, GeomContext, ScaleResolver};
use super::scale::{Scale, ScaleRegistry};
use crate::scales::input::InputRange;

#[cfg(feature = "text")]
use super::scale::AxisSide;
#[cfg(feature = "text")]
use crate::composition::Patch;
#[cfg(feature = "text")]
use crate::layout::Cell;

// ─── Identifiers ─────────────────────────────────────────────────────────────

/// Stable identifier returned by [`Plot::add_geom`]. Use it with
/// [`Plot::update_geom`] / [`Plot::remove_geom`] to address a specific
/// geom later. Internal; the value isn't user-meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GeomId(pub u32);

/// How the plot's fixed-aspect constraint is enforced. Applies to
/// every projection family, with per-family interpretation:
///
/// - **Cartesian / Custom** — the constraint is the data-space ratio
///   set via [`Plot::aspect_ratio`].
/// - **Polar** — the constraint is the projection's bbox aspect (e.g.
///   `2:1` for a half-disk gauge, `1:1` for a full circle). No
///   [`Plot::aspect_ratio`] is needed; the bbox supplies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AspectMode {
    /// Lock the panel rect to match the constraint. For Cartesian /
    /// Custom that's `(x_extent * ratio, y_extent)`; for Polar that's
    /// the projection's bbox aspect. The layout solver shrinks the
    /// panel to honor it; surrounding tracks absorb the slack. This
    /// is the default and matches ggplot's `coord_fixed` /
    /// `coord_polar`.
    #[default]
    Panel,
    /// Leave the panel free to fill its layout cell; honor the
    /// constraint inside the panel instead. For Cartesian / Custom
    /// the bound x or y scale's input range is symmetrically expanded
    /// so one x-unit takes `ratio` times the screen space of one
    /// y-unit at the actual panel aspect. For Polar the inscribed
    /// disk is centred in the panel — empty space appears on the
    /// sides that don't match the bbox aspect.
    Range,
}

// ─── Always-available chrome helpers ─────────────────────────────────────────

impl Plot {
    /// Add just the `Slot::Panel` cell to `patch`. Always available —
    /// does not require the `text` feature. The full
    /// [`Self::wire`] (text-feature only) calls this internally; the
    /// orchestrator's render flow calls this when chrome is unavailable
    /// (`text` feature off) so the panel rect still appears in the
    /// solved layout for [`Self::draw_panel_into`] to find.
    pub fn wire_panel(&self, patch: crate::composition::Patch) -> crate::composition::Patch {
        patch.slot(Slot::Panel, crate::layout::Cell::empty())
    }
}

// ─── Plot ────────────────────────────────────────────────────────────────────

/// A view spec bound to a named patch. Carries channel→scale-name
/// bindings and a list of geoms; the scales themselves live in a
/// [`ScaleRegistry`] (owned by the orchestrator in the canonical flow).
pub struct Plot {
    patch_id: Arc<str>,
    bindings: HashMap<String, String>,
    geoms: Vec<(GeomId, Box<dyn Geom>)>,
    next_geom_id: u32,

    // Chrome text.
    title: Option<String>,
    subtitle: Option<String>,
    caption: Option<String>,

    shapes: ShapeRegistry,

    /// Axes attached to this plot. Composed explicitly via
    /// [`Self::add_axis`]; no axis is rendered unless the caller
    /// adds one. Same opt-in model as legends.
    #[cfg(feature = "text")]
    axes: Vec<crate::plot::chrome::axis::Axis>,
    #[cfg(feature = "text")]
    next_axis_id: u32,

    /// Legends attached to this plot. Composed explicitly by the
    /// caller via [`Self::add_legend`] / [`Self::add_legend_separate`].
    /// Nothing is inferred from `bindings`. Gated on `feature = "text"`
    /// because `Legend` references types from the text-gated chrome
    /// module.
    #[cfg(feature = "text")]
    legends: Vec<crate::plot::chrome::legend::Legend>,
    /// Next [`LegendId`] to hand out from `add_legend*`.
    #[cfg(feature = "text")]
    next_legend_id: u32,

    /// Coordinate projection. v1 ships `Cartesian` only — geom output
    /// is unchanged from the pre-projection era. E.3b introduces
    /// `Polar` for partial-arc / gauge layouts.
    projection: crate::plot::projection::Projection,

    /// Whether geoms are clipped to the projection's outline when
    /// drawn. `true` by default. When `false`, geoms can spill
    /// beyond the panel (occasionally useful for debug renders or
    /// when the outline is itself decorative). Always uses the
    /// projection's outline (rect for cartesian, circle / arc /
    /// polygon for polar) so clipping behaves consistently across
    /// projections.
    clip: bool,

    /// Patch-wide background fill — covers panel + axes + titles +
    /// padding, but not the outer margin. `None` by default (no
    /// fill; the canvas colour shows through). Painted in the
    /// orchestrator's first render pass across all plots, before
    /// any panel chrome / geom is drawn.

    /// Tracked for the orchestrator's partial-repaint heuristics; not
    /// currently consulted by the draw path.
    #[allow(dead_code)]
    dirty: bool,

    /// Data-space aspect ratio for cartesian plots — how much screen
    /// space one x-axis unit takes compared to one y-axis unit. A
    /// ratio of `2.0` makes one x-unit twice as wide on screen as one
    /// y-unit is tall (matching `coord_fixed(ratio = 0.5)` in ggplot:
    /// the convention here is x:y, where ggplot's `ratio` is y/x).
    /// `None` (the default) lets the panel flex.
    ///
    /// Ignored when the projection is non-cartesian — polar
    /// projections compute their own aspect from their bounding box.
    cartesian_aspect_ratio: Option<f64>,

    /// Which strategy enforces [`Self::cartesian_aspect_ratio`].
    /// [`AspectMode::Panel`] (the default) locks the patch's panel
    /// rect; [`AspectMode::Range`] keeps the panel flexible and
    /// expands the bound x or y scale's input range at draw time.
    aspect_mode: AspectMode,

    /// Optional per-plot theme override. When set, the orchestrator
    /// merges this on top of the composition's theme before
    /// rendering this plot. `None` (the default) means the plot
    /// uses the composition's theme unchanged.
    theme_override: Option<crate::plot::theme::ThemePart>,

    /// Facet-strip labels, one per [`AxisSide`]. Indexed by
    /// [`axis_side_index`]; `None` = no strip on that side. Each
    /// `Some(text)` reserves the matching `StripTop` / `StripRight` /
    /// `StripBottom` / `StripLeft` slot and renders against the
    /// theme's `strip_background` / `strip_text` / `strip_padding`.
    #[cfg(feature = "text")]
    strips: [Option<String>; 4],
}

/// Index into [`Plot::strips`] for the given [`AxisSide`]. Order is
/// Top / Right / Bottom / Left so iteration follows the same
/// clockwise convention used for wire / draw passes.
#[cfg(feature = "text")]
pub(crate) fn axis_side_index(side: AxisSide) -> usize {
    match side {
        AxisSide::Top => 0,
        AxisSide::Right => 1,
        AxisSide::Bottom => 2,
        AxisSide::Left => 3,
    }
}

/// Iteration order over all four `AxisSide` variants — used by the
/// strip wire / draw passes so the per-side loops match
/// [`axis_side_index`].
#[cfg(feature = "text")]
pub(crate) const STRIP_SIDES: [AxisSide; 4] = [
    AxisSide::Top,
    AxisSide::Right,
    AxisSide::Bottom,
    AxisSide::Left,
];

impl Plot {
    /// Bind a plot to the named patch in `composition`. Panics if no
    /// patch with `patch_id` exists in the composition tree. The
    /// composition reference is borrowed only for id validation; nothing
    /// about it is captured on the Plot.
    pub fn new(composition: &Composition, patch_id: impl Into<String>) -> Self {
        let patch_id: String = patch_id.into();
        if !composition.contains_patch_id(&patch_id) {
            panic!("Plot::new: no patch with id {patch_id:?} in the composition");
        }
        Self {
            patch_id: Arc::from(patch_id),
            bindings: HashMap::new(),
            geoms: Vec::new(),
            next_geom_id: 0,
            title: None,
            subtitle: None,
            caption: None,
            shapes: ShapeRegistry::with_builtins(),
            #[cfg(feature = "text")]
            axes: Vec::new(),
            #[cfg(feature = "text")]
            next_axis_id: 0,
            #[cfg(feature = "text")]
            legends: Vec::new(),
            #[cfg(feature = "text")]
            next_legend_id: 0,
            projection: crate::plot::projection::Projection::Cartesian,
            clip: true,
            dirty: true,
            cartesian_aspect_ratio: None,
            aspect_mode: AspectMode::default(),
            theme_override: None,
            #[cfg(feature = "text")]
            strips: [None, None, None, None],
        }
    }

    /// Install a per-plot theme override. The orchestrator merges
    /// this on top of the composition's theme before rendering this
    /// plot. Chainable builder form of [`Self::set_theme_override`].
    pub fn theme_override(mut self, part: crate::plot::theme::ThemePart) -> Self {
        self.theme_override = Some(part);
        self
    }

    /// Install or clear the per-plot theme override.
    pub fn set_theme_override(&mut self, part: Option<crate::plot::theme::ThemePart>) {
        self.theme_override = part;
    }

    /// Borrow the per-plot theme override, if any.
    pub fn theme_override_ref(&self) -> Option<&crate::plot::theme::ThemePart> {
        self.theme_override.as_ref()
    }

    /// Lock the panel's data-space aspect ratio to `ratio` (x-unit to
    /// y-unit). With `ratio = 2.0`, one x-axis unit takes up twice
    /// the screen space as one y-axis unit — equivalent to
    /// ggplot's `coord_fixed(ratio = 0.5)`. Computed against each
    /// scale's input-range extent at wire time; the patch's panel
    /// is then aspect-locked to `(x_extent * ratio, y_extent)`.
    ///
    /// Applies to [`Projection::Cartesian`](crate::plot::projection::Projection::Cartesian)
    /// and [`Projection::Custom`](crate::plot::projection::Projection::Custom),
    /// which share the same scale-extent math. Polar projections
    /// supply their own ratio from the bbox and ignore this setting —
    /// use [`Self::aspect_mode`] to control whether that bbox aspect
    /// locks the panel or merely centres the disk.
    pub fn aspect_ratio(mut self, ratio: f64) -> Self {
        self.cartesian_aspect_ratio = if ratio.is_finite() && ratio > 0.0 {
            Some(ratio)
        } else {
            None
        };
        self
    }

    /// Pick which strategy enforces the plot's fixed-aspect
    /// constraint. [`AspectMode::Panel`] (the default) locks the
    /// panel rect; [`AspectMode::Range`] keeps the panel filling its
    /// layout cell and honors the constraint inside the panel
    /// instead — for Cartesian / Custom by expanding the bound x / y
    /// scale's input range, for Polar by centring the inscribed disk
    /// in the available rect. Has no effect under Cartesian / Custom
    /// when [`Self::aspect_ratio`] is unset (no constraint to enforce
    /// either way).
    pub fn aspect_mode(mut self, mode: AspectMode) -> Self {
        self.aspect_mode = mode;
        self
    }

    /// Read the current aspect-ratio enforcement strategy.
    pub fn aspect_mode_ref(&self) -> AspectMode {
        self.aspect_mode
    }

    /// Override whether geoms are clipped to the projection's
    /// outline (default `true`). Set to `false` to let geoms spill
    /// past the panel boundary.
    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }

    /// Read accessor for the bound patch id.
    pub fn patch_id(&self) -> &str {
        &self.patch_id
    }

    /// Borrow the coordinate projection. Defaults to
    /// [`Projection::Cartesian`](crate::plot::projection::Projection);
    /// override via [`Self::projection`].
    pub fn projection_ref(&self) -> &crate::plot::projection::Projection {
        &self.projection
    }

    /// Aspect ratio this plot wants its panel cell locked to, as a
    /// `(width, height)` ratio. `None` means "I don't care; let the
    /// layout flex".
    ///
    /// - **Cartesian without `aspect_ratio`**: `None` — flex.
    /// - **Cartesian with `aspect_ratio = r`**: `(x_extent * r,
    ///   y_extent)` so one x-unit takes `r` times the screen space
    ///   of one y-unit. Requires both `"x"` and `"y"` bindings to
    ///   resolve to continuous scales with finite extents; returns
    ///   `None` otherwise.
    /// - **Polar with `fit_to_bbox = true`** (the default): the
    ///   projection's bbox aspect (e.g. `2:1` for a half-disk
    ///   gauge, `1:1` for a full circle), so the inscribed
    ///   projection geometry fills the panel without slack.
    /// - **Polar with `fit_to_bbox = false`**: `1:1` (a square
    ///   panel; the largest inscribed disk fills it).
    /// - **Any projection with `aspect_mode = Range`**: `None` —
    ///   the panel flexes and the constraint is honoured inside
    ///   the panel rect at draw time.
    ///
    /// The orchestrator collects each attached plot's aspect on a
    /// patch and locks the patch to it when every plot agrees; if
    /// they disagree it leaves the patch unlocked.
    pub fn desired_panel_aspect(&self, registry: &ScaleRegistry) -> Option<(f32, f32)> {
        match &self.projection {
            crate::plot::projection::Projection::Cartesian
            | crate::plot::projection::Projection::Custom(_) => {
                // Custom shares Cartesian's aspect math — its polygon
                // outline shapes the drawing surface but does not drive
                // aspect; the bound x/y scale extents do.
                let ratio = self.cartesian_aspect_ratio?;
                // Range mode honors the ratio by expanding scale ranges
                // at draw time instead of locking the panel.
                if self.aspect_mode == AspectMode::Range {
                    return None;
                }
                let x_binding = match &self.projection {
                    crate::plot::projection::Projection::Custom(c) => c.x_channel.as_str(),
                    _ => "x",
                };
                let y_binding = match &self.projection {
                    crate::plot::projection::Projection::Custom(c) => c.y_channel.as_str(),
                    _ => "y",
                };
                let x_extent = self
                    .bindings
                    .get(x_binding)
                    .and_then(|n| registry.get(n))
                    .and_then(|s| s.input_range())
                    .and_then(|r| r.extent())?;
                let y_extent = self
                    .bindings
                    .get(y_binding)
                    .and_then(|n| registry.get(n))
                    .and_then(|s| s.input_range())
                    .and_then(|r| r.extent())?;
                if !(x_extent > 0.0 && y_extent > 0.0) {
                    return None;
                }
                let w = (x_extent * ratio) as f32;
                let h = y_extent as f32;
                if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
                    Some((w, h))
                } else {
                    None
                }
            }
            crate::plot::projection::Projection::Polar(p) => {
                // Range mode centres the inscribed disk in whatever
                // panel rect the layout produces — no patch lock.
                if self.aspect_mode == AspectMode::Range {
                    return None;
                }
                if p.fit_to_bbox {
                    let (min_x, min_y, max_x, max_y) = p.bounding_box_units();
                    let bbox_w = (max_x - min_x) as f32;
                    let bbox_h = (max_y - min_y) as f32;
                    if bbox_w.is_finite() && bbox_h.is_finite() && bbox_w > 0.0 && bbox_h > 0.0 {
                        Some((bbox_w, bbox_h))
                    } else {
                        None
                    }
                } else {
                    Some((1.0, 1.0))
                }
            }
        }
    }

    /// Set the coordinate projection (consumes self; builder-style).
    /// v1 ships only `Cartesian` (default) — output is unchanged from
    /// the pre-projection era. E.3b introduces `Polar`.
    pub fn projection(mut self, p: crate::plot::projection::Projection) -> Self {
        self.projection = p;
        self
    }

    // ── Chaining (config) ──

    /// Set the plot's title, rendered in the [`Slot::Title`] chrome slot.
    pub fn title(mut self, s: impl Into<String>) -> Self {
        self.title = Some(s.into());
        self
    }

    /// Set the plot's subtitle, rendered in the [`Slot::Subtitle`] slot.
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    /// Set the plot's caption, rendered in the [`Slot::Caption`] slot.
    pub fn caption(mut self, s: impl Into<String>) -> Self {
        self.caption = Some(s.into());
        self
    }

    /// Set the facet-strip label on `side`. Each side has at most one
    /// strip; calling again with the same side replaces the previous
    /// label. Rendered in the matching `StripTop` / `StripRight` /
    /// `StripBottom` / `StripLeft` slot against the theme's
    /// `strip_background` / `strip_text` / `strip_padding`. Available
    /// only with the `text` feature, because the strip's label needs
    /// the shaper.
    #[cfg(feature = "text")]
    pub fn strip(mut self, side: AxisSide, text: impl Into<String>) -> Self {
        self.strips[axis_side_index(side)] = Some(text.into());
        self
    }

    /// Install or clear the facet-strip label for `side`. `None`
    /// removes the strip (no slot reserved); `Some` installs the
    /// label. Flips the plot's dirty flag.
    #[cfg(feature = "text")]
    pub fn set_strip(&mut self, side: AxisSide, text: Option<String>) {
        let idx = axis_side_index(side);
        if self.strips[idx] != text {
            self.strips[idx] = text;
            self.dirty = true;
        }
    }

    /// Read the facet-strip label for `side`, if any.
    #[cfg(feature = "text")]
    pub fn strip_at(&self, side: AxisSide) -> Option<&str> {
        self.strips[axis_side_index(side)].as_deref()
    }

    /// Install a channel → scale-name binding. `channel` is an arbitrary
    /// string the geom understands; `scale_name` resolves through the
    /// orchestrator's [`ScaleRegistry`] at draw time. Replaces any
    /// previous binding for the same channel.
    pub fn bind(mut self, channel: impl Into<String>, scale_name: impl Into<String>) -> Self {
        self.bindings.insert(channel.into(), scale_name.into());
        self
    }

    /// Replace this plot's [`ShapeRegistry`]. Geoms use the registry to
    /// look up marker / terminator shapes by name at draw time.
    pub fn shape_registry(mut self, r: ShapeRegistry) -> Self {
        self.shapes = r;
        self
    }

    // ── Mutators ──

    /// Replace the title text. Flips the plot's dirty flag.
    pub fn set_title(&mut self, s: impl Into<String>) {
        self.title = Some(s.into());
        self.dirty = true;
    }

    /// Clear the title. Flips the plot's dirty flag.
    pub fn clear_title(&mut self) {
        self.title = None;
        self.dirty = true;
    }

    /// Install (or replace) a channel → scale-name binding. Flips the
    /// plot's dirty flag.
    pub fn set_binding(&mut self, channel: impl Into<String>, scale_name: impl Into<String>) {
        self.bindings.insert(channel.into(), scale_name.into());
        self.dirty = true;
    }

    /// Remove the binding for `channel`. Returns the previous scale name
    /// if any. Flips the plot's dirty flag on removal.
    pub fn unbind(&mut self, channel: &str) -> Option<String> {
        let removed = self.bindings.remove(channel);
        if removed.is_some() {
            self.dirty = true;
        }
        removed
    }

    /// Iterate over `(channel, scale_name)` pairs. Order is unspecified.
    pub fn bindings(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.bindings.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Look up the scale name bound to `channel`, if any.
    pub fn binding(&self, channel: &str) -> Option<&str> {
        self.bindings.get(channel).map(|s| s.as_str())
    }

    // ── Geom management ──

    /// Append a geom to the plot's draw order. Returns a stable
    /// [`GeomId`] for later [`Self::update_geom`] / [`Self::remove_geom`]
    /// calls.
    pub fn add_geom<G: Geom>(&mut self, geom: G) -> GeomId {
        let id = GeomId(self.next_geom_id);
        self.next_geom_id = self.next_geom_id.wrapping_add(1);
        self.geoms.push((id, Box::new(geom)));
        self.dirty = true;
        id
    }

    /// Remove and return the geom with the given id, if any.
    pub fn remove_geom(&mut self, id: GeomId) -> Option<Box<dyn Geom>> {
        let idx = self.geoms.iter().position(|(g, _)| *g == id)?;
        self.dirty = true;
        Some(self.geoms.remove(idx).1)
    }

    /// Update a geom by id. Downcasts to the concrete geom type `G`;
    /// panics if the geom at `id` isn't a `G`.
    pub fn update_geom<G: Geom + 'static>(&mut self, id: GeomId, f: impl FnOnce(&mut G)) {
        for (gid, g) in self.geoms.iter_mut() {
            if *gid == id {
                let concrete = g.as_any_mut().downcast_mut::<G>().expect(
                    "Plot::update_geom: type mismatch — geom at this id is not the requested type",
                );
                f(concrete);
                self.dirty = true;
                return;
            }
        }
    }

    /// Iterate over the stable ids of every geom on this plot, in
    /// draw order.
    pub fn geom_ids(&self) -> impl Iterator<Item = GeomId> + '_ {
        self.geoms.iter().map(|(id, _)| *id)
    }
}

// ─── ScaleResolver bridge ────────────────────────────────────────────────────

/// Resolves a geom's channel name to a scale by chaining
/// `channel → bindings → scale_name → registry → &Scale`. Built once per
/// `draw_panel_into` call and passed to each geom's [`GeomContext`].
struct PlotScaleResolver<'a> {
    bindings: &'a HashMap<String, String>,
    registry: &'a ScaleRegistry,
    /// Per-plot scale overrides keyed by scale name. Used by Range-mode
    /// aspect adjustment to inject scales with expanded input ranges
    /// without mutating the shared registry. Empty in the common case.
    overrides: &'a HashMap<String, Scale>,
}

impl<'a> ScaleResolver for PlotScaleResolver<'a> {
    fn scale_for(&self, channel: &str) -> Option<&Scale> {
        let scale_name = self.bindings.get(channel)?;
        if let Some(scale) = self.overrides.get(scale_name.as_str()) {
            return Some(scale);
        }
        self.registry.get(scale_name)
    }
}

impl Plot {
    /// Build the per-plot scale-override map for [`AspectMode::Range`].
    /// Returns an empty map when the mode is `Panel`, the projection is
    /// Polar, no aspect ratio is set, the bound x/y scales aren't
    /// continuous, or the natural extents already match the panel's
    /// visual ratio. Otherwise contains a single cloned [`Scale`] keyed
    /// by scale name with its domain symmetrically expanded around the
    /// current center.
    fn build_aspect_overlay(
        &self,
        panel: Rect,
        registry: &ScaleRegistry,
    ) -> HashMap<String, Scale> {
        let mut overlay: HashMap<String, Scale> = HashMap::new();
        if self.aspect_mode != AspectMode::Range {
            return overlay;
        }
        let Some(ratio) = self.cartesian_aspect_ratio else {
            return overlay;
        };
        let (x_channel, y_channel) = match &self.projection {
            crate::plot::projection::Projection::Cartesian => ("x", "y"),
            crate::plot::projection::Projection::Custom(c) => {
                (c.x_channel.as_str(), c.y_channel.as_str())
            }
            crate::plot::projection::Projection::Polar(_) => return overlay,
        };
        let Some(x_scale_name) = self.bindings.get(x_channel) else {
            return overlay;
        };
        let Some(y_scale_name) = self.bindings.get(y_channel) else {
            return overlay;
        };
        let Some(x_scale) = registry.get(x_scale_name) else {
            return overlay;
        };
        let Some(y_scale) = registry.get(y_scale_name) else {
            return overlay;
        };
        let Some(InputRange::Continuous {
            min: x_min,
            max: x_max,
        }) = x_scale.input_range().cloned()
        else {
            return overlay;
        };
        let Some(InputRange::Continuous {
            min: y_min,
            max: y_max,
        }) = y_scale.input_range().cloned()
        else {
            return overlay;
        };
        let x_extent = x_max - x_min;
        let y_extent = y_max - y_min;
        let panel_w = panel.x1 - panel.x0;
        let panel_h = panel.y1 - panel.y0;
        if !(x_extent.is_finite()
            && y_extent.is_finite()
            && x_extent > 0.0
            && y_extent > 0.0
            && panel_w > 0.0
            && panel_h > 0.0
            && ratio > 0.0)
        {
            return overlay;
        }
        // Honor the data-space ratio at the actual panel aspect by
        // matching `x_extent / y_extent` to `panel_w / (panel_h * ratio)`.
        let target = panel_w / (panel_h * ratio);
        let current = x_extent / y_extent;
        // Tight tolerance — anything beyond a few ulps is worth a
        // correction, and a zero-pad clone is harmless.
        if (current - target).abs() <= target * 1e-12 {
            return overlay;
        }
        if current < target {
            let new_x_extent = y_extent * target;
            let half_pad = (new_x_extent - x_extent) * 0.5;
            let mut clone = x_scale.clone();
            clone.set_domain_continuous(x_min - half_pad, x_max + half_pad);
            overlay.insert(x_scale_name.clone(), clone);
        } else {
            let new_y_extent = x_extent / target;
            let half_pad = (new_y_extent - y_extent) * 0.5;
            let mut clone = y_scale.clone();
            clone.set_domain_continuous(y_min - half_pad, y_max + half_pad);
            overlay.insert(y_scale_name.clone(), clone);
        }
        overlay
    }
}

// ─── Wire / draw — feature-gated on `text` ───────────────────────────────────
//
// Wiring chrome cells and drawing axis chrome both depend on the `text`
// feature (axis labels are shaped via `TextRun`). The panel-side draw
// stays available regardless: geoms only need the scale registry +
// panel rect.

impl Plot {
    /// Paint this plot's [`Slot::Background`] from
    /// `theme.plot_background` — the patch-wide background covering
    /// panel + axes + titles + padding, but not the outer margin
    /// sized by `theme.plot_margin`. `Element::Blank` skips both
    /// fill and border. Called by the orchestrator in a first pass
    /// across all plots so backgrounds settle before any panel
    /// chrome / geom draws on top.
    pub fn draw_patch_background_into(
        &self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        theme: &crate::plot::theme::Theme,
        dpi: f64,
    ) {
        let Some(bg_slot) = theme.plot_background.as_set() else {
            return;
        };
        // Cascade plot_background through the rect root so partial
        // overrides pick up theme.rect's border / linewidth fields.
        let bg = bg_slot.cascade(&theme.rect);
        let defaults = crate::plot::theme::rect_concrete_defaults();
        let Some(rect) = layout.get(&self.patch_id, Slot::Background) else {
            return;
        };
        if rect.x1 <= rect.x0 || rect.y1 <= rect.y0 {
            return;
        }
        use kurbo::Shape;
        let radius_pt = bg
            .corner_radius
            .or(defaults.corner_radius)
            .map(|l| l.resolve(0.0))
            .unwrap_or(0.0);
        let radius_px = (radius_pt * dpi / 72.0).max(0.0);
        let path: crate::path::Path = if radius_px > 0.0 {
            crate::primitives::rounded_rect(rect, radius_px)
        } else {
            rect.to_path(0.0)
        };
        if let Some(fill) = bg.fill {
            let brush = crate::brush::Brush::Solid(fill.resolve(&theme.palette));
            scene.fill(
                crate::path::FillRule::NonZero,
                crate::geometry::Affine::IDENTITY,
                &brush,
                None,
                &path,
                crate::pick::PickId::Skip,
            );
        }
        let lw = bg
            .linewidth_pt
            .or(defaults.linewidth_pt)
            .expect("rect linewidth default");
        let width_pt = lw.resolve(1.0);
        if width_pt > 0.0 {
            use crate::stroke::{Cap, Join, Stroke};
            let stroke = Stroke::new(width_pt * dpi / 72.0)
                .with_caps(Cap::Butt)
                .with_join(Join::Miter);
            let color = bg.color.or(defaults.color).expect("rect color default");
            let brush = crate::brush::Brush::Solid(color.resolve(&theme.palette));
            scene.stroke(
                &stroke,
                crate::geometry::Affine::IDENTITY,
                &brush,
                None,
                &path,
                crate::pick::PickId::Skip,
            );
        }
    }

    /// Paint the projection's panel chrome — background fill, grid
    /// lines, and outline stroke — into the panel slot. No geoms.
    /// Called as the orchestrator's phase-2 pass across every plot
    /// so all panel backgrounds settle before any geom is drawn —
    /// otherwise a later plot's panel background would overpaint an
    /// earlier plot's geoms when the earlier plot has `clip = false`
    /// and its geoms spill into the later panel.
    pub fn draw_panel_chrome_into(
        &self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) {
        let panel = match layout.get(&self.patch_id, Slot::Panel) {
            Some(r) => r,
            None => return,
        };
        if panel.x1 <= panel.x0 || panel.y1 <= panel.y0 {
            return;
        }
        #[cfg(feature = "text")]
        {
            let overlay = self.build_aspect_overlay(panel, registry);
            let lookup = |name: &str| -> Option<&Scale> {
                let scale_name = self.bindings.get(name)?;
                overlay
                    .get(scale_name.as_str())
                    .or_else(|| registry.get(scale_name))
            };
            let channels = self.projection.consume_channels();
            let channel_0 = channels.first().and_then(|n| lookup(n));
            let channel_1 = channels.get(1).and_then(|n| lookup(n));
            crate::plot::chrome::panel::draw_panel_chrome(
                scene,
                &self.projection,
                panel,
                crate::plot::chrome::panel::PanelScales {
                    channel_0,
                    channel_1,
                },
                dpi,
                theme,
            );
        }
        // Suppress unused-vars under no-text.
        #[cfg(not(feature = "text"))]
        let _ = (scene, panel, registry, dpi, theme);
    }

    /// Draw geoms into the panel slot. Installs a clip layer using
    /// the projection's outline path when [`Plot::clip`] is `true`
    /// (the default). Phase-3 pass of the orchestrator render — all
    /// panel chromes have been painted by phase 2, so geoms layer
    /// cleanly without later chrome erasing earlier spilled output.
    ///
    /// Picking is opt-in per geom via the `"pick_id"` channel;
    /// geoms without one emit `PickId::Skip` for every primitive.
    pub fn draw_geoms_into(
        &mut self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) {
        let _ = theme; // Consumed by task #4 geom-defaults / chrome migration.
        let panel = match layout.get(&self.patch_id, Slot::Panel) {
            Some(r) => r,
            None => return,
        };
        if panel.x1 <= panel.x0 || panel.y1 <= panel.y0 {
            return;
        }

        // Rebuild diff state on every dirty geom before drawing.
        for (_, geom) in self.geoms.iter_mut() {
            geom.rebuild_diff_against_previous();
        }

        let overrides = self.build_aspect_overlay(panel, registry);
        let resolver = PlotScaleResolver {
            bindings: &self.bindings,
            registry,
            overrides: &overrides,
        };
        let ctx =
            GeomContext::with_projection(panel, dpi, &self.shapes, &resolver, &self.projection)
                .with_theme(theme);

        let clip_path: Option<crate::path::Path> = if self.clip {
            #[cfg(feature = "text")]
            {
                let radius_px = crate::plot::chrome::panel::panel_corner_radius_px(theme, dpi);
                // For Custom, the panel outline is resolved through the
                // projection's bound scales. For Cartesian / Polar the
                // pair is unused.
                let channels = self.projection.consume_channels();
                let x_scale = channels.first().and_then(|n| ctx.scale_for(n));
                let y_scale = channels.get(1).and_then(|n| ctx.scale_for(n));
                Some(crate::plot::chrome::panel::panel_outline_path(
                    &self.projection,
                    panel,
                    radius_px,
                    x_scale,
                    y_scale,
                ))
            }
            #[cfg(not(feature = "text"))]
            {
                Some(rect_to_path(panel))
            }
        } else {
            None
        };

        if let Some(path) = &clip_path {
            scene.push_layer(
                crate::blend::BlendMode::default(),
                1.0,
                crate::geometry::Affine::IDENTITY,
                path,
            );
        }
        for (_, geom) in self.geoms.iter() {
            geom.draw(scene, &ctx);
        }
        if clip_path.is_some() {
            scene.pop_layer();
        }
        self.dirty = false;
    }

    /// One-call panel draw: panel chrome + geoms in sequence.
    /// Convenience for stand-alone (non-orchestrator) callers that
    /// only have one plot per patch. The orchestrator's render
    /// flow splits these so multi-plot patches phase correctly.
    pub fn draw_panel_into(
        &mut self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) {
        self.draw_panel_chrome_into(scene, layout, registry, dpi, theme);
        self.draw_geoms_into(scene, layout, registry, dpi, theme);
    }
}

#[allow(dead_code)]
fn rect_to_path(r: Rect) -> crate::path::Path {
    use kurbo::Shape;
    r.to_path(0.0)
}

// ── Chrome wiring + draw (text-feature only) ─────────────────────────────────

#[cfg(feature = "text")]
impl Plot {
    /// Attach an axis to this plot. Validates the placement against
    /// the active projection — cartesian axes require a Cartesian
    /// projection; polar axes require a Polar projection. Panics
    /// otherwise, same trade-off as `Plot::new`.
    pub fn add_axis(
        &mut self,
        axis: crate::plot::chrome::axis::Axis,
    ) -> crate::plot::chrome::axis::AxisId {
        use crate::plot::chrome::axis::AxisPlacement;
        use crate::plot::projection::Projection;
        match (&axis.placement, &self.projection) {
            (AxisPlacement::Cartesian(_), Projection::Cartesian) => {}
            (AxisPlacement::PolarRadius { .. } | AxisPlacement::PolarAngular(_), Projection::Polar(_)) => {}
            (placement, projection) => panic!(
                "Plot::add_axis: placement {placement:?} is incompatible with projection {projection:?}"
            ),
        }
        let id = crate::plot::chrome::axis::AxisId(self.next_axis_id);
        self.next_axis_id += 1;
        self.axes.push(axis);
        id
    }

    /// Borrow the attached axes in insertion order.
    pub fn axes(&self) -> &[crate::plot::chrome::axis::Axis] {
        &self.axes
    }

    /// Remove all attached axes.
    pub fn clear_axes(&mut self) {
        self.axes.clear();
    }

    /// Attach a legend to this plot. If an existing legend matches
    /// on `(domain_scale, side, title)`, its keys are appended and
    /// the existing legend's id is returned. Otherwise a new legend
    /// is added and its id returned.
    pub fn add_legend(
        &mut self,
        legend: crate::plot::chrome::legend::Legend,
    ) -> crate::plot::chrome::legend::LegendId {
        use crate::plot::chrome::legend::LegendBody;
        if let Some(idx) = self
            .legends
            .iter()
            .position(|l| l.is_compatible_with(&legend))
        {
            // Only stack-style legends merge their keys; the
            // colorbar case is excluded by `is_compatible_with`.
            if let (LegendBody::Stack(existing), LegendBody::Stack(incoming)) =
                (&mut self.legends[idx].body, legend.body)
            {
                existing.keys.extend(incoming.keys);
            }
            return crate::plot::chrome::legend::LegendId(idx as u32);
        }
        self.add_legend_separate(legend)
    }

    /// Attach a legend without merging into a compatible existing
    /// legend. Use when two legends with the same triple should be
    /// rendered side-by-side instead.
    pub fn add_legend_separate(
        &mut self,
        legend: crate::plot::chrome::legend::Legend,
    ) -> crate::plot::chrome::legend::LegendId {
        let id = crate::plot::chrome::legend::LegendId(self.next_legend_id);
        self.next_legend_id += 1;
        self.legends.push(legend);
        id
    }

    /// Borrow the attached legends in insertion order.
    pub fn legends(&self) -> &[crate::plot::chrome::legend::Legend] {
        &self.legends
    }

    /// Remove all attached legends.
    pub fn clear_legends(&mut self) {
        self.legends.clear();
    }

    /// Wire chrome cells into `patch` based on this plot's current
    /// state. The returned `Patch` is ready to drop into a
    /// [`Composition`] for solving.
    ///
    /// Default slot assignments:
    /// - `Slot::AxisBottom` ← `bindings["x"]` → `scale.axis_measure(Bottom)`
    /// - `Slot::AxisLeft` ← `bindings["y"]` → `scale.axis_measure(Left)`
    /// - `Slot::Title` / `Subtitle` / `Caption` ← matching text fields
    /// - `Slot::AxisLeftTitle` / `AxisBottomTitle` ← matching text
    /// - `Slot::Panel` ← `Cell::empty()`
    ///
    /// Unbound channels (e.g. no `"x"` binding) skip their slot.
    /// Unknown scale names also skip — `wire` is lenient by design;
    /// `PlotComposition::validate()` (Phase 7) surfaces such mismatches.
    pub fn wire(
        &self,
        mut patch: Patch,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) -> Patch {
        // Aspect lock from the projection's natural geometry — see
        // `Self::desired_panel_aspect`. Cartesian plots return
        // `None`; polar plots return either the projection bbox
        // aspect (fit_to_bbox = true) or 1:1 (fit_to_bbox = false).
        // When the orchestrator merges multiple plots into one
        // patch it cross-checks every plot's desired aspect for
        // agreement before applying it to the final patch; this
        // single-plot path sets it unconditionally.
        if let Some((w, h)) = self.desired_panel_aspect(registry) {
            patch = patch.aspect(w, h);
        }

        // Title row + variants — styles come from the theme.
        // `theme.plot_text_align_to` picks the column span: `Plot`
        // (default) uses the full `Slot::Title` anatomical span
        // (rows 3 / 4 / 14, cols PLOT_LEFT..=PLOT_RIGHT) so chrome
        // text spans the whole plot interior; `Panel` narrows the
        // span to just the panel column (`PANEL_COL`) so the title
        // sits directly above / below the panel, regardless of how
        // wide the side chrome (axes, legends, strips) grew. Same
        // semantic as ggplot2's `plot.title.position`.
        let root_pt = theme
            .text
            .size_pt
            .map(|l| l.resolve(crate::plot::theme::DEFAULT_TEXT_SIZE_PT))
            .unwrap_or(crate::plot::theme::DEFAULT_TEXT_SIZE_PT);
        for (slot, text_opt, theme_slot) in [
            (Slot::Title, self.title.as_ref(), &theme.plot_title),
            (Slot::Subtitle, self.subtitle.as_ref(), &theme.plot_subtitle),
            (Slot::Caption, self.caption.as_ref(), &theme.plot_caption),
        ] {
            if let (Some(t), Some(el)) = (text_opt, effective_text(theme_slot, &theme.text)) {
                let (r, c, rs, cs) = title_band_placement(slot, theme.plot_text_align_to);
                patch = patch.place_at(
                    slot.name(),
                    r,
                    c,
                    crate::composition::Span::rc(rs, cs),
                    text_cell_for_element(t, &el, root_pt, dpi),
                );
            }
        }
        // Axes — explicitly composed by the caller via
        // `Plot::add_axis`. Each cartesian axis contributes a rail
        // cell (when its `scale_name` is set) and / or a title cell
        // (when its `title` is set) to the matching anatomical
        // slots. Polar axes wire nothing here; they render in-panel
        // from `draw_chrome_into`.
        patch = self.wire_axes(patch, registry, dpi, theme);

        // Legends — explicitly composed by the caller via
        // `Plot::add_legend{,_separate}`. Each attached side's
        // legends are aggregated into one `LegendStackMeasure` cell
        // through `legend_stack_measure`. In-panel legends reserve
        // zero chrome space and render against the resolved panel
        // rect from `draw_chrome_into`.
        patch = self.wire_legends(patch, registry, dpi, theme);

        // Strips — facet labels populated via `Plot::strip(side, _)`.
        // Each side that has a label reserves a `StripTop` / `StripRight`
        // / `StripBottom` / `StripLeft` slot sized to the rotated text
        // dim plus `theme.strip_padding`.
        patch = self.wire_strips(patch, theme, dpi);

        // Panel is always present (the geom panel lives here).
        self.wire_panel(patch)
    }

    fn wire_strips(&self, mut patch: Patch, theme: &crate::plot::theme::Theme, dpi: f64) -> Patch {
        use crate::plot::chrome::strip::{strip_slot, StripMeasure};
        for side in STRIP_SIDES {
            let Some(text) = self.strip_at(side) else {
                continue;
            };
            let Some(measure) = StripMeasure::new(text, side, theme, dpi) else {
                continue;
            };
            patch = patch.slot(strip_slot(side), Cell::measured(measure));
        }
        patch
    }

    fn wire_axes(
        &self,
        mut patch: Patch,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) -> Patch {
        use crate::plot::chrome::axis::AxisPlacement;
        for axis in &self.axes {
            match axis.placement {
                AxisPlacement::Cartesian(side) => {
                    // Rail cell → matching AxisBottom/Top/Left/Right slot.
                    // `axis_measure` resolves the chrome style from the
                    // theme internally, so the measure (which reserves
                    // the slot) and the draw call (which renders into
                    // it) shape labels at the same size.
                    if let Some(scale_name) = &axis.scale_name {
                        if let Some(scale) = registry.get(scale_name) {
                            let slot = cartesian_axis_slot(side);
                            patch = patch.slot(
                                slot,
                                Cell::measured(BoxMeasure::new(
                                    scale.axis_measure(side, dpi, theme),
                                )),
                            );
                        }
                    }
                    // Title cell → matching AxisBottomTitle/etc. slot.
                    // Vertical sides rotate the text 90°, so the slot's
                    // chrome contribution becomes the text's font height
                    // (not its natural width); horizontal sides keep the
                    // unrotated TextRun measure. Skip the slot when the
                    // theme places the title `Inside` the panel — that
                    // path draws the title against the panel rect at
                    // draw time and reserves no outer chrome space.
                    if let Some(title) = &axis.title {
                        let (ch, side_idx) =
                            crate::plot::chrome::axis::axis_side_to_channel_side(side);
                        let resolved = theme.axis.resolve(ch, side_idx);
                        if matches!(
                            resolved.title_location,
                            crate::plot::theme::TitleLocation::Outside
                        ) {
                            let slot = cartesian_axis_title_slot(side);
                            patch = patch.slot(slot, axis_title_cell(title, side, theme, dpi));
                        }
                    }
                }
                AxisPlacement::PolarRadius { .. } | AxisPlacement::PolarAngular(_) => {
                    // Polar axes draw in-panel during chrome
                    // rendering; the only patch-slot contribution
                    // is a bleed reservation handled below across
                    // all polar axes at once.
                }
            }
        }
        // InsidePanel chrome bleed reservation: axes that draw
        // inside the panel rect (polar today; a future ternary or
        // inset projection would join the family) need their
        // labels to bleed past the panel boundary. Collect every
        // such axis's label set, compute conservative per-side
        // bleed, and drop the resulting measure into the four axis
        // slots so the layout reserves room outside the inscribed
        // shape. The per-projection bleed arithmetic lives inside
        // the helper — polar is the only path implemented so far.
        if matches!(
            self.projection.chrome_strategy(),
            crate::plot::projection::ChromeStrategy::InsidePanel
        ) {
            patch = self.wire_chrome_bleed(patch, registry, dpi, theme);
        }
        patch
    }

    fn wire_chrome_bleed(
        &self,
        mut patch: Patch,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) -> Patch {
        use crate::plot::chrome::axis::{AxisPlacement, PolarRing};
        use crate::plot::chrome::polar::{
            BleedAxis, BleedLabel, BleedLabelKind, BleedTitle, BleedTitleKind, PolarBleedMeasure,
        };
        use crate::scales::breaks::DEFAULT_BREAK_COUNT;
        use crate::scales::value::Value;
        use crate::text::{Alignment, TextRun, TextStyle};

        // Polar projection's angle/sweep — needed to convert a
        // scale's break (as a `theta_frac`) into the math angle the
        // label projects from.
        let polar = match self.projection.as_polar() {
            Some(p) => p,
            None => return patch,
        };

        // sign convention mirrors `radius_axis_tick_direction` in
        // chrome::polar — +1 for CCW sweep, -1 for CW. Used to
        // compute the perpendicular "outside the sweep" direction
        // that radius axis ticks (and labels) follow.
        let sign = if polar.theta_end > polar.theta_start {
            1.0_f64
        } else {
            -1.0_f64
        };

        let label_style = TextStyle::new(crate::plot::chrome::linear_axis::LABEL_FONT_SIZE_PT);

        let mut axes: Vec<BleedAxis> = Vec::new();
        for axis in &self.axes {
            let kind = match axis.placement {
                AxisPlacement::PolarAngular(PolarRing::Outer) => BleedLabelKind::OuterAngular,
                AxisPlacement::PolarAngular(PolarRing::Inner) => BleedLabelKind::InnerAngular,
                AxisPlacement::PolarRadius { .. } => BleedLabelKind::Radius,
                AxisPlacement::Cartesian(_) => continue,
            };
            let Some(scale_name) = &axis.scale_name else {
                continue;
            };
            let Some(scale) = registry.get(scale_name) else {
                continue;
            };
            let mut labels = Vec::new();
            // Track the largest label dimension for use in title
            // placement. Mirrors `draw_angular_axis`'s `label_max`.
            let mut max_label_w = 0.0_f64;
            let mut max_label_h = 0.0_f64;
            match axis.placement {
                AxisPlacement::PolarRadius { theta_frac } => {
                    // Every radius break sits along the same spoke,
                    // so the tick direction is shared. Same formula
                    // as `radius_axis_tick_direction`.
                    let theta = polar.theta_for_frac(theta_frac);
                    let direction = (sign * theta.sin(), sign * theta.cos());
                    for v in scale.breaks(DEFAULT_BREAK_COUNT) {
                        if matches!(v, Value::Null) {
                            continue;
                        }
                        let text = scale.format(&v, &theme.locale);
                        let run = TextRun::new(&text, &label_style, dpi);
                        let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
                        let w = run.natural_width();
                        max_label_w = max_label_w.max(w);
                        max_label_h = max_label_h.max(h);
                        labels.push(BleedLabel {
                            text,
                            kind,
                            direction,
                        });
                    }
                }
                AxisPlacement::PolarAngular(_) => {
                    // Each angular break has its own theta from the
                    // scale's mapping. The tick direction radiates
                    // outward along the (cos θ, -sin θ) screen-space
                    // vector.
                    for v in scale.breaks(DEFAULT_BREAK_COUNT) {
                        if matches!(v, Value::Null) {
                            continue;
                        }
                        let Some(frac) = scale.map(&v).as_number() else {
                            continue;
                        };
                        if !frac.is_finite() || !(0.0..=1.0).contains(&frac) {
                            continue;
                        }
                        let theta = polar.theta_for_frac(frac);
                        let text = scale.format(&v, &theme.locale);
                        let run = TextRun::new(&text, &label_style, dpi);
                        let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
                        let w = run.natural_width();
                        max_label_w = max_label_w.max(w);
                        max_label_h = max_label_h.max(h);
                        labels.push(BleedLabel {
                            text,
                            kind,
                            direction: (theta.cos(), -theta.sin()),
                        });
                    }
                }
                _ => unreachable!(),
            }
            // Title contribution — only outer-angular titles bleed
            // past the panel in v1. Radius axis titles sit between
            // r_inner and r_outer (perpendicular to the spoke) so
            // they don't push past the disk's outer ring; inner
            // angular titles are unimplemented (see `draw_angular_axis`).
            let title = axis.title.as_ref().and_then(|title_text| {
                if matches!(
                    axis.placement,
                    AxisPlacement::PolarAngular(PolarRing::Outer)
                ) {
                    let span = polar.theta_end - polar.theta_start;
                    let is_full_circle = (span.abs() - std::f64::consts::TAU).abs() < 1e-6;
                    let theta_mid_math = if is_full_circle {
                        std::f64::consts::FRAC_PI_2
                    } else {
                        (polar.theta_start + polar.theta_end) * 0.5
                    };
                    let label_max_px = max_label_w.max(max_label_h);
                    Some(BleedTitle {
                        text: title_text.clone(),
                        kind: BleedTitleKind::OuterAngular {
                            direction: (theta_mid_math.cos(), -theta_mid_math.sin()),
                            label_max_px,
                        },
                    })
                } else {
                    None
                }
            });
            if !labels.is_empty() || title.is_some() {
                axes.push(BleedAxis { labels, title });
            }
        }
        if axes.is_empty() {
            return patch;
        }
        let bleed = crate::plot::chrome::polar::compute_polar_bleed(&axes, dpi, theme);
        for side in [
            AxisSide::Top,
            AxisSide::Right,
            AxisSide::Bottom,
            AxisSide::Left,
        ] {
            let slot = cartesian_axis_slot(side);
            patch = patch.slot(
                slot,
                Cell::measured(PolarBleedMeasure {
                    side,
                    bleed: bleed.clone(),
                }),
            );
        }
        patch
    }

    fn draw_axes_into(
        &self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        panel: Option<Rect>,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) {
        use crate::plot::chrome::axis::{AxisPlacement, PolarRing};
        // Range-mode aspect adjustment expands one of the bound x / y
        // scales — axes whose `scale_name` matches that scale must
        // render against the expanded range so ticks line up with the
        // panel gridlines.
        let overlay = match panel {
            Some(p) => self.build_aspect_overlay(p, registry),
            None => HashMap::new(),
        };
        let resolve_scale =
            |name: &str| -> Option<&Scale> { overlay.get(name).or_else(|| registry.get(name)) };
        for axis in &self.axes {
            match axis.placement {
                AxisPlacement::Cartesian(side) => {
                    if let Some(scale_name) = &axis.scale_name {
                        if let (Some(panel_rect), Some(scale)) =
                            (panel, resolve_scale(scale_name.as_str()))
                        {
                            let slot = cartesian_axis_slot(side);
                            if let Some(slot_rect) = layout.get(&self.patch_id, slot) {
                                scale.draw_axis(scene, slot_rect, panel_rect, side, dpi, theme);
                            }
                        }
                    }
                    // Cartesian titles render through the title-slot
                    // path the same way `Plot::title` does — handled
                    // by the title-slot draw loop below in
                    // `draw_chrome_into`.
                }
                AxisPlacement::PolarRadius { theta_frac } => {
                    if let Some(scale_name) = &axis.scale_name {
                        if let (Some(panel_rect), Some(polar), Some(scale)) =
                            (panel, self.projection.as_polar(), registry.get(scale_name))
                        {
                            crate::plot::chrome::polar::draw_radius_axis(
                                scene,
                                panel_rect,
                                polar,
                                scale,
                                theta_frac,
                                dpi,
                                axis.title.as_deref(),
                                theme,
                            );
                        }
                    }
                }
                AxisPlacement::PolarAngular(ring) => {
                    if let Some(scale_name) = &axis.scale_name {
                        if let (Some(panel_rect), Some(polar), Some(scale)) =
                            (panel, self.projection.as_polar(), registry.get(scale_name))
                        {
                            let ring = match ring {
                                PolarRing::Outer => crate::plot::chrome::polar::AngularRing::Outer,
                                PolarRing::Inner => crate::plot::chrome::polar::AngularRing::Inner,
                            };
                            crate::plot::chrome::polar::draw_angular_axis(
                                scene,
                                panel_rect,
                                polar,
                                scale,
                                ring,
                                dpi,
                                axis.title.as_deref(),
                                theme,
                            );
                        }
                    }
                }
            }
        }
    }

    fn draw_strips_into(
        &self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) {
        use crate::plot::chrome::strip::{draw_strip, strip_slot};
        for side in STRIP_SIDES {
            let Some(text) = self.strip_at(side) else {
                continue;
            };
            let Some(rect) = layout.get(&self.patch_id, strip_slot(side)) else {
                continue;
            };
            draw_strip(scene, text, rect, side, theme, dpi);
        }
    }

    fn wire_legends(
        &self,
        mut patch: Patch,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) -> Patch {
        for (side, slot, group) in legends_grouped_by_side(&self.legends) {
            if group.is_empty() {
                continue;
            }
            patch = patch.slot(
                slot,
                Cell::measured(BoxMeasure::new(
                    crate::plot::chrome::legend::legend_stack_measure(
                        &group, side, registry, dpi, theme,
                    ),
                )),
            );
        }
        patch
    }

    /// Render axes + text blocks into the resolved chrome slots from
    /// `layout`. Slots not populated by [`Self::wire`] are skipped
    /// (lookup returns `None`).
    pub fn draw_chrome_into(
        &self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        registry: &ScaleRegistry,
        dpi: f64,
        theme: &crate::plot::theme::Theme,
    ) {
        use crate::brush::Brush;
        use crate::text::TextRun;

        // Axes — explicit, no defaults.
        let panel = layout.get(&self.patch_id, Slot::Panel);
        self.draw_axes_into(scene, layout, panel, registry, dpi, theme);

        // Facet strips — one per side with a label installed via
        // `Plot::strip`. The chrome helper paints the background +
        // shaped text into the matching slot rect.
        self.draw_strips_into(scene, layout, dpi, theme);

        // Legends — render each side's stack of attached legends
        // into the matching slot. Mirrors the wiring loop.
        for (side, slot, group) in legends_grouped_by_side(&self.legends) {
            if group.is_empty() {
                continue;
            }
            if let Some(rect) = layout.get(&self.patch_id, slot) {
                crate::plot::chrome::legend::render_legend_stack(
                    &group,
                    side,
                    rect,
                    registry,
                    &self.shapes,
                    scene,
                    dpi,
                    theme,
                );
            }
        }

        // In-panel legends — overlay on top of the panel rect at
        // their anchor / inset. They reserve no chrome space; the
        // panel rect they paint into comes from the solved layout.
        if let Some(panel) = layout.get(&self.patch_id, Slot::Panel) {
            for (anchor, inset_pt, group) in legends_grouped_in_panel(&self.legends) {
                if group.is_empty() {
                    continue;
                }
                let inset_px = inset_pt * dpi / 72.0;
                let (w, h) = crate::plot::chrome::legend::legend_stack_natural_size(
                    &group, registry, dpi, theme,
                );
                if w <= 0.0 || h <= 0.0 {
                    continue;
                }
                let slot_rect =
                    crate::plot::chrome::legend::resolve_anchor(panel, anchor, inset_px, (w, h));
                crate::plot::chrome::legend::render_legend_stack(
                    &group,
                    crate::scales::chrome::LegendSide::Right,
                    slot_rect,
                    registry,
                    &self.shapes,
                    scene,
                    dpi,
                    theme,
                );
            }
        }

        // Plot-level text slots — title / subtitle / caption. Style
        // and ink come from the theme.
        let root_pt = theme
            .text
            .size_pt
            .map(|l| l.resolve(crate::plot::theme::DEFAULT_TEXT_SIZE_PT))
            .unwrap_or(crate::plot::theme::DEFAULT_TEXT_SIZE_PT);
        let entries: [(
            Slot,
            Option<&String>,
            &crate::plot::theme::Element<crate::plot::theme::TextElement>,
        ); 3] = [
            (Slot::Title, self.title.as_ref(), &theme.plot_title),
            (Slot::Subtitle, self.subtitle.as_ref(), &theme.plot_subtitle),
            (Slot::Caption, self.caption.as_ref(), &theme.plot_caption),
        ];
        for (slot, text, theme_slot) in entries {
            let (Some(text), Some(rect), Some(el)) = (
                text,
                layout.get(&self.patch_id, slot),
                effective_text(theme_slot, &theme.text),
            ) else {
                continue;
            };
            draw_text_element_in_rect(
                scene,
                text,
                &el,
                rect,
                &theme.palette,
                root_pt,
                dpi,
                crate::pick::PickId::Skip,
            );
        }

        // Axis title slots — sourced from `Axis::title` on each
        // attached cartesian axis. `TitleLocation::Outside` (default)
        // draws into the matching outer slot reserved at wire time;
        // `TitleLocation::Inside` draws a strip flush against the
        // panel edge instead, reserving no outer chrome. Polar axis
        // titles render inline through `draw_axes_into`.
        use crate::plot::chrome::axis::{axis_side_to_channel_side, AxisPlacement};
        use crate::plot::theme::{text_concrete_defaults, Rotation, TitleLocation};
        let text_defaults = text_concrete_defaults();
        for axis in &self.axes {
            let Some(title) = axis.title.as_ref() else {
                continue;
            };
            let AxisPlacement::Cartesian(side) = axis.placement else {
                continue;
            };
            let (ch, side_idx) = axis_side_to_channel_side(side);
            let resolved = theme.axis.resolve(ch, side_idx);
            let Some(el) = resolved.title else { continue };
            let style = text_style_from(&el, root_pt);
            let color = el
                .color
                .clone()
                .or_else(|| text_defaults.color.clone())
                .expect("text_concrete_defaults sets color");
            let brush = Brush::Solid(color.resolve(&theme.palette));
            let angle = el
                .angle
                .or(text_defaults.angle)
                .expect("text_concrete_defaults sets angle");
            let margin = el
                .margin
                .or(text_defaults.margin)
                .expect("text_concrete_defaults sets margin");
            let run = TextRun::new(title, &style, dpi);
            match resolved.title_location {
                TitleLocation::Outside => {
                    let slot = cartesian_axis_title_slot(side);
                    if let Some(rect) = layout.get(&self.patch_id, slot) {
                        draw_axis_title(scene, &run, rect, side, &brush, angle);
                    }
                }
                TitleLocation::Inside => {
                    let Some(panel) = layout.get(&self.patch_id, Slot::Panel) else {
                        continue;
                    };
                    // Resolve the angle so the strip dims and the
                    // draw helper see a concrete rotation.
                    let baseline_deg: f32 = match side {
                        AxisSide::Top | AxisSide::Bottom => 0.0,
                        AxisSide::Left => -90.0,
                        AxisSide::Right => 90.0,
                    };
                    let resolved_deg = angle.resolve(baseline_deg);
                    let theta = (resolved_deg as f64).to_radians();
                    // Rotated text bbox dims (axis-aligned bounding
                    // box of the rotated string). Cross-axis sides
                    // use the bbox height for strip thickness;
                    // length-axis sides use the bbox width.
                    let text_w = run.natural_width();
                    let text_h = run.natural_height();
                    let (cos_t, sin_t) = (theta.cos().abs(), theta.sin().abs());
                    let rotated_w = text_w * cos_t + text_h * sin_t;
                    let rotated_h = text_w * sin_t + text_h * cos_t;
                    let (mt, mr, mb, ml) = margin.resolve(root_pt);
                    let pt_to_px = dpi / 72.0;
                    let strip_rect = match side {
                        AxisSide::Bottom => {
                            let h = rotated_h + (mt + mb) * pt_to_px;
                            Rect::new(panel.x0, panel.y1 - h, panel.x1, panel.y1)
                        }
                        AxisSide::Top => {
                            let h = rotated_h + (mt + mb) * pt_to_px;
                            Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + h)
                        }
                        AxisSide::Left => {
                            let w = rotated_w + (ml + mr) * pt_to_px;
                            Rect::new(panel.x0, panel.y0, panel.x0 + w, panel.y1)
                        }
                        AxisSide::Right => {
                            let w = rotated_w + (ml + mr) * pt_to_px;
                            Rect::new(panel.x1 - w, panel.y0, panel.x1, panel.y1)
                        }
                    };
                    // Use the layout-aware draw helper so the title
                    // element's align / valign / margin all flow
                    // through. `angle` is already baked into
                    // `concrete_angle_el` below — drop the original
                    // Along/Across into Degrees so the helper
                    // doesn't try to resolve against a baseline it
                    // doesn't know.
                    let concrete_angle_el = crate::plot::theme::TextElement {
                        angle: Some(Rotation::Degrees(resolved_deg)),
                        ..el.clone()
                    };
                    draw_text_element_in_rect(
                        scene,
                        title,
                        &concrete_angle_el,
                        strip_rect,
                        &theme.palette,
                        root_pt,
                        dpi,
                        crate::pick::PickId::Skip,
                    );
                }
            }
        }
    }
}

/// Build a chrome text cell whose measure includes both the shaped
/// run **and** the element's margin. The slot the layout solver
/// reserves is therefore sized to text + margin; the draw helper
/// then insets back to position the text inside.
#[cfg(feature = "text")]
fn text_cell_for_element(
    s: &str,
    el: &crate::plot::theme::TextElement,
    parent_pt: f64,
    dpi: f64,
) -> Cell {
    use crate::plot::theme::text_concrete_defaults;
    let style = text_style_from(el, parent_pt);
    let run = crate::text::TextRun::new(s, &style, dpi);
    let margin = el
        .margin
        .or(text_concrete_defaults().margin)
        .expect("text_concrete_defaults sets margin");
    let (mt, mr, mb, ml) = margin.resolve(parent_pt);
    let pt_to_px = dpi / 72.0;
    let margins_px = (mt * pt_to_px, mr * pt_to_px, mb * pt_to_px, ml * pt_to_px);
    if margins_px.0 == 0.0 && margins_px.1 == 0.0 && margins_px.2 == 0.0 && margins_px.3 == 0.0 {
        Cell::measured(run)
    } else {
        Cell::measured(crate::text::WithMargin::new(Box::new(run), margins_px))
    }
}

/// Convert a theme [`TextElement`](crate::plot::theme::TextElement)
/// into a shaper-facing [`crate::text::TextStyle`]. Resolves
/// `size_pt` against `parent_pt` (typically the root text size) and
/// translates every `FontSpec` axis into the matching `TextStyle`
/// field: family chain (named + generic fallbacks), weight, width,
/// style (italic / oblique angle), OpenType feature toggles, and
/// variable-font axis assignments. Empty / `None` `FontSpec` fields
/// leave the corresponding `TextStyle` field at its default.
#[cfg(feature = "text")]
pub(crate) fn text_style_from(
    el: &crate::plot::theme::TextElement,
    parent_pt: f64,
) -> crate::text::TextStyle {
    use crate::plot::theme::{text_concrete_defaults, FontFamily, FontStyle, FontWidth, Length};
    use crate::text::{
        FontFamilyEntry, FontFeatureSetting, FontStyleKind, FontVariationSetting,
        GenericFamilyKind, LineHeight,
    };
    let defaults = text_concrete_defaults();
    let size_len = el.size_pt.or(defaults.size_pt).expect("size_pt default");
    let size = size_len.resolve(parent_pt) as f32;
    let mut style = crate::text::TextStyle::new(size);
    // Line height: `Length::Rel(m)` → font-size multiplier; `Abs(pt)`
    // → absolute pt. Preserves the resolved-vs-relative semantics
    // across DPI changes.
    let lineheight = el
        .lineheight
        .or(defaults.lineheight)
        .expect("lineheight default");
    style = style.line_height(match lineheight {
        Length::Rel(mult) => LineHeight::Relative(mult as f32),
        Length::Abs(pt) => LineHeight::Absolute(pt as f32),
    });
    if let Some(weight) = el.font.weight {
        style = style.weight(weight.0);
    }
    if let Some(width) = el.font.width {
        style = style.width(match width {
            FontWidth::UltraCondensed => 0.5,
            FontWidth::ExtraCondensed => 0.625,
            FontWidth::Condensed => 0.75,
            FontWidth::SemiCondensed => 0.875,
            FontWidth::Normal => 1.0,
            FontWidth::SemiExpanded => 1.125,
            FontWidth::Expanded => 1.25,
            FontWidth::ExtraExpanded => 1.5,
            FontWidth::UltraExpanded => 2.0,
        });
    }
    style = style.style(match el.font.style {
        Some(FontStyle::Italic) => FontStyleKind::Italic,
        Some(FontStyle::Oblique(angle)) => FontStyleKind::Oblique(angle),
        Some(FontStyle::Normal) | None => FontStyleKind::Normal,
    });
    if let Some(family) = &el.font.family {
        let entries: Vec<FontFamilyEntry> = match family {
            FontFamily::Named(names) => names
                .iter()
                .map(|n| FontFamilyEntry::Named(n.clone()))
                .collect(),
            FontFamily::Serif => vec![FontFamilyEntry::Generic(GenericFamilyKind::Serif)],
            FontFamily::SansSerif => vec![FontFamilyEntry::Generic(GenericFamilyKind::SansSerif)],
            FontFamily::Mono => vec![FontFamilyEntry::Generic(GenericFamilyKind::Mono)],
            FontFamily::Cursive => vec![FontFamilyEntry::Generic(GenericFamilyKind::Cursive)],
            FontFamily::Fantasy => vec![FontFamilyEntry::Generic(GenericFamilyKind::Fantasy)],
            FontFamily::SystemUi => vec![FontFamilyEntry::Generic(GenericFamilyKind::SystemUi)],
        };
        style = style.families(entries);
    }
    if !el.font.features.is_empty() {
        let features: Vec<FontFeatureSetting> = el
            .font
            .features
            .iter()
            .map(|f| FontFeatureSetting {
                tag: f.tag,
                // Theme stores feature values as u32 to accommodate any
                // future encoding; parley uses u16, which covers every
                // OpenType feature value in practice.
                value: f.value.min(u16::MAX as u32) as u16,
            })
            .collect();
        style = style.features(features);
    }
    if !el.font.variations.is_empty() {
        let variations: Vec<FontVariationSetting> = el
            .font
            .variations
            .iter()
            .map(|v| FontVariationSetting {
                tag: v.tag,
                value: v.value,
            })
            .collect();
        style = style.variations(variations);
    }
    style
}

/// Resolve the effective [`TextElement`](crate::plot::theme::TextElement)
/// for an `Element<TextElement>` slot. `Blank` short-circuits to
/// `None`; otherwise the slot's sparse fields cascade onto `root`,
/// producing an owned `TextElement` whose `Some`-set fields reflect
/// the per-field merge of override → root.
///
/// Callers must still fall through to
/// [`text_concrete_defaults`](crate::plot::theme::text_concrete_defaults)
/// for any field left `None` (typically by passing the resolved
/// element to [`text_style_from`], which handles the fallback).
#[cfg(feature = "text")]
pub(crate) fn effective_text(
    slot: &crate::plot::theme::Element<crate::plot::theme::TextElement>,
    root: &crate::plot::theme::TextElement,
) -> Option<crate::plot::theme::TextElement> {
    match slot {
        crate::plot::theme::Element::Blank => None,
        crate::plot::theme::Element::Inherit => Some(root.clone()),
        crate::plot::theme::Element::Set(el) => Some(el.cascade(root)),
    }
}

/// Build the `Cell` for a cartesian axis title slot. Vertical sides
/// (Left/Right) wrap the shaped run in a [`RotatedAxisTitleMeasure`]
/// so the slot's column width reflects the rotated text's footprint
/// (one font line height) rather than the natural string width.
/// Horizontal sides reuse the unrotated `TextRun` measure directly.
#[cfg(feature = "text")]
fn axis_title_cell(
    title: &str,
    side: AxisSide,
    theme: &crate::plot::theme::Theme,
    dpi: f64,
) -> Cell {
    let (ch, side_idx) = crate::plot::chrome::axis::axis_side_to_channel_side(side);
    let resolved = theme.axis.resolve(ch, side_idx);
    let root_pt = theme
        .text
        .size_pt
        .map(|l| l.resolve(crate::plot::theme::DEFAULT_TEXT_SIZE_PT))
        .unwrap_or(crate::plot::theme::DEFAULT_TEXT_SIZE_PT);
    let style = match resolved.title {
        Some(el) => text_style_from(&el, root_pt),
        None => return Cell::empty(),
    };
    let run = crate::text::TextRun::new(title, &style, dpi);
    if side.is_vertical() {
        Cell::measured(RotatedAxisTitleMeasure {
            rotated_w: run.natural_height(),
        })
    } else {
        Cell::measured(run)
    }
}

/// Measure for an axis title rotated 90° onto a vertical chrome
/// column. The slot's horizontal contribution is the font's line
/// height (post-rotation width); the vertical extent is panel-driven,
/// so the cell reports no row contribution.
#[cfg(feature = "text")]
struct RotatedAxisTitleMeasure {
    rotated_w: f64,
}

#[cfg(feature = "text")]
impl crate::layout::Measure for RotatedAxisTitleMeasure {
    fn width_hint(&self, _dpi: f64) -> crate::layout::WidthHint {
        crate::layout::WidthHint::Min(self.rotated_w)
    }

    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        0.0
    }

    fn width_at(&self, _height: f64, _dpi: f64) -> f64 {
        self.rotated_w
    }
}

/// Centre a cartesian axis title inside `rect`. Horizontal sides
/// (Bottom/Top) layout the run at the slot's full width with
/// [`crate::text::Alignment::Center`] so the line balances across
/// the panel column. Vertical sides (Left/Right) rotate the run 90°
/// — CCW for Left (text reads bottom-to-top), CW for Right — and
/// drop the rotated centre at the slot rect's centre so it parallels
/// the axis it labels.
#[cfg(feature = "text")]
/// Render `text` styled by `el` inside `rect`, honoring every
/// layout-affecting field on the [`TextElement`]: `margin` insets the
/// rect before wrapping, `align` controls horizontal justification
/// (parley `Alignment`), `valign` positions the wrapped block
/// vertically (Top / Middle / Bottom; `Baseline` treated as Top),
/// `angle` rotates the rendered block around its own centre (only
/// `Rotation::Degrees(_)` resolves here — `Along` / `Across` need a
/// baseline context and are deferred to per-side helpers like
/// [`draw_axis_title`]). `lineheight` flows through the cached
/// `TextRun` via [`text_style_from`].
#[cfg(feature = "text")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text_element_in_rect(
    scene: &mut dyn SceneBuilder,
    text: &str,
    el: &crate::plot::theme::TextElement,
    rect: Rect,
    palette: &crate::plot::theme::Palette,
    parent_pt: f64,
    dpi: f64,
    pick_id: crate::pick::PickId,
) {
    use crate::brush::Brush;
    use crate::geometry::{Affine, Vec2};
    use crate::plot::theme::{text_concrete_defaults, HAlign, Rotation, VAlign};
    use crate::text::{draw_text, Alignment, TextRun};

    let defaults = text_concrete_defaults();
    // Inset by margin (pt → px).
    let margin = el.margin.or(defaults.margin).expect("margin default");
    let (mt, mr, mb, ml) = margin.resolve(parent_pt);
    let pt_to_px = dpi / 72.0;
    let inset = Rect::new(
        rect.x0 + ml * pt_to_px,
        rect.y0 + mt * pt_to_px,
        (rect.x1 - mr * pt_to_px).max(rect.x0 + ml * pt_to_px),
        (rect.y1 - mb * pt_to_px).max(rect.y0 + mt * pt_to_px),
    );
    let style = text_style_from(el, parent_pt);
    let color = el
        .color
        .clone()
        .or_else(|| defaults.color.clone())
        .expect("color default");
    let brush = Brush::Solid(color.resolve(palette));
    let run = TextRun::new(text, &style, dpi);
    let align = el.align.or(defaults.align).expect("align default");
    let alignment = match align {
        HAlign::Start => Alignment::Start,
        HAlign::Center => Alignment::Center,
        HAlign::End => Alignment::End,
        HAlign::Justify => Alignment::Justify,
    };
    let inner_w = (inset.x1 - inset.x0) as f32;
    let _ = run.set_max_width(inner_w, alignment);
    // Inked height (first-line ascender top → last-line descender
    // bottom) drives layout. `ascender_offset` is the half-leading
    // the parley layout reserves above the first line; the draw
    // helper compensates by shifting the layout up by that much so
    // the visible glyphs land flush with the slot edge.
    let block_h = run.inked_height();
    let ascender_offset = run.first_line_ascender_offset();
    let inner_h = inset.y1 - inset.y0;
    let valign = el.valign.or(defaults.valign).expect("valign default");
    let y_offset = match valign {
        VAlign::Top | VAlign::Baseline => 0.0,
        VAlign::Middle => ((inner_h - block_h) * 0.5).max(0.0),
        VAlign::Bottom => (inner_h - block_h).max(0.0),
    };
    let angle = el.angle.or(defaults.angle).expect("angle default");
    let angle_rad = match angle {
        Rotation::Degrees(d) => (d as f64).to_radians(),
        // Along / Across need a baseline orientation — chrome that
        // knows the baseline (axis titles, polar rails) handles those
        // variants in its own helper. Default to no rotation here.
        Rotation::Along | Rotation::Across => 0.0,
    };
    if angle_rad.abs() < 1e-9 {
        draw_text(
            scene,
            &run,
            inset.x0,
            inset.y0 + y_offset - ascender_offset,
            &brush,
            Affine::IDENTITY,
            pick_id,
        );
    } else {
        let content_w = run.content_width();
        let pivot_x = inset.x0 + (inner_w as f64) * 0.5;
        // Pivot is the inked centre — `y_offset + block_h/2` lands
        // on the inked centre inside the inset, not the metric-box
        // centre.
        let pivot_y = inset.y0 + y_offset + block_h * 0.5;
        // The inner translate maps the layout's inked centre (at
        // local `(content_w/2, ascender_offset + block_h/2)`) onto
        // the pivot — i.e., translate by the negation of that.
        let inked_centre_y = ascender_offset + block_h * 0.5;
        let transform = Affine::translate(Vec2::new(pivot_x, pivot_y))
            * Affine::rotate(angle_rad)
            * Affine::translate(Vec2::new(-content_w * 0.5, -inked_centre_y));
        draw_text(scene, &run, 0.0, 0.0, &brush, transform, pick_id);
    }
}

/// Draw an axis title into `rect`, honoring `angle` from the theme.
/// `Along` and `Across` resolve against the per-side baseline
/// direction: Top / Bottom baselines run horizontally (0°), Left
/// rotates -90° (text reads bottom-to-top), Right rotates +90°. A
/// concrete `Rotation::Degrees(_)` bypasses that and uses the
/// absolute angle.
#[cfg(feature = "text")]
fn draw_axis_title(
    scene: &mut dyn SceneBuilder,
    run: &crate::text::TextRun,
    rect: Rect,
    side: AxisSide,
    brush: &crate::brush::Brush,
    angle: crate::plot::theme::Rotation,
) {
    use crate::geometry::{Affine, Vec2};
    use crate::text::{draw_text, Alignment};
    let cx = (rect.x0 + rect.x1) * 0.5;
    let cy = (rect.y0 + rect.y1) * 0.5;
    let pid = crate::pick::PickId::Skip;
    let baseline_deg: f32 = match side {
        AxisSide::Top | AxisSide::Bottom => 0.0,
        AxisSide::Left => -90.0,
        AxisSide::Right => 90.0,
    };
    let resolved_deg = angle.resolve(baseline_deg);
    let theta = (resolved_deg as f64).to_radians();
    if theta.abs() < 1e-9 {
        let w = (rect.x1 - rect.x0) as f32;
        run.set_max_width(w, Alignment::Center);
        draw_text(scene, run, rect.x0, rect.y0, brush, Affine::IDENTITY, pid);
    } else {
        // Lay out unconstrained so the run stays single-line; the
        // surrounding slot drives how much the rotated text can grow.
        let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
        let w = run.content_width();
        let transform = Affine::translate(Vec2::new(cx, cy))
            * Affine::rotate(theta)
            * Affine::translate(Vec2::new(-w * 0.5, -h * 0.5));
        draw_text(scene, run, 0.0, 0.0, brush, transform, pid);
    }
}

// ─── BoxMeasure shim ─────────────────────────────────────────────────────────
//
// `Cell::measured` takes `impl Measure + 'static`. The Scale axis path
// returns `Box<dyn Measure>`. Bridge it through a thin wrapper.

#[cfg(feature = "text")]
struct BoxMeasure(Box<dyn crate::layout::Measure>);

#[cfg(feature = "text")]
impl BoxMeasure {
    fn new(inner: Box<dyn crate::layout::Measure>) -> Self {
        Self(inner)
    }
}

#[cfg(feature = "text")]
impl crate::layout::Measure for BoxMeasure {
    fn width_hint(&self, dpi: f64) -> crate::layout::WidthHint {
        self.0.width_hint(dpi)
    }

    fn height_at(&self, width: f64, dpi: f64) -> f64 {
        self.0.height_at(width, dpi)
    }

    fn width_at(&self, height: f64, dpi: f64) -> f64 {
        self.0.width_at(height, dpi)
    }
}

/// Bucket the plot's attached legends by `LegendSide`. Returns one
/// `(side, slot, members)` triple per side in a stable order (Right,
/// Left, Top, Bottom) so the layout solver and the draw loop iterate
/// in lockstep. Empty sides are still yielded — the caller checks
/// `members.is_empty()` to skip.
#[cfg(feature = "text")]
/// Pick the `(row, col, row_span, col_span)` placement for a plot-
/// level text slot (Title / Subtitle / Caption) based on the
/// theme's [`crate::plot::theme::AlignTo`] setting. `Plot` uses the
/// canonical anatomical span (PLOT_LEFT..=PLOT_RIGHT); `Panel`
/// narrows the column span to just the panel column so chrome text
/// aligns against the panel rather than the full plot interior.
#[cfg(feature = "text")]
fn title_band_placement(slot: Slot, align_to: crate::plot::theme::AlignTo) -> (u16, u16, u16, u16) {
    let (row, col, rs, cs) = slot.placement();
    match align_to {
        crate::plot::theme::AlignTo::Plot => (row, col, rs, cs),
        crate::plot::theme::AlignTo::Panel => (row, crate::composition::PANEL_COL, rs, 1),
    }
}

#[cfg(feature = "text")]
fn cartesian_axis_slot(side: AxisSide) -> Slot {
    match side {
        AxisSide::Left => Slot::AxisLeft,
        AxisSide::Right => Slot::AxisRight,
        AxisSide::Bottom => Slot::AxisBottom,
        AxisSide::Top => Slot::AxisTop,
    }
}

#[cfg(feature = "text")]
fn cartesian_axis_title_slot(side: AxisSide) -> Slot {
    match side {
        AxisSide::Left => Slot::AxisLeftTitle,
        AxisSide::Right => Slot::AxisRightTitle,
        AxisSide::Bottom => Slot::AxisBottomTitle,
        AxisSide::Top => Slot::AxisTopTitle,
    }
}

#[cfg(feature = "text")]
fn legends_grouped_by_side(
    legends: &[crate::plot::chrome::legend::Legend],
) -> Vec<(
    crate::scales::chrome::LegendSide,
    Slot,
    Vec<&crate::plot::chrome::legend::Legend>,
)> {
    use crate::scales::chrome::LegendSide;
    let mut out: Vec<(LegendSide, Slot, Vec<&crate::plot::chrome::legend::Legend>)> = vec![
        (LegendSide::Right, Slot::LegendRight, Vec::new()),
        (LegendSide::Left, Slot::LegendLeft, Vec::new()),
        (LegendSide::Top, Slot::LegendTop, Vec::new()),
        (LegendSide::Bottom, Slot::LegendBottom, Vec::new()),
    ];
    for legend in legends {
        if matches!(legend.side, LegendSide::InPanel { .. }) {
            continue;
        }
        for (side, _, group) in out.iter_mut() {
            if *side == legend.side {
                group.push(legend);
                break;
            }
        }
    }
    out
}

/// Partition the plot's legends by their [`crate::scales::chrome::Anchor`]
/// and `inset_pt` so each anchor's group is rendered as a single
/// in-panel stack. Only in-panel legends appear; the four
/// anatomical-side variants are skipped (see [`legends_grouped_by_side`]).
#[cfg(feature = "text")]
fn legends_grouped_in_panel(
    legends: &[crate::plot::chrome::legend::Legend],
) -> Vec<(
    crate::scales::chrome::Anchor,
    f64,
    Vec<&crate::plot::chrome::legend::Legend>,
)> {
    use crate::scales::chrome::LegendSide;
    let mut groups: Vec<(
        crate::scales::chrome::Anchor,
        f64,
        Vec<&crate::plot::chrome::legend::Legend>,
    )> = Vec::new();
    for legend in legends {
        let LegendSide::InPanel { anchor, inset_pt } = legend.side else {
            continue;
        };
        if let Some((_, _, group)) = groups
            .iter_mut()
            .find(|(a, inset, _)| *a == anchor && (inset - inset_pt).abs() < f64::EPSILON)
        {
            group.push(legend);
        } else {
            groups.push((anchor, inset_pt, vec![legend]));
        }
    }
    groups
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::{beside, Patch as CompPatch};
    use crate::plot::geom::PointGeom;
    #[cfg(feature = "text")]
    use crate::plot::scale;
    use crate::plot::theme::Theme;

    fn default_theme() -> Theme {
        Theme::default()
    }

    fn comp_with_two() -> Composition {
        beside(CompPatch::new("a"), CompPatch::new("b"))
    }

    #[cfg(feature = "text")]
    #[test]
    fn text_style_from_propagates_full_font_spec() {
        use crate::plot::theme::{
            FontFamily, FontFeature, FontSpec, FontStyle, FontVariation, FontWeight, FontWidth,
            Length, TextElement,
        };
        use crate::text::{FontFamilyEntry, FontStyleKind, GenericFamilyKind};

        let element = TextElement {
            size_pt: Some(Length::Abs(12.0)),
            font: FontSpec {
                family: Some(FontFamily::Named(vec!["Helvetica".into(), "Arial".into()])),
                weight: Some(FontWeight::BOLD),
                width: Some(FontWidth::Condensed),
                style: Some(FontStyle::Oblique(12.0)),
                features: vec![FontFeature::new(*b"tnum", 1), FontFeature::new(*b"ss01", 1)],
                variations: vec![FontVariation::new(*b"wght", 650.0)],
            },
            ..TextElement::default()
        };
        let style = text_style_from(&element, 10.0);
        assert!((style.size_pt - 12.0).abs() < 1e-3);
        assert_eq!(style.weight, 700);
        assert!((style.width - 0.75).abs() < 1e-6);
        assert_eq!(style.style, FontStyleKind::Oblique(12.0));
        assert_eq!(
            style.families,
            vec![
                FontFamilyEntry::Named("Helvetica".into()),
                FontFamilyEntry::Named("Arial".into()),
            ]
        );
        assert_eq!(style.features.len(), 2);
        assert_eq!(style.features[0].tag, *b"tnum");
        assert_eq!(style.features[0].value, 1);
        assert_eq!(style.features[1].tag, *b"ss01");
        assert_eq!(style.variations.len(), 1);
        assert_eq!(style.variations[0].tag, *b"wght");
        assert!((style.variations[0].value - 650.0).abs() < 1e-6);

        let serif = TextElement {
            size_pt: Some(Length::Abs(11.0)),
            font: FontSpec {
                family: Some(FontFamily::Serif),
                ..FontSpec::default()
            },
            ..TextElement::default()
        };
        let s2 = text_style_from(&serif, 10.0);
        assert_eq!(
            s2.families,
            vec![FontFamilyEntry::Generic(GenericFamilyKind::Serif)]
        );
        assert_eq!(s2.style, FontStyleKind::Normal);
    }

    #[cfg(feature = "text")]
    #[test]
    fn text_style_from_propagates_lineheight() {
        use crate::plot::theme::{Length, TextElement};
        use crate::text::LineHeight;
        let rel = TextElement {
            lineheight: Some(Length::Rel(1.4)),
            ..TextElement::default()
        };
        assert_eq!(
            text_style_from(&rel, 10.0).line_height,
            LineHeight::Relative(1.4)
        );
        let abs = TextElement {
            lineheight: Some(Length::Abs(14.0)),
            ..TextElement::default()
        };
        assert_eq!(
            text_style_from(&abs, 10.0).line_height,
            LineHeight::Absolute(14.0)
        );
    }

    #[test]
    #[should_panic(expected = "no patch with id")]
    fn new_panics_on_unknown_patch() {
        let c = comp_with_two();
        let _ = Plot::new(&c, "nope");
    }

    #[test]
    fn new_accepts_known_patch() {
        let c = comp_with_two();
        let p = Plot::new(&c, "a");
        assert_eq!(p.patch_id(), "a");
    }

    #[test]
    fn chaining_sets_fields() {
        let c = comp_with_two();
        let p = Plot::new(&c, "a")
            .title("T")
            .subtitle("S")
            .bind("x", "time")
            .bind("y", "price");
        assert_eq!(p.title.as_deref(), Some("T"));
        assert_eq!(p.subtitle.as_deref(), Some("S"));
        assert_eq!(p.binding("x"), Some("time"));
        assert_eq!(p.binding("y"), Some("price"));
    }

    #[cfg(feature = "text")]
    #[test]
    fn cartesian_aspect_ratio_propagates_extents() {
        let c = comp_with_two();
        let mut reg = ScaleRegistry::new();
        reg.insert("x", scale::continuous(0.0..=10.0));
        reg.insert("y", scale::continuous(0.0..=5.0));
        // ratio = 2 means x-step is 2x y-step. Panel demand:
        // (x_extent * ratio, y_extent) = (10 * 2, 5) = (20, 5).
        let p = Plot::new(&c, "a")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(2.0);
        let (w, h) = p.desired_panel_aspect(&reg).expect("aspect set");
        assert!((w - 20.0).abs() < 1e-4, "w = {w}");
        assert!((h - 5.0).abs() < 1e-4, "h = {h}");
    }

    #[cfg(feature = "text")]
    #[test]
    fn cartesian_aspect_ratio_default_is_none() {
        let c = comp_with_two();
        let reg = ScaleRegistry::new();
        let p = Plot::new(&c, "a");
        assert!(p.desired_panel_aspect(&reg).is_none());
    }

    #[cfg(feature = "text")]
    #[test]
    fn cartesian_aspect_ratio_needs_continuous_extents() {
        // Discrete scales have no `extent()` — should fall back to
        // None even with aspect_ratio set.
        let c = comp_with_two();
        let mut reg = ScaleRegistry::new();
        reg.insert(
            "x",
            scale::discrete(Vec::<crate::scales::value::Value>::new()),
        );
        reg.insert("y", scale::continuous(0.0..=1.0));
        let p = Plot::new(&c, "a")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(1.0);
        assert!(p.desired_panel_aspect(&reg).is_none());
    }

    #[cfg(feature = "text")]
    #[test]
    fn range_mode_skips_panel_lock() {
        // Range mode honors the ratio at draw time by expanding scale
        // ranges, so the patch should not be aspect-locked.
        let c = comp_with_two();
        let mut reg = ScaleRegistry::new();
        reg.insert("x", scale::continuous(0.0..=10.0));
        reg.insert("y", scale::continuous(0.0..=5.0));
        let p = Plot::new(&c, "a")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(2.0)
            .aspect_mode(AspectMode::Range);
        assert!(p.desired_panel_aspect(&reg).is_none());
    }

    #[cfg(feature = "text")]
    #[test]
    fn range_mode_expands_x_when_panel_is_wide() {
        // ratio=1, x=[0,10], y=[0,10], panel 200×100.
        // target x_extent/y_extent = 200 / (100 * 1) = 2.
        // current = 1 < 2 → expand x to extent 20, padding ±5.
        let c = comp_with_two();
        let mut reg = ScaleRegistry::new();
        reg.insert("x", scale::continuous(0.0..=10.0));
        reg.insert("y", scale::continuous(0.0..=10.0));
        let p = Plot::new(&c, "a")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(1.0)
            .aspect_mode(AspectMode::Range);
        let panel = Rect::new(0.0, 0.0, 200.0, 100.0);
        let overlay = p.build_aspect_overlay(panel, &reg);
        let adjusted = overlay.get("x").expect("x scale overridden");
        match adjusted.input_range() {
            Some(crate::scales::input::InputRange::Continuous { min, max }) => {
                assert!((min - (-5.0)).abs() < 1e-9, "x min = {min}");
                assert!((max - 15.0).abs() < 1e-9, "x max = {max}");
            }
            other => panic!("expected continuous range, got {other:?}"),
        }
        assert!(!overlay.contains_key("y"), "y should be untouched");
    }

    #[cfg(feature = "text")]
    #[test]
    fn range_mode_expands_y_when_panel_is_tall() {
        // ratio=1, x=[0,10], y=[0,10], panel 100×200.
        // target = 100 / 200 = 0.5. current = 1 > 0.5 → expand y.
        // new_y_extent = 10 / 0.5 = 20, padding ±5.
        let c = comp_with_two();
        let mut reg = ScaleRegistry::new();
        reg.insert("x", scale::continuous(0.0..=10.0));
        reg.insert("y", scale::continuous(0.0..=10.0));
        let p = Plot::new(&c, "a")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(1.0)
            .aspect_mode(AspectMode::Range);
        let panel = Rect::new(0.0, 0.0, 100.0, 200.0);
        let overlay = p.build_aspect_overlay(panel, &reg);
        let adjusted = overlay.get("y").expect("y scale overridden");
        match adjusted.input_range() {
            Some(crate::scales::input::InputRange::Continuous { min, max }) => {
                assert!((min - (-5.0)).abs() < 1e-9, "y min = {min}");
                assert!((max - 15.0).abs() < 1e-9, "y max = {max}");
            }
            other => panic!("expected continuous range, got {other:?}"),
        }
        assert!(!overlay.contains_key("x"), "x should be untouched");
    }

    #[cfg(feature = "text")]
    #[test]
    fn range_mode_panel_mode_skips_overlay() {
        // Default Panel mode → overlay is always empty even with a
        // ratio set; the patch-aspect path handles it instead.
        let c = comp_with_two();
        let mut reg = ScaleRegistry::new();
        reg.insert("x", scale::continuous(0.0..=10.0));
        reg.insert("y", scale::continuous(0.0..=10.0));
        let p = Plot::new(&c, "a")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(1.0);
        let panel = Rect::new(0.0, 0.0, 200.0, 100.0);
        let overlay = p.build_aspect_overlay(panel, &reg);
        assert!(overlay.is_empty());
    }

    #[cfg(feature = "text")]
    #[test]
    fn range_mode_polar_drops_bbox_lock() {
        // Default-polar Panel mode locks the patch to the bbox aspect
        // (1:1 for a full circle); Range mode releases that lock so
        // the panel can flex and the disk centres inside.
        use crate::plot::projection::Projection;
        let c = comp_with_two();
        let reg = ScaleRegistry::new();
        let panel_mode = Plot::new(&c, "a").projection(Projection::polar());
        let (w, h) = panel_mode
            .desired_panel_aspect(&reg)
            .expect("polar reports bbox aspect");
        assert!(
            (w - h).abs() < 1e-4,
            "expected 1:1 full-circle, got {w}:{h}"
        );
        let range_mode = Plot::new(&c, "a")
            .projection(Projection::polar())
            .aspect_mode(AspectMode::Range);
        assert!(range_mode.desired_panel_aspect(&reg).is_none());
    }

    #[cfg(feature = "text")]
    #[test]
    fn range_mode_polar_skips_overlay() {
        // Polar projections never produce a scale overlay — Range mode
        // just affects the patch lock; the disk geometry is handled by
        // the projection's own panel-inscription code.
        use crate::plot::projection::Projection;
        let c = comp_with_two();
        let mut reg = ScaleRegistry::new();
        reg.insert("x", scale::continuous(0.0..=10.0));
        reg.insert("y", scale::continuous(0.0..=10.0));
        let p = Plot::new(&c, "a")
            .projection(Projection::polar())
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(2.0)
            .aspect_mode(AspectMode::Range);
        let panel = Rect::new(0.0, 0.0, 200.0, 100.0);
        assert!(p.build_aspect_overlay(panel, &reg).is_empty());
    }

    #[cfg(feature = "text")]
    #[test]
    fn range_mode_custom_projection_uses_custom_bindings() {
        use crate::plot::projection::{CustomProjection, Projection};
        use crate::scales::geometry::Polygon as GeoPolygon;
        // Custom projection with non-default channel names — overlay
        // must pick up "lon" / "lat", not "x" / "y".
        let c = comp_with_two();
        let mut reg = ScaleRegistry::new();
        reg.insert("lon_scale", scale::continuous(0.0..=10.0));
        reg.insert("lat_scale", scale::continuous(0.0..=10.0));
        let outline = GeoPolygon::new(vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        let proj = Projection::Custom(CustomProjection::new(outline).channels("lon", "lat"));
        let p = Plot::new(&c, "a")
            .projection(proj)
            .bind("lon", "lon_scale")
            .bind("lat", "lat_scale")
            .aspect_ratio(1.0)
            .aspect_mode(AspectMode::Range);
        let panel = Rect::new(0.0, 0.0, 200.0, 100.0);
        let overlay = p.build_aspect_overlay(panel, &reg);
        assert!(
            overlay.contains_key("lon_scale"),
            "should overlay lon_scale"
        );
        assert!(
            !overlay.contains_key("lat_scale"),
            "lat_scale should be untouched"
        );
        match overlay.get("lon_scale").unwrap().input_range() {
            Some(crate::scales::input::InputRange::Continuous { min, max }) => {
                assert!((min - (-5.0)).abs() < 1e-9, "lon min = {min}");
                assert!((max - 15.0).abs() < 1e-9, "lon max = {max}");
            }
            other => panic!("expected continuous range, got {other:?}"),
        }
    }

    #[test]
    fn unbind_removes_binding() {
        let c = comp_with_two();
        let mut p = Plot::new(&c, "a").bind("x", "time");
        assert_eq!(p.binding("x"), Some("time"));
        assert_eq!(p.unbind("x").as_deref(), Some("time"));
        assert!(p.binding("x").is_none());
    }

    #[test]
    fn add_remove_geom_round_trip() {
        let c = comp_with_two();
        let mut p = Plot::new(&c, "a");
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .build();
        let id = p.add_geom(g);
        assert!(p.geom_ids().any(|gid| gid == id));
        assert!(p.remove_geom(id).is_some());
        assert!(!p.geom_ids().any(|gid| gid == id));
    }

    #[test]
    fn update_geom_runs_closure() {
        let c = comp_with_two();
        let mut p = Plot::new(&c, "a");
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
        let id = p.add_geom(g);
        p.update_geom::<PointGeom>(id, |g| {
            g.set("y", vec![5.0_f64, 6.0]);
        });
        // No assertion on internal channel state — just verify the
        // closure ran without panicking and the geom is still there.
        assert!(p.geom_ids().any(|gid| gid == id));
    }

    #[test]
    fn draw_panel_into_skips_when_panel_missing() {
        // A composition where patch "a" exists but has no Panel slot in
        // the solved layout. draw_panel_into should silently no-op.
        let c = comp_with_two();
        let mut p = Plot::new(&c, "a");
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", crate::color::Color::new([1.0, 0.0, 0.0, 1.0]))
            .build();
        p.add_geom(g);
        let layout = c.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
        let mut scene = crate::scene::recording::RecordingScene::default();
        let registry = ScaleRegistry::new();
        p.draw_panel_into(&mut scene, &layout, &registry, 96.0, &default_theme());
        // The composition itself emits no ops; only checking that we
        // didn't panic.
        let _ = scene.ops.len();
    }

    #[test]
    fn draw_panel_into_emits_user_pick_ids() {
        // End-to-end: a plot with a user-supplied pick_id channel emits
        // those ids directly through the SceneBuilder. No table, no
        // translation.
        let c = comp_with_two();
        let mut p = Plot::new(&c, "a");
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .set("fill", crate::color::Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("pick_id", vec![100_i64, 200, 300])
            .build();
        p.add_geom(g);
        use crate::composition::Patch as CompPatch;
        use crate::layout::Cell;
        let comp = beside(
            CompPatch::new("a").slot(Slot::Panel, Cell::empty()),
            CompPatch::new("b"),
        );
        let layout = comp.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
        let mut scene = crate::scene::recording::RecordingScene::default();
        let registry = ScaleRegistry::new();
        p.draw_panel_into(&mut scene, &layout, &registry, 96.0, &default_theme());
        let ids: Vec<u32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                crate::scene::recording::Op::Fill {
                    pick_id: crate::pick::PickId::Id(n),
                    ..
                } => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![100, 200, 300]);
    }

    // Text-feature-only tests below.
    #[cfg(feature = "text")]
    mod text {
        use super::*;
        use crate::composition::Patch as CompPatch;

        fn make_with_x() -> (Composition, ScaleRegistry, Plot) {
            let c = beside(CompPatch::new("a"), CompPatch::new("b"));
            let registry = ScaleRegistry::new().with("x", scale::continuous(0.0..=10.0));
            let plot = Plot::new(&c, "a").bind("x", "x");
            (c, registry, plot)
        }

        #[test]
        fn wire_drops_axis_bottom_when_explicit_axis_added() {
            // Explicit `add_axis` populates the matching slot;
            // without it the slot stays empty.
            use crate::plot::chrome::axis::{Axis, AxisPlacement};
            let (_c, registry, mut plot) = make_with_x();
            plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
            let patch = plot.wire(CompPatch::new("a"), &registry, 96.0, &default_theme());
            let comp = beside(patch, CompPatch::new("b"));
            let layout = comp.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
            assert!(layout.get("a", Slot::AxisBottom).is_some());
            assert!(layout.get("a", Slot::Panel).is_some());
            assert!(layout.get("a", Slot::AxisLeft).is_none());
        }

        #[test]
        fn wire_includes_title_slot() {
            let c = beside(CompPatch::new("a"), CompPatch::new("b"));
            let plot = Plot::new(&c, "a").title("Hello");
            let registry = ScaleRegistry::new();
            let patch = plot.wire(CompPatch::new("a"), &registry, 96.0, &default_theme());
            let comp = beside(patch, CompPatch::new("b"));
            let layout = comp.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
            assert!(layout.get("a", Slot::Title).is_some());
        }

        #[test]
        fn wire_skips_unbound_axis() {
            let c = beside(CompPatch::new("a"), CompPatch::new("b"));
            let plot = Plot::new(&c, "a"); // no bindings
            let registry = ScaleRegistry::new();
            let patch = plot.wire(CompPatch::new("a"), &registry, 96.0, &default_theme());
            let comp = beside(patch, CompPatch::new("b"));
            let layout = comp.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
            // Only Panel; no axes / titles.
            assert!(layout.get("a", Slot::AxisBottom).is_none());
            assert!(layout.get("a", Slot::AxisLeft).is_none());
            assert!(layout.get("a", Slot::Panel).is_some());
        }

        #[test]
        fn shared_x_scale_drives_two_plots() {
            // Two plots sharing the same scale name, each with an
            // explicit bottom axis → both get AxisBottom chrome
            // cells that report the same dimensions.
            use crate::plot::chrome::axis::{Axis, AxisPlacement};
            let c = beside(CompPatch::new("a"), CompPatch::new("b"));
            let registry = ScaleRegistry::new().with("time", scale::continuous(0.0..=100.0));
            let mut plot_a = Plot::new(&c, "a").bind("x", "time");
            plot_a.add_axis(Axis::rail(
                "time",
                AxisPlacement::Cartesian(AxisSide::Bottom),
            ));
            let mut plot_b = Plot::new(&c, "b").bind("x", "time");
            plot_b.add_axis(Axis::rail(
                "time",
                AxisPlacement::Cartesian(AxisSide::Bottom),
            ));
            let theme = default_theme();
            let comp = beside(
                plot_a.wire(CompPatch::new("a"), &registry, 96.0, &theme),
                plot_b.wire(CompPatch::new("b"), &registry, 96.0, &theme),
            );
            let layout = comp.solve(crate::geometry::Size::new(1000.0, 300.0), 96.0);
            let axis_a = layout.get("a", Slot::AxisBottom).unwrap();
            let axis_b = layout.get("b", Slot::AxisBottom).unwrap();
            assert!((axis_a.y1 - axis_a.y0 - (axis_b.y1 - axis_b.y0)).abs() < 0.5);
        }
    }
}
