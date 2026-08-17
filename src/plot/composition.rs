//! `PlotComposition` — the orchestrator that owns the layout composition,
//! the scale registry, and the attached plots. The canonical user-facing
//! surface above the per-plot primitives in [`crate::plot::plot`].
//!
//! Lifecycle:
//!
//! 1. User builds a `Composition` (the layout shape — named, empty
//!    patches) and hands it to `PlotComposition::new(&comp)`.
//!    The orchestrator captures a clone-friendly description of the
//!    composition shape (placements + ids + tracks + the composition's
//!    own aspect / margin / padding), leaving the caller's value intact.
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
use crate::layout::{Inset, Track};
use crate::scene::SceneBuilder;
use crate::shape::ShapeRegistry;

use super::plot::Plot;
use super::scale::{Scale, ScaleRegistry};
use super::theme::Theme;

#[cfg(feature = "text")]
use super::scale::AxisSide;

/// Id given to the root composition when the caller didn't name it with
/// [`Composition::id`]. Composition-level chrome rects resolve as
/// `(composition_id, region)`, so the root needs an id before any of its
/// chrome can be looked up in the solved layout.
const ROOT_COMPOSITION_ID: &str = "__hephaestus_root__";

// ─── Template ────────────────────────────────────────────────────────────────

/// Clone-friendly description of a [`Composition`]'s shape — captured at
/// `PlotComposition::new` so a composition (which may contain non-Clone
/// `Cell` values) can be rebuilt fresh on every render with each plot's
/// chrome wired in.
///
/// Captures placement metadata, ids, and the composition's own geometry
/// fields (aspect / margin / padding). Chrome *cells* attached directly
/// to a patch or a composition before passing it to
/// `PlotComposition::new` are **dropped** — the orchestrator expects
/// empty patches and re-attaches chrome from each plot (and from its own
/// composition-level chrome state) on every render. A raw `Cell` can
/// reserve space but carries nothing the orchestrator could draw, so
/// preserving it would produce reserved-but-blank chrome bands.
#[derive(Debug, Clone)]
struct CompositionTemplate {
    /// The composition's id. Composition-level chrome rects resolve as
    /// `(id, region)`, so the root is always given one — see
    /// [`ROOT_COMPOSITION_ID`].
    id: Option<String>,
    rows: usize,
    cols: usize,
    widths: Vec<Track>,
    heights: Vec<Track>,
    aspect: Option<(f32, f32)>,
    margin: Inset,
    padding: Inset,
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
    /// Nested composition (placed via `Composition::place`). Boxed —
    /// a nested template dwarfs the other variants.
    Composition(Box<CompositionTemplate>),
}

impl CompositionTemplate {
    /// Capture the shape of a `Composition` into a clone-friendly
    /// description. The walker visits every placement; nested
    /// compositions recurse.
    fn capture(c: &Composition) -> Self {
        Self {
            id: c.composition_id().map(str::to_string),
            rows: c.rows(),
            cols: c.cols(),
            widths: c.widths_slice().to_vec(),
            heights: c.heights_slice().to_vec(),
            aspect: c.aspect_ratio(),
            margin: c.margin_inset().clone(),
            padding: c.padding_inset().clone(),
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
    /// patch's chrome via the attached plots and each named
    /// composition's chrome from `ctx`. Per-patch aspect locks
    /// are propagated into the outer grid's Fr weights by
    /// `emit_patch_into` at solve time — depending on whether each
    /// patch is alone in its row or column, the aspect is encoded
    /// into either the row Fr or the column Fr so siblings on the
    /// other axis don't conflict.
    fn rebuild(&self, ctx: &RebuildCtx<'_>) -> Composition {
        let mut c = Composition::empty(self.rows, self.cols);
        if let Some(id) = &self.id {
            c = c.id(id.clone());
        }
        if !self.widths.is_empty() {
            c = c.widths(self.widths.clone());
        }
        if !self.heights.is_empty() {
            c = c.heights(self.heights.clone());
        }
        if let Some((w, h)) = self.aspect {
            c = c.aspect(w, h);
        }
        c = c.margin(self.margin.clone()).padding(self.padding.clone());
        for p in &self.placements {
            let element = p.element.rebuild(ctx);
            c = c.place(p.row, p.col, p.span, element);
        }
        // Composition-level chrome, keyed on this composition's id.
        // Wired after the placements so the chrome slots land on the
        // canonical wrapping block around the finished facet grid.
        if let Some(chrome) = self.id.as_deref().and_then(|id| ctx.chrome.get(id)) {
            c = chrome.wire(c, ctx);
        }
        c
    }
}

/// The per-render inputs `CompositionTemplate::rebuild` needs: the
/// attached plots, the scales they resolve through, and the
/// composition-level chrome keyed by composition id.
struct RebuildCtx<'a> {
    plots: &'a HashMap<String, Vec<Plot>>,
    registry: &'a ScaleRegistry,
    dpi: f64,
    theme: &'a Theme,
    chrome: &'a HashMap<String, CompositionChrome>,
    /// Registry the composition's own legend keys size their markers
    /// against — the same one their draw step resolves shapes through.
    #[cfg(feature = "text")]
    shapes: &'a ShapeRegistry,
}

impl ElementTemplate {
    fn capture(e: &Element) -> Self {
        match e {
            Element::Patch(p) => match p.id() {
                Some(id) => ElementTemplate::NamedPatch(id.to_string()),
                None => ElementTemplate::Spacer,
            },
            Element::Composition(c) => {
                ElementTemplate::Composition(Box::new(CompositionTemplate::capture(c)))
            }
        }
    }

