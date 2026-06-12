//! Legend rendering — explicit, manual API.
//!
//! A [`Legend`] is composed by the caller (no inference from
//! bindings). It carries:
//!
//! - the **domain scale** whose `breaks()` drive the rows,
//! - a [`LegendSide`] + optional title,
//! - a stack of [`LegendKeySpec`]s — each a geom-shaped marker
//!   primitive ([`LegendKey::Point`] / [`Line`] / [`Rect`]) with
//!   its own per-aesthetic [`AestheticSource`] map.
//!
//! Each row of the legend computes a [`ResolvedKey`] per stack
//! member by walking its bindings (scale lookup at the row's domain
//! value, or fixed value), then renders the member's marker using
//! the resolved aesthetics. Different stack members can pull from
//! different scales, or hard-code fixed values, independently — so
//! e.g. a Line with a scaled stroke colour can sit under a Point
//! whose fill is scaled and whose stroke is a fixed black.

use std::collections::HashMap;
use std::sync::Arc;

use crate::brush::Brush;
use crate::color::{rgb, Color};
use crate::geometry::{Affine, Point, Rect};
use crate::layout::{Measure, WidthHint};
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::{
    draw_axis_label, pt_to_px, AxisLabelAt, LABEL_FONT_SIZE_PT,
};
use crate::plot::scale::ScaleRegistry;
use crate::primitives::{circle, segment};
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::chrome::{Anchor, LegendSide};

/// Map a [`LegendSide`] to the cardinal direction the legend renders
/// against. The four anatomical-slot variants pass through; the
/// [`LegendSide::InPanel`] overlay variant renders with Right-style
/// vertical layout against its synthetic panel-anchored rect.
fn cardinal_side(side: LegendSide) -> LegendSide {
    match side {
        LegendSide::Left => LegendSide::Left,
        LegendSide::Right => LegendSide::Right,
        LegendSide::Top => LegendSide::Top,
        LegendSide::Bottom => LegendSide::Bottom,
        LegendSide::InPanel { .. } => LegendSide::Right,
    }
}

/// Compute the title's `(x, y)` baseline against the slot rect for a
/// cardinal-side legend body. `block_h` is the legend's total primary
/// extent (so `Top` legends can anchor their title at the bottom of
/// the slot); `anchor_width` is whatever the body uses as its primary
/// reference (row width for stack legends, swatch dim for binned
/// stacks, bar thickness for colorbars).
fn title_anchor(
    side: LegendSide,
    slot_rect: Rect,
    padding: f64,
    block_h: f64,
    title_w_px: f64,
    anchor_width: f64,
) -> (f64, f64) {
    let y = match side {
        LegendSide::Top => slot_rect.y1 - block_h + padding,
        _ => slot_rect.y0 + padding,
    };
    let x = match side {
        LegendSide::Left => slot_rect.x1 - padding - title_w_px.max(anchor_width),
        _ => slot_rect.x0 + padding,
    };
    (x, y)
}

/// Pick the axis baseline (start, end, outward tick direction) for a
/// tick rail running along the panel-facing long edge of `bar_rect`.
/// Shared by binned stacks and colorbars — both lay a rail along the
/// bar's long edge with ticks pointing away from the panel.
fn axis_baseline(side: LegendSide, bar_rect: Rect) -> (Point, Point, (f64, f64)) {
    match side {
        LegendSide::Right => (
            Point::new(bar_rect.x1, bar_rect.y1),
            Point::new(bar_rect.x1, bar_rect.y0),
            (1.0, 0.0),
        ),
        LegendSide::Left => (
            Point::new(bar_rect.x0, bar_rect.y1),
            Point::new(bar_rect.x0, bar_rect.y0),
            (-1.0, 0.0),
        ),
        LegendSide::Top => (
            Point::new(bar_rect.x0, bar_rect.y0),
            Point::new(bar_rect.x1, bar_rect.y0),
            (0.0, -1.0),
        ),
        LegendSide::Bottom => (
            Point::new(bar_rect.x0, bar_rect.y1),
            Point::new(bar_rect.x1, bar_rect.y1),
            (0.0, 1.0),
        ),
        LegendSide::InPanel { .. } => unreachable!("cardinal_side flattens InPanel"),
    }
}

/// Pin a `size` rectangle inside `panel` at `anchor`, offset from the
/// matching panel edge by `inset_px` on both axes. Centre anchors
/// receive no inset along their centred axis.
pub fn resolve_anchor(panel: Rect, anchor: Anchor, inset_px: f64, size: (f64, f64)) -> Rect {
    let (w, h) = size;
    let (x0, y0) = match anchor {
        Anchor::TopLeft => (panel.x0 + inset_px, panel.y0 + inset_px),
        Anchor::TopCenter => (
            panel.x0 + (panel.x1 - panel.x0 - w) * 0.5,
            panel.y0 + inset_px,
        ),
        Anchor::TopRight => (panel.x1 - w - inset_px, panel.y0 + inset_px),
        Anchor::CenterLeft => (
            panel.x0 + inset_px,
            panel.y0 + (panel.y1 - panel.y0 - h) * 0.5,
        ),
        Anchor::Center => (
            panel.x0 + (panel.x1 - panel.x0 - w) * 0.5,
            panel.y0 + (panel.y1 - panel.y0 - h) * 0.5,
        ),
        Anchor::CenterRight => (
            panel.x1 - w - inset_px,
            panel.y0 + (panel.y1 - panel.y0 - h) * 0.5,
        ),
        Anchor::BottomLeft => (panel.x0 + inset_px, panel.y1 - h - inset_px),
        Anchor::BottomCenter => (
            panel.x0 + (panel.x1 - panel.x0 - w) * 0.5,
            panel.y1 - h - inset_px,
        ),
        Anchor::BottomRight => (panel.x1 - w - inset_px, panel.y1 - h - inset_px),
    };
    Rect::new(x0, y0, x0 + w, y0 + h)
}

/// Combined primary + cross extent of a stack of in-panel legends.
/// In-panel legends use Right-style layout: primary = column width,
/// cross = stacked row heights + inter-legend gaps. Returns `(w, h)`
/// in pixels.
pub fn legend_stack_natural_size(
    legends: &[&Legend],
    registry: &ScaleRegistry,
    dpi: f64,
) -> (f64, f64) {
    let measures: Vec<LegendMeasure> = legends
        .iter()
        .map(|l| LegendMeasure::new(l, registry, dpi))
        .filter(|m| !m.is_empty())
        .collect();
    if measures.is_empty() {
        return (0.0, 0.0);
    }
    let gap_px = pt_to_px(LEGEND_GAP_PT, dpi);
    let primary = measures
        .iter()
        .map(|m| m.primary_dim_px(dpi))
        .fold(0.0_f64, f64::max);
    let cross: f64 = measures.iter().map(|m| m.cross_dim_px(dpi)).sum::<f64>()
        + gap_px * (measures.len() as f64 - 1.0).max(0.0);
    (primary, cross)
}
use crate::scales::value::{LinetypeStep, Value};
use crate::scene::{Glyph, GlyphRun, SceneBuilder};
use crate::shape::{ShapeKind, ShapeRegistry, ShapeStyle};
use crate::stroke::Stroke;
use crate::text::{draw_text, Alignment, TextRun, TextStyle};
use kurbo::Shape;

// ─── Style constants (pt) ───────────────────────────────────────────────────

/// Default swatch cell size in pt — used for everything except
/// markers whose size is explicitly scaled (e.g. a Point key with a
/// `scaled("size", …)` binding, where the cell grows to fit the
/// biggest marker).
const SWATCH_SIZE_PT: f64 = 12.0;
/// Default line length for `Line` swatches, pt.
const LINE_SWATCH_LEN_PT: f64 = 18.0;
/// Default Point marker diameter when no size is bound, pt.
const DEFAULT_POINT_DIAMETER_PT: f64 = 8.0;
/// Default linewidth for stroke outlines / line swatches, pt.
const DEFAULT_LINEWIDTH_PT: f64 = 1.0;
/// Gap between a row's swatch and its label, pt.
const SWATCH_LABEL_GAP_PT: f64 = 4.0;
/// Vertical gap between stacked rows in a Right/Left legend, pt.
const ROW_GAP_PT: f64 = 4.0;
/// Gap between adjacent legends in a stack (same-side multi-legend), pt.
const LEGEND_GAP_PT: f64 = 10.0;
/// Outer padding around the legend so it doesn't butt against the
/// panel or adjacent slot, pt.
const PADDING_PT: f64 = 6.0;
/// The default `circle` point shape's intrinsic radius (in shape
/// coordinates). Mirrored from `crate::shape::builtins::circle` so
/// the legend's Point key renders at the same diameter the geom
/// would for an equal `size_pt`.
const POINT_SHAPE_RADIUS: f64 = 0.8;
/// Glyph normalisation target — height the glyph is rescaled to in
/// shape space before the size scale is applied. Mirrors
/// `crate::plot::geom::point::GLYPH_BBOX_REFERENCE` so a glyph
/// marker at a given `size_pt` renders at the same visual extent in
/// the legend as in the plot.
const GLYPH_BBOX_REFERENCE: f64 = 1.6;

fn ink() -> Color {
    rgb(0.0, 0.0, 0.0)
}

// ─── Public types ───────────────────────────────────────────────────────────

/// Stable identifier returned by [`crate::plot::Plot::add_legend`].
/// Used to remove or update a legend later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LegendId(pub u32);

/// Marker primitives a [`LegendKeySpec`] can draw.
///
/// Each variant has a fixed set of aesthetics it reads from a
/// [`ResolvedKey`]; aesthetics not relevant to the variant are
/// silently ignored. Variants not yet implemented (Wedge, Segment,
/// …) can be added without touching the surrounding machinery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LegendKey {
    /// Sized marker (default shape: `circle` from the registry).
    /// Consumes: fill, stroke, size, shape (TODO), alpha, linewidth
    /// (as the marker's outline width).
    Point,
    /// Short horizontal stroke. Consumes: stroke (or `color` as a
    /// fallback), linewidth, linetype, alpha.
    Line,
    /// Filled rectangle covering the swatch cell. Consumes: fill,
    /// stroke, alpha.
    Rect,
}

