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
    background_color: Option<crate::color::Color>,

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
}

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
            background_color: None,
            dirty: true,
            cartesian_aspect_ratio: None,
        }
    }

    /// Lock the panel's data-space aspect ratio to `ratio` (x-unit to
    /// y-unit). With `ratio = 2.0`, one x-axis unit takes up twice
    /// the screen space as one y-axis unit — equivalent to
    /// ggplot's `coord_fixed(ratio = 0.5)`. Computed against each
    /// scale's input-range extent at wire time; the patch's panel
    /// is then aspect-locked to `(x_extent * ratio, y_extent)`.
    ///
    /// Only meaningful for the cartesian projection. Polar
    /// projections override with their own bbox aspect.
    pub fn aspect_ratio(mut self, ratio: f64) -> Self {
        self.cartesian_aspect_ratio = if ratio.is_finite() && ratio > 0.0 {
            Some(ratio)
        } else {
            None
        };
        self
    }

    /// Override whether geoms are clipped to the projection's
    /// outline (default `true`). Set to `false` to let geoms spill
    /// past the panel boundary.
    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }

    /// Set the patch-wide background fill colour. Drawn in the
    /// orchestrator's first render pass; covers panel + axes +
    /// titles + padding, but not the outer margin. Pass `None`
    /// (the default) to skip the fill entirely.
    pub fn background_color(mut self, color: Option<crate::color::Color>) -> Self {
        self.background_color = color;
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
    ///
    /// The orchestrator collects each attached plot's aspect on a
    /// patch and locks the patch to it when every plot agrees; if
    /// they disagree it leaves the patch unlocked.
    pub fn desired_panel_aspect(&self, registry: &ScaleRegistry) -> Option<(f32, f32)> {
        match &self.projection {
            crate::plot::projection::Projection::Cartesian => {
                let ratio = self.cartesian_aspect_ratio?;
                let x_extent = self
                    .bindings
                    .get("x")
                    .and_then(|n| registry.get(n))
                    .and_then(|s| s.input_range())
                    .and_then(|r| r.extent())?;
                let y_extent = self
                    .bindings
                    .get("y")
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
}

impl<'a> ScaleResolver for PlotScaleResolver<'a> {
    fn scale_for(&self, channel: &str) -> Option<&Scale> {
        let scale_name = self.bindings.get(channel)?;
        self.registry.get(scale_name)
    }
}

// ─── Wire / draw — feature-gated on `text` ───────────────────────────────────
//
// Wiring chrome cells and drawing axis chrome both depend on the `text`
// feature (axis labels are shaped via `TextRun`). The panel-side draw
// stays available regardless: geoms only need the scale registry +
// panel rect.

impl Plot {
    /// Paint this plot's [`Slot::Background`] fill — the patch-wide
    /// background covering panel + axes + titles + padding (but not
    /// the outer margin). `None` background colour skips the fill.
    /// Called by the orchestrator in a first pass across all plots
    /// so backgrounds settle before any panel chrome / geom draws
    /// on top — important when multiple plots share a patch.
    pub fn draw_patch_background_into(
        &self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
    ) {
        let Some(color) = self.background_color else {
            return;
        };
        let Some(rect) = layout.get(&self.patch_id, Slot::Background) else {
            return;
        };
        if rect.x1 <= rect.x0 || rect.y1 <= rect.y0 {
            return;
        }
        use kurbo::Shape;
        let path: crate::path::Path = rect.to_path(0.0);
        scene.fill(
            crate::path::FillRule::NonZero,
            crate::geometry::Affine::IDENTITY,
            &crate::brush::Brush::Solid(color),
            None,
            &path,
            crate::pick::PickId::Skip,
        );
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
            let channels = self.projection.consume_channels();
            let channel_0 = channels
                .first()
                .and_then(|name| self.bindings.get(*name))
                .and_then(|scale_name| registry.get(scale_name));
            let channel_1 = channels
                .get(1)
                .and_then(|name| self.bindings.get(*name))
                .and_then(|scale_name| registry.get(scale_name));
            crate::plot::chrome::panel::draw_panel_chrome(
                scene,
                &self.projection,
                panel,
                crate::plot::chrome::panel::PanelScales {
                    channel_0,
                    channel_1,
                },
                dpi,
            );
        }
        // Suppress unused-vars under no-text.
        #[cfg(not(feature = "text"))]
        let _ = (scene, panel, registry, dpi);
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
    ) {
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

        let resolver = PlotScaleResolver {
            bindings: &self.bindings,
            registry,
        };
        let ctx =
            GeomContext::with_projection(panel, dpi, &self.shapes, &resolver, &self.projection);

        let clip_path: Option<crate::path::Path> = if self.clip {
            #[cfg(feature = "text")]
            {
                Some(crate::plot::chrome::panel::panel_outline_path(
                    &self.projection,
                    panel,
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
    ) {
        self.draw_panel_chrome_into(scene, layout, registry, dpi);
        self.draw_geoms_into(scene, layout, registry, dpi);
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
    pub fn wire(&self, mut patch: Patch, registry: &ScaleRegistry, dpi: f64) -> Patch {
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

        // Title row + variants.
        if let Some(t) = &self.title {
            patch = patch.slot(Slot::Title, text_cell(t, title_style()));
        }
        if let Some(t) = &self.subtitle {
            patch = patch.slot(Slot::Subtitle, text_cell(t, subtitle_style()));
        }
        if let Some(t) = &self.caption {
            patch = patch.slot(Slot::Caption, text_cell(t, caption_style()));
        }
        // Axes — explicitly composed by the caller via
        // `Plot::add_axis`. Each cartesian axis contributes a rail
        // cell (when its `scale_name` is set) and / or a title cell
        // (when its `title` is set) to the matching anatomical
        // slots. Polar axes wire nothing here; they render in-panel
        // from `draw_chrome_into`.
        patch = self.wire_axes(patch, registry, dpi);

        // Legends — explicitly composed by the caller via
        // `Plot::add_legend{,_separate}`. Each attached side's
        // legends are aggregated into one `LegendStackMeasure` cell
        // through `legend_stack_measure`. In-panel legends reserve
        // zero chrome space and render against the resolved panel
        // rect from `draw_chrome_into`.
        patch = self.wire_legends(patch, registry, dpi);

        // Panel is always present (the geom panel lives here).
        self.wire_panel(patch)
    }

    fn wire_axes(&self, mut patch: Patch, registry: &ScaleRegistry, dpi: f64) -> Patch {
        use crate::plot::chrome::axis::AxisPlacement;
        for axis in &self.axes {
            match axis.placement {
                AxisPlacement::Cartesian(side) => {
                    // Rail cell → matching AxisBottom/Top/Left/Right slot.
                    if let Some(scale_name) = &axis.scale_name {
                        if let Some(scale) = registry.get(scale_name) {
                            let slot = cartesian_axis_slot(side);
                            patch = patch.slot(
                                slot,
                                Cell::measured(BoxMeasure::new(scale.axis_measure(side, dpi))),
                            );
                        }
                    }
                    // Title cell → matching AxisBottomTitle/etc. slot.
                    if let Some(title) = &axis.title {
                        let slot = cartesian_axis_title_slot(side);
                        patch = patch.slot(slot, text_cell(title, axis_title_style()));
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
            patch = self.wire_chrome_bleed(patch, registry, dpi);
        }
        patch
    }

    fn wire_chrome_bleed(&self, mut patch: Patch, registry: &ScaleRegistry, dpi: f64) -> Patch {
        use crate::plot::chrome::axis::{AxisPlacement, PolarRing};
        use crate::plot::chrome::polar::{
            BleedAxis, BleedLabel, BleedLabelKind, PolarBleedMeasure,
        };
        use crate::scales::breaks::DEFAULT_BREAK_COUNT;
        use crate::scales::value::Value;

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
                        labels.push(BleedLabel {
                            text: scale.format(&v),
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
                        labels.push(BleedLabel {
                            text: scale.format(&v),
                            kind,
                            direction: (theta.cos(), -theta.sin()),
                        });
                    }
                }
                _ => unreachable!(),
            }
            if !labels.is_empty() {
                axes.push(BleedAxis { labels });
            }
        }
        if axes.is_empty() {
            return patch;
        }
        let bleed = crate::plot::chrome::polar::compute_polar_bleed(&axes, dpi);
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
    ) {
        use crate::plot::chrome::axis::{AxisPlacement, PolarRing};
        for axis in &self.axes {
            match axis.placement {
                AxisPlacement::Cartesian(side) => {
                    if let Some(scale_name) = &axis.scale_name {
                        if let (Some(panel_rect), Some(scale)) = (panel, registry.get(scale_name)) {
                            let slot = cartesian_axis_slot(side);
                            if let Some(slot_rect) = layout.get(&self.patch_id, slot) {
                                scale.draw_axis(scene, slot_rect, panel_rect, side, dpi);
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
                            );
                        }
                    }
                }
            }
        }
    }

    fn wire_legends(&self, mut patch: Patch, registry: &ScaleRegistry, dpi: f64) -> Patch {
        for (side, slot, group) in legends_grouped_by_side(&self.legends) {
            if group.is_empty() {
                continue;
            }
            patch = patch.slot(
                slot,
                Cell::measured(BoxMeasure::new(
                    crate::plot::chrome::legend::legend_stack_measure(&group, side, registry, dpi),
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
    ) {
        use crate::brush::Brush;
        use crate::color::Color;
        use crate::text::{draw_text_in_rect, TextRun, TextStyle};
        type StyleFn = fn() -> TextStyle;

        // Axes — explicit, no defaults.
        let panel = layout.get(&self.patch_id, Slot::Panel);
        self.draw_axes_into(scene, layout, panel, registry, dpi);

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
                let (w, h) =
                    crate::plot::chrome::legend::legend_stack_natural_size(&group, registry, dpi);
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
                );
            }
        }

        // Plot-level text slots — title / subtitle / caption.
        let ink = Brush::Solid(Color::new([0.0, 0.0, 0.0, 1.0]));
        let entries: [(Slot, Option<&String>, StyleFn); 3] = [
            (Slot::Title, self.title.as_ref(), title_style),
            (Slot::Subtitle, self.subtitle.as_ref(), subtitle_style),
            (Slot::Caption, self.caption.as_ref(), caption_style),
        ];
        for (slot, text, style_fn) in entries {
            if let (Some(text), Some(rect)) = (text, layout.get(&self.patch_id, slot)) {
                let run = TextRun::new(text, &style_fn());
                draw_text_in_rect(scene, &run, rect, &ink, crate::pick::PickId::Skip);
            }
        }

        // Axis title slots — sourced from `Axis::title` on each
        // attached cartesian axis. Polar axis titles render inline
        // through the polar draw path in `draw_axes_into` (radius
        // titles along the spoke, angular titles curving past the
        // outer ring) and don't participate in slot rendering.
        use crate::plot::chrome::axis::AxisPlacement;
        for axis in &self.axes {
            let Some(title) = axis.title.as_ref() else {
                continue;
            };
            let AxisPlacement::Cartesian(side) = axis.placement else {
                continue;
            };
            let slot = cartesian_axis_title_slot(side);
            if let Some(rect) = layout.get(&self.patch_id, slot) {
                let run = TextRun::new(title, &axis_title_style());
                draw_text_in_rect(scene, &run, rect, &ink, crate::pick::PickId::Skip);
            }
        }
    }
}

#[cfg(feature = "text")]
fn text_cell(s: &str, style: crate::text::TextStyle) -> Cell {
    Cell::measured(crate::text::TextRun::new(s, &style))
}

#[cfg(feature = "text")]
fn title_style() -> crate::text::TextStyle {
    crate::text::TextStyle::new(16.0).weight(700)
}

#[cfg(feature = "text")]
fn subtitle_style() -> crate::text::TextStyle {
    crate::text::TextStyle::new(12.0)
}

#[cfg(feature = "text")]
fn caption_style() -> crate::text::TextStyle {
    crate::text::TextStyle::new(10.0).italic(true)
}

#[cfg(feature = "text")]
pub(crate) fn axis_title_style() -> crate::text::TextStyle {
    crate::text::TextStyle::new(12.0)
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

    fn comp_with_two() -> Composition {
        beside(CompPatch::new("a"), CompPatch::new("b"))
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
        p.draw_panel_into(&mut scene, &layout, &registry, 96.0);
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
        p.draw_panel_into(&mut scene, &layout, &registry, 96.0);
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
            let patch = plot.wire(CompPatch::new("a"), &registry, 96.0);
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
            let patch = plot.wire(CompPatch::new("a"), &registry, 96.0);
            let comp = beside(patch, CompPatch::new("b"));
            let layout = comp.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
            assert!(layout.get("a", Slot::Title).is_some());
        }

        #[test]
        fn wire_skips_unbound_axis() {
            let c = beside(CompPatch::new("a"), CompPatch::new("b"));
            let plot = Plot::new(&c, "a"); // no bindings
            let registry = ScaleRegistry::new();
            let patch = plot.wire(CompPatch::new("a"), &registry, 96.0);
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
            let comp = beside(
                plot_a.wire(CompPatch::new("a"), &registry, 96.0),
                plot_b.wire(CompPatch::new("b"), &registry, 96.0),
            );
            let layout = comp.solve(crate::geometry::Size::new(1000.0, 300.0), 96.0);
            let axis_a = layout.get("a", Slot::AxisBottom).unwrap();
            let axis_b = layout.get("b", Slot::AxisBottom).unwrap();
            assert!((axis_a.y1 - axis_a.y0 - (axis_b.y1 - axis_b.y0)).abs() < 0.5);
        }
    }
}
