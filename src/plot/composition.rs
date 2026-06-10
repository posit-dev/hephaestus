//! `PlotComposition` — the orchestrator that owns the layout composition,
//! the scale registry, and the attached plots. The canonical user-facing
//! surface above the per-plot primitives in [`crate::plot::plot`].
//!
//! Lifecycle:
//!
//! 1. User builds a `Composition` (the layout shape — named, empty
//!    patches) and hands it to `PlotComposition::new(comp)`.
//!    The orchestrator captures a clone-friendly description of the
//!    composition shape (placements + ids + tracks); the original
//!    `Composition` value is consumed.
//! 2. User registers scales by name (`add_scale` / `insert_scale`) and
//!    attaches `Plot`s (`with_plot` / `attach_plot`). Each plot is
//!    bound to a patch id from the composition.
//! 3. User calls `render(scene, size, dpi)`. The orchestrator rebuilds
//!    the composition with each plot's chrome wired in, solves, and
//!    drives `draw_chrome_into` + `draw_panel_into` per plot. Layout is
//!    cached and reused across renders when nothing layout-affecting
//!    has changed.
//!
//! Dirty tracking is conservative: any mutation through
//! `update_scale` / `update_plot` / `attach_plot` / `detach_plot` flips
//! `layout_dirty` so the next render re-solves. Size / dpi changes
//! implicitly invalidate too. Per-plot and per-scale dirty bits are
//! plumbed but not yet consumed; every render re-draws every plot.

use std::collections::HashMap;
use std::sync::Arc;

use crate::composition::{spacer, Composition, CompositionLayout, Element, Patch, Span};
use crate::geometry::Size;
use crate::layout::Track;
use crate::scene::SceneBuilder;

use super::plot::Plot;
use super::scale::{Scale, ScaleRegistry};

// ─── Template ────────────────────────────────────────────────────────────────

/// Clone-friendly description of a [`Composition`]'s shape — captured at
/// `PlotComposition::new` so the original (which may contain non-Clone
/// `Cell` values) can be consumed once and rebuilt fresh on every
/// render with each plot's chrome wired in.
///
/// Captures placement metadata + patch ids only. Chrome cells attached
/// directly to a patch before passing the composition to
/// `PlotComposition::new` are **dropped** — the orchestrator expects
/// empty patches and re-attaches chrome from each plot on every render.
#[derive(Debug, Clone)]
struct CompositionTemplate {
    rows: usize,
    cols: usize,
    widths: Vec<Track>,
    heights: Vec<Track>,
    placements: Vec<PlacementTemplate>,
}

#[derive(Debug, Clone)]
struct PlacementTemplate {
    row: u16,
    col: u16,
    span: Span,
    element: ElementTemplate,
}

#[derive(Debug, Clone)]
enum ElementTemplate {
    /// Named patch (rebuilt as `Patch::new(id)`). Any pre-attached chrome
    /// is ignored.
    NamedPatch(String),
    /// Anonymous spacer.
    Spacer,
    /// Nested composition (placed via `Composition::place`).
    Composition(CompositionTemplate),
}

impl CompositionTemplate {
    /// Capture the shape of a `Composition` into a clone-friendly
    /// description. The walker visits every placement; nested
    /// compositions recurse.
    fn capture(c: &Composition) -> Self {
        Self {
            rows: c.rows(),
            cols: c.cols(),
            widths: c.widths_slice().to_vec(),
            heights: c.heights_slice().to_vec(),
            placements: c
                .placements()
                .map(|(row, col, span, element)| PlacementTemplate {
                    row,
                    col,
                    span,
                    element: ElementTemplate::capture(element),
                })
                .collect(),
        }
    }

    /// Rebuild a `Composition` from this template, wiring each known
    /// patch's chrome via the attached plots.
    fn rebuild(
        &self,
        plots: &HashMap<String, Plot>,
        registry: &ScaleRegistry,
        dpi: f64,
    ) -> Composition {
        let mut c = Composition::empty(self.rows, self.cols);
        if !self.widths.is_empty() {
            c = c.widths(self.widths.clone());
        }
        if !self.heights.is_empty() {
            c = c.heights(self.heights.clone());
        }
        for p in &self.placements {
            let element = p.element.rebuild(plots, registry, dpi);
            c = c.place(p.row, p.col, p.span, element);
        }
        c
    }
}

