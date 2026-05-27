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

// ─── Pick table ──────────────────────────────────────────────────────────────

/// What was hit by a pick. Marked `#[non_exhaustive]` so v1.5 chrome
/// picking (axes, legends, titles, panel-background drag) can add
/// variants without breaking the public surface.
///
/// In v1, only [`PickEntry::Geom`] is populated by the standard draw
/// paths — [`Plot::draw_panel_into`] reserves geom rows. The other
/// variants are reserved on the type system for forward compatibility;
/// chrome rendering paths (`Scale::draw_axis`, `Scale::draw_legend`,
/// chrome text) will populate them when chrome picking is wired up.
/// [`PickEntry::Custom`] is the escape hatch for callers (or future
/// geoms) that want to register arbitrary pickable regions today.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum PickEntry {
    /// A drawn geom primitive.
    Geom {
        plot_id: Arc<str>,
        geom_id: GeomId,
        row: usize,
    },
    /// A tick (mark or label) on an axis. Populated in v1.5.
    AxisTick {
        plot_id: Arc<str>,
        side: super::scale::AxisSide,
        tick_idx: usize,
    },
    /// An entry in a legend swatch. Populated in v1.5. `channel` names
    /// the binding the legend was built for; `idx` is the entry index
    /// (matches `scale.breaks()` order for ordinal legends).
    LegendItem {
        plot_id: Arc<str>,
        channel: Arc<str>,
        idx: usize,
    },
    /// A piece of chrome text — title, subtitle, axis-axis title, etc.
    /// Populated in v1.5.
    TextSlot {
        plot_id: Arc<str>,
        slot: PickTextSlot,
    },
    /// A pick that landed on bare panel (no geom, no chrome) — useful
    /// for drag-to-pan, lasso-select, brushing. Populated when the
    /// panel reserves a background ticket in v1.5.
    PanelBackground { plot_id: Arc<str> },
    /// User-defined pickable region. `kind` namespaces the entry (e.g.
    /// `"brush"`, `"violin_handle"`); `data` is opaque per-kind payload
    /// (typically a `usize` cast or a small enum discriminant).
    Custom {
        plot_id: Arc<str>,
        kind: Arc<str>,
        data: u64,
    },
}

impl PickEntry {
    /// Read accessor for the patch id present on every variant.
    pub fn plot_id(&self) -> &str {
        match self {
            PickEntry::Geom { plot_id, .. }
            | PickEntry::AxisTick { plot_id, .. }
            | PickEntry::LegendItem { plot_id, .. }
            | PickEntry::TextSlot { plot_id, .. }
            | PickEntry::PanelBackground { plot_id }
            | PickEntry::Custom { plot_id, .. } => plot_id,
        }
    }
}

/// Which chrome-text slot a [`PickEntry::TextSlot`] refers to.
/// `#[non_exhaustive]` so additions (e.g. axis subtitles) don't break
/// matches.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickTextSlot {
    Title,
    Subtitle,
    Caption,
    AxisLeftTitle,
    AxisBottomTitle,
    AxisRightTitle,
    AxisTopTitle,
}

/// How a [`PickRange`] derives a [`PickEntry`] from a ticket offset.
/// Stored compactly so a 1M-row geom reserves a single range, not a
/// million `PickEntry` clones.
#[derive(Debug, Clone)]
enum PickTemplate {
    /// Ticket offset `i` → `PickEntry::Geom { row: i }`.
    GeomRows { geom_id: GeomId },
    /// Ticket offset `i` → `PickEntry::AxisTick { tick_idx: i }`.
    /// (Reserved for v1.5; the reserve method lands when chrome picking
    /// is populated.)
    #[allow(dead_code)]
    AxisTicks { side: super::scale::AxisSide },
    /// Ticket offset `i` → `PickEntry::LegendItem { idx: i }`.
    #[allow(dead_code)]
    LegendItems { channel: Arc<str> },
    /// Every ticket in this range resolves to the same fixed element
    /// — single-element ranges for chrome / custom regions.
    Fixed(FixedTemplate),
}

