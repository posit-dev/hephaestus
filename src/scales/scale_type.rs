//! Scale-type algorithms as free functions, one per kind.
//!
//! [`ScaleTypeKind`] tags the family; the per-kind functions
//! ([`continuous_map`], [`discrete_map`], [`ordinal_map`], [`binned_map`],
//! [`identity_map`], plus their `_breaks` / `_band_width` siblings) operate
//! on plain inputs and return plain outputs. No traits, no `Arc<dyn>` —
//! callers either dispatch on the kind themselves (see hephaestus's
//! `Scale::map`) or call the per-kind function directly.

use crate::color::Color;

use super::breaks::{
    extended_breaks, linear_minor_breaks_between, log_minor_breaks, log_pretty_breaks, sqrt_breaks,
    symlog_breaks, symlog_minor_breaks, temporal_breaks_from_f64, temporal_minor_breaks_from_f64,
};
use super::input::InputRange;
use super::output::OutputRange;
use super::transform::{Transform, TransformKind};
use super::value::Value;

/// Temporal scales know which calendar unit their f64 domain represents
/// — needed to emit calendar-aligned ticks (year/month/week/day/hour
/// boundaries rather than mid-domain numeric ticks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemporalUnit {
    /// Domain f64 = days since 1970-01-01. Tick values come back as
    /// [`Value::Date`].
    Date,
    /// Domain f64 = microseconds since 1970-01-01T00:00:00Z. Tick
    /// values come back as [`Value::DateTime`].
    DateTime,
    /// Domain f64 = nanoseconds since midnight (matches `Time(i64)`'s
    /// Arrow `Time64(Nanosecond)` storage). Tick values come back as
    /// [`Value::Time`]. Calendar-unit selection only emits sub-day
    /// units (Hour / Minute / Second).
    Time,
    /// Domain f64 = signed microseconds. Tick values come back as
    /// [`Value::Duration`]. Calendar-unit selection emits Day /
    /// Hour / Minute / Second.
    Duration,
}

/// Discriminator for the scale-type family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScaleTypeKind {
    /// Linear mapping over a numeric domain. Output range can be unset
    /// (returns normalised `[0, 1]` fraction), `Numbers` (piecewise-linear
    /// interpolation across stops), or `Colors` (piecewise-linear
    /// componentwise interpolation).
    #[default]
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
    /// Continuous domain interpreted as a calendar quantity (date /
    /// datetime / time-of-day / duration). Mapping is linear like
    /// [`Self::Continuous`]; breaks are calendar-aligned
    /// (year / quarter / month / week / day / hour / minute / second
    /// boundaries instead of Wilkinson "nice numbers").
    Temporal(TemporalUnit),
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
            ScaleTypeKind::Temporal(_) => "temporal",
        }
    }
}

// ─── Continuous ──────────────────────────────────────────────────────────────

/// Map a value through a continuous scale.
///
/// Applies the transform, normalises to `[0, 1]` against the input range,
/// then interpolates through the output range (or returns the fraction
/// directly when the output range is unset).
///
/// Returns `Value::Null` if `input` has no numeric projection or the input
/// range is missing / not continuous.
pub fn continuous_map(
    input: &Value,
    input_range: Option<&InputRange>,
    output_range: Option<&OutputRange>,
    transform: &Transform,
) -> Value {
    let v = match input.as_number() {
        Some(n) => n,
        None => return Value::Null,
    };
    let (d_min, d_max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Value::Null,
    };
    let v_t = transform.forward(v);
    let dmin_t = transform.forward(d_min);
    let dmax_t = transform.forward(d_max);
    let t = if dmax_t == dmin_t {
        0.0
    } else {
        (v_t - dmin_t) / (dmax_t - dmin_t)
    };
    interpolate_range(t, output_range)
}

