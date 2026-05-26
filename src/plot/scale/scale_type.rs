//! Scale-type behaviour — Continuous / Discrete / Ordinal / Binned /
//! Identity. Each variant determines how a [`Scale`] maps inputs, what
//! ticks/breaks it produces, and (for discrete variants) the band width
//! used by future in-band geoms (violins, dodges, …).
//!
//! All variants are stateless; their configuration (domain, range,
//! transform) lives on the [`Scale`] struct itself.

use std::fmt::Debug;
use std::sync::Arc;

use super::breaks::extended_breaks;
use super::input::InputRange;
use super::output::OutputRange;
use super::transform::TransformKind;
use super::Scale;
use crate::color::Color;
use crate::plot::value::Value;

/// Discriminator for the scale-type family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScaleTypeKind {
    /// Linear mapping over a numeric domain. Output range can be unset
    /// (returns normalised `[0, 1]` fraction), `Numbers` (piecewise-linear
    /// interpolation across stops), or `Colors` (piecewise-linear
    /// componentwise interpolation).
    Continuous,
    /// One-to-one lookup over an unordered set of values. Each domain
    /// entry maps to exactly the output entry at the same index. Use this
    /// when each category should pick a distinct visual (per-category
    /// colors, sizes, strings, …).
    Discrete,
    /// **Ordered** domain with **continuous** output interpretation: each
    /// domain entry's position `idx` is converted to a normalised
    /// `t = idx / (n - 1)`, then interpolated through the output range
    /// (same engine as Continuous). When the domain and output range
    /// have the same length the result coincides with Discrete's
    /// one-to-one lookup; when they differ, intermediate domain entries
    /// fall on interpolated points along the gradient.
    Ordinal,
    /// Continuous domain pre-binned into discrete output bins by an
    /// explicit list of break points.
    Binned,
    /// Pass-through. Input is returned untouched.
    Identity,
}

impl ScaleTypeKind {
    /// Stable name for diagnostics / serialisation.
    pub fn name(self) -> &'static str {
        match self {
            ScaleTypeKind::Continuous => "continuous",
            ScaleTypeKind::Discrete => "discrete",
            ScaleTypeKind::Ordinal => "ordinal",
            ScaleTypeKind::Binned => "binned",
            ScaleTypeKind::Identity => "identity",
        }
    }
}

/// Behaviour of a scale type. The trait methods take `&Scale` so they can
/// read the scale's domain / range / transform without owning copies.
pub trait ScaleTypeTrait: Debug + Send + Sync {
    /// Discriminator.
    fn kind(&self) -> ScaleTypeKind;

    /// Stable name. Default delegates to [`ScaleTypeKind::name`].
    fn name(&self) -> &'static str {
        self.kind().name()
    }

    /// Which [`TransformKind`]s are compatible with this scale type.
    /// Default = `[Identity]` (the only transform v1 ships).
    fn allowed_transforms(&self) -> &'static [TransformKind] {
        const ID: &[TransformKind] = &[TransformKind::Identity];
        ID
    }

    /// Map an input to its scaled output.
    fn map(&self, input: &Value, scale: &Scale) -> Value;

    /// Tick / category positions in **input** space. Position scales feed
    /// these through `map` to land them on the panel.
    fn breaks(&self, scale: &Scale, n: usize) -> Vec<Value>;

    /// Band width as a fraction of the panel (in `[0, 1]`), averaged
    /// across all bands. Continuous scales return `0.0`; discrete /
    /// ordinal scales return `1.0 / n_bands`. Binned scales with
    /// unequal-width bins return their *average* (`1.0 / n_bins`).
    ///
    /// For per-band variation use [`Self::band_width_at`].
    fn band_width(&self, _scale: &Scale) -> f64 {
        0.0
    }

    /// Width (as panel fraction) of the band containing `input`. For
    /// scales with uniform bands this matches [`Self::band_width`]
    /// regardless of `input`. For [`Binned`] scales with non-uniform
    /// bin widths this returns the width of the specific bin `input`
    /// falls into. Used by [`Scale::map_with_offset`] to apply
    /// `*_band` channel offsets correctly across non-uniform scales.
    ///
    /// Default delegates to [`Self::band_width`] (uniform).
    fn band_width_at(&self, scale: &Scale, _input: &Value) -> f64 {
        self.band_width(scale)
    }
}

