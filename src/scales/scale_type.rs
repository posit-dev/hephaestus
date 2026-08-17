//! Scale-type algorithms as free functions, one per kind.
//!
//! [`ScaleTypeKind`] tags the family; the per-kind functions
//! ([`continuous_map`], [`discrete_map`], [`ordinal_map`], [`binned_map`],
//! [`identity_map`], plus their `_breaks` / `_band_width` siblings) operate
//! on plain inputs and return plain outputs. No traits, no `Arc<dyn>` —
//! callers either dispatch on the kind themselves (see hephaestus's
//! `Scale::map`) or call the per-kind function directly.

use crate::color::{lerp_color, ColorSpace};

use super::breaks::{
    extended_breaks, linear_minor_breaks_between, log_minor_breaks, log_pretty_breaks, sqrt_breaks,
    symlog_breaks, symlog_minor_breaks, temporal_breaks_date, temporal_breaks_datetime,
    temporal_breaks_from_f64, temporal_breaks_time, temporal_minor_breaks_from_f64,
    temporal_minor_breaks_from_f64_with_interval, TemporalInterval,
};
use super::direction::Direction;
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

// Sort a domain's endpoints. Reversal is [`Direction`]'s job, so the
// tick algorithms — which all require `lo < hi` and return nothing for a
// descending pair — are handed the endpoints in order rather than as the
// caller happened to write them.
fn ordered(min: f64, max: f64) -> (f64, f64) {
    if min <= max {
        (min, max)
    } else {
        (max, min)
    }
}

// ─── Continuous ──────────────────────────────────────────────────────────────

/// Map a value through a continuous scale.
///
/// Applies the transform, normalises to `[0, 1]` against the input range,
/// then interpolates through the output range (or returns the fraction
/// directly when the output range is unset). `color_space` is the space a
/// [`OutputRange::Colors`] ramp interpolates in and is ignored by every
/// other output type. [`Direction::Reversed`] mirrors the normalised
/// fraction before it reaches the output range.
///
/// Returns `Value::Null` if `input` has no numeric projection or the input
/// range is missing / not continuous.
pub fn continuous_map(
    input: &Value,
    input_range: Option<&InputRange>,
    output_range: Option<&OutputRange>,
    transform: &Transform,
    color_space: ColorSpace,
    direction: Direction,
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
    interpolate_range(direction.apply_fraction(t), output_range, color_space)
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
        Some(InputRange::Continuous { min, max }) => ordered(*min, *max),
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
        Some(InputRange::Continuous { min, max }) => ordered(*min, *max),
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
/// The tick label formatter (`format` in `crate::plot::scale`)
/// renders each variant in calendar form (`YYYY-MM-DD`, etc.).
pub fn temporal_breaks(
    input_range: Option<&InputRange>,
    unit: TemporalUnit,
    n: usize,
) -> Vec<Value> {
    let (min, max) = match input_range {
        Some(InputRange::Continuous { min, max }) => ordered(*min, *max),
        _ => return Vec::new(),
    };
    temporal_breaks_from_f64(min, max, unit, n)
        .into_iter()
        .map(|raw| wrap_temporal_value(raw, unit))
        .collect()
}

/// Calendar-aligned major breaks at a user-specified interval (e.g.
/// every 2 weeks). Skips the automatic interval picker — the caller has
/// already chosen the cadence. Output variant matches `unit`; see
/// [`temporal_breaks`] for the variant table.
pub fn temporal_breaks_with_interval(
    input_range: Option<&InputRange>,
    unit: TemporalUnit,
    interval: TemporalInterval,
) -> Vec<Value> {
    let (min, max) = match input_range {
        Some(InputRange::Continuous { min, max }) => ordered(*min, *max),
        _ => return Vec::new(),
    };
    if !min.is_finite() || !max.is_finite() || min >= max {
        return Vec::new();
    }
    let raws: Vec<f64> = match unit {
        TemporalUnit::Date => temporal_breaks_date(min as i32, max as i32, interval)
            .into_iter()
            .filter(|d| (*d as f64) >= min && (*d as f64) <= max)
            .map(|d| d as f64)
            .collect(),
        TemporalUnit::DateTime | TemporalUnit::Duration => {
            temporal_breaks_datetime(min as i64, max as i64, interval)
                .into_iter()
                .filter(|us| (*us as f64) >= min && (*us as f64) <= max)
                .map(|us| us as f64)
                .collect()
        }
        TemporalUnit::Time => temporal_breaks_time(min as i64, max as i64, interval)
            .into_iter()
            .filter(|us| (*us as f64) >= min && (*us as f64) <= max)
            .map(|us| us as f64)
            .collect(),
    };
    raws.into_iter()
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
        Some(InputRange::Continuous { min, max }) => ordered(*min, *max),
        _ => return Vec::new(),
    };
    temporal_minor_breaks_from_f64(min, max, unit, n)
        .into_iter()
        .map(|raw| wrap_temporal_value(raw, unit))
        .collect()
}