/// Per-aesthetic source for a [`LegendKeySpec`].
#[derive(Clone, Debug)]
pub enum AestheticSource {
    /// Resolve via `registry.get(scale_name).map(row_value)`.
    Scaled(String),
    /// Fixed value across every row.
    Fixed(Value),
}

/// One key in a legend's stack — what to draw + how to resolve its
/// aesthetics for the current row.
#[derive(Clone, Debug)]
pub struct LegendKeySpec {
    pub kind: LegendKey,
    /// Per-aesthetic name → source. Aesthetics not listed fall back
    /// to the key's built-in default.
    pub bindings: HashMap<String, AestheticSource>,
}

impl LegendKeySpec {
    /// Start a `Point` key with no aesthetic bindings.
    pub fn point() -> Self {
        Self {
            kind: LegendKey::Point,
            bindings: HashMap::new(),
        }
    }
    /// Start a `Line` key with no aesthetic bindings.
    pub fn line() -> Self {
        Self {
            kind: LegendKey::Line,
            bindings: HashMap::new(),
        }
    }
    /// Start a `Rect` key with no aesthetic bindings.
    pub fn rect() -> Self {
        Self {
            kind: LegendKey::Rect,
            bindings: HashMap::new(),
        }
    }
    /// Pull this aesthetic from `scale_name` at the row's domain
    /// value.
    pub fn scaled(mut self, aesthetic: impl Into<String>, scale_name: impl Into<String>) -> Self {
        self.bindings
            .insert(aesthetic.into(), AestheticSource::Scaled(scale_name.into()));
        self
    }
    /// Pin this aesthetic to a fixed value across every row.
    pub fn fixed(mut self, aesthetic: impl Into<String>, value: impl Into<Value>) -> Self {
        self.bindings
            .insert(aesthetic.into(), AestheticSource::Fixed(value.into()));
        self
    }
}

/// A legend. Composed manually by the caller and attached via
/// [`crate::plot::Plot::add_legend`].
///
/// The common shell carries the side / title / domain scale. The
/// [`LegendBody`] picks the actual visualisation: stacked discrete
/// keys, or a continuous gradient colorbar.
#[derive(Clone, Debug)]
pub struct Legend {
    pub side: LegendSide,
    pub title: Option<String>,
    /// Scale whose `breaks()` drive the legend's tick / label
    /// positions.
    pub domain_scale: String,
    pub body: LegendBody,
    /// Suppress the first tick + label on the legend's rail to
    /// communicate an unbounded bottom bin (the swatch still renders
    /// full-size). Honoured by binned-stack and stepped-colorbar
    /// bodies; ignored on continuous colorbars and non-binned stacks.
    pub open_lower: bool,
    /// Suppress the last tick + label on the legend's rail to
    /// communicate an unbounded top bin. Mirrors `open_lower`.
    pub open_upper: bool,
    /// Whether binned bodies size each bin proportionally to its
    /// break span (the default) or give every bin the same extent
    /// along the bar.
    pub bin_spacing: BinSpacing,
}

/// How a binned legend distributes its bins along the bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BinSpacing {
    /// Each bin's extent is `(break_i+1 − break_i) / (max − min)` of
    /// the bar — bins map to their data-axis position one-to-one.
    #[default]
    Proportional,
    /// Every bin gets exactly `1 / n_bins` of the bar. Tick labels
    /// still report the underlying break values; the rail loses its
    /// to-scale relationship with the data axis in exchange for
    /// equal-weight colour cues.
    Equal,
}

/// What a legend looks like.
#[derive(Clone, Debug)]
pub enum LegendBody {
    /// Discrete: marker keys arranged either as one row per break
    /// (`binned == false`, the default — labels next to each row)
    /// or one row per bin (`binned == true` — N rows for N+1
    /// breaks, with an axis-style tick rail labelling the
    /// boundaries *between* the rows).
    Stack(StackBody),
    /// Continuous gradient colorbar: a bar sampled along the
    /// domain scale's colour output range, with axis-style tick
    /// labels alongside it (drawn via the shared
    /// [`draw_linear_axis_at`](crate::plot::chrome::linear_axis)
    /// helper).
    Colorbar(ColorbarSpec),
}

/// Stack legend configuration.
#[derive(Clone, Debug)]
pub struct StackBody {
    /// Stack of keys drawn into each row's swatch cell, painters'
    /// order.
    pub keys: Vec<LegendKeySpec>,
    /// If `true`, treat the domain scale's breaks as **bin
    /// boundaries** (N+1 breaks → N swatches). Each swatch is
    /// rendered at the midpoint of its bin; an axis-style tick
    /// rail labels the boundaries between rows, drawn through the
    /// shared [`draw_linear_axis_at`](crate::plot::chrome::linear_axis)
    /// helper for visual consistency with cartesian / polar axes.
    pub binned: bool,
}

/// Configuration for a colorbar legend body.
///
/// Like [`LegendKeySpec`], a colorbar carries per-aesthetic
/// `bindings`. Each gradient stop resolves a [`ResolvedKey`] from
/// these and uses its `fill` (with `alpha` modulating the
/// per-channel opacity) as the stop colour. By default the `fill`
/// binding is the legend's `domain_scale` — so the simplest
/// colorbar (`Legend::colorbar("scale_name")`) gradients over that
/// scale's colour output range. Layering an `alpha` scale on top
/// just means adding another binding:
///
/// ```ignore
/// Legend::colorbar("value_scale")
///     .scaled("alpha", "value_alpha_scale")
/// ```
#[derive(Clone, Debug)]
pub struct ColorbarSpec {
    /// Thickness of the bar (perpendicular to the domain axis), pt.
    /// For Right/Left legends this is the bar's width; for
    /// Top/Bottom it's the height.
    pub thickness_pt: f64,
    /// Number of gradient stops sampled along the bar in continuous
    /// mode. Ignored when `stepped` is true (the breaks then drive
    /// the stop count directly).
    pub samples: usize,
    /// If `true`, the colorbar renders one constant-colour block
    /// per pair of adjacent breaks (e.g. for binned colour scales
    /// or any continuous scale you want shown as steps). Stop
    /// colours are sampled at each bin's midpoint; tick labels
    /// still come from the domain scale's breaks. If `false`
    /// (default), the bar is a smooth gradient with `samples`
    /// stops along its length.
    pub stepped: bool,
    /// Aesthetic name → source. `fill` falls back to
    /// `Scaled(domain_scale)` if absent.
    pub bindings: HashMap<String, AestheticSource>,
}

impl Default for ColorbarSpec {
    fn default() -> Self {
        Self {
            thickness_pt: 12.0,
            samples: 64,
            stepped: false,
            bindings: HashMap::new(),
        }
    }
}

impl Legend {
    /// Discrete legend: rows driven by `domain_scale`'s breaks, with
    /// a stack of marker keys appended via [`Self::key`]. Default
    /// side: `Right`; no title; no keys. Flip into binned mode
    /// (N+1 breaks → N bins, ticks between rows) via [`Self::binned`].
    pub fn new(domain_scale: impl Into<String>) -> Self {
        Self {
            side: LegendSide::Right,
            title: None,
            domain_scale: domain_scale.into(),
            body: LegendBody::Stack(StackBody {
                keys: Vec::new(),
                binned: false,
            }),
            open_lower: false,
            open_upper: false,
            bin_spacing: BinSpacing::Proportional,
        }
    }
    /// Continuous colorbar legend: gradient bar sampled from
    /// `domain_scale`'s colour output range, tick labels from its
    /// `breaks()`. Configure via [`Self::thickness`] / [`Self::samples`].
    pub fn colorbar(domain_scale: impl Into<String>) -> Self {
        Self {
            side: LegendSide::Right,
            title: None,
            domain_scale: domain_scale.into(),
            body: LegendBody::Colorbar(ColorbarSpec::default()),
            open_lower: false,
            open_upper: false,
            bin_spacing: BinSpacing::Proportional,
        }
    }
    /// Override the side (default `Right`).
    pub fn side(mut self, s: LegendSide) -> Self {
        self.side = s;
        self
    }
    /// Set the legend title.
    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }
    /// Append a key to a [`LegendBody::Stack`] legend. No-op on
    /// colorbar legends.
    pub fn key(mut self, k: LegendKeySpec) -> Self {
        if let LegendBody::Stack(stack) = &mut self.body {
            stack.keys.push(k);
        }
        self
    }
    /// Flip the legend into **binned** mode — both visual variants
    /// encode the same underlying scale-type (an N+1-break ladder
    /// that defines N bins), just expressed differently per body:
    ///
    /// - **Stack legends**: rows become bin *swatches* sampled at
    ///   bin midpoints, with an axis-style tick rail labelling each
    ///   boundary between rows.
    /// - **Colorbar legends**: the gradient bar is replaced by
    ///   constant-colour blocks between adjacent breaks.
    pub fn binned(mut self) -> Self {
        match &mut self.body {
            LegendBody::Stack(stack) => stack.binned = true,
            LegendBody::Colorbar(spec) => spec.stepped = true,
        }
        self
    }
    /// Mark the bottom bin as open-ended: the rail's first tick + label
    /// are suppressed, signalling an unbounded outer bin (the swatch /
    /// gradient block still renders full-size). Applies to binned-stack
    /// and stepped-colorbar bodies; ignored elsewhere.
    pub fn open_lower(mut self) -> Self {
        self.open_lower = true;
        self
    }
    /// Mark the top bin as open-ended. Mirrors [`Self::open_lower`].
    pub fn open_upper(mut self) -> Self {
        self.open_upper = true;
        self
    }
    /// Switch the legend to equal-width bins. Shorthand for
    /// `bin_spacing(BinSpacing::Equal)`.
    pub fn equal_bins(mut self) -> Self {
        self.bin_spacing = BinSpacing::Equal;
        self
    }
    /// Set the bin spacing mode (proportional or equal). Applies to
    /// binned-stack and stepped-colorbar bodies; ignored elsewhere.
    pub fn bin_spacing(mut self, spacing: BinSpacing) -> Self {
        self.bin_spacing = spacing;
        self
    }
    /// Set the colorbar's bar thickness (pt). No-op on stack legends.
    pub fn thickness(mut self, pt: f64) -> Self {
        if let LegendBody::Colorbar(spec) = &mut self.body {
            spec.thickness_pt = pt;
        }
        self
    }
    /// Set the colorbar's gradient sample count. No-op on stack legends.
    pub fn samples(mut self, n: usize) -> Self {
        if let LegendBody::Colorbar(spec) = &mut self.body {
            spec.samples = n.max(2);
        }
        self
    }

    /// Bind a colorbar aesthetic to a scale (e.g. `alpha` keyed off
    /// a separate alpha scale). The fill is implicitly bound to the
    /// legend's `domain_scale` unless overridden here. No-op on
    /// stack legends — use [`LegendKeySpec::scaled`] there.
    pub fn scaled(mut self, aesthetic: impl Into<String>, scale_name: impl Into<String>) -> Self {
        if let LegendBody::Colorbar(spec) = &mut self.body {
            spec.bindings
                .insert(aesthetic.into(), AestheticSource::Scaled(scale_name.into()));
        }
        self
    }

    /// Pin a colorbar aesthetic to a fixed value across the gradient.
    /// No-op on stack legends.
    pub fn fixed(mut self, aesthetic: impl Into<String>, value: impl Into<Value>) -> Self {
        if let LegendBody::Colorbar(spec) = &mut self.body {
            spec.bindings
                .insert(aesthetic.into(), AestheticSource::Fixed(value.into()));
        }
        self
    }
    /// `true` if this legend's `(domain_scale, side, title, body
    /// kind, binned flag)` match `other` — i.e. the two legends
    /// are stack-compatible and `add_legend` should merge their
    /// stack keys. Colorbars never merge (each gets its own slot).
    pub fn is_compatible_with(&self, other: &Legend) -> bool {
        if self.domain_scale != other.domain_scale
            || self.side != other.side
            || self.title != other.title
            || self.open_lower != other.open_lower
            || self.open_upper != other.open_upper
            || self.bin_spacing != other.bin_spacing
        {
            return false;
        }
        match (&self.body, &other.body) {
            (LegendBody::Stack(a), LegendBody::Stack(b)) => a.binned == b.binned,
            _ => false,
        }
    }
}