/// Tick positions for a continuous scale, in input space, projected
/// to `Value::Number` for the formatter to handle. Transform-aware:
/// log scales emit the 1-2-5 pattern across decades; sqrt scales emit
/// Wilkinson-Extended in sqrt space; symmetric-log scales (Asinh /
/// PseudoLog) emit log breaks on each branch around zero.
pub fn continuous_breaks(
    input_range: Option<&InputRange>,
    transform: &Transform,
    n: usize,
) -> Vec<Value> {
    let (min, max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Vec::new(),
    };
    transform_breaks(min, max, n, transform.kind)
        .into_iter()
        .map(Value::Number)
        .collect()
}

/// Minor (sub-tick) positions for a continuous scale. Per-transform:
/// log scales emit geometric 2..9 between decades; sqrt / linear /
/// other scales emit one evenly-spaced minor between each pair of
/// majors; symmetric-log scales mirror log on each branch.
pub fn continuous_minor_breaks(
    input_range: Option<&InputRange>,
    transform: &Transform,
    majors: &[Value],
) -> Vec<Value> {
    let (min, max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Vec::new(),
    };
    transform_minor_breaks(min, max, transform.kind, majors)
        .into_iter()
        .map(Value::Number)
        .collect()
}

/// Dispatch major break generation by [`TransformKind`].
fn transform_breaks(min: f64, max: f64, n: usize, kind: TransformKind) -> Vec<f64> {
    match kind {
        TransformKind::Identity
        | TransformKind::Square
        | TransformKind::Exp10
        | TransformKind::Exp2
        | TransformKind::Exp => extended_breaks(min, max, n),
        TransformKind::Log10 => log_pretty_breaks(min, max, n, 10.0),
        TransformKind::Log2 => log_pretty_breaks(min, max, n, 2.0),
        TransformKind::Log => log_pretty_breaks(min, max, n, std::f64::consts::E),
        TransformKind::Sqrt => sqrt_breaks(min, max, n),
        TransformKind::Asinh | TransformKind::PseudoLog => {
            symlog_breaks(min, max, n, std::f64::consts::E)
        }
        TransformKind::PseudoLog2 => symlog_breaks(min, max, n, 2.0),
        TransformKind::PseudoLog10 => symlog_breaks(min, max, n, 10.0),
    }
}

/// Dispatch minor break generation by [`TransformKind`]. `majors` are
/// the major breaks (in input space) — needed for transforms that
/// subdivide between majors (linear default); ignored by transforms
/// that compute minors directly from the domain (log family, symlog).
fn transform_minor_breaks(min: f64, max: f64, kind: TransformKind, majors: &[Value]) -> Vec<f64> {
    match kind {
        TransformKind::Log10 => log_minor_breaks(min, max, 10.0),
        TransformKind::Log2 => log_minor_breaks(min, max, 2.0),
        TransformKind::Log => log_minor_breaks(min, max, std::f64::consts::E),
        TransformKind::Asinh | TransformKind::PseudoLog => {
            symlog_minor_breaks(min, max, std::f64::consts::E)
        }
        TransformKind::PseudoLog2 => symlog_minor_breaks(min, max, 2.0),
        TransformKind::PseudoLog10 => symlog_minor_breaks(min, max, 10.0),
        // Identity / Square / Exp* / Sqrt: linear subdivision between
        // majors, one minor per interval.
        _ => {
            let m: Vec<f64> = majors.iter().filter_map(|v| v.as_number()).collect();
            linear_minor_breaks_between(&m, 1)
        }
    }
}

// ─── Temporal ────────────────────────────────────────────────────────────────

