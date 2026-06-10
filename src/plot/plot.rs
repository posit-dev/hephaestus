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
    axis_left_title: Option<String>,
    axis_bottom_title: Option<String>,
    axis_right_title: Option<String>,
    axis_top_title: Option<String>,

    shapes: ShapeRegistry,

    /// Coordinate projection. v1 ships `Cartesian` only — geom output
    /// is unchanged from the pre-projection era. E.3b introduces
    /// `Polar` for partial-arc / gauge layouts.
    projection: crate::plot::projection::Projection,

    /// Tracked for the orchestrator's partial-repaint heuristics; not
    /// currently consulted by the draw path.
    #[allow(dead_code)]
    dirty: bool,
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
            axis_left_title: None,
            axis_bottom_title: None,
            axis_right_title: None,
            axis_top_title: None,
            shapes: ShapeRegistry::with_builtins(),
            projection: crate::plot::projection::Projection::Cartesian,
            dirty: true,
        }
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

    /// Set the left axis title.
    pub fn axis_left_title(mut self, s: impl Into<String>) -> Self {
        self.axis_left_title = Some(s.into());
        self
    }

    /// Set the bottom axis title.
    pub fn axis_bottom_title(mut self, s: impl Into<String>) -> Self {
        self.axis_bottom_title = Some(s.into());
        self
    }

    /// Set the right axis title.
    pub fn axis_right_title(mut self, s: impl Into<String>) -> Self {
        self.axis_right_title = Some(s.into());
        self
    }

    /// Set the top axis title.
    pub fn axis_top_title(mut self, s: impl Into<String>) -> Self {
        self.axis_top_title = Some(s.into());
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
    /// Draw geoms into the panel slot rect from `layout`. Installs a
    /// panel clip (`push_layer` / `pop_layer`), builds a
    /// [`PlotScaleResolver`] over the plot's bindings and the given
    /// registry, and iterates the geom list.
    ///
    /// Picking is opt-in per geom via the `"pick_id"` channel. Geoms
    /// without a `pick_id` channel set are non-pickable (they emit
    /// `PickId::Skip` for every primitive); no ticket allocation
    /// happens at this layer.
    pub fn draw_panel_into(
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

        // Clip to the panel.
        let panel_path = rect_to_path(panel);
        scene.push_layer(
            crate::blend::BlendMode::default(),
            1.0,
            crate::geometry::Affine::IDENTITY,
            &panel_path,
        );

        // In-panel chrome: background, minor + major grid lines,
        // panel outline. Shared across all projections — the
        // projection contributes the boundary path and per-channel
        // grid-line geometry. Drawn before the geoms so they paint
        // on top.
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

        let resolver = PlotScaleResolver {
            bindings: &self.bindings,
            registry,
        };
        let ctx =
            GeomContext::with_projection(panel, dpi, &self.shapes, &resolver, &self.projection);
        for (_, geom) in self.geoms.iter() {
            geom.draw(scene, &ctx);
        }

        scene.pop_layer();
        self.dirty = false;
    }
}

fn rect_to_path(r: Rect) -> crate::path::Path {
    use kurbo::Shape;
    r.to_path(0.0)
}

// ── Chrome wiring + draw (text-feature only) ─────────────────────────────────

#[cfg(feature = "text")]
impl Plot {
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
        if let Some(t) = &self.axis_left_title {
            patch = patch.slot(Slot::AxisLeftTitle, text_cell(t, axis_title_style()));
        }
        if let Some(t) = &self.axis_bottom_title {
            patch = patch.slot(Slot::AxisBottomTitle, text_cell(t, axis_title_style()));
        }
        if let Some(t) = &self.axis_right_title {
            patch = patch.slot(Slot::AxisRightTitle, text_cell(t, axis_title_style()));
        }
        if let Some(t) = &self.axis_top_title {
            patch = patch.slot(Slot::AxisTopTitle, text_cell(t, axis_title_style()));
        }

        // Axes — wire the scale bound to "x" into AxisBottom and the
        // scale bound to "y" into AxisLeft. Unbound or unknown-scale
        // channels skip their slot.
        //
        // Polar / future Ternary draw chrome inside the panel (their
        // `chrome_strategy()` returns `InsidePanel`); skip the axis
        // slot population entirely. Labels may extend beyond the
        // inscribed disk; reserving bleed strips around the panel for
        // them is a follow-up (see `ChromeStrategy::InsidePanel` doc).
        if self.projection.chrome_strategy() == crate::plot::projection::ChromeStrategy::PatchSlots
        {
            if let Some(s) = self.resolved_scale("x", registry) {
                patch = patch.slot(
                    Slot::AxisBottom,
                    Cell::measured(BoxMeasure::new(s.axis_measure(AxisSide::Bottom, dpi))),
                );
            }
            if let Some(s) = self.resolved_scale("y", registry) {
                patch = patch.slot(
                    Slot::AxisLeft,
                    Cell::measured(BoxMeasure::new(s.axis_measure(AxisSide::Left, dpi))),
                );
            }
        }