/// Per-row resolved aesthetic bundle. Each [`LegendKey`] reads the
/// fields it cares about; the rest are ignored.
#[derive(Clone, Debug, Default)]
pub struct ResolvedKey {
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub size_pt: Option<f64>,
    pub shape: Option<Arc<str>>,
    pub alpha: Option<f64>,
    pub linewidth_pt: Option<f64>,
    pub linetype: Option<Arc<[LinetypeStep]>>,
}

impl ResolvedKey {
    /// Apply an aesthetic value to the matching field. Unknown
    /// aesthetic names are silently ignored.
    fn apply(&mut self, aesthetic: &str, value: Value) {
        match aesthetic {
            "fill" | "color" => {
                if let Some(c) = value.as_color() {
                    self.fill = Some(c);
                }
            }
            "stroke" => {
                if let Some(c) = value.as_color() {
                    self.stroke = Some(c);
                }
            }
            "size" => {
                if let Some(n) = value.as_number() {
                    self.size_pt = Some(n);
                }
            }
            "shape" => {
                if let Some(s) = value.as_str() {
                    self.shape = Some(Arc::from(s));
                }
            }
            "alpha" | "fill_opacity" | "stroke_opacity" => {
                if let Some(n) = value.as_number() {
                    self.alpha = Some(n);
                }
            }
            "linewidth" => {
                if let Some(n) = value.as_number() {
                    self.linewidth_pt = Some(n);
                }
            }
            "linetype" => {
                if let Some(p) = value.as_linetype() {
                    self.linetype = Some(Arc::from(p.to_vec()));
                }
            }
            _ => {}
        }
    }
}

// ─── Entry point ────────────────────────────────────────────────────────────

/// Pre-shape a legend into a [`Measure`] so the composition solver
/// can reserve space for its slot. Same machinery (peak resolved
/// aesthetics + per-key swatch dims) drives the draw step, so what
/// is reserved matches what is drawn.
pub fn legend_measure(legend: &Legend, registry: &ScaleRegistry, dpi: f64) -> Box<dyn Measure> {
    Box::new(LegendMeasure::new(legend, registry, dpi))
}

/// Pre-shape a stack of legends sharing the same side. Reserves the
/// max primary extent (column width / row height) across children
/// and the sum of cross extents plus inter-legend gaps. Pair with
/// [`render_legend_stack`] at draw time so what's reserved matches
/// what's drawn.
pub fn legend_stack_measure(
    legends: &[&Legend],
    side: LegendSide,
    registry: &ScaleRegistry,
    dpi: f64,
) -> Box<dyn Measure> {
    let children: Vec<LegendMeasure> = legends
        .iter()
        .map(|l| LegendMeasure::new(l, registry, dpi))
        .collect();
    Box::new(LegendStackMeasure {
        side: cardinal_side(side),
        children,
    })
}

/// Draw a stack of same-side legends into `slot_rect`. Children
/// stack along the cross axis (Right/Left: top→bottom;
/// Top/Bottom: left→right) with [`LEGEND_GAP_PT`] between them.
/// Each child uses its own `cross_dim_px` for its share of the
/// slot; the full primary extent is available to every child.
/// `shapes` lets [`LegendKey::Point`] resolve a `shape` aesthetic
/// to a registered marker.
pub fn render_legend_stack(
    legends: &[&Legend],
    side: LegendSide,
    slot_rect: Rect,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
) {
    let gap_px = pt_to_px(LEGEND_GAP_PT, dpi);
    let measures: Vec<(usize, LegendMeasure)> = legends
        .iter()
        .enumerate()
        .map(|(i, l)| (i, LegendMeasure::new(l, registry, dpi)))
        .filter(|(_, m)| !m.is_empty())
        .collect();
    if measures.is_empty() {
        return;
    }
    let stack_axis_is_y = matches!(side, LegendSide::Right | LegendSide::Left);
    let mut cursor = if stack_axis_is_y {
        slot_rect.y0
    } else {
        slot_rect.x0
    };
    for (orig_idx, measure) in &measures {
        let cross = measure.cross_dim_px(dpi);
        let sub_rect = if stack_axis_is_y {
            Rect::new(slot_rect.x0, cursor, slot_rect.x1, cursor + cross)
        } else {
            Rect::new(cursor, slot_rect.y0, cursor + cross, slot_rect.y1)
        };
        render_legend(legends[*orig_idx], registry, shapes, sub_rect, scene, dpi);
        cursor += cross + gap_px;
    }
}