/// Calendar-aligned major breaks for a temporal scale. Picks a
/// calendar unit (year / quarter / month / week / day / hour / minute /
/// second) sized to fit the target tick count, then enumerates that
/// unit's boundaries inside the domain.
///
/// Returns `Vec<Value>` whose variant matches `unit`:
/// - [`TemporalUnit::Date`] → `Value::Date(days)`
/// - [`TemporalUnit::DateTime`] → `Value::DateTime(μs)`
/// - [`TemporalUnit::Time`] → `Value::Time(ns)`
/// - [`TemporalUnit::Duration`] → `Value::Duration(μs)`
///
/// The tick label formatter ([`Scale::format`] in `crate::plot::scale`)
/// renders each variant in calendar form (`YYYY-MM-DD`, etc.).
pub fn temporal_breaks(
    input_range: Option<&InputRange>,
    unit: TemporalUnit,
    n: usize,
) -> Vec<Value> {
    let (min, max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Vec::new(),
    };
    temporal_breaks_from_f64(min, max, unit, n)
        .into_iter()
        .map(|raw| wrap_temporal_value(raw, unit))
        .collect()
}

/// Calendar-aligned minor breaks for a temporal scale. Subdivides each
/// major-unit interval by a sensible sub-unit: year → quarter, quarter
/// → month, month → week, week → day, day → 6-hour, hour → 15-minute,
/// minute → 15-second.
pub fn temporal_minor_breaks(
    input_range: Option<&InputRange>,
    unit: TemporalUnit,
    _majors: &[Value],
    n: usize,
) -> Vec<Value> {
    let (min, max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Vec::new(),
    };
    temporal_minor_breaks_from_f64(min, max, unit, n)
        .into_iter()
        .map(|raw| wrap_temporal_value(raw, unit))
        .collect()
}

fn wrap_temporal_value(raw: f64, unit: TemporalUnit) -> Value {
    match unit {
        TemporalUnit::Date => Value::Date(raw as i32),
        TemporalUnit::DateTime => Value::DateTime(raw as i64),
        TemporalUnit::Time => Value::Time(raw as i64),
        TemporalUnit::Duration => Value::Duration(raw as i64),
    }
}

// ─── Discrete ────────────────────────────────────────────────────────────────

/// One-to-one lookup: returns the output-range entry at the same index as
/// the matching domain entry. When the output range is unset, returns the
/// band-centre fraction `(idx + 0.5) / n` (positional rendering on a
/// discrete axis).
pub fn discrete_map(
    input: &Value,
    input_range: Option<&InputRange>,
    output_range: Option<&OutputRange>,
) -> Value {
    let domain = match input_range {
        Some(InputRange::Discrete(d)) => d,
        _ => return Value::Null,
    };
    let n = domain.len();
    let idx = match domain.iter().position(|d| d.key_eq(input)) {
        Some(i) => i,
        None => return Value::Null,
    };
    match output_range {
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
        Some(OutputRange::Linetypes(vs)) => vs
            .get(idx)
            .cloned()
            .map(Value::Linetype)
            .unwrap_or(Value::Null),
    }
}

// ─── Ordinal ─────────────────────────────────────────────────────────────────