/// Type-erased scale type. Wraps an `Arc<dyn ScaleTypeTrait>` so it's
/// cheap to clone across [`Scale`] copies.
#[derive(Clone)]
pub struct ScaleType(Arc<dyn ScaleTypeTrait>);

impl ScaleType {
    pub fn continuous() -> Self {
        ScaleType(Arc::new(Continuous))
    }

    pub fn discrete() -> Self {
        ScaleType(Arc::new(Discrete))
    }

    pub fn ordinal() -> Self {
        ScaleType(Arc::new(Ordinal))
    }

    pub fn binned() -> Self {
        ScaleType(Arc::new(Binned))
    }

    pub fn identity() -> Self {
        ScaleType(Arc::new(Identity))
    }

    pub fn kind(&self) -> ScaleTypeKind {
        self.0.kind()
    }

    pub fn name(&self) -> &'static str {
        self.0.name()
    }

    pub fn allowed_transforms(&self) -> &'static [TransformKind] {
        self.0.allowed_transforms()
    }

    pub(crate) fn map(&self, input: &Value, scale: &Scale) -> Value {
        self.0.map(input, scale)
    }

    pub(crate) fn breaks(&self, scale: &Scale, n: usize) -> Vec<Value> {
        self.0.breaks(scale, n)
    }

    pub(crate) fn band_width(&self, scale: &Scale) -> f64 {
        self.0.band_width(scale)
    }

    pub(crate) fn band_width_at(&self, scale: &Scale, input: &Value) -> f64 {
        self.0.band_width_at(scale, input)
    }
}

impl Debug for ScaleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ScaleType").field(&self.kind()).finish()
    }
}

impl Default for ScaleType {
    fn default() -> Self {
        ScaleType::continuous()
    }
}

// ─── Continuous ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct Continuous;

impl ScaleTypeTrait for Continuous {
    fn kind(&self) -> ScaleTypeKind {
        ScaleTypeKind::Continuous
    }

    fn map(&self, input: &Value, scale: &Scale) -> Value {
        let v = match input.as_number() {
            Some(n) => n,
            None => return Value::Null,
        };
        let (d_min, d_max) = match scale.input_range() {
            Some(InputRange::Continuous { min, max }) => (*min, *max),
            _ => return Value::Null,
        };
        // Apply transform (only Identity is wired in v1; future-proof).
        let v_t = scale.transform().transform(v);
        let dmin_t = scale.transform().transform(d_min);
        let dmax_t = scale.transform().transform(d_max);
        // Linear interpolate. If the transformed span is zero we
        // degenerate to t = 0.0.
        let t = if dmax_t == dmin_t {
            0.0
        } else {
            (v_t - dmin_t) / (dmax_t - dmin_t)
        };
        interpolate_range(t, scale.output_range())
    }

    fn breaks(&self, scale: &Scale, n: usize) -> Vec<Value> {
        match scale.input_range() {
            Some(InputRange::Continuous { min, max }) => extended_breaks(*min, *max, n)
                .into_iter()
                .map(Value::Number)
                .collect(),
            _ => Vec::new(),
        }
    }
}

// ─── Discrete (one-to-one) ───────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct Discrete;

impl ScaleTypeTrait for Discrete {
    fn kind(&self) -> ScaleTypeKind {
        ScaleTypeKind::Discrete
    }

    fn map(&self, input: &Value, scale: &Scale) -> Value {
        let domain = match scale.input_range() {
            Some(InputRange::Discrete(d)) => d,
            _ => return Value::Null,
        };
        let n = domain.len();
        let idx = match domain.iter().position(|d| d.key_eq(input)) {
            Some(i) => i,
            None => return Value::Null,
        };
        match scale.output_range() {
            // No output range → band centre (positional rendering on a
            // discrete axis). Bands evenly distributed; centre of band i
            // is (i + 0.5) / n.
            None => {
                if n == 0 {
                    Value::Null
                } else {
                    Value::Number((idx as f64 + 0.5) / n as f64)
                }
            }
            Some(OutputRange::Numbers(vs)) => vs
                .get(idx)
                .copied()
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Some(OutputRange::Colors(vs)) => vs
                .get(idx)
                .copied()
                .map(Value::Color)
                .unwrap_or(Value::Null),
            Some(OutputRange::Strings(vs)) => vs
                .get(idx)
                .cloned()
                .map(Value::String)
                .unwrap_or(Value::Null),
        }
    }