/// Draw the legend into `slot_rect`. Dispatches on the legend's
/// [`LegendBody`]: stack legends render their marker stack per row;
/// colorbar legends render a gradient bar plus an axis-style tick
/// rail. The legend block hugs the panel-facing edge of the slot
/// regardless of how much extra space the layout solver leaves on
/// the far side.
pub fn render_legend(
    legend: &Legend,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    slot_rect: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
) {
    let measure = LegendMeasure::new(legend, registry, dpi);
    if measure.is_empty() {
        return;
    }
    match &legend.body {
        LegendBody::Stack(stack) if stack.binned => render_binned_stack_body(
            legend,
            &stack.keys,
            &measure,
            registry,
            shapes,
            slot_rect,
            scene,
            dpi,
        ),
        LegendBody::Stack(stack) => render_stack_body(
            legend,
            &stack.keys,
            &measure,
            registry,
            shapes,
            slot_rect,
            scene,
            dpi,
        ),
        LegendBody::Colorbar(spec) => {
            render_colorbar_body(legend, spec, &measure, registry, slot_rect, scene, dpi)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_stack_body(
    legend: &Legend,
    keys: &[LegendKeySpec],
    measure: &LegendMeasure,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    slot_rect: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
) {
    let side = cardinal_side(legend.side);
    let domain = match registry.get(&legend.domain_scale) {
        Some(s) => s,
        None => return,
    };
    let swatch_dim_px = match measure.body {
        BodyMeasure::Stack { swatch_dim_px, .. } => swatch_dim_px,
        _ => return,
    };

    let padding = pt_to_px(PADDING_PT, dpi);
    let swatch_label_gap = pt_to_px(SWATCH_LABEL_GAP_PT, dpi);
    let row_gap = pt_to_px(ROW_GAP_PT, dpi);
    let title_gap = if legend.title.is_some() && measure.title_h_px > 0.0 {
        pt_to_px(ROW_GAP_PT, dpi)
    } else {
        0.0
    };
    let label_style = TextStyle::new(LABEL_FONT_SIZE_PT);
    let title_style = TextStyle::new(LABEL_FONT_SIZE_PT).weight(700);
    let ink_brush = Brush::Solid(ink());

    let breaks = domain.breaks(DEFAULT_BREAK_COUNT);
    let entries: Vec<&Value> = breaks
        .iter()
        .filter(|v| !matches!(v, Value::Null))
        .collect();

    let row_h = measure.max_label_h_px.max(swatch_dim_px);
    let row_w = swatch_dim_px + swatch_label_gap + measure.max_label_w_px;
    let block_h = measure.primary_dim_px(dpi);

    let (title_x, title_y) =
        title_anchor(side, slot_rect, padding, block_h, measure.title_w_px, row_w);
    let entries_y = title_y + measure.title_h_px + title_gap;
    let entries_x = match side {
        LegendSide::Left => slot_rect.x1 - padding - row_w,
        _ => slot_rect.x0 + padding,
    };

    if let Some(title) = &legend.title {
        let run = TextRun::new(title, &title_style);
        let _ = run.set_max_width(f32::INFINITY, Alignment::Start);
        draw_text(
            scene,
            &run,
            title_x,
            title_y,
            &ink_brush,
            Affine::IDENTITY,
            PickId::Skip,
        );
    }

    let (mut cursor_x, mut cursor_y) = (entries_x, entries_y);
    for v in entries {
        let swatch_cell = Rect::new(
            cursor_x,
            cursor_y + (row_h - swatch_dim_px) * 0.5,
            cursor_x + swatch_dim_px,
            cursor_y + (row_h + swatch_dim_px) * 0.5,
        );
        for key in keys {
            let resolved = resolve_key(key, registry, v);
            render_key(key.kind, &resolved, swatch_cell, shapes, scene, dpi);
        }

        let label = domain.format(v);
        let anchor = Point::new(
            cursor_x + swatch_dim_px + swatch_label_gap,
            cursor_y + row_h * 0.5,
        );
        draw_axis_label(
            scene,
            &label,
            &label_style,
            &ink_brush,
            AxisLabelAt {
                anchor,
                direction: (1.0, 0.0),
            },
            dpi,
        );

        match side {
            LegendSide::Right | LegendSide::Left => cursor_y += row_h + row_gap,
            LegendSide::Top | LegendSide::Bottom => cursor_x += row_w + row_gap,
            LegendSide::InPanel { .. } => unreachable!("cardinal_side flattens InPanel"),
        }
    }
}

/// Render a binned-stack legend: N+1 breaks define N bins; one row
/// of marker keys is drawn per bin (sampled at the bin's midpoint),
/// and an axis-style tick rail labels the boundaries between rows
/// — same `draw_linear_axis_at` helper the cartesian + colorbar
/// axes use.
#[allow(clippy::too_many_arguments)]
fn render_binned_stack_body(
    legend: &Legend,
    keys: &[LegendKeySpec],
    measure: &LegendMeasure,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    slot_rect: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
) {
    let domain = match registry.get(&legend.domain_scale) {
        Some(s) => s,
        None => return,
    };
    let side = cardinal_side(legend.side);
    let swatch_dim_px = match measure.body {
        BodyMeasure::Stack { swatch_dim_px, .. } => swatch_dim_px,
        _ => return,
    };
    let breaks: Vec<f64> = domain
        .breaks(DEFAULT_BREAK_COUNT)
        .iter()
        .filter_map(|v| v.as_number().or_else(|| v.as_temporal_f64()))
        .filter(|n| n.is_finite())
        .collect();
    if breaks.len() < 2 {
        return;
    }
    let (min, max) = (breaks[0], *breaks.last().unwrap());
    let span = max - min;
    if !span.is_finite() || span.abs() < f64::EPSILON {
        return;
    }

    let padding = pt_to_px(PADDING_PT, dpi);
    let title_gap = if legend.title.is_some() && measure.title_h_px > 0.0 {
        pt_to_px(ROW_GAP_PT, dpi)
    } else {
        0.0
    };
    let title_style = TextStyle::new(LABEL_FONT_SIZE_PT).weight(700);
    let ink_brush = Brush::Solid(ink());
    let block_h = measure.primary_dim_px(dpi);
    let n_bins = breaks.len() - 1;
    let bar_len = n_bins as f64 * swatch_dim_px;

    // Anchor the legend block to the panel-facing slot edge.
    let (title_x, title_y) = title_anchor(
        side,
        slot_rect,
        padding,
        block_h,
        measure.title_w_px,
        swatch_dim_px,
    );
    if let Some(title) = &legend.title {
        let run = TextRun::new(title, &title_style);
        let _ = run.set_max_width(f32::INFINITY, Alignment::Start);
        draw_text(
            scene,
            &run,
            title_x,
            title_y,
            &ink_brush,
            Affine::IDENTITY,
            PickId::Skip,
        );
    }

    // Bar rect = the stack of touching swatches. Long axis runs
    // along the slot's cross direction; short axis = swatch_dim_px.
    let bar_rect = match side {
        LegendSide::Right => Rect::new(
            slot_rect.x0 + padding,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x0 + padding + swatch_dim_px,
            title_y + measure.title_h_px + title_gap + bar_len,
        ),
        LegendSide::Left => Rect::new(
            slot_rect.x1 - padding - swatch_dim_px,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x1 - padding,
            title_y + measure.title_h_px + title_gap + bar_len,
        ),
        LegendSide::Top => Rect::new(
            slot_rect.x0 + padding,
            slot_rect.y1 - padding - swatch_dim_px,
            slot_rect.x0 + padding + bar_len,
            slot_rect.y1 - padding,
        ),
        LegendSide::Bottom => Rect::new(
            slot_rect.x0 + padding,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x0 + padding + bar_len,
            title_y + measure.title_h_px + title_gap + swatch_dim_px,
        ),
        LegendSide::InPanel { .. } => unreachable!("cardinal_side flattens InPanel"),
    };
    let horizontal = matches!(side, LegendSide::Top | LegendSide::Bottom);

    // For Right/Left the bar runs BOTTOM (low frac) to TOP (high
    // frac) — matches the cartesian y convention. For Top/Bottom
    // it's left → right.
    let equal_bins = legend.bin_spacing == BinSpacing::Equal;
    for i in 0..n_bins {
        let (lo, hi) = (breaks[i], breaks[i + 1]);
        let midpoint = Value::Number((lo + hi) * 0.5);
        let (lo_t, hi_t) = if equal_bins {
            (i as f64 / n_bins as f64, (i + 1) as f64 / n_bins as f64)
        } else {
            ((lo - min) / span, (hi - min) / span)
        };
        let cell = if horizontal {
            Rect::new(
                bar_rect.x0 + lo_t * (bar_rect.x1 - bar_rect.x0),
                bar_rect.y0,
                bar_rect.x0 + hi_t * (bar_rect.x1 - bar_rect.x0),
                bar_rect.y1,
            )
        } else {
            // Flip so low_frac → bottom of bar (high y).
            Rect::new(
                bar_rect.x0,
                bar_rect.y0 + (1.0 - hi_t) * (bar_rect.y1 - bar_rect.y0),
                bar_rect.x1,
                bar_rect.y0 + (1.0 - lo_t) * (bar_rect.y1 - bar_rect.y0),
            )
        };
        for key in keys {
            let resolved = resolve_key(key, registry, &midpoint);
            render_key(key.kind, &resolved, cell, shapes, scene, dpi);
        }
    }

    // Axis along the bar's long edge (away from the panel) with
    // ticks at each break boundary. Reuse `draw_linear_axis_at` so
    // the rail matches the cartesian / colorbar axes pixel-for-pixel.
    let (axis_start, axis_end, tick_direction) = axis_baseline(side, bar_rect);
    let majors_owned = colorbar_majors(domain);
    let majors_owned = if legend.bin_spacing == BinSpacing::Equal {
        colorbar_majors_remap_equal(&majors_owned)
    } else {
        majors_owned
    };
    let majors = open_end_trim(&majors_owned, legend.open_lower, legend.open_upper);
    crate::plot::chrome::linear_axis::draw_linear_axis_at(
        scene,
        axis_start,
        axis_end,
        tick_direction,
        majors,
        &[],
        dpi,
    );
}

/// Render a gradient colorbar + tick rail. The bar is approximated by
/// `samples` constant-colour rects (each sampled from the domain
/// scale's colour output range); the tick rail goes through the
/// shared [`draw_linear_axis_at`] so it stays visually consistent
/// with the cartesian + polar radius axes.
fn render_colorbar_body(
    legend: &Legend,
    spec: &ColorbarSpec,
    measure: &LegendMeasure,
    registry: &ScaleRegistry,
    slot_rect: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
) {
    let domain = match registry.get(&legend.domain_scale) {
        Some(s) => s,
        None => return,
    };
    let side = cardinal_side(legend.side);
    let (bar_thickness_px, samples) = match measure.body {
        BodyMeasure::Colorbar {
            bar_thickness_px,
            samples,
        } => (bar_thickness_px, samples),
        _ => return,
    };

    let padding = pt_to_px(PADDING_PT, dpi);
    let title_gap = if legend.title.is_some() && measure.title_h_px > 0.0 {
        pt_to_px(ROW_GAP_PT, dpi)
    } else {
        0.0
    };
    let title_style = TextStyle::new(LABEL_FONT_SIZE_PT).weight(700);
    let ink_brush = Brush::Solid(ink());
    let block_h = measure.primary_dim_px(dpi);

    // Anchor the colorbar block to the panel-facing slot edge.
    let (title_x, title_y) = title_anchor(
        side,
        slot_rect,
        padding,
        block_h,
        measure.title_w_px,
        bar_thickness_px,
    );

    if let Some(title) = &legend.title {
        let run = TextRun::new(title, &title_style);
        let _ = run.set_max_width(f32::INFINITY, Alignment::Start);
        draw_text(
            scene,
            &run,
            title_x,
            title_y,
            &ink_brush,
            Affine::IDENTITY,
            PickId::Skip,
        );
    }

    // The bar's body rect. Long axis lies along the slot's cross
    // direction; short axis is `bar_thickness_px`.
    let cross_dim_px = measure.cross_dim_px(dpi);
    let bar_rect = match side {
        LegendSide::Right => Rect::new(
            slot_rect.x0 + padding,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x0 + padding + bar_thickness_px,
            title_y
                + measure.title_h_px
                + title_gap
                + (cross_dim_px - 2.0 * padding - measure.title_h_px - title_gap),
        ),
        LegendSide::Left => Rect::new(
            slot_rect.x1 - padding - bar_thickness_px,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x1 - padding,
            title_y
                + measure.title_h_px
                + title_gap
                + (cross_dim_px - 2.0 * padding - measure.title_h_px - title_gap),
        ),
        LegendSide::Top => Rect::new(
            slot_rect.x0 + padding,
            slot_rect.y1 - padding - bar_thickness_px,
            slot_rect.x0 + padding + (cross_dim_px - 2.0 * padding),
            slot_rect.y1 - padding,
        ),
        LegendSide::Bottom => Rect::new(
            slot_rect.x0 + padding,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x0 + padding + (cross_dim_px - 2.0 * padding),
            title_y + measure.title_h_px + title_gap + bar_thickness_px,
        ),
        LegendSide::InPanel { .. } => unreachable!("cardinal_side flattens InPanel"),
    };
    draw_gradient_bar(
        &legend.domain_scale,
        domain,
        spec,
        legend.bin_spacing,
        registry,
        &bar_rect,
        side,
        scene,
    );
    let _ = samples; // sample count carried on the spec, used inside draw_gradient_bar

    // Axis along the bar's long edge — uses the shared linear-axis
    // function so ticks and labels match the cartesian / polar
    // radius axes pixel-for-pixel.
    let (axis_start, axis_end, tick_direction) = axis_baseline(side, bar_rect);

    let majors_owned = colorbar_majors(domain);
    let majors_owned = if legend.bin_spacing == BinSpacing::Equal {
        colorbar_majors_remap_equal(&majors_owned)
    } else {
        majors_owned
    };
    let majors = open_end_trim(&majors_owned, legend.open_lower, legend.open_upper);
    crate::plot::chrome::linear_axis::draw_linear_axis_at(
        scene,
        axis_start,
        axis_end,
        tick_direction,
        majors,
        &[],
        dpi,
    );
}

/// Remap each major's fraction to its equal-spaced position
/// (`i / (n − 1)`), preserving order and labels. Used when the legend
/// is in [`BinSpacing::Equal`] mode so the tick rail's labels still
/// report the underlying break values but their positions line up
/// with the equal-width bin / colour blocks.
fn colorbar_majors_remap_equal(majors: &[(f64, String)]) -> Vec<(f64, String)> {
    let n = majors.len();
    if n <= 1 {
        return majors.to_vec();
    }
    majors
        .iter()
        .enumerate()
        .map(|(i, (_, label))| (i as f64 / (n - 1) as f64, label.clone()))
        .collect()
}

/// Drop the first and / or last element from a majors slice when the
/// caller has marked the corresponding outer bin as open. Operates on
/// the per-break `(frac, label)` pairs `draw_linear_axis_at`
/// consumes — the swatches / gradient blocks themselves are unaffected.
fn open_end_trim(majors: &[(f64, String)], open_lower: bool, open_upper: bool) -> &[(f64, String)] {
    let start = if open_lower && !majors.is_empty() {
        1
    } else {
        0
    };
    let end_excl = if open_upper && majors.len() > start {
        majors.len() - 1
    } else {
        majors.len()
    };
    if start <= end_excl {
        &majors[start..end_excl]
    } else {
        &[]
    }
}

/// Domain-fraction (axis-frac) + label string per break, for the
/// colorbar's tick rail. The frac is `(break - min) / (max - min)`
/// — the position the break maps to along the bar regardless of the
/// scale's output range.
fn colorbar_majors(domain: &crate::plot::scale::Scale) -> Vec<(f64, String)> {
    let (min, max) = match domain.input_range() {
        Some(crate::scales::input::InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Vec::new(),
    };
    let span = max - min;
    if !span.is_finite() || span.abs() < f64::EPSILON {
        return Vec::new();
    }
    domain
        .breaks(DEFAULT_BREAK_COUNT)
        .iter()
        .filter(|v| !matches!(v, Value::Null))
        .filter_map(|v| {
            let n = v.as_number().or_else(|| v.as_temporal_f64())?;
            if !n.is_finite() {
                return None;
            }
            let frac = (n - min) / span;
            Some((frac, domain.format(v)))
        })
        .collect()
}

/// Fill the bar with a single linear-gradient brush whose stops
/// resolve a [`ResolvedKey`] per sample from the spec's bindings,
/// picking `fill` (modulated by `alpha` if set) as the stop colour.
/// `fill` defaults to the legend's `domain_scale` if not in
/// `bindings`. Single `scene.fill` call — no AA seams between
/// adjacent sample rects.
#[allow(clippy::too_many_arguments)]
fn draw_gradient_bar(
    domain_scale_name: &str,
    domain: &crate::plot::scale::Scale,
    spec: &ColorbarSpec,
    bin_spacing: BinSpacing,
    registry: &ScaleRegistry,
    bar: &Rect,
    side: LegendSide,
    scene: &mut dyn SceneBuilder,
) {
    let (min, max) = match domain.input_range() {
        Some(crate::scales::input::InputRange::Continuous { min, max }) => (*min, *max),
        _ => return,
    };
    let span = max - min;
    if !span.is_finite() || span.abs() < f64::EPSILON {
        return;
    }
    let n = spec.samples.max(2);
    let horizontal = matches!(side, LegendSide::Top | LegendSide::Bottom);

    // Gradient endpoints (in pixel space). For Right/Left the
    // gradient runs from BOTTOM (low frac) to TOP (high frac) so
    // positive y_frac maps "up" — same convention as the cartesian
    // y-axis. For Top/Bottom it runs left → right.
    let (grad_start, grad_end) = if horizontal {
        (
            Point::new(bar.x0, bar.y0 + (bar.y1 - bar.y0) * 0.5),
            Point::new(bar.x1, bar.y0 + (bar.y1 - bar.y0) * 0.5),
        )
    } else {
        (
            Point::new(bar.x0 + (bar.x1 - bar.x0) * 0.5, bar.y1),
            Point::new(bar.x0 + (bar.x1 - bar.x0) * 0.5, bar.y0),
        )
    };

    // Implicit `fill = Scaled(domain_scale)` if the spec doesn't
    // bind it explicitly. Same semantics as a Rect key with a
    // single scaled fill binding.
    let has_explicit_fill =
        spec.bindings.contains_key("fill") || spec.bindings.contains_key("color");

    // Resolve one stop colour at a domain value, honouring the
    // spec's bindings, the implicit fill fallback, and alpha
    // modulation. Shared between the smooth and stepped paths.
    let resolve_stop_colour = |value: Value| -> Color {
        let mut resolved = ResolvedKey::default();
        for (aesthetic, source) in &spec.bindings {
            let v = match source {
                AestheticSource::Scaled(name) => match registry.get(name) {
                    Some(scale) => scale.map(&value),
                    None => continue,
                },
                AestheticSource::Fixed(val) => val.clone(),
            };
            resolved.apply(aesthetic, v);
        }
        if !has_explicit_fill {
            if let Some(c) = domain.map(&value).as_color() {
                resolved.fill = Some(c);
            }
        }
        apply_alpha(
            resolved.fill.unwrap_or_else(|| rgb(0.5, 0.5, 0.5)),
            resolved.alpha,
        )
    };

    let stops: Vec<peniko::ColorStop> = if spec.stepped {
        // Constant-colour blocks between adjacent breaks. Two stops
        // per bin at the *same* colour share offsets with the
        // adjacent bin's outer stop — peniko interpolates between
        // them across zero distance, producing an instant
        // transition (a step) in the gradient.
        let mut break_values: Vec<f64> = domain
            .breaks(DEFAULT_BREAK_COUNT)
            .iter()
            .filter_map(|v| {
                let n = v.as_number().or_else(|| v.as_temporal_f64())?;
                if !n.is_finite() {
                    return None;
                }
                Some(n)
            })
            .filter(|n| *n >= min && *n <= max)
            .collect();
        // Make sure the bar is fully covered even if the breaks
        // don't reach the domain endpoints — clamp to [min, max] on
        // either side.
        if break_values.first().copied().unwrap_or(max) > min {
            break_values.insert(0, min);
        }
        if break_values.last().copied().unwrap_or(min) < max {
            break_values.push(max);
        }
        if break_values.len() < 2 {
            return;
        }
        let n_bins = break_values.len() - 1;
        let mut out = Vec::with_capacity(break_values.len() * 2);
        for (i, w) in break_values.windows(2).enumerate() {
            let (lo, hi) = (w[0], w[1]);
            let mid_value = Value::Number((lo + hi) * 0.5);
            let (lo_t, hi_t) = match bin_spacing {
                BinSpacing::Proportional => ((lo - min) / span, (hi - min) / span),
                BinSpacing::Equal => (i as f64 / n_bins as f64, (i + 1) as f64 / n_bins as f64),
            };
            let color = resolve_stop_colour(mid_value);
            out.push(peniko::ColorStop {
                offset: lo_t as f32,
                color: color.into(),
            });
            out.push(peniko::ColorStop {
                offset: hi_t as f32,
                color: color.into(),
            });
        }
        out
    } else {
        (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                let value = Value::Number(min + t * span);
                peniko::ColorStop {
                    offset: t as f32,
                    color: resolve_stop_colour(value).into(),
                }
            })
            .collect()
    };

    let gradient = peniko::Gradient::new_linear(grad_start, grad_end).with_stops(stops.as_slice());
    let path: Path = bar.to_path(0.0);
    scene.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(gradient),
        None,
        &path,
        PickId::Skip,
    );
    // Suppress unused param when no resolver is needed — keeps the
    // signature stable for future callers that might want to inspect
    // the legend's domain scale name.
    let _ = domain_scale_name;
}

// ─── Resolution ─────────────────────────────────────────────────────────────

fn resolve_key(spec: &LegendKeySpec, registry: &ScaleRegistry, row: &Value) -> ResolvedKey {
    let mut resolved = ResolvedKey::default();
    for (aesthetic, source) in &spec.bindings {
        let value = match source {
            AestheticSource::Scaled(name) => match registry.get(name) {
                Some(scale) => scale.map(row),
                None => continue,
            },
            AestheticSource::Fixed(v) => v.clone(),
        };
        resolved.apply(aesthetic, value);
    }
    resolved
}

// ─── Measure ────────────────────────────────────────────────────────────────

struct LegendMeasure {
    side: LegendSide,
    body: BodyMeasure,
    /// Shaped label dims, max across breaks.
    max_label_w_px: f64,
    max_label_h_px: f64,
    title_w_px: f64,
    title_h_px: f64,
    /// Number of non-null breaks the domain scale produces.
    entry_count: usize,
}

enum BodyMeasure {
    /// Discrete stack — `swatch_dim_px` is the cell size for the
    /// biggest marker in the stack; `no_keys` short-circuits the
    /// measure to zero when the stack is empty; `binned` switches
    /// to N-bins-from-N+1-breaks layout with a between-row tick
    /// rail.
    Stack {
        swatch_dim_px: f64,
        no_keys: bool,
        binned: bool,
    },
    /// Colorbar — `bar_thickness_px` is the bar's perpendicular
    /// extent; `samples` is forwarded to the renderer.
    Colorbar {
        bar_thickness_px: f64,
        samples: usize,
    },
}

impl LegendMeasure {
    fn new(legend: &Legend, registry: &ScaleRegistry, dpi: f64) -> Self {
        let label_style = TextStyle::new(LABEL_FONT_SIZE_PT);
        let domain = registry.get(&legend.domain_scale);
        let breaks = domain
            .map(|s| s.breaks(DEFAULT_BREAK_COUNT))
            .unwrap_or_default();

        let mut entry_count = 0usize;
        let mut max_label_w: f64 = 0.0;
        let mut max_label_h: f64 = 0.0;
        for v in &breaks {
            if matches!(v, Value::Null) {
                continue;
            }
            let label = domain.map(|s| s.format(v)).unwrap_or_default();
            let run = TextRun::new(&label, &label_style);
            let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
            let w = match run.width_hint(dpi) {
                WidthHint::Min(w) => w,
                WidthHint::NeedsHeight { seed } => seed,
            };
            max_label_w = max_label_w.max(w);
            max_label_h = max_label_h.max(h);
            entry_count += 1;
        }

        let body = match &legend.body {
            LegendBody::Stack(stack) => {
                let peak = peak_resolved_for_stack(&stack.keys, registry, &breaks);
                let swatch_dim_px = stack
                    .keys
                    .iter()
                    .map(|k| swatch_dim_for(k.kind, &peak, dpi))
                    .fold(0.0_f64, f64::max)
                    .max(pt_to_px(SWATCH_SIZE_PT, dpi));
                BodyMeasure::Stack {
                    swatch_dim_px,
                    no_keys: stack.keys.is_empty(),
                    binned: stack.binned,
                }
            }
            LegendBody::Colorbar(spec) => BodyMeasure::Colorbar {
                bar_thickness_px: pt_to_px(spec.thickness_pt, dpi),
                samples: spec.samples.max(2),
            },
        };

        let (title_w_px, title_h_px) = match &legend.title {
            Some(text) if !text.is_empty() => {
                let title_style = TextStyle::new(LABEL_FONT_SIZE_PT).weight(700);
                let run = TextRun::new(text, &title_style);
                let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
                let w = match run.width_hint(dpi) {
                    WidthHint::Min(w) => w,
                    WidthHint::NeedsHeight { seed } => seed,
                };
                (w, h)
            }
            _ => (0.0, 0.0),
        };

        LegendMeasure {
            // Store the cardinal layout direction so primary/cross
            // dim matches can use a 4-arm pattern. In-panel legends
            // size themselves the same as a Right legend.
            side: cardinal_side(legend.side),
            body,
            entry_count,
            max_label_w_px: max_label_w,
            max_label_h_px: max_label_h,
            title_w_px,
            title_h_px,
        }
    }

    fn is_empty(&self) -> bool {
        if self.entry_count == 0 {
            return true;
        }
        matches!(self.body, BodyMeasure::Stack { no_keys: true, .. })
    }

    /// Block dimension along the side's primary axis: column width
    /// for Right/Left, row height for Top/Bottom.
    fn primary_dim_px(&self, dpi: f64) -> f64 {
        let padding = pt_to_px(PADDING_PT, dpi);
        let gap = pt_to_px(SWATCH_LABEL_GAP_PT, dpi);
        let title_gap = if self.title_h_px > 0.0 {
            pt_to_px(ROW_GAP_PT, dpi)
        } else {
            0.0
        };
        let tick_px = pt_to_px(crate::plot::chrome::linear_axis::TICK_LENGTH_PT, dpi);
        let label_gap_axis = pt_to_px(crate::plot::chrome::linear_axis::LABEL_GAP_PT, dpi);

        match (&self.body, self.side) {
            (
                BodyMeasure::Stack {
                    swatch_dim_px,
                    binned: false,
                    ..
                },
                LegendSide::Right | LegendSide::Left,
            ) => {
                let row_w = swatch_dim_px + gap + self.max_label_w_px;
                row_w.max(self.title_w_px) + 2.0 * padding
            }
            (
                BodyMeasure::Stack {
                    swatch_dim_px,
                    binned: false,
                    ..
                },
                LegendSide::Top | LegendSide::Bottom,
            ) => {
                let row_h = self.max_label_h_px.max(*swatch_dim_px);
                self.title_h_px + title_gap + row_h + 2.0 * padding
            }
            (
                BodyMeasure::Stack {
                    swatch_dim_px,
                    binned: true,
                    ..
                },
                LegendSide::Right | LegendSide::Left,
            )
            | (
                BodyMeasure::Colorbar {
                    bar_thickness_px: swatch_dim_px,
                    ..
                },
                LegendSide::Right | LegendSide::Left,
            ) => {
                // Column width = swatch/bar + tick + gap + label_w
                // (or title_w, whichever is wider). Binned stack and
                // colorbar share the same axis-arm layout — only the
                // contents of the "bar column" differ.
                let axis_arm = swatch_dim_px + tick_px + label_gap_axis + self.max_label_w_px;
                axis_arm.max(self.title_w_px) + 2.0 * padding
            }
            (
                BodyMeasure::Stack {
                    swatch_dim_px,
                    binned: true,
                    ..
                },
                LegendSide::Top | LegendSide::Bottom,
            )
            | (
                BodyMeasure::Colorbar {
                    bar_thickness_px: swatch_dim_px,
                    ..
                },
                LegendSide::Top | LegendSide::Bottom,
            ) => {
                // Row height = title + gap + swatch/bar + tick + gap + label_h.
                self.title_h_px
                    + title_gap
                    + swatch_dim_px
                    + tick_px
                    + label_gap_axis
                    + self.max_label_h_px
                    + 2.0 * padding
            }
            (_, LegendSide::InPanel { .. }) => {
                unreachable!("LegendMeasure stores cardinal side, never InPanel")
            }
        }
    }

    /// Cross-axis dim: height for Right/Left, width for Top/Bottom.
    /// Used by [`LegendStackMeasure`] to split the slot rect among
    /// stacked legends.
    fn cross_dim_px(&self, dpi: f64) -> f64 {
        let padding = pt_to_px(PADDING_PT, dpi);
        let row_gap = pt_to_px(ROW_GAP_PT, dpi);
        let gap = pt_to_px(SWATCH_LABEL_GAP_PT, dpi);
        let title_gap = if self.title_h_px > 0.0 {
            pt_to_px(ROW_GAP_PT, dpi)
        } else {
            0.0
        };
        let n = self.entry_count as f64;

        match (&self.body, self.side) {
            (
                BodyMeasure::Stack {
                    swatch_dim_px,
                    binned: false,
                    ..
                },
                LegendSide::Right | LegendSide::Left,
            ) => {
                let row_h = self.max_label_h_px.max(*swatch_dim_px);
                self.title_h_px
                    + title_gap
                    + n * row_h
                    + (n - 1.0).max(0.0) * row_gap
                    + 2.0 * padding
            }
            (
                BodyMeasure::Stack {
                    swatch_dim_px,
                    binned: false,
                    ..
                },
                LegendSide::Top | LegendSide::Bottom,
            ) => {
                let row_w = swatch_dim_px + gap + self.max_label_w_px;
                let entries_w = n * row_w + (n - 1.0).max(0.0) * row_gap;
                entries_w.max(self.title_w_px) + 2.0 * padding
            }
            (
                BodyMeasure::Stack {
                    swatch_dim_px,
                    binned: true,
                    ..
                },
                LegendSide::Right | LegendSide::Left,
            ) => {
                // Binned: N bins from N+1 breaks → N rows touching
                // vertically. Bar length = (n_breaks − 1) × swatch_h.
                let n_bins = (n - 1.0).max(1.0);
                let bar_len = n_bins * swatch_dim_px;
                self.title_h_px + title_gap + bar_len + 2.0 * padding
            }
            (
                BodyMeasure::Stack {
                    swatch_dim_px,
                    binned: true,
                    ..
                },
                LegendSide::Top | LegendSide::Bottom,
            ) => {
                let n_bins = (n - 1.0).max(1.0);
                let bar_len = n_bins * swatch_dim_px;
                bar_len.max(self.title_w_px) + 2.0 * padding
            }
            (BodyMeasure::Colorbar { .. }, LegendSide::Right | LegendSide::Left) => {
                // Vertical bar length defaults to (n−1) × label-pitch
                // + label height — enough to space the major ticks
                // legibly. The actual rendered length scales to the
                // available slot height at draw time.
                let pitch = self.max_label_h_px + row_gap;
                let bar_len = (n - 1.0).max(1.0) * pitch + self.max_label_h_px;
                self.title_h_px + title_gap + bar_len + 2.0 * padding
            }
            (BodyMeasure::Colorbar { .. }, LegendSide::Top | LegendSide::Bottom) => {
                // Horizontal bar length: (n−1) × label-pitch +
                // label_w to leave clear gaps between tick labels.
                let pitch = self.max_label_w_px + gap * 3.0;
                let bar_len = (n - 1.0).max(1.0) * pitch + self.max_label_w_px;
                bar_len.max(self.title_w_px) + 2.0 * padding
            }
            (_, LegendSide::InPanel { .. }) => {
                unreachable!("LegendMeasure stores cardinal side, never InPanel")
            }
        }
    }
}

impl Measure for LegendMeasure {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        if self.is_empty() {
            return WidthHint::Min(0.0);
        }
        match self.side {
            LegendSide::Right | LegendSide::Left => WidthHint::Min(self.primary_dim_px(dpi)),
            LegendSide::Top | LegendSide::Bottom => WidthHint::Min(0.0),
            LegendSide::InPanel { .. } => {
                unreachable!("LegendMeasure stores cardinal side, never InPanel")
            }
        }
    }

    fn height_at(&self, _width: f64, dpi: f64) -> f64 {
        if self.is_empty() {
            return 0.0;
        }
        match self.side {
            LegendSide::Top | LegendSide::Bottom => self.primary_dim_px(dpi),
            LegendSide::Right | LegendSide::Left => 0.0,
            LegendSide::InPanel { .. } => {
                unreachable!("LegendMeasure stores cardinal side, never InPanel")
            }
        }
    }
}

/// Composite measure for multiple legends stacked on the same side.
/// The primary extent is reserved for the *widest* child (so all
/// children get the same column width / row height); the cross
/// extent is the sum of children plus inter-legend gaps.
struct LegendStackMeasure {
    side: LegendSide,
    children: Vec<LegendMeasure>,
}

impl LegendStackMeasure {
    fn non_empty(&self) -> impl Iterator<Item = &LegendMeasure> {
        self.children.iter().filter(|c| !c.is_empty())
    }
    fn primary_max(&self, dpi: f64) -> f64 {
        self.non_empty()
            .map(|c| c.primary_dim_px(dpi))
            .fold(0.0_f64, f64::max)
    }
}

impl Measure for LegendStackMeasure {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        let any = self.non_empty().next().is_some();
        if !any {
            return WidthHint::Min(0.0);
        }
        match self.side {
            LegendSide::Right | LegendSide::Left => WidthHint::Min(self.primary_max(dpi)),
            LegendSide::Top | LegendSide::Bottom => WidthHint::Min(0.0),
            LegendSide::InPanel { .. } => {
                unreachable!("LegendStackMeasure is constructed with a cardinal side")
            }
        }
    }
    fn height_at(&self, _width: f64, dpi: f64) -> f64 {
        let any = self.non_empty().next().is_some();
        if !any {
            return 0.0;
        }
        match self.side {
            LegendSide::Top | LegendSide::Bottom => self.primary_max(dpi),
            LegendSide::Right | LegendSide::Left => 0.0,
            LegendSide::InPanel { .. } => {
                unreachable!("LegendStackMeasure is constructed with a cardinal side")
            }
        }
    }
}