        // Panel is always present (the geom panel lives here).
        self.wire_panel(patch)
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

        // Axes / polar chrome.
        let panel = layout.get(&self.patch_id, Slot::Panel);
        match self.projection.chrome_strategy() {
            crate::plot::projection::ChromeStrategy::PatchSlots => {
                if let (Some(panel), Some(scale)) = (panel, self.resolved_scale("x", registry)) {
                    if let Some(slot) = layout.get(&self.patch_id, Slot::AxisBottom) {
                        scale.draw_axis(scene, slot, panel, AxisSide::Bottom, dpi);
                    }
                }
                if let (Some(panel), Some(scale)) = (panel, self.resolved_scale("y", registry)) {
                    if let Some(slot) = layout.get(&self.patch_id, Slot::AxisLeft) {
                        scale.draw_axis(scene, slot, panel, AxisSide::Left, dpi);
                    }
                }
            }
            crate::plot::projection::ChromeStrategy::InsidePanel => {
                if let (Some(panel), Some(polar)) = (panel, self.projection.as_polar()) {
                    let angle_scale = self.resolved_scale(&polar.angle_channel, registry);
                    let radius_scale = self.resolved_scale(&polar.radius_channel, registry);
                    crate::plot::chrome::polar::draw_polar_chrome(
                        scene,
                        panel,
                        polar,
                        angle_scale,
                        radius_scale,
                        dpi,
                    );
                }
            }
        }

        // Text slots.
        let ink = Brush::Solid(Color::new([0.0, 0.0, 0.0, 1.0]));
        let entries: [(Slot, Option<&String>, StyleFn); 7] = [
            (Slot::Title, self.title.as_ref(), title_style),
            (Slot::Subtitle, self.subtitle.as_ref(), subtitle_style),
            (Slot::Caption, self.caption.as_ref(), caption_style),
            (
                Slot::AxisLeftTitle,
                self.axis_left_title.as_ref(),
                axis_title_style,
            ),
            (
                Slot::AxisBottomTitle,
                self.axis_bottom_title.as_ref(),
                axis_title_style,
            ),
            (
                Slot::AxisRightTitle,
                self.axis_right_title.as_ref(),
                axis_title_style,
            ),
            (
                Slot::AxisTopTitle,
                self.axis_top_title.as_ref(),
                axis_title_style,
            ),
        ];
        for (slot, text, style_fn) in entries {
            if let (Some(text), Some(rect)) = (text, layout.get(&self.patch_id, slot)) {
                let run = TextRun::new(text, &style_fn());
                draw_text_in_rect(scene, &run, rect, &ink, crate::pick::PickId::Skip);
            }
        }
    }

    fn resolved_scale<'r>(&self, channel: &str, registry: &'r ScaleRegistry) -> Option<&'r Scale> {
        let name = self.bindings.get(channel)?;
        registry.get(name)
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
fn axis_title_style() -> crate::text::TextStyle {
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
        fn wire_drops_axis_bottom_when_x_bound() {
            let (_c, registry, plot) = make_with_x();
            // Wire into a fresh patch; verify it now carries an
            // AxisBottom slot by solving and checking the rect exists.
            let patch = plot.wire(CompPatch::new("a"), &registry, 96.0);
            let comp = beside(patch, CompPatch::new("b"));
            let layout = comp.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
            assert!(layout.get("a", Slot::AxisBottom).is_some());
            assert!(layout.get("a", Slot::Panel).is_some());
            // No y binding → no AxisLeft.
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
            // Two plots sharing the same scale name → both get
            // AxisBottom chrome cells that report the same dimensions.
            let c = beside(CompPatch::new("a"), CompPatch::new("b"));
            let registry = ScaleRegistry::new().with("time", scale::continuous(0.0..=100.0));
            let plot_a = Plot::new(&c, "a").bind("x", "time");
            let plot_b = Plot::new(&c, "b").bind("x", "time");
            let comp = beside(
                plot_a.wire(CompPatch::new("a"), &registry, 96.0),
                plot_b.wire(CompPatch::new("b"), &registry, 96.0),
            );
            let layout = comp.solve(crate::geometry::Size::new(1000.0, 300.0), 96.0);
            let axis_a = layout.get("a", Slot::AxisBottom).unwrap();
            let axis_b = layout.get("b", Slot::AxisBottom).unwrap();
            // Both AxisBottom rects share the same height (chrome row is
            // merged across blocks in the composition solver).
            assert!((axis_a.y1 - axis_a.y0 - (axis_b.y1 - axis_b.y0)).abs() < 0.5);
        }
    }
}