    fn rebuild(&self, ctx: &RebuildCtx<'_>) -> Element {
        match self {
            ElementTemplate::NamedPatch(id) => {
                let patch = wire_into_patch(id, ctx.plots, ctx.registry, ctx.dpi, ctx.theme);
                Element::Patch(patch)
            }
            ElementTemplate::Spacer => Element::Patch(spacer()),
            ElementTemplate::Composition(inner) => Element::Composition(inner.rebuild(ctx)),
        }
    }
}

/// Build a `Patch` for `id` by wiring every attached plot's chrome,
/// merging slot contributions across plots. Each plot's `wire()`
/// builds a single-plot patch independently; the orchestrator then
/// harvests their [`PatchPlacement`]s, groups by region, and
/// re-emits one cell per region — wrapping multiple contributions
/// in [`MaxMergeMeasure`](crate::layout::MaxMergeMeasure) so a
/// region's track sizes to fit *every* contributing plot's
/// requirement, not just the last one.
fn wire_into_patch(
    id: &str,
    plots: &HashMap<String, Vec<Plot>>,
    registry: &ScaleRegistry,
    dpi: f64,
    comp_theme: &Theme,
) -> Patch {
    let plot_list = plots.get(id);
    #[cfg(feature = "text")]
    {
        if let Some(list) = plot_list {
            if !list.is_empty() {
                return wire_merged_patch(id, list, registry, dpi, comp_theme);
            }
        }
    }
    let _ = registry;
    let _ = dpi;
    // Unattached patches, or non-text builds: just attach a Panel
    // slot so the layout still places a rect for that patch. The
    // outer margin band is still sized from `theme.plot_margin` so
    // an empty patch sits within the same chrome rhythm as a
    // populated one.
    let p = apply_plot_chrome(Patch::new(id), comp_theme);
    match plot_list.and_then(|list| list.first()) {
        Some(pl) => pl.wire_panel(p),
        None => pl_wire_panel_fallback(p),
    }
}

/// Stitch the theme's outer-chrome fields onto the patch:
///
/// - `theme.plot_margin` → `Patch::margin` (outermost ring tracks).
/// - `theme.plot_padding` → `Patch::padding` (second-ring tracks).
/// - `theme.plot_background` → if `Set`, drop an empty cell into
///   `Slot::Background` so the solver emits a rect for it (the cell
///   has no size opinion of its own; the slot's anatomical span
///   takes whatever the surrounding tracks allow). When `Blank`,
///   the slot is left out — `draw_patch_background_into` then
///   no-ops via the `as_set()` short-circuit.
fn apply_plot_chrome(patch: Patch, theme: &Theme) -> Patch {
    use crate::composition::Slot;
    use crate::layout::{Cell, Inset, Length as LayoutLength};
    let root_pt = theme
        .text
        .size_pt
        .map(|l| l.resolve(crate::plot::theme::DEFAULT_TEXT_SIZE_PT))
        .unwrap_or(crate::plot::theme::DEFAULT_TEXT_SIZE_PT);
    let margin_inset = {
        let (mt, mr, mb, ml) = theme.plot_margin.resolve(root_pt);
        (mt != 0.0 || mr != 0.0 || mb != 0.0 || ml != 0.0).then(|| {
            Inset::default()
                .top(LayoutLength::pt(mt))
                .right(LayoutLength::pt(mr))
                .bottom(LayoutLength::pt(mb))
                .left(LayoutLength::pt(ml))
        })
    };
    let padding_inset = {
        let (pt, pr, pb, pl) = theme.plot_padding.resolve(root_pt);
        (pt != 0.0 || pr != 0.0 || pb != 0.0 || pl != 0.0).then(|| {
            Inset::default()
                .top(LayoutLength::pt(pt))
                .right(LayoutLength::pt(pr))
                .bottom(LayoutLength::pt(pb))
                .left(LayoutLength::pt(pl))
        })
    };
    let mut patch = patch;
    if let Some(inset) = margin_inset {
        patch = patch.margin(inset);
    }
    if let Some(inset) = padding_inset {
        patch = patch.padding(inset);
    }
    if theme.plot_background.as_set().is_some() {
        patch = patch.slot(Slot::Background, Cell::empty());
    }
    patch
}

/// Per-plot harvest + max-merge. Each plot wires into its own
/// fresh patch; we then group the resulting placements by region
/// across plots, max-merging the underlying [`Measure`]s.
#[cfg(feature = "text")]
fn wire_merged_patch(
    id: &str,
    plots: &[Plot],
    registry: &ScaleRegistry,
    dpi: f64,
    comp_theme: &Theme,
) -> Patch {
    use crate::composition::PatchPlacement;
    use crate::layout::{Cell, MaxMergeMeasure, Placement};

    // Collect (region → Vec of (placement, measure)). Insertion order
    // captures the first plot's placement geometry, which all other
    // plots' contributions to the same region must match (they
    // resolve through the same `Slot::name()` →
    // `Slot::placement()` mapping).
    type RegionEntry = (String, Placement, Vec<Box<dyn crate::layout::Measure>>);
    let mut by_region: Vec<RegionEntry> = Vec::new();
    for plot in plots {
        let effective_theme = match plot.theme_override_ref() {
            Some(part) => comp_theme.merge(part),
            None => comp_theme.clone(),
        };
        let per_plot = plot.wire(Patch::new(id), registry, dpi, &effective_theme);
        for PatchPlacement {
            placement,
            region,
            cell,
        } in per_plot.into_placements()
        {
            if let Some((_, _, measures)) = by_region.iter_mut().find(|(r, _, _)| *r == region) {
                measures.push(cell.into_measure());
            } else {
                by_region.push((region, placement, vec![cell.into_measure()]));
            }
        }
    }

    // Emit one cell per region with a max-merged measure.
    let mut patch = apply_plot_chrome(Patch::new(id), comp_theme);
    // Cross-plot aspect agreement: every plot that has an opinion
    // on the panel aspect (polar projections, today) contributes
    // its `desired_panel_aspect`. If all opinions agree on the
    // same ratio, lock the patch to that aspect. If any pair
    // disagrees, skip the lock — the user is in an advanced
    // multi-projection layout and the orchestrator can't pick a
    // winner.
    let aspects: Vec<(f32, f32)> = plots
        .iter()
        .filter_map(|p| p.desired_panel_aspect(registry))
        .collect();
    if let Some(agreed) = unify_aspect(&aspects) {
        patch = patch.aspect(agreed.0, agreed.1);
    }
    for (region, placement, mut measures) in by_region {
        let cell = if measures.len() == 1 {
            // Single contribution — no need to wrap.
            Cell::measured_boxed(measures.pop().unwrap())
        } else {
            Cell::measured(MaxMergeMeasure::new(measures))
        };
        patch = patch.place_at(
            region,
            placement.row,
            placement.col,
            crate::composition::Span::rc(placement.row_span, placement.col_span),
            cell,
        );
    }
    patch
}

/// Reduce a list of `(width, height)` aspect demands to one, if
/// every demand has the same `w/h` ratio (within a small epsilon).
/// Returns the first demand if they all agree, `None` if any
/// disagrees or the list is empty.
#[cfg(feature = "text")]
fn unify_aspect(aspects: &[(f32, f32)]) -> Option<(f32, f32)> {
    let first = *aspects.first()?;
    let ratio0 = (first.0 as f64) / (first.1 as f64);
    const EPS: f64 = 1e-3;
    for &(w, h) in &aspects[1..] {
        let r = (w as f64) / (h as f64);
        if (r - ratio0).abs() > EPS {
            return None;
        }
    }
    Some(first)
}

/// Same as `Plot::wire_panel` but for unattached patches (no Plot
/// instance to dispatch through).
fn pl_wire_panel_fallback(p: Patch) -> Patch {
    use crate::composition::Slot;
    use crate::layout::Cell;
    p.slot(Slot::Panel, Cell::empty())
}

// ─── Composition-level chrome ────────────────────────────────────────────────

/// The four cartesian sides, in `axis_side_index` order.
#[cfg(feature = "text")]
const AXIS_TITLE_SIDES: [AxisSide; 4] = [
    AxisSide::Top,
    AxisSide::Right,
    AxisSide::Bottom,
    AxisSide::Left,
];

/// Composition legends sit in the composition's anatomical legend ring;
/// there is no composition-owned panel rect to anchor against.
#[cfg(feature = "text")]
fn reject_in_panel_legend(legend: &crate::plot::chrome::legend::Legend) {
    use crate::scales::chrome::LegendSide;
    assert!(
        !matches!(legend.side, LegendSide::InPanel { .. }),
        "PlotComposition legends cannot use LegendSide::InPanel; the composition's panel cell is filled by its facets"
    );
}

/// Chrome owned by a composition rather than by one of its patches: a
/// title / subtitle / caption spanning every facet, axis titles shared
/// across a facet row or column, and legends placed in the
/// composition's own legend ring.
///
/// [`PlotComposition`] holds one of these per composition id and
/// addresses the root through [`ROOT_COMPOSITION_ID`], so the same
/// wiring path serves the root and any named nested composition.
#[derive(Default)]
struct CompositionChrome {
    title: Option<String>,
    subtitle: Option<String>,
    caption: Option<String>,
    /// Axis titles by side, indexed as by
    /// [`axis_side_index`](crate::plot::plot::axis_side_index).
    #[cfg(feature = "text")]
    axis_titles: [Option<String>; 4],
    /// Legends attached to the composition. Same opt-in model as
    /// [`Plot`]: nothing is inferred from the plots' bindings.
    #[cfg(feature = "text")]
    legends: Vec<crate::plot::chrome::legend::Legend>,
    #[cfg(feature = "text")]
    next_legend_id: u32,
}

impl CompositionChrome {
    /// Drop this chrome's cells into `c`'s composition-level slots.
    /// Populating any slot wraps the facets in a canonical 13×16 block,
    /// so the chrome spans the whole facet grid.
    #[cfg(feature = "text")]
    fn wire(&self, mut c: Composition, ctx: &RebuildCtx<'_>) -> Composition {
        use super::plot::{
            axis_title_cell, cartesian_axis_title_slot, effective_text, legends_grouped_by_side,
            text_cell_for_element, title_band_placement,
        };
        use crate::composition::Slot;
        use crate::layout::Cell;

        let theme = ctx.theme;
        let root_pt = theme
            .text
            .size_pt
            .map(|l| l.resolve(crate::plot::theme::DEFAULT_TEXT_SIZE_PT))
            .unwrap_or(crate::plot::theme::DEFAULT_TEXT_SIZE_PT);

        // Title / subtitle / caption — `theme.plot_text_align_to`
        // chooses between spanning the whole composition interior and
        // spanning just the facet band, exactly as it does per plot.
        for (slot, text_opt, theme_slot) in [
            (Slot::Title, self.title.as_ref(), &theme.plot_title),
            (Slot::Subtitle, self.subtitle.as_ref(), &theme.plot_subtitle),
            (Slot::Caption, self.caption.as_ref(), &theme.plot_caption),
        ] {
            if let (Some(t), Some(el)) = (text_opt, effective_text(theme_slot, &theme.text)) {
                let (r, col, rs, cs) = title_band_placement(slot, theme.plot_text_align_to);
                c = c.place_at(
                    slot.name(),
                    r,
                    col,
                    Span::rc(rs, cs),
                    text_cell_for_element(t, &el, root_pt, ctx.dpi, theme),
                );
            }
        }

        // Axis titles. A composition has no panel of its own, so
        // `TitleLocation::Inside` has nothing to sit against — the
        // title always takes the outer slot.
        for side in AXIS_TITLE_SIDES {
            if let Some(title) = self.axis_title_at(side) {
                c = c.slot(
                    cartesian_axis_title_slot(side),
                    axis_title_cell(title, side, theme, ctx.dpi),
                );
            }
        }

        // Legends — one stack per populated side, sized through the
        // same measure the per-plot ring uses.
        let collapsed = crate::plot::chrome::legend::collapse_legends(
            &self.legends,
            ctx.registry,
            &theme.locale,
        );
        for (side, slot, group) in legends_grouped_by_side(&collapsed) {
            if group.is_empty() {
                continue;
            }
            c = c.slot(
                slot,
                Cell::measured_boxed(crate::plot::chrome::legend::legend_stack_measure(
                    &group,
                    side,
                    ctx.registry,
                    ctx.shapes,
                    ctx.dpi,
                    theme,
                )),
            );
        }
        c
    }