    fn breaks(&self, scale: &Scale, _n: usize) -> Vec<Value> {
        discrete_breaks(scale)
    }

    fn band_width(&self, scale: &Scale) -> f64 {
        discrete_band_width(scale)
    }
}

// ─── Ordinal (interpolated across a continuous output range) ─────────────────

#[derive(Debug)]
pub(crate) struct Ordinal;

impl ScaleTypeTrait for Ordinal {
    fn kind(&self) -> ScaleTypeKind {
        ScaleTypeKind::Ordinal
    }

    fn map(&self, input: &Value, scale: &Scale) -> Value {
        let domain = match scale.input_range() {
            Some(InputRange::Discrete(d)) => d,
            _ => return Value::Null,
        };
        let n = domain.len();
        let idx = match domain.iter().position(|d| d.key_eq(input)) {
            Some(i) => i,
            None => return Value::Null,
        };
        if n == 0 {
            return Value::Null;
        }
        match scale.output_range() {
            // No output range → band centre (used when an ordinal scale
            // drives a position axis). Same convention as Discrete.
            None => Value::Number((idx as f64 + 0.5) / n as f64),
            // With an output range → interpolate at t = idx / (n - 1).
            // For n == 1 the fraction collapses to 0. When domain and
            // output range happen to have the same length and the stops
            // are evenly spaced, this matches Discrete's one-to-one
            // behaviour.
            Some(range) => {
                let t = if n > 1 {
                    idx as f64 / (n - 1) as f64
                } else {
                    0.0
                };
                interpolate_range(t, Some(range))
            }
        }
    }

    fn breaks(&self, scale: &Scale, _n: usize) -> Vec<Value> {
        discrete_breaks(scale)
    }

    fn band_width(&self, scale: &Scale) -> f64 {
        discrete_band_width(scale)
    }
}

fn discrete_breaks(scale: &Scale) -> Vec<Value> {
    match scale.input_range() {
        Some(InputRange::Discrete(d)) => d.clone(),
        _ => Vec::new(),
    }
}

fn discrete_band_width(scale: &Scale) -> f64 {
    match scale.input_range() {
        Some(InputRange::Discrete(d)) if !d.is_empty() => 1.0 / d.len() as f64,
        _ => 0.0,
    }
}

// ─── Interpolation helpers (shared by Continuous + Ordinal) ──────────────────

/// Interpolate `t` (typically in `[0, 1]`, but unclamped — extrapolation
/// is allowed; the user is responsible for domain conditioning) through
/// an output range.
///
/// - `None` → `Value::Number(t)` (raw fraction; used by position channels
///   with no explicit output range).
/// - `Numbers(vs)` → piecewise-linear interpolation across `vs.len() - 1`
///   segments. Empty vec returns `Null`; single-stop returns that stop.
/// - `Colors(vs)` → piecewise-linear componentwise interpolation in sRGB
///   space. Not perceptually uniform — a documented v1 limitation.
/// - `Strings(_)` → `Null` (strings can't be interpolated).
fn interpolate_range(t: f64, range: Option<&OutputRange>) -> Value {
    match range {
        None => Value::Number(t),
        Some(OutputRange::Numbers(vs)) => match vs.len() {
            0 => Value::Null,
            1 => Value::Number(vs[0]),
            n => {
                let (lo, frac) = pick_segment(t, n);
                Value::Number(lerp_f64(vs[lo], vs[lo + 1], frac))
            }
        },
        Some(OutputRange::Colors(vs)) => match vs.len() {
            0 => Value::Null,
            1 => Value::Color(vs[0]),
            n => {
                let (lo, frac) = pick_segment(t, n);
                Value::Color(lerp_color(vs[lo], vs[lo + 1], frac as f32))
            }
        },
        // Strings have no interpolation semantics — use Discrete for
        // string-output mappings.
        Some(OutputRange::Strings(_)) => Value::Null,
    }
}

/// Pick the segment `lo` such that the result of an interpolation is
/// `lerp(stops[lo], stops[lo + 1], frac)`. Out-of-range `t` clamps to
/// the first / last segment so the lerp extrapolates linearly.
fn pick_segment(t: f64, n: usize) -> (usize, f64) {
    debug_assert!(n >= 2, "pick_segment requires n >= 2");
    let segments = (n - 1) as f64;
    let scaled = t * segments;
    let raw_lo = scaled.floor();
    let lo = (raw_lo as isize).clamp(0, n as isize - 2) as usize;
    let frac = scaled - lo as f64;
    (lo, frac)
}