/// Ordered discrete domain mapped through a continuous output range.
/// Each domain entry's normalised position `idx / (n - 1)` is
/// interpolated through the output range (or returns the band-centre
/// fraction when the output range is unset).
pub fn ordinal_map(
    input: &Value,
    input_range: Option<&InputRange>,
    output_range: Option<&OutputRange>,
) -> Value {
    let domain = match input_range {
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
    match output_range {
        None => Value::Number((idx as f64 + 0.5) / n as f64),
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

// ─── Discrete / Ordinal shared helpers ───────────────────────────────────────

/// Break values for a discrete / ordinal scale — just the domain entries.
pub fn discrete_breaks(input_range: Option<&InputRange>) -> Vec<Value> {
    match input_range {
        Some(InputRange::Discrete(d)) => d.clone(),
        _ => Vec::new(),
    }
}

/// Uniform band width for a discrete / ordinal scale: `1.0 / n_bands`.
pub fn discrete_band_width(input_range: Option<&InputRange>) -> f64 {
    match input_range {
        Some(InputRange::Discrete(d)) if !d.is_empty() => 1.0 / d.len() as f64,
        _ => 0.0,
    }
}

// ─── Binned ──────────────────────────────────────────────────────────────────

/// Map a value through a binned scale. Returns the bin's domain-space
/// centre projected onto `[0, 1]`; out-of-range inputs return `Null`. For
/// uneven-width bins the centre placement naturally widens the bin's
/// panel slot, matching histogram conventions.
pub fn binned_map(
    input: &Value,
    input_range: Option<&InputRange>,
    output_range: Option<&OutputRange>,
) -> Value {
    let v = match input.as_number() {
        Some(n) => n,
        None => return Value::Null,
    };
    let (d_min, d_max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Value::Null,
    };
    let edges = match output_range {
        Some(OutputRange::Numbers(vs)) if vs.len() >= 2 => vs,
        _ => return Value::Null,
    };
    if !v.is_finite() || v < d_min || v > d_max {
        return Value::Null;
    }
    let span = d_max - d_min;
    if span <= 0.0 {
        return Value::Number(0.0);
    }
    let bin = find_bin(v, edges);
    let centre = (edges[bin] + edges[bin + 1]) * 0.5;
    Value::Number((centre - d_min) / span)
}

/// Bin edges of a binned scale, as `Value::Number`.
pub fn binned_breaks(output_range: Option<&OutputRange>) -> Vec<Value> {
    match output_range {
        Some(OutputRange::Numbers(vs)) => vs.iter().copied().map(Value::Number).collect(),
        _ => Vec::new(),
    }
}

/// Uniform band width for a binned scale: `1.0 / n_bins`.
pub fn binned_band_width(output_range: Option<&OutputRange>) -> f64 {
    match output_range {
        Some(OutputRange::Numbers(vs)) if vs.len() >= 2 => 1.0 / (vs.len() - 1) as f64,
        _ => 0.0,
    }
}

/// Per-bin band width — the proportional panel slot of the bin containing
/// `input`. Lets [`Scale::map_with_offset`] (in `crate::plot::scale`) apply
/// `*_band` channel offsets correctly across non-uniform bin widths.
pub fn binned_band_width_at(
    input: &Value,
    input_range: Option<&InputRange>,
    output_range: Option<&OutputRange>,
) -> f64 {
    let v = match input.as_number() {
        Some(n) => n,
        None => return 0.0,
    };
    let (d_min, d_max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return 0.0,
    };
    let edges = match output_range {
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

// ─── Identity ────────────────────────────────────────────────────────────────

/// Pass-through map — returns the input verbatim.
pub fn identity_map(input: &Value) -> Value {
    input.clone()
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
///   space. Not perceptually uniform — a documented limitation.
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
        // Strings have no numeric interpolation; pick the nearest
        // index along the output range, mirroring Linetypes. Lets
        // continuous scales drive non-numeric discrete outputs like
        // shape names without forcing a separate ordinal/binned
        // scale type.
        Some(OutputRange::Strings(vs)) => match vs.len() {
            0 => Value::Null,
            1 => Value::String(vs[0].clone()),
            n => {
                let idx = (t * (n - 1) as f64).round() as isize;
                let clamped = idx.clamp(0, n as isize - 1) as usize;
                Value::String(vs[clamped].clone())
            }
        },
        Some(OutputRange::Linetypes(vs)) => match vs.len() {
            0 => Value::Null,
            1 => Value::Linetype(vs[0].clone()),
            n => {
                let idx = (t * (n - 1) as f64).round() as isize;
                let idx = idx.clamp(0, n as isize - 1) as usize;
                Value::Linetype(vs[idx].clone())
            }
        },
    }
}

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

/// Find the bin index whose `[edges[i], edges[i+1])` bracket contains
/// `v`. The last bin is closed on both sides so values at the upper
/// boundary still land in the final bin rather than falling off.
fn find_bin(v: f64, edges: &[f64]) -> usize {
    let n_bins = edges.len() - 1;
    (0..n_bins)
        .find(|&i| v >= edges[i] && (v < edges[i + 1] || i == n_bins - 1))
        .unwrap_or(n_bins - 1)
}