/// Walk a stack's keys + breaks and produce a `ResolvedKey` whose
/// numeric fields hold the peak (max) value any key would see. Used
/// to size the swatch cell so the largest marker fits.
fn peak_resolved_for_stack(
    keys: &[LegendKeySpec],
    registry: &ScaleRegistry,
    breaks: &[Value],
) -> ResolvedKey {
    let mut peak = ResolvedKey::default();
    for key in keys {
        for v in breaks {
            if matches!(v, Value::Null) {
                continue;
            }
            let resolved = resolve_key(key, registry, v);
            if let Some(s) = resolved.size_pt {
                peak.size_pt = Some(peak.size_pt.map_or(s, |p| p.max(s)));
            }
            if let Some(lw) = resolved.linewidth_pt {
                peak.linewidth_pt = Some(peak.linewidth_pt.map_or(lw, |p| p.max(lw)));
            }
        }
    }
    peak
}

// ─── Per-key swatch dim + render ───────────────────────────────────────────

fn swatch_dim_for(kind: LegendKey, peak: &ResolvedKey, dpi: f64) -> f64 {
    match kind {
        LegendKey::Point => {
            let size_pt = peak.size_pt.unwrap_or(DEFAULT_POINT_DIAMETER_PT);
            // Match PointGeom's circle path (radius 0.8) so the
            // rendered marker matches the geom for the same size.
            pt_to_px(size_pt * 2.0 * POINT_SHAPE_RADIUS, dpi)
        }
        LegendKey::Line => pt_to_px(LINE_SWATCH_LEN_PT, dpi),
        LegendKey::Rect => pt_to_px(SWATCH_SIZE_PT, dpi),
    }
}