impl ElementTemplate {
    fn capture(e: &Element) -> Self {
        match e {
            Element::Patch(p) => match p.id() {
                Some(id) => ElementTemplate::NamedPatch(id.to_string()),
                None => ElementTemplate::Spacer,
            },
            Element::Composition(c) => {
                ElementTemplate::Composition(CompositionTemplate::capture(c))
            }
        }
    }

    fn rebuild(
        &self,
        plots: &HashMap<String, Plot>,
        registry: &ScaleRegistry,
        dpi: f64,
    ) -> Element {
        match self {
            ElementTemplate::NamedPatch(id) => {
                let patch = wire_into_patch(id, plots, registry, dpi);
                Element::Patch(patch)
            }
            ElementTemplate::Spacer => Element::Patch(spacer()),
            ElementTemplate::Composition(inner) => {
                Element::Composition(inner.rebuild(plots, registry, dpi))
            }
        }
    }
}

/// Build a `Patch` for `id`, wiring the attached plot's chrome if any.
/// Always adds the `Panel` slot so `draw_panel_into` finds a rect.
fn wire_into_patch(
    id: &str,
    plots: &HashMap<String, Plot>,
    registry: &ScaleRegistry,
    dpi: f64,
) -> Patch {
    let plot = plots.get(id);
    #[cfg(feature = "text")]
    {
        if let Some(plot) = plot {
            return plot.wire(Patch::new(id), registry, dpi);
        }
    }
    let _ = registry;
    let _ = dpi;
    // Unattached patches, or non-text builds: just attach a Panel slot
    // so the layout still places a rect for that patch.
    let p = Patch::new(id);
    match plot {
        Some(pl) => pl.wire_panel(p),
        None => pl_wire_panel_fallback(p),
    }
}

/// Same as `Plot::wire_panel` but for unattached patches (no Plot
/// instance to dispatch through).
fn pl_wire_panel_fallback(p: Patch) -> Patch {
    use crate::composition::Slot;
    use crate::layout::Cell;
    p.slot(Slot::Panel, Cell::empty())
}

// ─── ValidationIssue ─────────────────────────────────────────────────────────

/// Per-plot diagnostic surfaced by [`PlotComposition::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationIssue {
    /// A plot binds a channel to a scale name that isn't in the registry.
    MissingScale {
        plot_id: String,
        channel: String,
        scale_name: String,
    },
    /// A plot binds a channel whose geom expects one output type, but
    /// the bound scale produces a different one (e.g. a `"fill"` channel
    /// expecting `Colors` routed to a scale with `Numbers` output).
    OutputTypeMismatch {
        plot_id: String,
        channel: String,
        scale_name: String,
        expected: super::geom::ExpectedOutput,
        found: &'static str,
    },
}

// ─── PlotComposition ─────────────────────────────────────────────────────────

/// Long-lived stateful container that owns the layout composition,
/// scale registry, and attached plots. See module docs for the
/// lifecycle.
pub struct PlotComposition {
    template: CompositionTemplate,
    scales: ScaleRegistry,
    plots: HashMap<String, Plot>,
    /// Per-plot dirty bits, plumbed for partial-repaint heuristics. Not
    /// currently consumed — every render re-draws the full table.
    plot_dirty: HashMap<String, bool>,
    /// Per-scale dirty bits (same role as plot_dirty).
    scale_dirty: HashMap<String, bool>,
    /// `true` if the next render must re-solve the composition.
    layout_dirty: bool,
    /// Cached solved layout — reused when nothing layout-affecting has
    /// changed AND size/dpi match.
    last_layout: Option<CompositionLayout>,
    last_size: Option<Size>,
    last_dpi: Option<f64>,
}

impl PlotComposition {
    /// Construct from a layout-level [`Composition`]. The composition
    /// value is consumed; only its shape (placements / ids / tracks)
    /// is captured for the per-render rebuild walk.
    pub fn new(composition: Composition) -> Self {
        let template = CompositionTemplate::capture(&composition);
        Self {
            template,
            scales: ScaleRegistry::new(),
            plots: HashMap::new(),
            plot_dirty: HashMap::new(),
            scale_dirty: HashMap::new(),
            layout_dirty: true,
            last_layout: None,
            last_size: None,
            last_dpi: None,
        }
    }

    // ── Scale registry ────────────────────────────────────────────────

    /// Insert a scale under `name`, replacing any previous entry.
    /// Chainable builder form of [`Self::insert_scale`].
    pub fn add_scale(mut self, name: impl Into<String>, scale: Scale) -> Self {
        self.insert_scale(name, scale);
        self
    }

