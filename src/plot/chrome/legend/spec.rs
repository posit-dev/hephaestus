//! What a legend *is*, before anything measures or draws it.
//!
//! [`Legend`] carries the shell — side, title, domain scale, bin flags
//! — plus a [`LegendBody`] that picks the visualisation: a stack of
//! [`LegendKeySpec`] markers or a [`ColorbarSpec`] gradient. Both
//! express their aesthetics as [`AestheticSource`] bindings, and
//! [`resolve_key`] turns those into a [`ResolvedKey`] at one domain
//! value — the bundle every draw pass in the module reads.
//!
//! [`collapse_legends`] folds legends describing the same block into
//! one, so the layout, colorbar and measure passes only ever see the
//! blocks that actually render.

use std::collections::HashMap;
use std::sync::Arc;

use crate::color::Color;
use crate::plot::geom::resolve::{cap_from_str, join_from_str};
use crate::plot::scale::ScaleRegistry;
use crate::scales::chrome::LegendSide;
use crate::scales::value::{LinetypeStep, Value};
use crate::stroke::{Cap, Join};

/// Stable identifier returned by [`crate::plot::Plot::add_legend`],
/// unique per attached legend. Used to remove or update a legend
/// later; render-time collapse doesn't consume or reassign ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LegendId(u32);