    /// Non-text builds have no shaper, so composition chrome reserves
    /// no space — mirrors `Plot::wire_panel` standing in for `wire`.
    #[cfg(not(feature = "text"))]
    fn wire(&self, c: Composition, _ctx: &RebuildCtx<'_>) -> Composition {
        c
    }

    /// Read the axis title installed for `side`, if any.
    #[cfg(feature = "text")]
    fn axis_title_at(&self, side: AxisSide) -> Option<&str> {
        self.axis_titles[super::plot::axis_side_index(side)].as_deref()
    }

    /// Render this chrome into the rects solved for `comp_id`.
    #[cfg(feature = "text")]
    #[allow(clippy::too_many_arguments)]
    fn draw_into(
        &self,
        comp_id: &str,
        scene: &mut dyn SceneBuilder,
        layout: &CompositionLayout,
        registry: &ScaleRegistry,
        shapes: &ShapeRegistry,
        dpi: f64,
        theme: &Theme,
    ) {
        use super::plot::{
            cartesian_axis_title_slot, draw_axis_title, draw_text_element_in_rect, effective_text,
            legends_grouped_by_side,
        };
        use crate::brush::Brush;
        use crate::composition::Slot;
        use crate::plot::theme::text_concrete_defaults;
        use crate::text::TextRun;

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
            let (Some(text), Some(rect), Some(el)) = (
                text_opt,
                layout.get(comp_id, slot),
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
                Some(&theme.rich_text),
            );
        }