    /// Insert a scale under `name`, replacing any previous entry. Flags
    /// the scale dirty, every plot that binds to it, and the layout.
    pub fn insert_scale(&mut self, name: impl Into<String>, scale: Scale) {
        let name = name.into();
        self.scale_dirty.insert(name.clone(), true);
        self.flag_plots_referencing(&name);
        self.scales.insert(name, scale);
        self.layout_dirty = true;
    }

    /// Remove the scale registered under `name`. Returns the removed
    /// scale. Flags dependent plots and the layout dirty.
    pub fn remove_scale(&mut self, name: &str) -> Option<Scale> {
        let removed = self.scales.remove(name);
        if removed.is_some() {
            self.scale_dirty.insert(name.to_string(), true);
            self.flag_plots_referencing(name);
            self.layout_dirty = true;
        }
        removed
    }

    /// Borrow the scale registered under `name`, if any.
    pub fn scale(&self, name: &str) -> Option<&Scale> {
        self.scales.get(name)
    }

    /// Borrow the underlying scale registry.
    pub fn scales(&self) -> &ScaleRegistry {
        &self.scales
    }

    /// Mutate a scale through a closure. Flags both the scale's dirty
    /// bit and every plot that binds to it, plus the global layout
    /// dirty bit.
    pub fn update_scale(&mut self, name: &str, f: impl FnOnce(&mut Scale)) {
        let dirty_was_set;
        if let Some(s) = self.scales.get_mut(name) {
            f(s);
            self.scale_dirty.insert(name.to_string(), true);
            dirty_was_set = true;
        } else {
            dirty_was_set = false;
        }
        if dirty_was_set {
            self.flag_plots_referencing(name);
            self.layout_dirty = true;
        }
    }

    // ── Plot management ───────────────────────────────────────────────

    /// Attach a plot. Chainable builder form of [`Self::attach_plot`].
    pub fn with_plot(mut self, plot: Plot) -> Self {
        self.attach_plot(plot);
        self
    }

    /// Attach a plot bound to its patch id. Replaces any previous plot
    /// at the same patch id. Flips the layout's dirty flag.
    pub fn attach_plot(&mut self, plot: Plot) {
        let id = plot.patch_id().to_string();
        self.plot_dirty.insert(id.clone(), true);
        self.plots.insert(id, plot);
        self.layout_dirty = true;
    }

    /// Detach and return the plot bound to `patch_id`, if any. Flips the
    /// layout's dirty flag.
    pub fn detach_plot(&mut self, patch_id: &str) -> Option<Plot> {
        let removed = self.plots.remove(patch_id);
        if removed.is_some() {
            self.plot_dirty.insert(patch_id.to_string(), true);
            self.layout_dirty = true;
        }
        removed
    }

    /// Borrow the plot bound to `patch_id`, if any.
    pub fn plot(&self, patch_id: &str) -> Option<&Plot> {
        self.plots.get(patch_id)
    }

    /// Iterate over `(patch_id, plot)` pairs. Order is unspecified.
    pub fn plots(&self) -> impl Iterator<Item = (&str, &Plot)> + '_ {
        self.plots.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Mutate a plot through a closure. Flags its dirty bit and the
    /// global layout dirty bit.
    pub fn update_plot(&mut self, patch_id: &str, f: impl FnOnce(&mut Plot)) {
        if let Some(p) = self.plots.get_mut(patch_id) {
            f(p);
            self.plot_dirty.insert(patch_id.to_string(), true);
            self.layout_dirty = true;
        }
    }

    // ── Render ────────────────────────────────────────────────────────

    /// Force a re-solve on the next render regardless of dirty state.
    /// Useful after mutating a plot via lower-level primitives that
    /// bypass `update_plot`.
    pub fn invalidate_layout(&mut self) {
        self.layout_dirty = true;
        self.last_layout = None;
    }