fn render_key(
    kind: LegendKey,
    resolved: &ResolvedKey,
    cell: Rect,
    shapes: &ShapeRegistry,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
) {
    match kind {
        LegendKey::Point => render_point(resolved, cell, shapes, scene, dpi),
        LegendKey::Line => render_line(resolved, cell, scene, dpi),
        LegendKey::Rect => render_rect(resolved, cell, scene, dpi),
    }
}

fn apply_alpha(c: Color, alpha: Option<f64>) -> Color {
    match alpha {
        Some(a) => {
            let [r, g, b, base] = c.components;
            let combined = (base as f64 * a.clamp(0.0, 1.0)) as f32;
            Color::new([r, g, b, combined])
        }
        None => c,
    }
}

fn render_point(
    resolved: &ResolvedKey,
    cell: Rect,
    shapes: &ShapeRegistry,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
) {
    let size_pt = resolved.size_pt.unwrap_or(DEFAULT_POINT_DIAMETER_PT);
    let size_px = pt_to_px(size_pt, dpi);
    let centre = Point::new(
        cell.x0 + (cell.x1 - cell.x0) * 0.5,
        cell.y0 + (cell.y1 - cell.y0) * 0.5,
    );

    // Honour `resolved.shape` if it names a registered shape with
    // path content. Same scaling convention as `PointGeom` (the
    // shape's path is scaled by `size_px`). For Glyph-backed
    // shapes (font glyphs) we fall back to the default circle —
    // the legend chrome doesn't currently shape glyph markers.
    let shape = resolved.shape.as_deref().and_then(|name| shapes.get(name));
    let xform = Affine::translate((centre.x, centre.y)) * Affine::scale(size_px);

    let fill_color = resolved
        .fill
        .map(|c| Brush::Solid(apply_alpha(c, resolved.alpha)));
    let stroke_brush = resolved
        .stroke
        .map(|c| Brush::Solid(apply_alpha(c, resolved.alpha)));
    let stroke = stroke_brush.as_ref().map(|_| {
        Stroke::new(pt_to_px(
            resolved.linewidth_pt.unwrap_or(DEFAULT_LINEWIDTH_PT),
            dpi,
        ))
    });

    if let Some(s) = shape {
        match s.kind() {
            ShapeKind::Paths { paths, style } => {
                for sub in paths {
                    match style {
                        ShapeStyle::Fill => {
                            if let Some(fill) = &fill_color {
                                scene.fill(FillRule::NonZero, xform, fill, None, sub, PickId::Skip);
                            }
                            if let (Some(stroke_brush), Some(stroke)) = (&stroke_brush, &stroke) {
                                scene.stroke(stroke, xform, stroke_brush, None, sub, PickId::Skip);
                            }
                        }
                        ShapeStyle::Stroke => {
                            if let (Some(stroke_brush), Some(stroke)) = (&stroke_brush, &stroke) {
                                scene.stroke(stroke, xform, stroke_brush, None, sub, PickId::Skip);
                            }
                        }
                    }
                }
                return;
            }
            ShapeKind::Glyph {
                font,
                glyph_id,
                em_bbox,
                em_origin,
            } => {
                // Glyph marker — bake the em-to-pixel scale into
                // `font_size` rather than into the transform so
                // vello picks the right bitmap strike for colour
                // emoji fonts. Outline (scalable) fonts are
                // unaffected; bitmap fonts ship discrete strikes
                // at fixed pixel sizes and `font_size: 1.0` would
                // pick the smallest one and upscale (= fuzzy at
                // typical chart sizes).
                let Some(fill) = &fill_color else { return };
                let h = em_bbox.height();
                if !(h.is_finite() && h > 0.0) {
                    return;
                }
                let bbox_norm = GLYPH_BBOX_REFERENCE / h;
                let effective_font_size_px = size_px * bbox_norm;
                // The original transform multiplied em-space by
                // `size_px * bbox_norm`; doing that via `font_size`
                // means the transform is just a translate to the
                // cell centre + the em-space centring offset
                // converted to pixels.
                let centring_px =
                    (em_origin.to_vec2() - em_bbox.center().to_vec2()) * effective_font_size_px;
                let glyphs = [Glyph {
                    id: glyph_id,
                    x: 0.0,
                    y: 0.0,
                }];
                let run = GlyphRun {
                    font,
                    font_size: effective_font_size_px as f32,
                    transform: Affine::translate((
                        centre.x + centring_px.x,
                        centre.y + centring_px.y,
                    )),
                    glyph_transform: None,
                    brush: fill,
                    brush_alpha: 1.0,
                    hint: false,
                    glyphs: &glyphs,
                };
                scene.draw_glyphs(&run, PickId::Skip);
                return;
            }
        }
    }

    // Default / fallback: circle, sized to match PointGeom's
    // built-in circle (radius 0.8 in shape space).
    let radius = size_px * POINT_SHAPE_RADIUS;
    let path = circle(centre, radius);
    if let Some(fill) = &fill_color {
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            fill,
            None,
            &path,
            PickId::Skip,
        );
    }
    if let (Some(stroke_brush), Some(stroke)) = (&stroke_brush, &stroke) {
        scene.stroke(
            stroke,
            Affine::IDENTITY,
            stroke_brush,
            None,
            &path,
            PickId::Skip,
        );
    }
}