        let text_defaults = text_concrete_defaults();
        for side in AXIS_TITLE_SIDES {
            let Some(title) = self.axis_title_at(side) else {
                continue;
            };
            let (ch, side_idx) = crate::plot::chrome::axis::axis_side_to_channel_side(side);
            let Some(el) = theme.axis.resolve(ch, side_idx).title else {
                continue;
            };
            let Some(rect) = layout.get(comp_id, cartesian_axis_title_slot(side)) else {
                continue;
            };
            let color = el
                .color
                .clone()
                .or_else(|| text_defaults.color.clone())
                .expect("text_concrete_defaults sets color");
            let angle = el
                .angle
                .or(text_defaults.angle)
                .expect("text_concrete_defaults sets angle");
            let style = super::plot::text_style_from(&el, root_pt);
            if matches!(el.markdown, Some(true)) {
                super::plot::draw_axis_title_markdown(
                    scene,
                    title,
                    &style,
                    color.resolve(&theme.palette),
                    &theme.palette,
                    &theme.rich_text,
                    dpi,
                    rect,
                    side,
                    angle,
                );
                continue;
            }
            let run = TextRun::new(title, &style, dpi);
            let outline = super::plot::text_outline_from(&el, &theme.palette, dpi);
            draw_axis_title(
                scene,
                &run,
                rect,
                side,
                &Brush::Solid(color.resolve(&theme.palette)),
                outline.as_ref(),
                angle,
            );
        }

        // Must collapse identically to `wire` for the measured space
        // to match what gets drawn.
        let collapsed =
            crate::plot::chrome::legend::collapse_legends(&self.legends, registry, &theme.locale);
        for (side, slot, group) in legends_grouped_by_side(&collapsed) {
            if group.is_empty() {
                continue;
            }
            if let Some(rect) = layout.get(comp_id, slot) {
                crate::plot::chrome::legend::render_legend_stack(
                    &group, side, rect, registry, shapes, scene, dpi, theme,
                );
            }
        }
    }
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
    /// Shared theme applied to every attached plot at render time.
    /// Each plot can override with its own [`ThemePart`]; the
    /// orchestrator merges per plot before drawing.
    theme: Arc<Theme>,
    /// Plots attached to each patch id, in attach order. Multiple
    /// plots per patch are supported — each draws its own chrome
    /// into the same patch slots, with later plots overlaying
    /// earlier ones (the user controls draw order via attach
    /// order).
    plots: HashMap<String, Vec<Plot>>,
    /// Composition-level chrome by composition id. The root's entry is
    /// keyed on `root_id`; the builder / mutator methods on this type
    /// all target that entry.
    chrome: HashMap<String, CompositionChrome>,
    /// Id of the root composition — the caller's [`Composition::id`] if
    /// they set one, otherwise [`ROOT_COMPOSITION_ID`].
    root_id: String,
    /// Registry backing the glyphs drawn in composition-level legend
    /// keys. Per-plot legends use their own plot's registry.
    shapes: ShapeRegistry,
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
    /// Construct from a layout-level [`Composition`]. Captures the
    /// shape (placements / ids / tracks) plus the composition's own
    /// aspect / margin / padding for the per-render rebuild walk. The
    /// root is given an id when it doesn't have one, so its
    /// composition-level chrome is addressable in the solved layout.
    pub fn new(composition: &Composition) -> Self {
        let mut template = CompositionTemplate::capture(composition);
        let root_id = template
            .id
            .get_or_insert_with(|| ROOT_COMPOSITION_ID.to_string())
            .clone();
        Self {
            template,
            scales: ScaleRegistry::new(),
            theme: Arc::new(Theme::default()),
            plots: HashMap::new(),
            chrome: HashMap::new(),
            root_id,
            shapes: ShapeRegistry::with_builtins(),
            plot_dirty: HashMap::new(),
            scale_dirty: HashMap::new(),
            layout_dirty: true,
            last_layout: None,
            last_size: None,
            last_dpi: None,
        }
    }

    // ── Theme ─────────────────────────────────────────────────────────

    /// Install a [`Theme`] on this composition. Chainable builder form
    /// of [`Self::set_theme`]. Replaces any previously installed
    /// theme; flags the layout dirty.
    pub fn theme(mut self, theme: Theme) -> Self {
        self.set_theme(theme);
        self
    }