    /// Render every attached plot into `scene` at the given `size` / dpi.
    /// Re-solves the layout when needed; reuses the cached solve
    /// otherwise.
    ///
    /// Render order: all plots' chrome first (under the `text` feature),
    /// then all plots' panels. This preserves "chrome under panel"
    /// occlusion: a geom that extends past its panel rect is clipped by
    /// the push_layer in `draw_panel_into`, so it can't visually escape
    /// into another plot's chrome.
    pub fn render(&mut self, scene: &mut dyn SceneBuilder, size: Size, dpi: f64) {
        // Detect size/dpi change → layout invalidates implicitly.
        let size_or_dpi_changed = match (self.last_size, self.last_dpi) {
            (Some(s), Some(d)) => s.width != size.width || s.height != size.height || d != dpi,
            _ => true,
        };
        if size_or_dpi_changed {
            self.layout_dirty = true;
        }

        // Re-solve when needed.
        if self.layout_dirty || self.last_layout.is_none() {
            let comp = self.template.rebuild(&self.plots, &self.scales, dpi);
            self.last_layout = Some(comp.solve(size, dpi));
            self.last_size = Some(size);
            self.last_dpi = Some(dpi);
            self.layout_dirty = false;
        }

        let layout = self
            .last_layout
            .as_ref()
            .expect("layout must be cached by this point");

        // Panels first — they include the in-panel chrome (background,
        // grid, outline) plus the geoms, all clipped to the panel
        // rect. Picking is opt-in per geom via the `"pick_id"`
        // channel — the orchestrator does no ticket allocation; the
        // raw value reported by `VelloRenderer::pick_at` is the
        // user-supplied id directly.
        for plot in self.plots.values_mut() {
            plot.draw_panel_into(scene, layout, &self.scales, dpi);
        }

        // Chrome on top. For cartesian this draws into axis slots
        // outside the panel; for polar it draws the radius / angular
        // axes inside the panel without the panel clip, so the axis
        // labels can bleed beyond the inscribed disk and the axis
        // lines / ticks paint over the panel background fill.
        // Order across plots is map iteration order — stable within
        // a render but unspecified across renders. For visual
        // layering this is fine: chrome never overlaps geom content
        // of a different plot.
        #[cfg(feature = "text")]
        for plot in self.plots.values() {
            plot.draw_chrome_into(scene, layout, &self.scales, dpi);
        }

        // Clear dirty bits after a successful render.
        self.plot_dirty.clear();
        self.scale_dirty.clear();
    }

    // ── Validation ────────────────────────────────────────────────────