fn render_line(resolved: &ResolvedKey, cell: Rect, scene: &mut dyn SceneBuilder, dpi: f64) {
    // Pick stroke colour: explicit `stroke` channel wins, else fall
    // back to `fill` (callers sometimes write `color` → fill on the
    // ResolvedKey via the alias in `apply`).
    let color = resolved
        .stroke
        .or(resolved.fill)
        .map(|c| apply_alpha(c, resolved.alpha))
        .unwrap_or_else(ink);
    let lw_pt = resolved.linewidth_pt.unwrap_or(DEFAULT_LINEWIDTH_PT);
    let mid_y = cell.y0 + (cell.y1 - cell.y0) * 0.5;
    let p0 = Point::new(cell.x0, mid_y);
    let p1 = Point::new(cell.x1, mid_y);
    let path = segment(p0, p1);
    let stroke = match &resolved.linetype {
        Some(pattern) if !pattern.is_empty() => {
            let dashes_pt = crate::plot::geom::linetype::to_kurbo_dashes(pattern);
            let dashes_px: Vec<f64> = dashes_pt.into_iter().map(|d| pt_to_px(d, dpi)).collect();
            Stroke::new(pt_to_px(lw_pt, dpi)).with_dashes(0.0, dashes_px)
        }
        _ => Stroke::new(pt_to_px(lw_pt, dpi)),
    };
    scene.stroke(
        &stroke,
        Affine::IDENTITY,
        &Brush::Solid(color),
        None,
        &path,
        PickId::Skip,
    );
}