/// Calendar-aligned minor breaks under a caller-chosen major interval.
/// Subdivides `interval` — the counterpart to
/// [`temporal_breaks_with_interval`], so majors pinned to an interval and
/// their minors agree on what they're subdividing.
pub fn temporal_minor_breaks_with_interval(
    input_range: Option<&InputRange>,
    unit: TemporalUnit,
    interval: TemporalInterval,
) -> Vec<Value> {
    let (min, max) = match input_range {
        Some(InputRange::Continuous { min, max }) => ordered(*min, *max),
        _ => return Vec::new(),
    };
    temporal_minor_breaks_from_f64_with_interval(min, max, unit, interval)
        .into_iter()
        .map(|raw| wrap_temporal_value(raw, unit))
        .collect()
}

/// Wrap a raw f64 domain position as the typed [`Value`] its
/// [`TemporalUnit`] stands for — the inverse of the projection a temporal
/// scale applies on the way in.
pub fn wrap_temporal_value(raw: f64, unit: TemporalUnit) -> Value {
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
/// discrete axis). [`Direction::Reversed`] mirrors the index, so the first
/// category takes the last band and the last palette entry.
pub fn discrete_map(
    input: &Value,
    input_range: Option<&InputRange>,
    output_range: Option<&OutputRange>,
    direction: Direction,
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
    let idx = direction.apply_index(idx, n);
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
/// fraction when the output range is unset). `color_space` is the space a
/// [`OutputRange::Colors`] ramp interpolates in and is ignored by every
/// other output type. [`Direction::Reversed`] mirrors the index, so the
/// domain walks the gradient from its far end.
pub fn ordinal_map(
    input: &Value,
    input_range: Option<&InputRange>,
    output_range: Option<&OutputRange>,
    color_space: ColorSpace,
    direction: Direction,
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
    let idx = direction.apply_index(idx, n);
    match output_range {
        None => Value::Number((idx as f64 + 0.5) / n as f64),
        Some(range) => {
            let t = if n > 1 {
                idx as f64 / (n - 1) as f64
            } else {
                0.0
            };
            interpolate_range(t, Some(range), color_space)
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

/// Map a value through a binned scale. `bins` is the bin-edge list
/// (strictly increasing, length ≥ 2); values outside the domain, and any
/// input on a scale with no bins, return `Null`.
///
/// With no output range the result is the containing bin's domain-space
/// centre projected onto `[0, 1]` — the position case. For uneven-width
/// bins the centre placement naturally widens the bin's panel slot,
/// matching histogram conventions.
///
/// With an output range the bin's index picks a palette entry the way an
/// ordinal scale picks one for its levels: an N-entry palette over N bins
/// is a one-to-one lookup, and a shorter palette interpolates across the
/// bins. `color_space` is the space a [`OutputRange::Colors`] ramp
/// interpolates in and is ignored by every other output type.
///
/// [`Direction::Reversed`] mirrors the bin: its centre lands the same
/// distance from the far end of the panel, and its palette entry is
/// counted from the far end of the range.
pub fn binned_map(
    input: &Value,
    input_range: Option<&InputRange>,
    bins: Option<&[f64]>,
    output_range: Option<&OutputRange>,
    color_space: ColorSpace,
    direction: Direction,
) -> Value {
    let v = match input.as_number() {
        Some(n) => n,
        None => return Value::Null,
    };
    let (d_min, d_max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Value::Null,
    };
    let edges = match bins {
        Some(es) if es.len() >= 2 => es,
        _ => return Value::Null,
    };
    if !v.is_finite() || v < d_min || v > d_max {
        return Value::Null;
    }
    let bin = find_bin(v, edges);
    match output_range {
        None => {
            let span = d_max - d_min;
            if span <= 0.0 {
                return Value::Number(0.0);
            }
            let centre = (edges[bin] + edges[bin + 1]) * 0.5;
            Value::Number(direction.apply_fraction((centre - d_min) / span))
        }
        Some(range) => {
            let n_bins = edges.len() - 1;
            let bin = direction.apply_index(bin, n_bins);
            let t = if n_bins > 1 {
                bin as f64 / (n_bins - 1) as f64
            } else {
                0.0
            };
            interpolate_range(t, Some(range), color_space)
        }
    }
}

/// Position a binned scale's break on the panel: the value's own
/// domain fraction, not the centre of the bin containing it. Chrome
/// uses this so each bin edge is drawn at the boundary it labels;
/// out-of-domain inputs return `Null`. [`Direction::Reversed`] mirrors
/// the fraction, keeping edges with the bins they bound.
pub fn binned_map_break(
    input: &Value,
    input_range: Option<&InputRange>,
    direction: Direction,
) -> Value {
    let v = match input.as_number() {
        Some(n) => n,
        None => return Value::Null,
    };
    let (d_min, d_max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Value::Null,
    };
    if !v.is_finite() || v < d_min || v > d_max {
        return Value::Null;
    }
    let span = d_max - d_min;
    if span <= 0.0 {
        return Value::Number(0.0);
    }
    Value::Number(direction.apply_fraction((v - d_min) / span))
}

/// Bin edges of a binned scale, as `Value::Number`.
pub fn binned_breaks(bins: Option<&[f64]>) -> Vec<Value> {
    match bins {
        Some(es) => es.iter().copied().map(Value::Number).collect(),
        None => Vec::new(),
    }
}

/// Uniform band width for a binned scale: `1.0 / n_bins`.
pub fn binned_band_width(bins: Option<&[f64]>) -> f64 {
    match bins {
        Some(es) if es.len() >= 2 => 1.0 / (es.len() - 1) as f64,
        _ => 0.0,
    }
}

/// Per-bin band width — the proportional panel slot of the bin containing
/// `input`. Lets `map_with_offset` (in `crate::plot::scale`) apply
/// `*_band` channel offsets correctly across non-uniform bin widths.
pub fn binned_band_width_at(
    input: &Value,
    input_range: Option<&InputRange>,
    bins: Option<&[f64]>,
) -> f64 {
    let v = match input.as_number() {
        Some(n) => n,
        None => return 0.0,
    };
    let (d_min, d_max) = match input_range {
        Some(InputRange::Continuous { min, max }) => (*min, *max),
        _ => return 0.0,
    };
    let edges = match bins {
        Some(es) if es.len() >= 2 => es,
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

/// Pass-through map — returns the input verbatim. Takes no
/// [`Direction`]: nothing is normalised, so there is no ordering to
/// reverse.
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
/// - `Colors(vs)` → piecewise-linear interpolation through `color_space`.
/// - `Strings(vs)` / `Linetypes(vs)` → the entry nearest `t`. Neither
///   has a meaningful midpoint, so the fraction selects rather than
///   blends. Empty vec returns `Null`.
fn interpolate_range(t: f64, range: Option<&OutputRange>, color_space: ColorSpace) -> Value {
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
                Value::Color(lerp_color(vs[lo], vs[lo + 1], frac, color_space))
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

/// Find the bin index whose `[edges[i], edges[i+1])` bracket contains
/// `v`. The last bin is closed on both sides so values at the upper
/// boundary still land in the final bin rather than falling off. Values
/// outside the edge list clamp to the nearest bin, so an edge list
/// narrower than the domain sends low values to the first bin rather
/// than wrapping them round to the last.
fn find_bin(v: f64, edges: &[f64]) -> usize {
    let n_bins = edges.len() - 1;
    // Edges are strictly increasing, so the first edge greater than `v`
    // bounds the containing bin.
    edges
        .partition_point(|e| *e <= v)
        .saturating_sub(1)
        .min(n_bins - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_bin_brackets_are_half_open_with_a_closed_top() {
        let edges = [10.0, 20.0, 30.0];
        assert_eq!(find_bin(10.0, &edges), 0);
        assert_eq!(find_bin(15.0, &edges), 0);
        assert_eq!(find_bin(20.0, &edges), 1);
        assert_eq!(find_bin(25.0, &edges), 1);
        // The last bin is closed on both sides.
        assert_eq!(find_bin(30.0, &edges), 1);
    }

    #[test]
    fn find_bin_clamps_outside_the_edge_list() {
        let edges = [10.0, 20.0, 30.0];
        // Below the first edge belongs to the first bin, not the last.
        assert_eq!(find_bin(5.0, &edges), 0);
        assert_eq!(find_bin(-100.0, &edges), 0);
        assert_eq!(find_bin(35.0, &edges), 1);
    }

    fn srgb() -> ColorSpace {
        ColorSpace::Srgb
    }

    fn cat(s: &str) -> Value {
        Value::String(std::sync::Arc::from(s))
    }

    fn strings(items: &[&str]) -> OutputRange {
        OutputRange::Strings(items.iter().map(|s| std::sync::Arc::from(*s)).collect())
    }

    fn dash(len: f64) -> std::sync::Arc<[crate::scales::value::LinetypeStep]> {
        std::sync::Arc::from(vec![crate::scales::value::LinetypeStep::Dash(len)])
    }

    // ── pick_segment ────────────────────────────────────────────────

    #[test]
    fn pick_segment_walks_one_segment_per_stop_pair() {
        // Three stops = two segments; each covers half the unit interval.
        assert_eq!(pick_segment(0.0, 3), (0, 0.0));
        assert_eq!(pick_segment(0.25, 3), (0, 0.5));
        assert_eq!(pick_segment(0.5, 3), (1, 0.0));
        assert_eq!(pick_segment(0.75, 3), (1, 0.5));
    }

    #[test]
    fn pick_segment_holds_the_top_stop_pair_at_the_upper_end() {
        // `t = 1` must address the last segment fully rather than
        // indexing one past the final stop.
        let (lo, frac) = pick_segment(1.0, 3);
        assert_eq!(lo, 1);
        assert!((frac - 1.0).abs() < 1e-12, "{frac}");
    }

    #[test]
    fn pick_segment_extrapolates_past_both_ends() {
        // Out-of-range fractions keep the nearest segment and let the
        // interpolation weight run past `[0, 1]`.
        let (lo, frac) = pick_segment(1.5, 3);
        assert_eq!(lo, 1);
        assert!((frac - 2.0).abs() < 1e-12, "{frac}");
        let (lo, frac) = pick_segment(-0.5, 3);
        assert_eq!(lo, 0);
        assert!((frac + 1.0).abs() < 1e-12, "{frac}");
    }

    // ── interpolate_range ───────────────────────────────────────────

    #[test]
    fn interpolate_range_without_a_range_returns_the_fraction() {
        let v = interpolate_range(0.42, None, srgb());
        assert_eq!(v.as_number(), Some(0.42));
    }

    #[test]
    fn interpolate_range_over_numbers_is_piecewise_linear() {
        let range = OutputRange::Numbers(vec![0.0, 10.0, 100.0]);
        let at = |t: f64| {
            interpolate_range(t, Some(&range), srgb())
                .as_number()
                .unwrap()
        };
        assert!((at(0.0) - 0.0).abs() < 1e-12);
        assert!((at(0.25) - 5.0).abs() < 1e-12);
        assert!((at(0.5) - 10.0).abs() < 1e-12);
        assert!((at(0.75) - 55.0).abs() < 1e-12);
        assert!((at(1.0) - 100.0).abs() < 1e-12);
    }

    #[test]
    fn interpolate_range_over_an_empty_or_single_stop_range() {
        assert!(interpolate_range(0.5, Some(&OutputRange::Numbers(vec![])), srgb()).is_null());
        let one = OutputRange::Numbers(vec![7.0]);
        assert_eq!(
            interpolate_range(0.9, Some(&one), srgb()).as_number(),
            Some(7.0)
        );
        assert!(interpolate_range(0.5, Some(&OutputRange::Colors(vec![])), srgb()).is_null());
        assert!(interpolate_range(0.5, Some(&strings(&[])), srgb()).is_null());
        assert!(interpolate_range(0.5, Some(&OutputRange::Linetypes(vec![])), srgb()).is_null());
    }

    #[test]
    fn interpolate_range_over_colors_blends_componentwise_in_the_named_space() {
        let range = OutputRange::Colors(vec![
            crate::color::rgb(0.0, 0.0, 0.0),
            crate::color::rgb(1.0, 0.0, 0.0),
            crate::color::rgb(1.0, 1.0, 1.0),
        ]);
        let mid = match interpolate_range(0.25, Some(&range), ColorSpace::Srgb) {
            Value::Color(c) => c,
            other => panic!("expected a color, got {other:?}"),
        };
        let [r, g, b, a] = mid.components;
        assert!((r - 0.5).abs() < 1e-6, "{r}");
        assert!(g.abs() < 1e-6 && b.abs() < 1e-6);
        assert!((a - 1.0).abs() < 1e-6);
    }

    #[test]
    fn interpolate_range_over_strings_picks_the_nearest_entry() {
        let range = strings(&["a", "b", "c"]);
        let at = |t: f64| match interpolate_range(t, Some(&range), srgb()) {
            Value::String(s) => s.to_string(),
            other => panic!("expected a string, got {other:?}"),
        };
        assert_eq!(at(0.0), "a");
        assert_eq!(at(0.4), "b");
        assert_eq!(at(0.9), "c");
        // Out-of-range fractions clamp to the ends rather than wrapping.
        assert_eq!(at(-2.0), "a");
        assert_eq!(at(3.0), "c");
    }

    #[test]
    fn interpolate_range_over_linetypes_picks_the_nearest_entry() {
        let range = OutputRange::Linetypes(vec![dash(1.0), dash(2.0), dash(3.0)]);
        let at = |t: f64| match interpolate_range(t, Some(&range), srgb()) {
            Value::Linetype(p) => p,
            other => panic!("expected a linetype, got {other:?}"),
        };
        assert_eq!(&*at(0.1), &*dash(1.0));
        assert_eq!(&*at(0.5), &*dash(2.0));
        assert_eq!(&*at(1.0), &*dash(3.0));
        assert_eq!(&*at(4.0), &*dash(3.0));
    }

    #[test]
    fn ordinal_map_interpolates_a_palette_shorter_than_the_domain() {
        // Five levels over a two-stop ramp: intermediate levels land on
        // interpolated points instead of falling off the palette.
        let domain = InputRange::Discrete(vec![cat("a"), cat("b"), cat("c"), cat("d"), cat("e")]);
        let range = OutputRange::Numbers(vec![0.0, 100.0]);
        let at = |v: &str| {
            ordinal_map(
                &cat(v),
                Some(&domain),
                Some(&range),
                srgb(),
                Direction::Forward,
            )
            .as_number()
            .unwrap()
        };
        assert!((at("a") - 0.0).abs() < 1e-12);
        assert!((at("b") - 25.0).abs() < 1e-12);
        assert!((at("c") - 50.0).abs() < 1e-12);
        assert!((at("e") - 100.0).abs() < 1e-12);
    }

    // ── Direction::Reversed ─────────────────────────────────────────

    #[test]
    fn discrete_map_reversed_mirrors_band_and_palette_index() {
        let domain =
            InputRange::Discrete(vec![Value::from("a"), Value::from("b"), Value::from("c")]);
        let band = |v: &str| {
            discrete_map(&cat(v), Some(&domain), None, Direction::Reversed)
                .as_number()
                .unwrap()
        };
        // The first category takes the last band centre.
        assert!((band("a") - 2.5 / 3.0).abs() < 1e-12, "{}", band("a"));
        assert!((band("c") - 0.5 / 3.0).abs() < 1e-12, "{}", band("c"));

        let range = OutputRange::Numbers(vec![1.0, 2.0, 3.0]);
        let pick = |v: &str| {
            discrete_map(&cat(v), Some(&domain), Some(&range), Direction::Reversed)
                .as_number()
                .unwrap()
        };
        assert_eq!(pick("a"), 3.0);
        assert_eq!(pick("c"), 1.0);
    }

    #[test]
    fn ordinal_map_reversed_walks_the_gradient_from_the_far_end() {
        let domain =
            InputRange::Discrete(vec![Value::from("a"), Value::from("b"), Value::from("c")]);
        let range = OutputRange::Numbers(vec![0.0, 10.0]);
        let at = |v: &str| {
            ordinal_map(
                &cat(v),
                Some(&domain),
                Some(&range),
                srgb(),
                Direction::Reversed,
            )
            .as_number()
            .unwrap()
        };
        assert!((at("a") - 10.0).abs() < 1e-12);
        assert!((at("b") - 5.0).abs() < 1e-12);
        assert!((at("c") - 0.0).abs() < 1e-12);
    }

    #[test]
    fn ordinal_map_reversed_without_a_range_mirrors_the_band_centre() {
        let domain = InputRange::Discrete(vec![cat("a"), cat("b")]);
        let at = |v: &str| {
            ordinal_map(&cat(v), Some(&domain), None, srgb(), Direction::Reversed)
                .as_number()
                .unwrap()
        };
        assert!((at("a") - 0.75).abs() < 1e-12);
        assert!((at("b") - 0.25).abs() < 1e-12);
    }

    // ── binned_band_width_at ────────────────────────────────────────

    #[test]
    fn binned_band_width_at_reports_the_containing_bins_own_slot() {
        // Uneven bins: each value's band is its own bin's share of the
        // domain, not the uniform `1 / n_bins` the scale-wide helper
        // reports.
        let domain = InputRange::Continuous {
            min: 0.0,
            max: 100.0,
        };
        let edges = [0.0, 10.0, 50.0, 100.0];
        let at = |v: f64| binned_band_width_at(&Value::Number(v), Some(&domain), Some(&edges));
        assert!((at(5.0) - 0.1).abs() < 1e-12, "{}", at(5.0));
        assert!((at(20.0) - 0.4).abs() < 1e-12, "{}", at(20.0));
        assert!((at(60.0) - 0.5).abs() < 1e-12, "{}", at(60.0));
        // The uniform helper can't tell the three bins apart.
        assert!((binned_band_width(Some(&edges)) - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn binned_band_width_at_is_zero_without_a_domain_or_bins() {
        let domain = InputRange::Continuous {
            min: 0.0,
            max: 10.0,
        };
        let edges = [0.0, 5.0, 10.0];
        assert_eq!(
            binned_band_width_at(&cat("a"), Some(&domain), Some(&edges)),
            0.0
        );
        assert_eq!(
            binned_band_width_at(&Value::Number(1.0), None, Some(&edges)),
            0.0
        );
        assert_eq!(
            binned_band_width_at(&Value::Number(1.0), Some(&domain), Some(&[3.0])),
            0.0
        );
    }

    // ── wrap_temporal_value ─────────────────────────────────────────

    #[test]
    fn wrap_temporal_value_restores_each_units_typed_variant() {
        assert!(matches!(
            wrap_temporal_value(19_723.0, TemporalUnit::Date),
            Value::Date(19_723)
        ));
        assert!(matches!(
            wrap_temporal_value(1_704_067_200_000_000.0, TemporalUnit::DateTime),
            Value::DateTime(1_704_067_200_000_000)
        ));
        assert!(matches!(
            wrap_temporal_value(3_600_000_000_000.0, TemporalUnit::Time),
            Value::Time(3_600_000_000_000)
        ));
        assert!(matches!(
            wrap_temporal_value(-90_000_000.0, TemporalUnit::Duration),
            Value::Duration(-90_000_000)
        ));
    }

    #[test]
    fn binned_map_sends_below_edge_values_to_the_first_bin() {
        // A domain wider than the edge list is legal; a value under the
        // first edge must not wrap round to the top bin's colour.
        let domain = InputRange::Continuous {
            min: 0.0,
            max: 100.0,
        };
        let edges = [10.0, 20.0, 30.0];
        let low = binned_map(
            &Value::Number(5.0),
            Some(&domain),
            Some(&edges),
            None,
            ColorSpace::default(),
            Direction::Forward,
        );
        let high = binned_map(
            &Value::Number(95.0),
            Some(&domain),
            Some(&edges),
            None,
            ColorSpace::default(),
            Direction::Forward,
        );
        let (low, high) = (low.as_number().unwrap(), high.as_number().unwrap());
        assert!(
            low < high,
            "below-range {low} should sit under above-range {high}"
        );
        // Bin 0 spans [10, 20], centre 15 → 0.15 of the 0..100 domain.
        assert!((low - 0.15).abs() < 1e-12, "{low}");
    }
}