fn lerp_f64(a: f64, b: f64, t: f64) -> f64 {
    a + t * (b - a)
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let [ar, ag, ab, aa] = a.components;
    let [br, bg, bb, ba] = b.components;
    Color::new([
        ar + t * (br - ar),
        ag + t * (bg - ag),
        ab + t * (bb - ab),
        aa + t * (ba - aa),
    ])
}

// ─── Binned ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct Binned;

impl ScaleTypeTrait for Binned {
    fn kind(&self) -> ScaleTypeKind {
        ScaleTypeKind::Binned
    }

    fn map(&self, input: &Value, scale: &Scale) -> Value {
        let v = match input.as_number() {
            Some(n) => n,
            None => return Value::Null,
        };
        // Binned needs both a continuous domain (the overall range) and
        // a Numbers output range (the bin edges defining the bins).
        let (d_min, d_max) = match scale.input_range() {
            Some(InputRange::Continuous { min, max }) => (*min, *max),
            _ => return Value::Null,
        };
        let edges = match scale.output_range() {
            Some(OutputRange::Numbers(vs)) if vs.len() >= 2 => vs,
            _ => return Value::Null,
        };
        if !v.is_finite() || v < d_min || v > d_max {
            return Value::Null;
        }
        // Pick the bin whose edges bracket `v`. Output is the bin's
        // domain-space *centre* projected proportionally onto `[0, 1]`.
        // For uneven-width bins this naturally produces wider panel
        // slots for wider bins — matching histogram conventions and
        // letting `band_width_at` describe each bin individually.
        let span = d_max - d_min;
        if span <= 0.0 {
            return Value::Number(0.0);
        }
        let bin = find_bin(v, edges);
        let centre = (edges[bin] + edges[bin + 1]) * 0.5;
        Value::Number((centre - d_min) / span)
    }

    fn breaks(&self, scale: &Scale, _n: usize) -> Vec<Value> {
        match scale.output_range() {
            Some(OutputRange::Numbers(vs)) => vs.iter().copied().map(Value::Number).collect(),
            _ => Vec::new(),
        }
    }

    fn band_width(&self, scale: &Scale) -> f64 {
        match scale.output_range() {
            Some(OutputRange::Numbers(vs)) if vs.len() >= 2 => 1.0 / (vs.len() - 1) as f64,
            _ => 0.0,
        }
    }

    fn band_width_at(&self, scale: &Scale, input: &Value) -> f64 {
        // Per-bin width — the proportional panel slot occupied by the
        // bin containing `input`.
        let v = match input.as_number() {
            Some(n) => n,
            None => return 0.0,
        };
        let (d_min, d_max) = match scale.input_range() {
            Some(InputRange::Continuous { min, max }) => (*min, *max),
            _ => return 0.0,
        };
        let edges = match scale.output_range() {
            Some(OutputRange::Numbers(vs)) if vs.len() >= 2 => vs,
            _ => return 0.0,
        };
        let span = d_max - d_min;
        if span <= 0.0 {
            return 0.0;
        }
        let bin = find_bin(v, edges);
        (edges[bin + 1] - edges[bin]) / span
    }
}

/// Find the bin index whose `[edges[i], edges[i+1])` bracket contains
/// `v`. The last bin is closed on both sides so values at the upper
/// boundary still land in the final bin rather than falling off.
fn find_bin(v: f64, edges: &[f64]) -> usize {
    let n_bins = edges.len() - 1;
    (0..n_bins)
        .find(|&i| v >= edges[i] && (v < edges[i + 1] || i == n_bins - 1))
        .unwrap_or(n_bins - 1)
}

// ─── Identity ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct Identity;

impl ScaleTypeTrait for Identity {
    fn kind(&self) -> ScaleTypeKind {
        ScaleTypeKind::Identity
    }

    fn map(&self, input: &Value, _scale: &Scale) -> Value {
        input.clone()
    }

    fn breaks(&self, _scale: &Scale, _n: usize) -> Vec<Value> {
        // Identity has no notion of "ticks" — return nothing.
        Vec::new()
    }
}