fn render_rect(resolved: &ResolvedKey, cell: Rect, scene: &mut dyn SceneBuilder, dpi: f64) {
    let path: Path = cell.to_path(0.0);
    if let Some(fill) = resolved.fill {
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Solid(apply_alpha(fill, resolved.alpha)),
            None,
            &path,
            PickId::Skip,
        );
    }
    if let Some(stroke_color) = resolved.stroke {
        let lw = resolved.linewidth_pt.unwrap_or(DEFAULT_LINEWIDTH_PT);
        let stroke = Stroke::new(pt_to_px(lw, dpi));
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &Brush::Solid(apply_alpha(stroke_color, resolved.alpha)),
            None,
            &path,
            PickId::Skip,
        );
    } else if resolved.fill.is_none() {
        // Placeholder outline so the row isn't visually empty.
        let stroke = Stroke::new(pt_to_px(DEFAULT_LINEWIDTH_PT, dpi));
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &Brush::Solid(ink()),
            None,
            &path,
            PickId::Skip,
        );
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::scale;
    use crate::scene::recording::{Op, RecordingScene};

    fn dpi_96() -> f64 {
        96.0
    }

    fn build_registry() -> ScaleRegistry {
        let mut reg = ScaleRegistry::new();
        reg.insert(
            "category_color",
            scale::discrete([
                Value::String(Arc::from("A")),
                Value::String(Arc::from("B")),
                Value::String(Arc::from("C")),
            ])
            .range_colors([
                rgb(1.0, 0.0, 0.0),
                rgb(0.0, 1.0, 0.0),
                rgb(0.0, 0.0, 1.0),
            ]),
        );
        reg.insert(
            "category_size",
            scale::discrete([
                Value::String(Arc::from("A")),
                Value::String(Arc::from("B")),
                Value::String(Arc::from("C")),
            ])
            .range_numbers([4.0, 8.0, 12.0]),
        );
        reg
    }

    #[test]
    fn empty_legend_reports_zero_size() {
        let legend = Legend::new("category_color");
        let reg = build_registry();
        let m = legend_measure(&legend, &reg, dpi_96());
        assert_eq!(m.width_hint(dpi_96()), WidthHint::Min(0.0));
        assert_eq!(m.height_at(100.0, dpi_96()), 0.0);
    }

    #[test]
    fn point_legend_with_scaled_fill_reports_nonzero() {
        let legend = Legend::new("category_color")
            .title("Category")
            .key(LegendKeySpec::point().scaled("fill", "category_color"));
        let reg = build_registry();
        let m = legend_measure(&legend, &reg, dpi_96());
        let w = match m.width_hint(dpi_96()) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => seed,
        };
        assert!(w > 0.0);
    }

    #[test]
    fn fixed_stroke_is_applied_alongside_scaled_fill() {
        // Three rows; Point key with scaled fill + fixed black stroke.
        // The renderer should emit 3 fills (one per row, from the
        // scale) and 3 strokes (all black, fixed).
        let legend = Legend::new("category_color").key(
            LegendKeySpec::point()
                .scaled("fill", "category_color")
                .fixed("stroke", Value::Color(rgb(0.0, 0.0, 0.0))),
        );
        let reg = build_registry();
        let mut scene = RecordingScene::default();
        let shapes = ShapeRegistry::with_builtins();
        render_legend(
            &legend,
            &reg,
            &shapes,
            Rect::new(0.0, 0.0, 200.0, 200.0),
            &mut scene,
            dpi_96(),
        );
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(fills, 3);
        assert_eq!(strokes, 3);
    }

    #[test]
    fn line_plus_point_keys_emit_both() {
        // Two-key stack: Line under Point. Three rows → 3 line
        // strokes + 3 point fills (Point has no stroke binding here).
        let legend = Legend::new("category_color")
            .key(LegendKeySpec::line().scaled("stroke", "category_color"))
            .key(LegendKeySpec::point().scaled("fill", "category_color"));
        let reg = build_registry();
        let mut scene = RecordingScene::default();
        let shapes = ShapeRegistry::with_builtins();
        render_legend(
            &legend,
            &reg,
            &shapes,
            Rect::new(0.0, 0.0, 200.0, 200.0),
            &mut scene,
            dpi_96(),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(strokes, 3, "one line stroke per row");
        assert_eq!(fills, 3, "one point fill per row");
    }

    #[test]
    fn legend_is_compatible_with_matching_triple() {
        let a = Legend::new("x").side(LegendSide::Right).title("T");
        let b = Legend::new("x").side(LegendSide::Right).title("T");
        let c = Legend::new("x").side(LegendSide::Right).title("U");
        assert!(a.is_compatible_with(&b));
        assert!(!a.is_compatible_with(&c));
    }

    #[test]
    fn point_swatch_dim_scales_with_size_channel() {
        let small = Legend::new("category_color").key(
            LegendKeySpec::point()
                .scaled("fill", "category_color")
                .fixed("size", 4.0_f64),
        );
        let large = Legend::new("category_color").key(
            LegendKeySpec::point()
                .scaled("fill", "category_color")
                .scaled("size", "category_size"),
        );
        let reg = build_registry();
        let s_w = match legend_measure(&small, &reg, dpi_96()).width_hint(dpi_96()) {
            WidthHint::Min(w) => w,
            _ => 0.0,
        };
        let l_w = match legend_measure(&large, &reg, dpi_96()).width_hint(dpi_96()) {
            WidthHint::Min(w) => w,
            _ => 0.0,
        };
        assert!(
            l_w > s_w,
            "legend with scaled size up to 12pt should be wider than fixed-4pt: {s_w} vs {l_w}"
        );
    }

    fn sample_majors() -> Vec<(f64, String)> {
        vec![
            (0.0, "0".into()),
            (0.25, "1".into()),
            (0.5, "2".into()),
            (0.75, "3".into()),
            (1.0, "4".into()),
        ]
    }

    #[test]
    fn open_lower_drops_first_major() {
        let m = sample_majors();
        let trimmed = open_end_trim(&m, true, false);
        assert_eq!(trimmed.len(), 4);
        assert_eq!(trimmed[0].1, "1");
        assert_eq!(trimmed[3].1, "4");
    }

    #[test]
    fn open_upper_drops_last_major() {
        let m = sample_majors();
        let trimmed = open_end_trim(&m, false, true);
        assert_eq!(trimmed.len(), 4);
        assert_eq!(trimmed[0].1, "0");
        assert_eq!(trimmed[3].1, "3");
    }

    #[test]
    fn open_both_drops_both_terminals() {
        let m = sample_majors();
        let trimmed = open_end_trim(&m, true, true);
        assert_eq!(trimmed.len(), 3);
        assert_eq!(trimmed[0].1, "1");
        assert_eq!(trimmed[2].1, "3");
    }

    #[test]
    fn open_neither_returns_full_slice() {
        let m = sample_majors();
        let trimmed = open_end_trim(&m, false, false);
        assert_eq!(trimmed.len(), 5);
    }

    #[test]
    fn open_trim_handles_short_slices() {
        // Single element + open_lower yields empty.
        let one = vec![(0.5_f64, "mid".to_string())];
        assert!(open_end_trim(&one, true, false).is_empty());
        // Empty slice in is empty slice out.
        let empty: Vec<(f64, String)> = vec![];
        assert!(open_end_trim(&empty, true, true).is_empty());
    }

    #[test]
    fn legends_with_different_open_flags_are_not_compatible() {
        let a = Legend::new("x").open_lower();
        let b = Legend::new("x");
        assert!(!a.is_compatible_with(&b));
        let c = Legend::new("x").open_upper();
        assert!(!a.is_compatible_with(&c));
    }

    #[test]
    fn equal_remap_spaces_majors_uniformly() {
        // Pathological proportional split with five breaks.
        let m = vec![
            (0.0, "0".into()),
            (0.01, "1".into()),
            (0.05, "5".into()),
            (0.5, "50".into()),
            (1.0, "100".into()),
        ];
        let remapped = colorbar_majors_remap_equal(&m);
        assert_eq!(remapped.len(), 5);
        // Labels preserved in order.
        assert_eq!(remapped[0].1, "0");
        assert_eq!(remapped[4].1, "100");
        // Fractions are i / (n - 1) = i / 4.
        for (i, (frac, _)) in remapped.iter().enumerate() {
            let expected = i as f64 / 4.0;
            assert!(
                (frac - expected).abs() < 1e-12,
                "remap[{i}] = {frac}, expected {expected}"
            );
        }
    }

    #[test]
    fn equal_remap_short_slice_is_passthrough() {
        let single = vec![(0.42_f64, "lonely".to_string())];
        let remapped = colorbar_majors_remap_equal(&single);
        assert_eq!(remapped.len(), 1);
        assert!((remapped[0].0 - 0.42).abs() < 1e-12);
    }

    #[test]
    fn legends_with_different_bin_spacing_are_not_compatible() {
        let a = Legend::new("x").equal_bins();
        let b = Legend::new("x");
        assert!(!a.is_compatible_with(&b));
    }

    #[test]
    fn bin_spacing_default_is_proportional() {
        let legend = Legend::new("x");
        assert_eq!(legend.bin_spacing, BinSpacing::Proportional);
    }

    #[test]
    fn resolve_anchor_top_right_six_pt() {
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let rect = resolve_anchor(panel, Anchor::TopRight, 6.0, (20.0, 10.0));
        // Legend's TR corner sits at (94, 6) = panel TR - inset; size is (20, 10).
        assert!((rect.x1 - 94.0).abs() < 1e-12);
        assert!((rect.y0 - 6.0).abs() < 1e-12);
        assert!((rect.x0 - 74.0).abs() < 1e-12);
        assert!((rect.y1 - 16.0).abs() < 1e-12);
    }

    #[test]
    fn resolve_anchor_centre_centres_on_panel() {
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let rect = resolve_anchor(panel, Anchor::Center, 6.0, (20.0, 10.0));
        // Centre anchor ignores inset — the legend bbox centre lands on the panel centre.
        let cx = (rect.x0 + rect.x1) * 0.5;
        let cy = (rect.y0 + rect.y1) * 0.5;
        assert!((cx - 50.0).abs() < 1e-12);
        assert!((cy - 50.0).abs() < 1e-12);
    }

    #[test]
    fn resolve_anchor_bottom_left() {
        let panel = Rect::new(10.0, 20.0, 110.0, 120.0);
        let rect = resolve_anchor(panel, Anchor::BottomLeft, 4.0, (30.0, 12.0));
        // BL anchor: legend BL = panel BL offset inward by 4 on both axes.
        assert!((rect.x0 - 14.0).abs() < 1e-12);
        assert!((rect.y1 - 116.0).abs() < 1e-12);
    }

    #[test]
    fn in_panel_legend_natural_size_is_nonzero_for_populated_stack() {
        let legend = Legend::new("category_color")
            .side(LegendSide::InPanel {
                anchor: Anchor::TopRight,
                inset_pt: 6.0,
            })
            .key(LegendKeySpec::point().scaled("fill", "category_color"));
        let reg = build_registry();
        let (w, h) = legend_stack_natural_size(&[&legend], &reg, dpi_96());
        assert!(w > 0.0);
        assert!(h > 0.0);
    }

    #[test]
    fn in_panel_legend_natural_size_zero_for_empty_stack() {
        let legend = Legend::new("category_color").side(LegendSide::InPanel {
            anchor: Anchor::TopRight,
            inset_pt: 6.0,
        });
        let reg = build_registry();
        let (w, h) = legend_stack_natural_size(&[&legend], &reg, dpi_96());
        assert_eq!(w, 0.0);
        assert_eq!(h, 0.0);
    }
}