#[derive(Debug, Clone)]
enum FixedTemplate {
    /// Reserved for v1.5 chrome picking (`reserve_text_slot`).
    #[allow(dead_code)]
    TextSlot(PickTextSlot),
    /// Reserved for v1.5 chrome picking (`reserve_panel_background`).
    #[allow(dead_code)]
    PanelBackground,
    Custom {
        kind: Arc<str>,
        data: u64,
    },
}

impl PickTemplate {
    fn at(&self, plot_id: Arc<str>, offset: usize) -> PickEntry {
        match self {
            PickTemplate::GeomRows { geom_id } => PickEntry::Geom {
                plot_id,
                geom_id: *geom_id,
                row: offset,
            },
            PickTemplate::AxisTicks { side } => PickEntry::AxisTick {
                plot_id,
                side: *side,
                tick_idx: offset,
            },
            PickTemplate::LegendItems { channel } => PickEntry::LegendItem {
                plot_id,
                channel: channel.clone(),
                idx: offset,
            },
            PickTemplate::Fixed(f) => match f {
                FixedTemplate::TextSlot(slot) => PickEntry::TextSlot {
                    plot_id,
                    slot: *slot,
                },
                FixedTemplate::PanelBackground => PickEntry::PanelBackground { plot_id },
                FixedTemplate::Custom { kind, data } => PickEntry::Custom {
                    plot_id,
                    kind: kind.clone(),
                    data: *data,
                },
            },
        }
    }
}

/// A range of contiguous tickets — one reservation. Resolution finds
/// the range covering a ticket via binary search, then asks
/// `template.at(offset)` to build the per-ticket `PickEntry`.
#[derive(Debug, Clone)]
struct PickRange {
    plot_id: Arc<str>,
    template: PickTemplate,
    /// First ticket index in this range (0-based, exclusive of `+1` bias).
    start: u32,
    /// One past the last ticket in this range.
    end: u32,
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

/// Pick-id-as-ticket lookup table. Each render fills it with contiguous
/// ranges per (plot, geom) and resolves raw `PickId::Id(n)` values into
/// `PickEntry { plot_id, geom_id, row }` via the ticket index.
///
/// The 24-bit `PickId` budget caps the table at ~16M tickets per render
/// (rows summed across every visible geom across every plot). Reserve
/// requests past that limit return a ticket base of `u32::MAX` so the
/// geom's `pick_id_for_row` will fall back to `PickId::Skip` for the
/// over-budget rows.
#[derive(Debug, Default, Clone)]
pub struct PickTable {
    ranges: Vec<PickRange>,
    total: u32,
}

impl PickTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Discard all reservations from a previous render. Call once per
    /// render-pass before drawing the first plot.
    pub fn clear(&mut self) {
        self.ranges.clear();
        self.total = 0;
    }

    /// Reserve `n` tickets for `geom_id` belonging to `plot_id`. Each
    /// ticket resolves to `PickEntry::Geom { plot_id, geom_id, row: i }`
    /// where `i` is the local offset. Returns the base ticket
    /// (caller-side: `pick_id = base + row + 1`).
    ///
    /// Overflow of the 24-bit pick budget is recorded as a no-op (no
    /// range pushed) and `u32::MAX` is returned so the geom's
    /// `pick_id_for_row` will emit `Skip` for that range.
    pub fn reserve_geom_rows(&mut self, plot_id: Arc<str>, geom_id: GeomId, n: usize) -> u32 {
        self.reserve_with_template(plot_id, n, PickTemplate::GeomRows { geom_id })
    }

    /// Reserve a single ticket that resolves to `PickEntry::Custom`
    /// with the given `kind` and `data`. Useful for ad-hoc pickable
    /// regions (brush handles, custom geoms, etc.).
    pub fn reserve_custom(&mut self, plot_id: Arc<str>, kind: Arc<str>, data: u64) -> u32 {
        self.reserve_with_template(
            plot_id,
            1,
            PickTemplate::Fixed(FixedTemplate::Custom { kind, data }),
        )
    }