    /// Install a [`Theme`] on this composition. Flags the layout
    /// dirty so the next render re-solves with the new chrome
    /// styling.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = Arc::new(theme);
        self.layout_dirty = true;
    }

    /// Mutate the active theme through a closure. Flags the layout
    /// dirty. Clones the theme if it's currently shared elsewhere
    /// (Arc-on-write).
    pub fn update_theme(&mut self, f: impl FnOnce(&mut Theme)) {
        let theme = Arc::make_mut(&mut self.theme);
        f(theme);
        self.layout_dirty = true;
    }

    /// Borrow the active theme. Cheap — `Arc` deref.
    pub fn theme_ref(&self) -> &Theme {
        &self.theme
    }

    /// Compute the effective theme for a specific plot — the
    /// composition's theme with the plot's `ThemePart` override
    /// (if any) applied on top.
    fn effective_theme_for(&self, plot: &Plot) -> Theme {
        match plot.theme_override_ref() {
            Some(part) => self.theme.merge(part),
            None => (*self.theme).clone(),
        }
    }

    // ── Composition-level chrome ──────────────────────────────────────

    /// Borrow the root composition's chrome, creating it on first use.
    fn root_chrome_mut(&mut self) -> &mut CompositionChrome {
        self.layout_dirty = true;
        self.chrome.entry(self.root_id.clone()).or_default()
    }

    /// Set the composition's title, rendered in the [`Slot::Title`]
    /// slot of the canonical block that wraps every facet. Chainable
    /// builder form of [`Self::set_title`].
    ///
    /// [`Slot::Title`]: crate::composition::Slot::Title
    pub fn title(mut self, s: impl Into<String>) -> Self {
        self.set_title(s);
        self
    }

    /// Set the composition's subtitle, rendered in the
    /// [`Slot::Subtitle`] slot spanning every facet.
    ///
    /// [`Slot::Subtitle`]: crate::composition::Slot::Subtitle
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.root_chrome_mut().subtitle = Some(s.into());
        self
    }

    /// Set the composition's caption, rendered in the [`Slot::Caption`]
    /// slot spanning every facet.
    ///
    /// [`Slot::Caption`]: crate::composition::Slot::Caption
    pub fn caption(mut self, s: impl Into<String>) -> Self {
        self.root_chrome_mut().caption = Some(s.into());
        self
    }

    /// Replace the composition's title. Flags the layout dirty.
    pub fn set_title(&mut self, s: impl Into<String>) {
        self.root_chrome_mut().title = Some(s.into());
    }

    /// Clear the composition's title, releasing its chrome row. Flags
    /// the layout dirty.
    pub fn clear_title(&mut self) {
        self.root_chrome_mut().title = None;
    }

    /// Set a shared axis title on `side` — one label for the whole
    /// facet grid rather than one per plot. Each side holds at most one
    /// title; calling again with the same side replaces it.
    ///
    /// The title always takes the outer chrome slot: a composition has
    /// no panel of its own, so `TitleLocation::Inside` has nothing to
    /// sit against.
    #[cfg(feature = "text")]
    pub fn axis_title(mut self, side: AxisSide, text: impl Into<String>) -> Self {
        self.set_axis_title(side, Some(text.into()));
        self
    }

    /// Install or clear the shared axis title for `side`. `None`
    /// removes it, releasing the slot. Flags the layout dirty.
    #[cfg(feature = "text")]
    pub fn set_axis_title(&mut self, side: AxisSide, text: Option<String>) {
        let idx = super::plot::axis_side_index(side);
        self.root_chrome_mut().axis_titles[idx] = text;
    }

    /// Read the shared axis title for `side`, if any.
    #[cfg(feature = "text")]
    pub fn axis_title_at(&self, side: AxisSide) -> Option<&str> {
        self.chrome
            .get(&self.root_id)
            .and_then(|c| c.axis_title_at(side))
    }

    /// Attach a legend to the composition, placed in the composition's
    /// own legend ring so one legend serves every facet, and return its
    /// id.
    ///
    /// The legend is stored as given. Merging with a compatible sibling
    /// happens at render time, once the scales it names can be resolved
    /// — see
    /// [`collapse_legends`](crate::plot::chrome::legend::collapse_legends).
    ///
    /// Panics on [`LegendSide::InPanel`] — an in-panel legend anchors
    /// to a panel rect, and the composition's panel cell is filled by
    /// its facets.
    ///
    /// [`LegendSide::InPanel`]: crate::scales::chrome::LegendSide::InPanel
    #[cfg(feature = "text")]
    pub fn add_legend(
        &mut self,
        legend: crate::plot::chrome::legend::Legend,
    ) -> crate::plot::chrome::legend::LegendId {
        self.push_legend(legend)
    }

    /// Attach a composition legend that renders as its own block even
    /// when a compatible legend precedes it. Use when two legends over
    /// the same domain should sit side-by-side instead of sharing rows.
    ///
    /// Panics on [`LegendSide::InPanel`], as [`Self::add_legend`] does.
    ///
    /// [`LegendSide::InPanel`]: crate::scales::chrome::LegendSide::InPanel
    #[cfg(feature = "text")]
    pub fn add_legend_separate(
        &mut self,
        mut legend: crate::plot::chrome::legend::Legend,
    ) -> crate::plot::chrome::legend::LegendId {
        legend.merge = false;
        self.push_legend(legend)
    }

    #[cfg(feature = "text")]
    fn push_legend(
        &mut self,
        legend: crate::plot::chrome::legend::Legend,
    ) -> crate::plot::chrome::legend::LegendId {
        reject_in_panel_legend(&legend);
        let chrome = self.root_chrome_mut();
        let id = crate::plot::chrome::legend::LegendId(chrome.next_legend_id);
        chrome.next_legend_id += 1;
        chrome.legends.push(legend);
        id
    }

    /// Borrow the composition's legends in insertion order.
    #[cfg(feature = "text")]
    pub fn legends(&self) -> &[crate::plot::chrome::legend::Legend] {
        self.chrome
            .get(&self.root_id)
            .map(|c| c.legends.as_slice())
            .unwrap_or(&[])
    }

    /// Remove every legend attached to the composition. Flags the
    /// layout dirty.
    #[cfg(feature = "text")]
    pub fn clear_legends(&mut self) {
        self.root_chrome_mut().legends.clear();
    }

    /// Replace the registry backing composition-level legend key
    /// glyphs. Per-plot legends resolve through their own plot's
    /// registry.
    pub fn shape_registry(mut self, r: ShapeRegistry) -> Self {
        self.shapes = r;
        self
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

    /// Attach a plot bound to its patch id. **Appends** to the patch's
    /// plot list — multiple plots per patch are supported; later
    /// plots draw on top of earlier ones. Flips the layout's dirty
    /// flag.
    pub fn attach_plot(&mut self, plot: Plot) {
        let id = plot.patch_id().to_string();
        self.plot_dirty.insert(id.clone(), true);
        self.plots.entry(id).or_default().push(plot);
        self.layout_dirty = true;
    }

    /// Detach every plot attached to `patch_id`, returning the full
    /// list in attach order. Flips the layout's dirty flag.
    pub fn detach_plot(&mut self, patch_id: &str) -> Vec<Plot> {
        let removed = self.plots.remove(patch_id).unwrap_or_default();
        if !removed.is_empty() {
            self.plot_dirty.insert(patch_id.to_string(), true);
            self.layout_dirty = true;
        }
        removed
    }

    /// Borrow the **first** plot attached to `patch_id`, if any.
    /// Use [`Self::plots_in`] when the patch may hold multiple
    /// plots.
    pub fn plot(&self, patch_id: &str) -> Option<&Plot> {
        self.plots.get(patch_id).and_then(|v| v.first())
    }

    /// Borrow every plot attached to `patch_id`, in attach order.
    pub fn plots_in(&self, patch_id: &str) -> &[Plot] {
        self.plots
            .get(patch_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Iterate over `(patch_id, plot)` pairs across every plot in
    /// every patch. Order is HashMap-iteration for patch ids and
    /// attach order within a patch.
    pub fn plots(&self) -> impl Iterator<Item = (&str, &Plot)> + '_ {
        self.plots
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |p| (k.as_str(), p)))
    }

    /// Mutate the **first** plot at `patch_id` through a closure.
    /// Flags its dirty bit and the global layout dirty bit. Use
    /// [`Self::update_plot_at`] to target a specific plot when the
    /// patch holds multiple.
    pub fn update_plot(&mut self, patch_id: &str, f: impl FnOnce(&mut Plot)) {
        if let Some(p) = self.plots.get_mut(patch_id).and_then(|v| v.first_mut()) {
            f(p);
            self.plot_dirty.insert(patch_id.to_string(), true);
            self.layout_dirty = true;
        }
    }

    /// Mutate the plot at `(patch_id, index)` (zero-based, in
    /// attach order). No-op if the index is out of range.
    pub fn update_plot_at(&mut self, patch_id: &str, index: usize, f: impl FnOnce(&mut Plot)) {
        if let Some(p) = self.plots.get_mut(patch_id).and_then(|v| v.get_mut(index)) {
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
            let comp = self.template.rebuild(&RebuildCtx {
                plots: &self.plots,
                registry: &self.scales,
                dpi,
                theme: &self.theme,
                chrome: &self.chrome,
                #[cfg(feature = "text")]
                shapes: &self.shapes,
            });
            self.last_layout = Some(comp.solve(size, dpi));
            self.last_size = Some(size);
            self.last_dpi = Some(dpi);
            self.layout_dirty = false;
        }

        let layout = self
            .last_layout
            .as_ref()
            .expect("layout must be cached by this point");

        // Phase 1: patch backgrounds. Every plot lays down its
        // `Slot::Background` fill from `theme.plot_background`.
        // Slot::Background's rect already excludes the margin band
        // sized via `theme.plot_margin` — the patch anatomy
        // automatically gives the margin space without any manual
        // translation.
        for list in self.plots.values() {
            for plot in list {
                let effective = self.effective_theme_for(plot);
                plot.draw_patch_background_into(scene, layout, &effective, dpi);
            }
        }

        // Phase 2: panel chromes. Every plot paints its projection
        // bg + grid + outline. Doing this across ALL plots before
        // any geoms means a `clip = false` plot's geoms that spill
        // into a neighbouring panel don't get overpainted by that
        // neighbour's panel chrome in a later step.
        for list in self.plots.values() {
            for plot in list {
                let effective = self.effective_theme_for(plot);
                plot.draw_panel_chrome_into(scene, layout, &self.scales, dpi, &effective);
            }
        }

        // Phase 3: geoms. Each plot installs its own clip (if
        // enabled) and paints its geoms; cross-plot overlays layer
        // in attach order. Picking is opt-in per geom via the
        // `"pick_id"` channel; the orchestrator does no ticket
        // allocation.
        //
        // The borrow checker prevents a single read of
        // `self.effective_theme_for(plot)` here (which borrows
        // `&self.theme`) while we also need `&mut plot`. Resolve the
        // theme into an owned `Theme` first, then mutate.
        let plot_themes: Vec<Theme> = self
            .plots
            .values()
            .flat_map(|list| list.iter())
            .map(|plot| self.effective_theme_for(plot))
            .collect();
        let mut theme_iter = plot_themes.into_iter();
        for list in self.plots.values_mut() {
            for plot in list {
                let effective = theme_iter
                    .next()
                    .expect("plot/theme iteration must stay in sync");
                plot.draw_geoms_into(scene, layout, &self.scales, dpi, &effective);
            }
        }

        // Phase 4: axes + legends + plot text. Cartesian axes
        // render in the patch's anatomical axis slots; polar axes
        // render inside the panel area (without the panel clip),
        // letting labels bleed beyond the inscribed disk and the
        // axis lines paint over the panel background.
        #[cfg(feature = "text")]
        for list in self.plots.values() {
            for plot in list {
                let effective = self.effective_theme_for(plot);
                plot.draw_chrome_into(scene, layout, &self.scales, dpi, &effective);
            }
        }

        // Phase 5: composition-level chrome. Drawn last so a shared
        // title / legend paints over the canonical chrome band it
        // shares with the border facets' own chrome (see
        // `build_wrapped_composition`: both resolve to the same
        // anatomical row, the composition's spanning the full width).
        #[cfg(feature = "text")]
        for (comp_id, chrome) in &self.chrome {
            chrome.draw_into(
                comp_id,
                scene,
                layout,
                &self.scales,
                &self.shapes,
                dpi,
                &self.theme,
            );
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
        for (plot_id, plot) in self
            .plots
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |p| (k, p)))
        {
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
        for (pid, list) in &self.plots {
            let referenced = list
                .iter()
                .any(|plot| plot.bindings().any(|(_, sn)| sn == &*target));
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
        let view = PlotComposition::new(&comp_two());
        // Template captures the two patches.
        assert_eq!(view.template.cols, 2);
        assert_eq!(view.template.rows, 1);
        assert_eq!(view.template.placements.len(), 2);
    }

    #[test]
    fn add_scale_flips_layout_dirty() {
        let mut view = PlotComposition::new(&comp_two());
        view.layout_dirty = false; // pretend a render just happened
        view.insert_scale("x", scale::continuous(0.0..=1.0));
        assert!(view.layout_dirty);
        assert!(view.scale_dirty.get("x").copied().unwrap_or(false));
    }

    #[test]
    fn update_scale_flips_plot_dirty_for_referencing_plots() {
        let mut view = PlotComposition::new(&comp_two())
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
            PlotComposition::new(&comp_two()).with_plot(crate::plot::Plot::new(&comp_two(), "a"));
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
            PlotComposition::new(&comp_two()).with_plot(crate::plot::Plot::new(&comp_two(), "a"));
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
            PlotComposition::new(&comp_two()).with_plot(crate::plot::Plot::new(&comp_two(), "a"));
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
        let mut view = PlotComposition::new(&comp_two()).with_plot(plot);
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
            PlotComposition::new(&comp_two()).with_plot(crate::plot::Plot::new(&comp_two(), "a"));
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(400.0, 300.0), 96.0);
    }

    // ── Composition-level chrome ──

    /// Two plots side by side, each with a panel, so composition chrome
    /// has facets to span.
    #[cfg(feature = "text")]
    fn view_two_plots() -> PlotComposition {
        PlotComposition::new(&comp_two())
            .with_plot(crate::plot::Plot::new(&comp_two(), "a"))
            .with_plot(crate::plot::Plot::new(&comp_two(), "b"))
    }

    #[test]
    fn capture_preserves_composition_geometry() {
        use crate::layout::{Inset, Length};
        let comp = comp_two()
            .id("outer")
            .aspect(1.0, 1.0)
            .margin(Inset::default().left(Length::pt(7.0)));
        let view = PlotComposition::new(&comp);
        assert_eq!(view.template.id.as_deref(), Some("outer"));
        assert_eq!(view.template.aspect, Some((1.0, 1.0)));
        assert!(view.template.margin.left.is_some());
        assert_eq!(view.root_id, "outer");
    }

    #[test]
    fn unnamed_composition_gets_a_root_id() {
        let view = PlotComposition::new(&comp_two());
        assert_eq!(view.root_id, ROOT_COMPOSITION_ID);
    }

    #[cfg(feature = "text")]
    #[test]
    fn composition_title_spans_every_facet() {
        let mut view = view_two_plots().title("Fuel economy");
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(600.0, 400.0), 96.0);

        let layout = view.last_layout.as_ref().unwrap();
        let title = layout
            .get(ROOT_COMPOSITION_ID, crate::composition::Slot::Title)
            .expect("composition title rect");
        let a = layout.get("a", crate::composition::Slot::Panel).unwrap();
        let b = layout.get("b", crate::composition::Slot::Panel).unwrap();
        assert!(title.y1 > title.y0, "title row has height");
        assert!(
            title.x0 <= a.x0 && title.x1 >= b.x1,
            "title {title:?} should span both facet panels {a:?} / {b:?}"
        );
        assert!(
            a.y0 >= title.y1,
            "both panels sit below the title band, got panel y0 {} vs title y1 {}",
            a.y0,
            title.y1
        );
    }

    #[cfg(feature = "text")]
    #[test]
    fn composition_title_is_drawn() {
        let mut bare = view_two_plots();
        let mut scene_bare = crate::scene::recording::RecordingScene::default();
        bare.render(&mut scene_bare, Size::new(600.0, 400.0), 96.0);

        let mut titled = view_two_plots().title("Fuel economy");
        let mut scene_titled = crate::scene::recording::RecordingScene::default();
        titled.render(&mut scene_titled, Size::new(600.0, 400.0), 96.0);

        let glyphs = |s: &crate::scene::recording::RecordingScene| {
            s.ops
                .iter()
                .filter(|op| matches!(op, crate::scene::recording::Op::DrawGlyphs(_)))
                .count()
        };
        assert!(
            glyphs(&scene_titled) > glyphs(&scene_bare),
            "the composition title should emit glyphs"
        );
    }

    #[cfg(feature = "text")]
    #[test]
    fn composition_chrome_uses_the_caller_id() {
        let mut view = PlotComposition::new(&comp_two().id("outer"))
            .with_plot(crate::plot::Plot::new(&comp_two(), "a"))
            .title("Shared");
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(600.0, 400.0), 96.0);
        let layout = view.last_layout.as_ref().unwrap();
        assert!(layout
            .get("outer", crate::composition::Slot::Title)
            .is_some());
        assert!(layout
            .get(ROOT_COMPOSITION_ID, crate::composition::Slot::Title)
            .is_none());
    }

    #[cfg(feature = "text")]
    #[test]
    fn composition_axis_title_spans_every_facet() {
        let mut view = view_two_plots().axis_title(AxisSide::Bottom, "Displacement (l)");
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(600.0, 400.0), 96.0);

        let layout = view.last_layout.as_ref().unwrap();
        let rect = layout
            .get(
                ROOT_COMPOSITION_ID,
                crate::composition::Slot::AxisBottomTitle,
            )
            .expect("composition axis title rect");
        let a = layout.get("a", crate::composition::Slot::Panel).unwrap();
        let b = layout.get("b", crate::composition::Slot::Panel).unwrap();
        assert!(rect.y1 > rect.y0, "axis title row has height");
        assert!(rect.x0 <= a.x0 && rect.x1 >= b.x1, "spans both panels");
        assert_eq!(
            view.axis_title_at(AxisSide::Bottom),
            Some("Displacement (l)")
        );
    }

    #[cfg(feature = "text")]
    #[test]
    fn composition_legend_reserves_the_ring_once() {
        use crate::plot::chrome::legend::{Legend, LegendKeySpec};
        use crate::scales::chrome::LegendSide;

        let scale = scale::discrete(vec![
            crate::scales::value::Value::from("a"),
            crate::scales::value::Value::from("b"),
        ])
        .range_colors([
            crate::color::Color::new([1.0, 0.0, 0.0, 1.0]),
            crate::color::Color::new([0.0, 0.0, 1.0, 1.0]),
        ]);
        let mut view = view_two_plots().add_scale("cat", scale);
        view.add_legend(
            Legend::new("cat")
                .side(LegendSide::Right)
                .title("Category")
                .key(LegendKeySpec::point().scaled("fill", "cat")),
        );
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(600.0, 400.0), 96.0);

        let layout = view.last_layout.as_ref().unwrap();
        let legend = layout
            .get(ROOT_COMPOSITION_ID, crate::composition::Slot::LegendRight)
            .expect("composition legend rect");
        let b = layout.get("b", crate::composition::Slot::Panel).unwrap();
        assert!(legend.x1 > legend.x0, "legend column has width");
        assert!(
            legend.x0 >= b.x1 - 0.5,
            "the composition legend sits outside the rightmost panel, got legend x0 {} vs panel x1 {}",
            legend.x0,
            b.x1
        );
        // No per-plot legend was attached, so neither patch reserves one.
        assert!(layout
            .get("a", crate::composition::Slot::LegendRight)
            .is_none());
        assert_eq!(view.legends().len(), 1);
    }

    #[cfg(feature = "text")]
    #[test]
    fn composition_collapses_compatible_keys_at_render_time() {
        use crate::plot::chrome::legend::{collapse_legends, Legend, LegendBody, LegendKeySpec};
        use crate::scales::chrome::LegendSide;
        let mut view = view_two_plots();
        let one = view.add_legend(
            Legend::new("cat")
                .side(LegendSide::Right)
                .title("Category")
                .key(LegendKeySpec::line().scaled("stroke", "cat")),
        );
        let two = view.add_legend(
            Legend::new("cat")
                .side(LegendSide::Right)
                .title("Category")
                .key(LegendKeySpec::point().scaled("fill", "cat")),
        );
        assert_ne!(one, two, "each attach gets its own id");
        assert_eq!(view.legends().len(), 2, "storage keeps both as attached");

        let collapsed = collapse_legends(view.legends(), view.scales(), &Theme::default().locale);
        assert_eq!(collapsed.len(), 1, "compatible legends render as one");
        let LegendBody::Stack(stack) = &collapsed[0].body else {
            panic!("stack body");
        };
        assert_eq!(stack.keys.len(), 2);
    }

    #[cfg(feature = "text")]
    #[test]
    fn composition_collapses_scales_trained_alike_after_attach() {
        use crate::plot::chrome::legend::{collapse_legends, Legend, LegendKeySpec};
        use crate::plot::scale;
        use crate::scales::chrome::LegendSide;
        use crate::scales::value::Value;
        let cats: Vec<Value> = ["A", "B"]
            .iter()
            .map(|s| Value::String(std::sync::Arc::from(*s)))
            .collect();
        let mut view = view_two_plots()
            .add_scale("cat_color", scale::discrete(cats.clone()))
            .add_scale("cat_shape", scale::discrete(cats));
        view.add_legend(
            Legend::new("cat_color")
                .side(LegendSide::Right)
                .title("Category")
                .key(LegendKeySpec::rect().scaled("fill", "cat_color")),
        );
        view.add_legend(
            Legend::new("cat_shape")
                .side(LegendSide::Right)
                .title("Category")
                .key(LegendKeySpec::point().scaled("shape", "cat_shape")),
        );
        let locale = Theme::default().locale;
        assert_eq!(
            collapse_legends(view.legends(), view.scales(), &locale).len(),
            1,
            "distinct scales over the same domain share one legend"
        );

        // Retrain one of them and the legends must part ways again.
        view.update_scale("cat_shape", |s| {
            *s = scale::discrete([Value::String(std::sync::Arc::from("A"))]);
        });
        assert_eq!(
            collapse_legends(view.legends(), view.scales(), &locale).len(),
            2,
            "collapse re-evaluates against the current scale state"
        );
    }

    #[cfg(feature = "text")]
    #[test]
    #[should_panic(expected = "LegendSide::InPanel")]
    fn composition_rejects_in_panel_legends() {
        use crate::plot::chrome::legend::Legend;
        use crate::scales::chrome::{Anchor, LegendSide};
        let mut view = view_two_plots();
        view.add_legend(Legend::new("cat").side(LegendSide::InPanel {
            anchor: Anchor::TopRight,
            inset_pt: 6.0,
        }));
    }

    #[cfg(feature = "text")]
    #[test]
    fn clearing_composition_chrome_releases_its_band() {
        let mut view = view_two_plots().title("Fuel economy");
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(600.0, 400.0), 96.0);
        let titled_panel = view
            .last_layout
            .as_ref()
            .unwrap()
            .get("a", crate::composition::Slot::Panel)
            .unwrap();

        view.clear_title();
        view.render(&mut scene, Size::new(600.0, 400.0), 96.0);
        let bare_panel = view
            .last_layout
            .as_ref()
            .unwrap()
            .get("a", crate::composition::Slot::Panel)
            .unwrap();
        assert!(
            bare_panel.y0 < titled_panel.y0,
            "clearing the title should give the row back to the panel"
        );
    }

    #[cfg(feature = "text")]
    #[test]
    fn composition_aspect_survives_alongside_chrome() {
        // A chrome-bearing composition still cascades its aspect lock to
        // leaf patches: `build_composition_grid` propagates aspect to the
        // children before dispatching to the wrapped-chrome path.
        let shape = || {
            Composition::empty(1, 1)
                .place(1, 1, Span::cell(), CompPatch::new("a"))
                .aspect(1.0, 1.0)
        };
        let mut view = PlotComposition::new(&shape())
            .with_plot(crate::plot::Plot::new(&shape(), "a"))
            .title("Wide canvas");
        let mut scene = crate::scene::recording::RecordingScene::default();
        view.render(&mut scene, Size::new(900.0, 400.0), 96.0);
        let layout = view.last_layout.as_ref().unwrap();
        let panel = layout.get("a", crate::composition::Slot::Panel).unwrap();
        let (pw, ph) = (panel.x1 - panel.x0, panel.y1 - panel.y0);
        assert!(
            (pw - ph).abs() < 2.0,
            "panel should stay square under a 900x400 canvas, got {pw} x {ph}"
        );
        assert!(layout
            .get(ROOT_COMPOSITION_ID, crate::composition::Slot::Title)
            .is_some());
    }

    // ── validate() ──

    #[test]
    fn validate_flags_missing_scale() {
        let view = PlotComposition::new(&comp_two())
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
        let view = PlotComposition::new(&comp_two())
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
        let view = PlotComposition::new(&comp_two())
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
        let view = PlotComposition::new(&comp_two())
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