impl LegendId {
    pub(crate) fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw handle value. Opaque — useful only as a key, and not
    /// comparable across plots.
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// Marker primitives a [`LegendKeySpec`] can draw.
///
/// Each variant has a fixed set of aesthetics it reads from a
/// [`ResolvedKey`]; aesthetics not relevant to the variant are
/// silently ignored. Variants not yet implemented (Wedge, Segment,
/// …) can be added without touching the surrounding machinery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LegendKey {
    /// Sized marker (default shape: `circle` from the registry).
    /// Consumes: fill, stroke, fill_opacity, stroke_opacity, size,
    /// shape, angle, linewidth (as the marker's outline width).
    Point,
    /// Short horizontal stroke, stamped with endpoint markers when
    /// bound. Consumes: stroke (or `color` as a fallback),
    /// stroke_opacity, linewidth, linetype, dash_offset, cap, join,
    /// start_marker / end_marker and their `_size` / `_fill` /
    /// `_invert` companions.
    Line,
    /// Filled rectangle covering the swatch cell, with its border
    /// inside the cell. Consumes: fill, stroke, fill_opacity,
    /// stroke_opacity, linewidth, linetype, dash_offset, cap, join,
    /// corner_radius.
    Rect,
    /// Glyph sample centred in the swatch cell — what a text layer's
    /// scales read as. Consumes: text, size (as the font size),
    /// weight, italic, family, tracking, underline,
    /// strikethrough, fill, fill_opacity, text_stroke,
    /// text_linewidth, angle. An unbound `text` draws
    /// [`DEFAULT_KEY_TEXT`](super::DEFAULT_KEY_TEXT). Background-rect
    /// aesthetics belong to a [`LegendKey::Rect`] stacked beneath this
    /// one.
    Text,
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
    /// Start a `Text` key with no aesthetic bindings. The glyph is
    /// [`DEFAULT_KEY_TEXT`](super::DEFAULT_KEY_TEXT) until a `"text"`
    /// aesthetic names another one.
    pub fn text() -> Self {
        Self {
            kind: LegendKey::Text,
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
/// Fields are crate-visible rather than public: every one of them has
/// a builder, and keeping the layout private keeps it off the semver
/// surface.
pub struct Legend {
    pub(crate) side: LegendSide,
    pub(crate) title: Option<String>,
    /// Scale whose `breaks()` drive the legend's tick / label
    /// positions.
    pub(crate) domain_scale: String,
    pub(crate) body: LegendBody,
    /// Suppress the first tick + label on the legend's rail to
    /// communicate an unbounded bottom bin (the swatch still renders
    /// full-size). Honoured by binned-stack and stepped-colorbar
    /// bodies; ignored on continuous colorbars and non-binned stacks.
    pub(crate) open_lower: bool,
    /// Suppress the last tick + label on the legend's rail to
    /// communicate an unbounded top bin. Mirrors `open_lower`.
    pub(crate) open_upper: bool,
    /// Whether binned bodies size each bin proportionally to its
    /// break span (the default) or give every bin the same extent
    /// along the bar.
    pub(crate) bin_spacing: BinSpacing,
    /// Optional named theme variant. When set, the legend renderer
    /// resolves
    /// `theme.legend_variants.get(name).unwrap_or(&theme.legend)` to
    /// pick the [`LegendTheme`](crate::plot::theme::LegendTheme)
    /// that styles this legend. `None` (the default) uses the
    /// theme's default `legend`.
    pub(crate) theme_variant: Option<String>,
    /// Whether this legend may fold into an earlier compatible
    /// legend at render time (see [`collapse_legends`]).
    /// `false` keeps it as its own block even when a compatible
    /// legend precedes it; it can still be folded *into*.
    pub(crate) merge: bool,
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
    /// `draw_linear_axis_at` helper).
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
    /// shared `draw_linear_axis_at` helper for visual consistency
    /// with cartesian / polar axes.
    pub binned: bool,
}

/// Configuration for a colorbar legend body.
///
/// Like [`LegendKeySpec`], a colorbar carries per-aesthetic
/// `bindings`. Each gradient stop resolves a [`ResolvedKey`] from
/// these and uses its `fill`, at its `fill_opacity`, as the stop
/// colour. By default the `fill` binding is the legend's
/// `domain_scale` — so the simplest colorbar
/// (`Legend::colorbar("scale_name")`) gradients over that scale's
/// colour output range. Layering an opacity scale on top just means
/// adding another binding:
///
/// ```ignore
/// Legend::colorbar("value_scale")
///     .scaled("fill_opacity", "value_opacity_scale")
/// ```
#[derive(Clone, Debug)]
pub struct ColorbarSpec {
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
            theme_variant: None,
            merge: true,
        }
    }
    /// Continuous colorbar legend: gradient bar sampled from
    /// `domain_scale`'s colour output range, tick labels from its
    /// `breaks()`. Configure via `thickness` / [`Self::samples`].
    pub fn colorbar(domain_scale: impl Into<String>) -> Self {
        Self {
            side: LegendSide::Right,
            title: None,
            domain_scale: domain_scale.into(),
            body: LegendBody::Colorbar(ColorbarSpec::default()),
            open_lower: false,
            open_upper: false,
            bin_spacing: BinSpacing::Proportional,
            theme_variant: None,
            merge: true,
        }
    }
    /// Opt into a named theme variant. The renderer looks up
    /// `theme.legend_variants.get(name)` and uses that
    /// `LegendTheme` to style this legend (falling back to
    /// `theme.legend` if the variant isn't registered).
    pub fn theme_variant(mut self, name: impl Into<String>) -> Self {
        self.theme_variant = Some(name.into());
        self
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
    /// Set the colorbar's gradient sample count. No-op on stack legends.
    pub fn samples(mut self, n: usize) -> Self {
        if let LegendBody::Colorbar(spec) = &mut self.body {
            spec.samples = n.max(2);
        }
        self
    }

    /// Bind a colorbar aesthetic to a scale (e.g. `fill_opacity` keyed
    /// off its own scale). The fill is implicitly bound to the
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
    /// `true` when the two legends describe one and the same block, so
    /// [`collapse_legends`] should fold `other` into this one. Requires
    /// matching side, title, theme variant, bin flags / spacing, bodies
    /// of the same kind, and **equivalent domain scales**.
    ///
    /// Domain equivalence is not name equality: two differently named
    /// scales that resolve to the same breaks and labels (see
    /// [`Scale::legend_equivalent_to`](crate::plot::scale::Scale::legend_equivalent_to))
    /// count as the same domain, so a colour scale and a shape scale
    /// configured over one shared set of categories collapse into a
    /// single legend. A name that isn't in `registry` only matches the
    /// identical name — an unresolvable scale has no breaks to compare.
    ///
    /// The bodies carry their own conditions. Two stacks agree when
    /// their `binned` flags match — the keys themselves then stack up.
    /// Two colorbars agree only when they *draw the same bar*: same
    /// step mode, same sample count where it's honoured, and every
    /// aesthetic resolving to the same value along the bar (see
    /// [`Scale::visual_equivalent_to`](crate::plot::scale::Scale::visual_equivalent_to)),
    /// since folding a colorbar keeps one bar and drops the other.
    pub fn is_compatible_with(
        &self,
        other: &Legend,
        registry: &ScaleRegistry,
        locale: &crate::scales::Locale,
    ) -> bool {
        if self.side != other.side
            || self.title != other.title
            || self.theme_variant != other.theme_variant
            || self.open_lower != other.open_lower
            || self.open_upper != other.open_upper
            || self.bin_spacing != other.bin_spacing
        {
            return false;
        }
        let bodies_agree = match (&self.body, &other.body) {
            (LegendBody::Stack(a), LegendBody::Stack(b)) => a.binned == b.binned,
            (LegendBody::Colorbar(a), LegendBody::Colorbar(b)) => {
                a.stepped == b.stepped
                    // `samples` only drives the smooth gradient; a
                    // stepped bar takes its stop count from the breaks.
                    && (a.stepped || a.samples == b.samples)
                    && colorbar_gradients_agree(
                        (a, &self.domain_scale),
                        (b, &other.domain_scale),
                        registry,
                        locale,
                    )
            }
            _ => false,
        };
        if !bodies_agree {
            return false;
        }
        self.domain_scale == other.domain_scale
            || match (
                registry.get(&self.domain_scale),
                registry.get(&other.domain_scale),
            ) {
                (Some(a), Some(b)) => a.legend_equivalent_to(b, locale),
                _ => false,
            }
    }
}

/// Where one colorbar aesthetic gets its value from, with the
/// implicit `fill` fallback already applied.
enum StopSource<'a> {
    /// Mapped through this registry entry at each stop's domain value.
    Scale(&'a str),
    /// Constant along the bar.
    Fixed(&'a Value),
}

/// The aesthetics a colorbar resolves per gradient stop, sorted by
/// name. Mirrors the draw path: an unbound `fill` (and no `color`
/// standing in for it) falls back to the legend's domain scale.
fn stop_sources<'a>(
    spec: &'a ColorbarSpec,
    domain_scale: &'a str,
) -> Vec<(&'a str, StopSource<'a>)> {
    let mut out: Vec<(&str, StopSource)> = spec
        .bindings
        .iter()
        .map(|(aesthetic, source)| {
            let src = match source {
                AestheticSource::Scaled(name) => StopSource::Scale(name.as_str()),
                AestheticSource::Fixed(v) => StopSource::Fixed(v),
            };
            (aesthetic.as_str(), src)
        })
        .collect();
    if !spec.bindings.contains_key("fill") && !spec.bindings.contains_key("color") {
        out.push(("fill", StopSource::Scale(domain_scale)));
    }
    out.sort_by_key(|(aesthetic, _)| *aesthetic);
    out
}

/// `true` when two colorbar bodies paint the same gradient — the same
/// aesthetics, each resolving identically along the bar.
fn colorbar_gradients_agree(
    a: (&ColorbarSpec, &str),
    b: (&ColorbarSpec, &str),
    registry: &ScaleRegistry,
    locale: &crate::scales::Locale,
) -> bool {
    let (mine, theirs) = (stop_sources(a.0, a.1), stop_sources(b.0, b.1));
    mine.len() == theirs.len()
        && mine
            .iter()
            .zip(theirs.iter())
            .all(|((a_aes, a_src), (b_aes, b_src))| {
                a_aes == b_aes && stop_sources_agree(a_src, b_src, registry, locale)
            })
}

/// `true` when two stop sources yield the same value at every domain
/// value. Scales match by name, or by mapping inputs to the same
/// outputs over the same domain; an unresolvable name only matches the
/// identical name.
fn stop_sources_agree(
    a: &StopSource,
    b: &StopSource,
    registry: &ScaleRegistry,
    locale: &crate::scales::Locale,
) -> bool {
    match (a, b) {
        (StopSource::Scale(x), StopSource::Scale(y)) => {
            x == y
                || match (registry.get(x), registry.get(y)) {
                    (Some(sx), Some(sy)) => sx.visual_equivalent_to(sy, locale),
                    _ => false,
                }
        }
        (StopSource::Fixed(x), StopSource::Fixed(y)) => x.key_eq(y),
        _ => false,
    }
}

/// Fold `legends` into the blocks actually rendered: every legend
/// whose `merge` flag is set and that is
/// [compatible](Legend::is_compatible_with) with an earlier survivor
/// disappears into it — a stack hands over its keys, and a colorbar
/// that would draw the same bar simply drops out.
///
/// Collapse runs per render rather than at attach time, so it reflects
/// whatever the scales hold now — registering or retraining a scale
/// after the legends were attached still merges them.
pub fn collapse_legends(
    legends: &[Legend],
    registry: &ScaleRegistry,
    locale: &crate::scales::Locale,
) -> Vec<Legend> {
    let mut out: Vec<Legend> = Vec::with_capacity(legends.len());
    for legend in legends {
        let target = legend.merge.then(|| {
            out.iter()
                .position(|kept| kept.is_compatible_with(legend, registry, locale))
        });
        match target.flatten() {
            Some(idx) => {
                // `is_compatible_with` established the bodies are of
                // the same kind. Stacks accumulate keys; compatible
                // colorbars are already identical bars, so the survivor
                // needs nothing from the incoming one.
                if let (LegendBody::Stack(kept), LegendBody::Stack(incoming)) =
                    (&mut out[idx].body, &legend.body)
                {
                    kept.keys.extend(incoming.keys.iter().cloned());
                }
            }
            None => out.push(legend.clone()),
        }
    }
    out
}

/// Per-row resolved aesthetic bundle. Each [`LegendKey`] reads the
/// fields it cares about; the rest are ignored.
#[derive(Clone, Debug, Default)]
pub struct ResolvedKey {
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub size_pt: Option<f64>,
    pub shape: Option<Arc<str>>,
    /// Opacity of the key's fill, overriding the fill colour's own
    /// alpha as the geom `"fill_opacity"` channel does.
    pub fill_opacity: Option<f64>,
    /// Opacity of the key's stroke, overriding the stroke colour's own
    /// alpha as the geom `"stroke_opacity"` channel does.
    pub stroke_opacity: Option<f64>,
    pub linewidth_pt: Option<f64>,
    pub linetype: Option<Arc<[LinetypeStep]>>,
    /// Dash-pattern phase shift in pt, as the geom `"dash_offset"`
    /// channel applies it.
    pub dash_offset_pt: Option<f64>,
    /// Endpoint cap for the stroked keys, from the same `"butt"` /
    /// `"round"` / `"square"` vocabulary the geom `"cap"` channel uses.
    pub cap: Option<Cap>,
    /// Segment join for the stroked keys, from the same `"miter"` /
    /// `"round"` / `"bevel"` vocabulary the geom `"join"` channel uses.
    pub join: Option<Join>,
    /// Corner radius in pt for [`LegendKey::Rect`], as the geom
    /// `"corner_radius"` channel applies it.
    pub corner_radius_pt: Option<f64>,
    /// Marker rotation in radians for [`LegendKey::Point`], positive
    /// counter-clockwise — the geom `"angle"` channel's convention.
    pub angle: Option<f64>,
    /// Marker stamped at the start of a [`LegendKey::Line`], from the
    /// geom's `"start_marker"` family of channels.
    pub start_marker: EndpointMarkerKey,
    /// Counterpart of [`Self::start_marker`] for the line's far end.
    pub end_marker: EndpointMarkerKey,
    /// Glyph string a [`LegendKey::Text`] draws, as the geom's
    /// `"text"` channel supplies it.
    pub text: Option<Arc<str>>,
    /// CSS font weight (100..=900) for [`LegendKey::Text`], from the
    /// geom's `"weight"` channel.
    pub weight: Option<u16>,
    /// Italic / oblique face for [`LegendKey::Text`], from the geom's
    /// `"italic"` channel.
    pub italic: Option<bool>,
    /// Font family name for [`LegendKey::Text`], from the geom's
    /// `"family"` channel.
    pub family: Option<Arc<str>>,
    /// Extra advance between glyphs in pt, from the geom's
    /// `"tracking"` channel.
    pub tracking: Option<f64>,
    /// Underline decoration for [`LegendKey::Text`], from the geom's
    /// `"underline"` channel.
    pub underline: Option<bool>,
    /// Strikethrough decoration for [`LegendKey::Text`], from the
    /// geom's `"strikethrough"` channel.
    pub strikethrough: Option<bool>,
    /// Per-glyph outline colour for [`LegendKey::Text`], from the
    /// geom's `"text_stroke"` channel.
    pub text_stroke: Option<Color>,
    /// Per-glyph outline width in pt, from the geom's
    /// `"text_linewidth"` channel.
    pub text_linewidth_pt: Option<f64>,
}

/// One end of a [`LegendKey::Line`]'s marker pair. Mirrors the geom's
/// per-endpoint marker channels; unset fields fall back the way the
/// geoms' do — size to `3 × linewidth`, fill to the stroke colour.
#[derive(Clone, Debug, Default)]
pub struct EndpointMarkerKey {
    /// Registered shape name. `None`, or a name the registry doesn't
    /// know, draws no marker.
    pub shape: Option<Arc<str>>,
    /// Marker size in pt.
    pub size_pt: Option<f64>,
    /// Marker interior colour.
    pub fill: Option<Color>,
    /// Flip the outward direction, mirroring the shape across the
    /// line's end.
    pub invert: Option<bool>,
}

impl ResolvedKey {
    /// Apply an aesthetic value to the matching field. Unknown
    /// aesthetic names are silently ignored.
    pub(super) fn apply(&mut self, aesthetic: &str, value: Value) {
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
            "fill_opacity" => {
                if let Some(n) = value.as_number() {
                    self.fill_opacity = Some(n);
                }
            }
            "stroke_opacity" => {
                if let Some(n) = value.as_number() {
                    self.stroke_opacity = Some(n);
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
            "dash_offset" => {
                if let Some(n) = value.as_number() {
                    self.dash_offset_pt = Some(n);
                }
            }
            "corner_radius" => {
                if let Some(n) = value.as_number() {
                    self.corner_radius_pt = Some(n);
                }
            }
            "angle" => {
                if let Some(n) = value.as_number() {
                    self.angle = Some(n);
                }
            }
            "cap" => {
                if let Some(c) = value.as_str().and_then(cap_from_str) {
                    self.cap = Some(c);
                }
            }
            "join" => {
                if let Some(j) = value.as_str().and_then(join_from_str) {
                    self.join = Some(j);
                }
            }
            "text" => {
                if let Some(s) = value.as_str() {
                    self.text = Some(Arc::from(s));
                }
            }
            "weight" => {
                if let Some(n) = value.as_number() {
                    self.weight = Some((n.round() as i64).clamp(1, 1000) as u16);
                }
            }
            // `TextGeom` takes its italic channel as either a boolean
            // or a face name, so the key reads both vocabularies too.
            "italic" => match value {
                Value::Bool(b) => self.italic = Some(b),
                Value::String(ref s) => self.italic = Some(matches!(&**s, "italic" | "oblique")),
                _ => {}
            },
            "family" => {
                if let Some(s) = value.as_str() {
                    self.family = Some(Arc::from(s));
                }
            }
            "tracking" => {
                if let Some(n) = value.as_number() {
                    self.tracking = Some(n);
                }
            }
            "underline" => {
                if let Some(b) = value.as_bool() {
                    self.underline = Some(b);
                }
            }
            "strikethrough" => {
                if let Some(b) = value.as_bool() {
                    self.strikethrough = Some(b);
                }
            }
            "text_stroke" => {
                if let Some(c) = value.as_color() {
                    self.text_stroke = Some(c);
                }
            }
            "text_linewidth" => {
                if let Some(n) = value.as_number() {
                    self.text_linewidth_pt = Some(n);
                }
            }
            "start_marker" | "start_marker_size" | "start_marker_fill" | "start_marker_invert" => {
                self.start_marker.apply(&aesthetic["start_".len()..], value)
            }
            "end_marker" | "end_marker_size" | "end_marker_fill" | "end_marker_invert" => {
                self.end_marker.apply(&aesthetic["end_".len()..], value)
            }
            _ => {}
        }
    }
}

impl EndpointMarkerKey {
    /// Apply one of the `marker` / `marker_size` / `marker_fill` /
    /// `marker_invert` suffixes of an endpoint's channel name.
    fn apply(&mut self, suffix: &str, value: Value) {
        match suffix {
            "marker" => {
                if let Some(s) = value.as_str() {
                    self.shape = Some(Arc::from(s));
                }
            }
            "marker_size" => {
                if let Some(n) = value.as_number() {
                    self.size_pt = Some(n);
                }
            }
            "marker_fill" => {
                if let Some(c) = value.as_color() {
                    self.fill = Some(c);
                }
            }
            "marker_invert" => {
                if let Value::Bool(b) = value {
                    self.invert = Some(b);
                }
            }
            _ => {}
        }
    }
}

// ─── Resolution ─────────────────────────────────────────────────────────────

/// Resolve every binding on `spec` at `row`, yielding the aesthetic
/// bundle the key renderers read. A binding naming a scale the
/// registry does not hold is skipped, leaving that aesthetic at its
/// default.
pub(super) fn resolve_key(
    spec: &LegendKeySpec,
    registry: &ScaleRegistry,
    row: &Value,
) -> ResolvedKey {
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

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb;
    use crate::plot::scale;
    use crate::scales::Locale;

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
    fn geom_style_aesthetics_reach_the_resolved_key() {
        // Every style channel a key can mirror from its geom, pinned at
        // once: names the spec accepts but `apply` drops would silently
        // render as the default.
        let spec = LegendKeySpec::point()
            .fixed("fill_opacity", 0.25_f64)
            .fixed("stroke_opacity", 0.75_f64)
            .fixed("dash_offset", 3.0_f64)
            .fixed("corner_radius", 4.0_f64)
            .fixed("angle", 1.5_f64);
        let resolved = resolve_key(&spec, &build_registry(), &Value::String(Arc::from("A")));
        assert_eq!(resolved.fill_opacity, Some(0.25));
        assert_eq!(resolved.stroke_opacity, Some(0.75));
        assert_eq!(resolved.dash_offset_pt, Some(3.0));
        assert_eq!(resolved.corner_radius_pt, Some(4.0));
        assert_eq!(resolved.angle, Some(1.5));
        // The specific opacity channels win over the general one.
        assert_eq!(resolved.fill_opacity, Some(0.25));
        assert_eq!(resolved.stroke_opacity, Some(0.75));
    }

    #[test]
    fn text_aesthetics_reach_the_resolved_key() {
        let spec = LegendKeySpec::text()
            .fixed("text", Value::String(Arc::from("Aa")))
            .fixed("weight", 700.0_f64)
            .fixed("family", Value::String(Arc::from("Helvetica")))
            .fixed("tracking", 1.5_f64)
            .fixed("underline", Value::Bool(true))
            .fixed("strikethrough", Value::Bool(true))
            .fixed("text_stroke", Value::Color(rgb(1.0, 0.0, 0.0)))
            .fixed("text_linewidth", 2.0_f64);
        let resolved = resolve_key(&spec, &build_registry(), &Value::String(Arc::from("A")));
        assert_eq!(resolved.text.as_deref(), Some("Aa"));
        assert_eq!(resolved.weight, Some(700));
        assert_eq!(resolved.family.as_deref(), Some("Helvetica"));
        assert_eq!(resolved.tracking, Some(1.5));
        assert_eq!(resolved.underline, Some(true));
        assert_eq!(resolved.strikethrough, Some(true));
        assert_eq!(resolved.text_stroke, Some(rgb(1.0, 0.0, 0.0)));
        assert_eq!(resolved.text_linewidth_pt, Some(2.0));
    }

    #[test]
    fn the_italic_aesthetic_reads_both_vocabularies() {
        // `TextGeom` accepts a boolean or a face name on `"italic"`, so
        // a key bound to either kind of scale output has to agree.
        let reg = build_registry();
        let row = Value::String(Arc::from("A"));
        for (bound, expected) in [
            (Value::Bool(true), Some(true)),
            (Value::String(Arc::from("italic")), Some(true)),
            (Value::String(Arc::from("oblique")), Some(true)),
            (Value::String(Arc::from("normal")), Some(false)),
        ] {
            let spec = LegendKeySpec::text().fixed("italic", bound.clone());
            assert_eq!(
                resolve_key(&spec, &reg, &row).italic,
                expected,
                "italic bound to {bound:?}"
            );
        }
    }

    #[test]
    fn endpoint_marker_aesthetics_reach_the_resolved_key() {
        let spec = LegendKeySpec::line()
            .fixed("start_marker", Value::String(Arc::from("arrow-open")))
            .fixed("start_marker_size", 6.0_f64)
            .fixed("start_marker_fill", Value::Color(rgb(1.0, 0.0, 0.0)))
            .fixed("start_marker_invert", Value::Bool(true))
            .fixed("end_marker", Value::String(Arc::from("arrow-closed")))
            .fixed("end_marker_size", 9.0_f64);
        let resolved = resolve_key(&spec, &build_registry(), &Value::String(Arc::from("A")));
        assert_eq!(resolved.start_marker.shape.as_deref(), Some("arrow-open"));
        assert_eq!(resolved.start_marker.size_pt, Some(6.0));
        assert_eq!(resolved.start_marker.fill, Some(rgb(1.0, 0.0, 0.0)));
        assert_eq!(resolved.start_marker.invert, Some(true));
        assert_eq!(resolved.end_marker.shape.as_deref(), Some("arrow-closed"));
        assert_eq!(resolved.end_marker.size_pt, Some(9.0));
        // Each end resolves on its own — the start's fill and invert
        // must not leak across.
        assert_eq!(resolved.end_marker.fill, None);
        assert_eq!(resolved.end_marker.invert, None);
    }

    #[test]
    fn cap_and_join_aesthetics_reach_the_resolved_key() {
        let spec = LegendKeySpec::line()
            .fixed("cap", Value::String(Arc::from("square")))
            .fixed("join", Value::String(Arc::from("bevel")));
        let resolved = resolve_key(&spec, &build_registry(), &Value::String(Arc::from("A")));
        assert_eq!(resolved.cap, Some(Cap::Square));
        assert_eq!(resolved.join, Some(Join::Bevel));
    }
    #[test]
    fn legend_is_compatible_with_matching_triple() {
        let reg = build_registry();
        let loc = Locale::default();
        let a = Legend::new("x").side(LegendSide::Right).title("T");
        let b = Legend::new("x").side(LegendSide::Right).title("T");
        let c = Legend::new("x").side(LegendSide::Right).title("U");
        assert!(a.is_compatible_with(&b, &reg, &loc));
        assert!(!a.is_compatible_with(&c, &reg, &loc));
    }

    #[test]
    fn legends_over_equivalent_scales_are_compatible() {
        // `category_color` and `category_size` are separate registry
        // entries trained to the same three categories; only their
        // output ranges differ.
        let reg = build_registry();
        let loc = Locale::default();
        let color = Legend::new("category_color").title("Category");
        let size = Legend::new("category_size").title("Category");
        assert!(color.is_compatible_with(&size, &reg, &loc));
    }

    #[test]
    fn legends_over_differently_trained_scales_are_not_compatible() {
        let mut reg = build_registry();
        reg.insert(
            "other_cats",
            scale::discrete([Value::String(Arc::from("A")), Value::String(Arc::from("B"))])
                .range_numbers([4.0, 8.0]),
        );
        let loc = Locale::default();
        let three = Legend::new("category_color").title("Category");
        let two = Legend::new("other_cats").title("Category");
        assert!(!three.is_compatible_with(&two, &reg, &loc));
    }

    #[test]
    fn unresolvable_domain_scales_only_match_by_name() {
        let reg = build_registry();
        let loc = Locale::default();
        let a = Legend::new("missing").title("T");
        let same = Legend::new("missing").title("T");
        let other = Legend::new("also_missing").title("T");
        assert!(a.is_compatible_with(&same, &reg, &loc));
        assert!(!a.is_compatible_with(&other, &reg, &loc));
    }

    #[test]
    fn collapse_folds_keys_of_equivalent_scales() {
        let reg = build_registry();
        let loc = Locale::default();
        let legends = vec![
            Legend::new("category_color")
                .title("Category")
                .key(LegendKeySpec::rect().scaled("fill", "category_color")),
            Legend::new("category_size")
                .title("Category")
                .key(LegendKeySpec::point().scaled("size", "category_size")),
        ];
        let collapsed = collapse_legends(&legends, &reg, &loc);
        assert_eq!(collapsed.len(), 1);
        let LegendBody::Stack(stack) = &collapsed[0].body else {
            panic!("stack body");
        };
        assert_eq!(stack.keys.len(), 2);
        // The survivor keeps the first legend's domain scale; each key
        // still resolves through the scale it was bound to.
        assert_eq!(collapsed[0].domain_scale, "category_color");
    }

    #[test]
    fn collapse_keeps_non_merging_legends_apart() {
        let reg = build_registry();
        let loc = Locale::default();
        let mut second = Legend::new("category_size")
            .title("Category")
            .key(LegendKeySpec::point().scaled("size", "category_size"));
        second.merge = false;
        let legends = vec![
            Legend::new("category_color")
                .title("Category")
                .key(LegendKeySpec::rect().scaled("fill", "category_color")),
            second,
        ];
        assert_eq!(collapse_legends(&legends, &reg, &loc).len(), 2);
    }

    /// Two continuous colour scales over one domain: `ramp` and
    /// `same_ramp` share a palette, `other_ramp` doesn't, and
    /// `ramp_opacity` maps the same domain to numbers.
    fn colorbar_registry() -> ScaleRegistry {
        let mut reg = ScaleRegistry::new();
        let palette = [rgb(1.0, 1.0, 0.0), rgb(0.0, 0.0, 1.0)];
        reg.insert("ramp", scale::continuous(0.0..=100.0).range_colors(palette));
        reg.insert(
            "same_ramp",
            scale::continuous(0.0..=100.0).range_colors(palette),
        );
        reg.insert(
            "other_ramp",
            scale::continuous(0.0..=100.0).range_colors([rgb(0.0, 0.0, 0.0), rgb(1.0, 1.0, 1.0)]),
        );
        reg.insert(
            "ramp_opacity",
            scale::continuous(0.0..=100.0).range_numbers([0.1, 1.0]),
        );
        reg
    }

    #[test]
    fn collapse_folds_identical_colorbars() {
        let reg = colorbar_registry();
        let loc = Locale::default();
        let legends = vec![
            Legend::colorbar("ramp").title("Value"),
            Legend::colorbar("ramp").title("Value"),
        ];
        let collapsed = collapse_legends(&legends, &reg, &loc);
        assert_eq!(collapsed.len(), 1);
        assert!(matches!(collapsed[0].body, LegendBody::Colorbar(_)));
    }

    #[test]
    fn collapse_folds_colorbars_over_equivalent_scales() {
        // Separate registry entries, same domain and same palette — the
        // surviving bar stands in for both.
        let reg = colorbar_registry();
        let loc = Locale::default();
        let legends = vec![
            Legend::colorbar("ramp").title("Value"),
            Legend::colorbar("same_ramp").title("Value"),
        ];
        assert_eq!(collapse_legends(&legends, &reg, &loc).len(), 1);
    }

    #[test]
    fn collapse_keeps_colorbars_with_different_palettes_apart() {
        let reg = colorbar_registry();
        let loc = Locale::default();
        let legends = vec![
            Legend::colorbar("ramp").title("Value"),
            Legend::colorbar("other_ramp").title("Value"),
        ];
        assert_eq!(collapse_legends(&legends, &reg, &loc).len(), 2);
    }

    #[test]
    fn collapse_keeps_colorbars_with_different_bindings_apart() {
        let reg = colorbar_registry();
        let loc = Locale::default();
        let plain = Legend::colorbar("ramp").title("Value");
        let faded = Legend::colorbar("ramp")
            .title("Value")
            .scaled("fill_opacity", "ramp_opacity");
        assert!(!plain.is_compatible_with(&faded, &reg, &loc));
        let also_faded = Legend::colorbar("ramp")
            .title("Value")
            .scaled("fill_opacity", "ramp_opacity");
        assert!(faded.is_compatible_with(&also_faded, &reg, &loc));
    }

    #[test]
    fn collapse_keeps_stepped_and_smooth_colorbars_apart() {
        let reg = colorbar_registry();
        let loc = Locale::default();
        let smooth = Legend::colorbar("ramp").title("Value");
        let stepped = Legend::colorbar("ramp").title("Value").binned();
        assert!(!smooth.is_compatible_with(&stepped, &reg, &loc));
        // `samples` is ignored on a stepped bar, so it can't keep two
        // otherwise-identical stepped colorbars apart.
        assert!(stepped.is_compatible_with(
            &Legend::colorbar("ramp").title("Value").binned().samples(8),
            &reg,
            &loc
        ));
        assert!(!smooth.is_compatible_with(
            &Legend::colorbar("ramp").title("Value").samples(8),
            &reg,
            &loc
        ));
    }

    #[test]
    fn collapse_keeps_a_colorbar_and_a_stack_apart() {
        let reg = build_registry();
        let loc = Locale::default();
        let bar = Legend::colorbar("category_color").title("Category");
        let stack = Legend::new("category_color")
            .title("Category")
            .key(LegendKeySpec::rect().scaled("fill", "category_color"));
        assert!(!bar.is_compatible_with(&stack, &reg, &loc));
    }

    #[test]
    fn collapse_separates_legends_with_different_theme_variants() {
        let reg = build_registry();
        let loc = Locale::default();
        let legends = vec![
            Legend::new("category_color")
                .title("Category")
                .theme_variant("hero")
                .key(LegendKeySpec::rect().scaled("fill", "category_color")),
            Legend::new("category_color")
                .title("Category")
                .key(LegendKeySpec::point().scaled("size", "category_size")),
        ];
        assert_eq!(collapse_legends(&legends, &reg, &loc).len(), 2);
    }
    #[test]
    fn legends_with_different_open_flags_are_not_compatible() {
        let reg = build_registry();
        let loc = Locale::default();
        let a = Legend::new("x").open_lower();
        let b = Legend::new("x");
        assert!(!a.is_compatible_with(&b, &reg, &loc));
        let c = Legend::new("x").open_upper();
        assert!(!a.is_compatible_with(&c, &reg, &loc));
    }

    #[test]
    fn legends_with_different_bin_spacing_are_not_compatible() {
        let reg = build_registry();
        let a = Legend::new("x").equal_bins();
        let b = Legend::new("x");
        assert!(!a.is_compatible_with(&b, &reg, &Locale::default()));
    }

    #[test]
    fn bin_spacing_default_is_proportional() {
        let legend = Legend::new("x");
        assert_eq!(legend.bin_spacing, BinSpacing::Proportional);
    }
}