    fn reserve_with_template(
        &mut self,
        plot_id: Arc<str>,
        n: usize,
        template: PickTemplate,
    ) -> u32 {
        if n == 0 {
            return self.total;
        }
        let new_total = match (n as u64).checked_add(self.total as u64) {
            Some(t) if t <= MAX_TABLE_TOTAL as u64 => t as u32,
            _ => return u32::MAX,
        };
        let base = self.total;
        self.ranges.push(PickRange {
            plot_id,
            template,
            start: base,
            end: new_total,
        });
        self.total = new_total;
        base
    }

    /// Resolve a raw `PickId::Id(n)` value. Returns `None` for `n = 0`
    /// (reserved for `PickId::Block`) or when the ticket falls outside
    /// any reserved range.
    pub fn resolve(&self, raw: u32) -> Option<PickEntry> {
        if raw == 0 {
            return None;
        }
        let ticket = raw - 1;
        // Ranges are pushed in ascending `start` order, so a partition_point
        // gives us the first range with start > ticket; the candidate is
        // its predecessor.
        let idx = self
            .ranges
            .partition_point(|r| r.start <= ticket)
            .checked_sub(1)?;
        let r = &self.ranges[idx];
        if ticket >= r.end {
            return None;
        }
        let offset = (ticket - r.start) as usize;
        Some(r.template.at(r.plot_id.clone(), offset))
    }