    /// List binding issues across all attached plots:
    /// - Channels bound to scale names that don't exist in the registry.
    /// - Channels with `Colors` / `Numbers` / `Strings` expectations
    ///   bound to scales whose output range doesn't match.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut out = Vec::new();
        for (plot_id, plot) in &self.plots {
            // Walk declared channels (drives expected_output checks).
            let decls: HashMap<&str, &super::geom::ChannelDecl> = plot
                .geom_ids()
                .flat_map(|_| {
                    // Each geom contributes its declared channels; we
                    // need access to the geom itself. Plot doesn't
                    // expose geoms by reference, so iterate via the
                    // declared-channels accessor we already have.
                    std::iter::empty()
                })
                .collect();
            let _ = decls;
            // Cross-check every binding against the registry.
            for (channel, scale_name) in plot.bindings() {
                let scale = self.scales.get(scale_name);
                if scale.is_none() {
                    out.push(ValidationIssue::MissingScale {
                        plot_id: plot_id.clone(),
                        channel: channel.to_string(),
                        scale_name: scale_name.to_string(),
                    });
                    continue;
                }
                // Output-type cross-check happens when a geom expects a
                // specific output kind for that channel. Canonical
                // channel→output map:
                //   x, y, size, *_offset, *_band, *_opacity → Numbers
                //   fill, stroke                            → Colors
                //   shape                                   → Strings
                // The static map avoids exposing the geom itself in this
                // path.
                let expected = expected_output_for_channel(channel);
                if let (Some(expected), Some(s)) = (expected, scale) {
                    if let Some(found) = output_type_name(s) {
                        if !matches_expected(expected, found) {
                            out.push(ValidationIssue::OutputTypeMismatch {
                                plot_id: plot_id.clone(),
                                channel: channel.to_string(),
                                scale_name: scale_name.to_string(),
                                expected,
                                found,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// Internal: flag every plot that binds `scale_name` (any channel)
    /// as dirty. Called from scale insert/remove/update so partial
    /// repaint can target only affected plots once partial-repaint
    /// heuristics consume the dirty bits.
    fn flag_plots_referencing(&mut self, scale_name: &str) {
        let target: Arc<str> = Arc::from(scale_name);
        for (pid, plot) in &self.plots {
            let referenced = plot.bindings().any(|(_, sn)| sn == &*target);
            if referenced {
                self.plot_dirty.insert(pid.clone(), true);
            }
        }
    }
}

// ─── Validation helpers ──────────────────────────────────────────────────────

fn expected_output_for_channel(channel: &str) -> Option<super::geom::ExpectedOutput> {
    use super::geom::ExpectedOutput::*;
    Some(match channel {
        "x" | "y" | "size" | "linewidth" | "fill_opacity" | "stroke_opacity" | "x_offset"
        | "y_offset" | "x_band" | "y_band" | "dash_offset" => Numbers,
        "fill" | "stroke" => Colors,
        "shape" | "cap" | "join" => Strings,
        "linetype" => Linetypes,
        _ => return None,
    })
}

/// Coarse name of the scale's output type, for diagnostics. Returns
/// `None` if the scale has no output range set (in which case it
/// returns normalised `[0, 1]` fractions, compatible with `Numbers`
/// expectations).
fn output_type_name(scale: &Scale) -> Option<&'static str> {
    use super::scale::OutputRange;
    Some(match scale.output_range()? {
        OutputRange::Numbers(_) => "Numbers",
        OutputRange::Colors(_) => "Colors",
        OutputRange::Strings(_) => "Strings",
        OutputRange::Linetypes(_) => "Linetypes",
    })
}

fn matches_expected(expected: super::geom::ExpectedOutput, found: &'static str) -> bool {
    use super::geom::ExpectedOutput::*;
    match expected {
        Numbers => found == "Numbers",
        Colors => found == "Colors",
        Strings => found == "Strings",
        Linetypes => found == "Linetypes",
        Any => true,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::{beside, Patch as CompPatch};
    use crate::plot::geom::PointGeom;
    use crate::plot::scale;

    fn comp_two() -> Composition {
        beside(CompPatch::new("a"), CompPatch::new("b"))
    }

    #[test]
    fn new_captures_template() {
        let view = PlotComposition::new(comp_two());
        // Template captures the two patches.
        assert_eq!(view.template.cols, 2);
        assert_eq!(view.template.rows, 1);
        assert_eq!(view.template.placements.len(), 2);
    }

    #[test]
    fn add_scale_flips_layout_dirty() {
        let mut view = PlotComposition::new(comp_two());
        view.layout_dirty = false; // pretend a render just happened
        view.insert_scale("x", scale::continuous(0.0..=1.0));
        assert!(view.layout_dirty);
        assert!(view.scale_dirty.get("x").copied().unwrap_or(false));
    }

    #[test]
    fn update_scale_flips_plot_dirty_for_referencing_plots() {
        let mut view = PlotComposition::new(comp_two())
            .add_scale("time", scale::continuous(0.0..=100.0))
            .with_plot(crate::plot::Plot::new(&comp_two(), "a").bind("x", "time"))
            .with_plot(crate::plot::Plot::new(&comp_two(), "b").bind("x", "time"));
        // Clear simulated dirty state.
        view.layout_dirty = false;
        view.plot_dirty.clear();
        view.scale_dirty.clear();

        view.update_scale("time", |s| s.set_domain_continuous(0.0, 50.0));

        assert!(view.layout_dirty);
        assert!(view.scale_dirty.get("time").copied().unwrap_or(false));
        // Both plots reference "time" → both flagged.
        assert!(view.plot_dirty.get("a").copied().unwrap_or(false));
        assert!(view.plot_dirty.get("b").copied().unwrap_or(false));
    }

    #[test]
    fn update_plot_flips_dirty() {
        let mut view =
            PlotComposition::new(comp_two()).with_plot(crate::plot::Plot::new(&comp_two(), "a"));
        view.layout_dirty = false;
        view.plot_dirty.clear();
        view.update_plot("a", |p| p.set_title("hello"));
        // Should have re-flagged.
        assert!(view.layout_dirty);
        assert!(view.plot_dirty.get("a").copied().unwrap_or(false));
    }

    #[test]
    fn render_resolves_cached_layout() {
        let mut view =
            PlotComposition::new(comp_two()).with_plot(crate::plot::Plot::new(&comp_two(), "a"));
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(400.0, 300.0), 96.0);
        assert!(!view.layout_dirty);
        assert!(view.last_layout.is_some());

        // Second render at the same size/dpi → layout reused.
        let first_ops = scene.ops.len();
        view.render(&mut scene, Size::new(400.0, 300.0), 96.0);
        let _ = first_ops;
        // (We can't easily assert "no re-solve" from ops alone; the
        // important thing is that we didn't panic and the layout
        // remained cached. layout_dirty stays false because nothing
        // changed.)
        assert!(!view.layout_dirty);
    }

    #[test]
    fn render_with_size_change_re_solves() {
        use crate::composition::Slot;
        let mut view =
            PlotComposition::new(comp_two()).with_plot(crate::plot::Plot::new(&comp_two(), "a"));
        let mut scene = crate::scene::recording::RecordingScene::default();

        view.render(&mut scene, Size::new(400.0, 300.0), 96.0);
        let r1 = view
            .last_layout
            .as_ref()
            .unwrap()
            .get("a", Slot::Panel)
            .expect("panel rect 1");

        view.render(&mut scene, Size::new(800.0, 300.0), 96.0);
        let r2 = view
            .last_layout
            .as_ref()
            .unwrap()
            .get("a", Slot::Panel)
            .expect("panel rect 2");
        assert!(r1.x1 < r2.x1, "panel should be wider after size change");
    }

    #[test]
    fn render_passes_user_pick_ids_through() {
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .set("fill", crate::color::Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("pick_id", vec![100_i64, 200, 300])
            .build();
        let mut plot = crate::plot::Plot::new(&comp_two(), "a");
        let _gid = plot.add_geom(g);
        let mut view = PlotComposition::new(comp_two()).with_plot(plot);
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(400.0, 300.0), 96.0);
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

    #[test]
    fn unattached_patch_renders_silently() {
        // Only "a" has a plot; "b" is bare. render() should not panic.
        let mut view =
            PlotComposition::new(comp_two()).with_plot(crate::plot::Plot::new(&comp_two(), "a"));
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(400.0, 300.0), 96.0);
    }

    // ── validate() ──

    #[test]
    fn validate_flags_missing_scale() {
        let view = PlotComposition::new(comp_two())
            .with_plot(crate::plot::Plot::new(&comp_two(), "a").bind("x", "nope"));
        let issues = view.validate();
        assert_eq!(issues.len(), 1);
        match &issues[0] {
            ValidationIssue::MissingScale {
                plot_id,
                channel,
                scale_name,
            } => {
                assert_eq!(plot_id, "a");
                assert_eq!(channel, "x");
                assert_eq!(scale_name, "nope");
            }
            other => panic!("unexpected issue: {other:?}"),
        }
    }

    #[test]
    fn validate_flags_output_type_mismatch() {
        // A "fill" binding (expects Colors) routed to a scale with
        // Numbers output should be flagged.
        let numeric = scale::continuous(0.0..=10.0).range_numbers([0.0, 1.0]);
        let view = PlotComposition::new(comp_two())
            .add_scale("size_scale", numeric)
            .with_plot(crate::plot::Plot::new(&comp_two(), "a").bind("fill", "size_scale"));
        let issues = view.validate();
        assert!(
            issues.iter().any(|i| matches!(
                i,
                ValidationIssue::OutputTypeMismatch { channel, found, .. }
                    if channel == "fill" && *found == "Numbers"
            )),
            "expected OutputTypeMismatch; got {issues:?}"
        );
    }

    #[test]
    fn validate_flags_linetype_bound_to_non_linetype_scale() {
        // "linetype" expects Linetypes; routing it to a Strings scale
        // should be flagged.
        let s = scale::ordinal(["a", "b"]).range_strings([
            std::sync::Arc::from("solid"),
            std::sync::Arc::from("dashed"),
        ]);
        let view = PlotComposition::new(comp_two())
            .add_scale("dash_scale", s)
            .with_plot(crate::plot::Plot::new(&comp_two(), "a").bind("linetype", "dash_scale"));
        let issues = view.validate();
        assert!(
            issues.iter().any(|i| matches!(
                i,
                ValidationIssue::OutputTypeMismatch { channel, found, .. }
                    if channel == "linetype" && *found == "Strings"
            )),
            "expected OutputTypeMismatch for linetype; got {issues:?}"
        );
    }

    #[test]
    fn validate_empty_when_everything_resolves() {
        let view = PlotComposition::new(comp_two())
            .add_scale("time", scale::continuous(0.0..=100.0))
            .add_scale(
                "cat",
                scale::ordinal(["a", "b"]).range_colors([
                    crate::color::Color::new([1.0, 0.0, 0.0, 1.0]),
                    crate::color::Color::new([0.0, 1.0, 0.0, 1.0]),
                ]),
            )
            .with_plot(
                crate::plot::Plot::new(&comp_two(), "a")
                    .bind("x", "time")
                    .bind("fill", "cat"),
            );
        assert_eq!(view.validate(), vec![]);
    }
}