    /// Number of tickets currently reserved.
    pub fn len(&self) -> usize {
        self.total as usize
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

/// 24-bit ticket cap — matches the `PickId` encoding budget.
const MAX_TABLE_TOTAL: u32 = 0xFFFFFF;

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

    /// Tracked but unused in v1 — Phase 7 orchestrator surfaces it
    /// for partial-repaint heuristics.
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
            dirty: true,
        }
    }

    /// Read accessor for the bound patch id.
    pub fn patch_id(&self) -> &str {
        &self.patch_id
    }

    // ── Chaining (config) ──

    pub fn title(mut self, s: impl Into<String>) -> Self {
        self.title = Some(s.into());
        self
    }

    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    pub fn caption(mut self, s: impl Into<String>) -> Self {
        self.caption = Some(s.into());
        self
    }

    pub fn axis_left_title(mut self, s: impl Into<String>) -> Self {
        self.axis_left_title = Some(s.into());
        self
    }

    pub fn axis_bottom_title(mut self, s: impl Into<String>) -> Self {
        self.axis_bottom_title = Some(s.into());
        self
    }

    pub fn axis_right_title(mut self, s: impl Into<String>) -> Self {
        self.axis_right_title = Some(s.into());
        self
    }

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

    pub fn shape_registry(mut self, r: ShapeRegistry) -> Self {
        self.shapes = r;
        self
    }

    // ── Mutators ──

    pub fn set_title(&mut self, s: impl Into<String>) {
        self.title = Some(s.into());
        self.dirty = true;
    }

    pub fn clear_title(&mut self) {
        self.title = None;
        self.dirty = true;
    }

    pub fn set_binding(&mut self, channel: impl Into<String>, scale_name: impl Into<String>) {
        self.bindings.insert(channel.into(), scale_name.into());
        self.dirty = true;
    }

    pub fn unbind(&mut self, channel: &str) -> Option<String> {
        let removed = self.bindings.remove(channel);
        if removed.is_some() {
            self.dirty = true;
        }
        removed
    }

    pub fn bindings(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.bindings.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn binding(&self, channel: &str) -> Option<&str> {
        self.bindings.get(channel).map(|s| s.as_str())
    }

    // ── Geom management ──

    pub fn add_geom<G: Geom>(&mut self, geom: G) -> GeomId {
        let id = GeomId(self.next_geom_id);
        self.next_geom_id = self.next_geom_id.wrapping_add(1);
        self.geoms.push((id, Box::new(geom)));
        self.dirty = true;
        id
    }

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
    /// For each geom, reserves `len()` contiguous tickets in
    /// `pick_table` and passes the base via [`GeomContext::ticket_base`];
    /// the geom emits `PickId::Id(base + row + 1)` per drawn primitive.
    /// Stand-alone callers that don't care about picking pass a fresh
    /// `PickTable::new()` and ignore it.
    pub fn draw_panel_into(
        &mut self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        registry: &ScaleRegistry,
        dpi: f64,
        pick_table: &mut PickTable,
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

        let resolver = PlotScaleResolver {
            bindings: &self.bindings,
            registry,
        };
        // Reserve a contiguous ticket range per geom, then draw each
        // with its base in the context. `Geom::mark_count()` returns the
        // pickable mark count (= rows for PointGeom; = unique-key /
        // group count for multi-row-per-mark geoms like LineGeom).
        // 0 → emit Skip.
        for (gid, geom) in self.geoms.iter() {
            let n = geom.mark_count();
            let base = pick_table.reserve_geom_rows(self.patch_id.clone(), *gid, n);
            let mut ctx = GeomContext::new(panel, dpi, &self.shapes, &resolver);
            ctx.ticket_base = if n > 0 && base != u32::MAX {
                Some(base)
            } else {
                None
            };
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

        // Axes — bottom (x binding) and left (y binding).
        let panel = layout.get(&self.patch_id, Slot::Panel);
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

    fn expect_geom(entry: &PickEntry) -> (&str, GeomId, usize) {
        match entry {
            PickEntry::Geom {
                plot_id,
                geom_id,
                row,
            } => (plot_id, *geom_id, *row),
            other => panic!("expected PickEntry::Geom, got {other:?}"),
        }
    }

    #[test]
    fn pick_table_round_trips_single_geom() {
        let mut table = PickTable::new();
        let gid = GeomId(7);
        let plot_id: Arc<str> = Arc::from("plot_a");
        let base = table.reserve_geom_rows(plot_id.clone(), gid, 100);
        assert_eq!(base, 0);
        // Ticket = base + row + 1 = 0 + 5 + 1 = 6.
        let entry = table.resolve(6).expect("expected to resolve");
        let (pid, g, r) = expect_geom(&entry);
        assert_eq!(pid, "plot_a");
        assert_eq!(g, gid);
        assert_eq!(r, 5);
    }

    #[test]
    fn pick_table_id_zero_is_none() {
        let mut table = PickTable::new();
        table.reserve_geom_rows(Arc::from("plot_a"), GeomId(0), 10);
        assert!(table.resolve(0).is_none());
    }

    #[test]
    fn pick_table_out_of_range_is_none() {
        let mut table = PickTable::new();
        // Reserve only 3 tickets (1, 2, 3).
        table.reserve_geom_rows(Arc::from("plot_a"), GeomId(0), 3);
        assert!(table.resolve(4).is_none());
        assert!(table.resolve(1_000_000).is_none());
    }

    #[test]
    fn pick_table_distinguishes_plots_and_geoms() {
        let mut table = PickTable::new();
        let plot_a: Arc<str> = Arc::from("plot_a");
        let plot_b: Arc<str> = Arc::from("plot_b");
        let _ = table.reserve_geom_rows(plot_a.clone(), GeomId(0), 3);
        let _ = table.reserve_geom_rows(plot_a.clone(), GeomId(1), 2);
        let _ = table.reserve_geom_rows(plot_b.clone(), GeomId(0), 3);

        let e1 = table.resolve(2).unwrap();
        assert_eq!(expect_geom(&e1), ("plot_a", GeomId(0), 1));
        let e2 = table.resolve(5).unwrap();
        assert_eq!(expect_geom(&e2), ("plot_a", GeomId(1), 1));
        let e3 = table.resolve(7).unwrap();
        assert_eq!(expect_geom(&e3), ("plot_b", GeomId(0), 1));
    }

    #[test]
    fn pick_table_reserve_zero_is_noop() {
        let mut table = PickTable::new();
        let base = table.reserve_geom_rows(Arc::from("p"), GeomId(0), 0);
        assert_eq!(base, 0);
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
    }

    #[test]
    fn pick_table_clear_resets() {
        let mut table = PickTable::new();
        table.reserve_geom_rows(Arc::from("p"), GeomId(0), 10);
        assert!(!table.is_empty());
        table.clear();
        assert!(table.is_empty());
        assert!(table.resolve(5).is_none());
    }

    #[test]
    fn pick_table_handles_24bit_cap() {
        let mut table = PickTable::new();
        let base_a = table.reserve_geom_rows(Arc::from("a"), GeomId(0), 0x800000);
        assert_eq!(base_a, 0);
        let base_b = table.reserve_geom_rows(Arc::from("b"), GeomId(0), 0x800000);
        assert_eq!(base_b, u32::MAX);
    }

    #[test]
    fn pick_table_custom_entry_round_trips() {
        let mut table = PickTable::new();
        let plot_id: Arc<str> = Arc::from("p");
        let kind: Arc<str> = Arc::from("brush_handle");
        let base = table.reserve_custom(plot_id.clone(), kind.clone(), 42);
        assert_eq!(base, 0);
        let entry = table.resolve(1).expect("ticket 1");
        match entry {
            PickEntry::Custom {
                plot_id,
                kind: k,
                data,
            } => {
                assert_eq!(&*plot_id, "p");
                assert_eq!(&*k, "brush_handle");
                assert_eq!(data, 42);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn pick_entry_plot_id_accessor() {
        // All variants expose plot_id via the accessor — even non-geom
        // ones (forward-looking for v1.5 chrome picking).
        let geom = PickEntry::Geom {
            plot_id: Arc::from("a"),
            geom_id: GeomId(0),
            row: 0,
        };
        assert_eq!(geom.plot_id(), "a");

        let panel = PickEntry::PanelBackground {
            plot_id: Arc::from("b"),
        };
        assert_eq!(panel.plot_id(), "b");

        let text = PickEntry::TextSlot {
            plot_id: Arc::from("c"),
            slot: PickTextSlot::Title,
        };
        assert_eq!(text.plot_id(), "c");
    }

    #[test]
    fn draw_panel_into_skips_when_panel_missing() {
        // A composition where patch "a" exists but has no Panel slot in
        // the solved layout (because we didn't wire one). draw_panel_into
        // should silently no-op.
        let c = comp_with_two();
        let mut p = Plot::new(&c, "a");
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", crate::color::Color::new([1.0, 0.0, 0.0, 1.0]))
            .build();
        p.add_geom(g);
        // Solve without wiring → no Panel slot for "a".
        let layout = c.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
        let mut scene = crate::scene::recording::RecordingScene::default();
        let registry = ScaleRegistry::new();
        let mut pick = PickTable::new();
        p.draw_panel_into(&mut scene, &layout, &registry, 96.0, &mut pick);
        // The composition itself emits no ops; only checking that we
        // didn't panic.
        let _ = scene.ops.len();
    }

    #[test]
    fn draw_panel_into_populates_pick_table() {
        // End-to-end: build a plot, wire+solve a composition that
        // produces a Panel rect, draw, then verify the pick table
        // resolves a known glyph back to the right (plot, geom, row).
        let c = comp_with_two();
        let mut p = Plot::new(&c, "a");
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .set("fill", crate::color::Color::new([1.0, 0.0, 0.0, 1.0]))
            .build();
        let id = p.add_geom(g);
        // Wire the plot's chrome (under text feature it adds axes; here
        // we just need the Panel slot — Plot has a stand-alone path
        // that's text-only. For non-text builds we'd manually attach a
        // Panel cell; for the always-on test we just patch a Panel slot
        // directly into the composition.
        use crate::composition::Patch as CompPatch;
        use crate::layout::Cell;
        let comp = beside(
            CompPatch::new("a").slot(Slot::Panel, Cell::empty()),
            CompPatch::new("b"),
        );
        let layout = comp.solve(crate::geometry::Size::new(400.0, 300.0), 96.0);
        let mut scene = crate::scene::recording::RecordingScene::default();
        let registry = ScaleRegistry::new();
        let mut pick = PickTable::new();
        p.draw_panel_into(&mut scene, &layout, &registry, 96.0, &mut pick);
        // Three rows reserved → tickets 1, 2, 3.
        assert_eq!(pick.len(), 3);
        let entry = pick.resolve(2).expect("ticket 2");
        let (pid, gid, row) = expect_geom(&entry);
        assert_eq!(pid, "a");
        assert_eq!(gid, id);
        assert_eq!(row, 1);
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
