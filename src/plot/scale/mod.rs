//! Scale primitives and the concrete [`Scale`] mapper.
//!
//! A [`Scale`] is a runtime-configurable value mapper composed of:
//! - a [`ScaleType`] (Continuous / Discrete / Ordinal / Binned / Identity)
//!   determining the mapping behaviour;
//! - a [`Transform`] (Identity in v1) applied inside continuous scales
//!   before linearisation;
//! - an [`InputRange`] (continuous domain or discrete value list);
//! - an optional [`OutputRange`] (the visual values to map into; left
//!   unset for position channels, which return the normalised [0, 1]
//!   fraction).
//!
//! Construction uses chained builder methods on [`Scale`] (consuming
//! `self`); mutation uses `set_*` methods that take `&mut self` and bump
//! the internal [generation](Scale::generation) counter so v1.5
//! scaled-output caching can detect changes without comparing values.
//!
//! Free-function constructors live in the [`scale`] sub-module re-exported
//! here, providing terse ggplot-style call sites
//! (`scale::continuous(0.0..=100.0)`, `scale::ordinal(...).range_colors(...)`,
//! etc.). Constructors are scale-type-named only — output-type sugar is
//! deliberately absent: a color scale is `ordinal(domain).range_colors(...)`,
//! a size scale is `ordinal(domain).range_numbers(...)`, etc.

pub mod breaks;
pub mod chrome;
pub mod input;
pub mod output;
pub mod scale_type;
pub mod transform;

#[cfg(feature = "text")]
pub mod axis;
#[cfg(feature = "text")]
pub mod legend;

pub use breaks::{extended_breaks, linear_breaks, DEFAULT_BREAK_COUNT};
pub use chrome::{AxisSide, LegendSide};
pub use input::InputRange;
pub use output::OutputRange;
pub use scale_type::{ScaleType, ScaleTypeKind, ScaleTypeTrait};
pub use transform::{Transform, TransformKind, TransformTrait};

use std::cell::Cell;
use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::color::Color;
use crate::plot::value::{DataColumn, Value};

// ─── Scale ───────────────────────────────────────────────────────────────────

/// A configurable value mapper. Combines a [`ScaleType`] with optional
/// input/output ranges, a [`Transform`], and a monotonic generation
/// counter for invalidating downstream caches.
#[derive(Clone)]
pub struct Scale {
    scale_type: ScaleType,
    transform: Transform,
    input_range: Option<InputRange>,
    output_range: Option<OutputRange>,
    /// Bumped on every mutation. Used by future per-channel cache
    /// invalidation (v1.5); v1 plumbs the counter but doesn't consult it.
    generation: Cell<u64>,
}

impl Scale {
    /// Build a fresh scale of the given type. Domain and range are unset
    /// until configured via the builder methods.
    pub fn new(scale_type: ScaleType) -> Self {
        Scale {
            scale_type,
            transform: Transform::default(),
            input_range: None,
            output_range: None,
            generation: Cell::new(0),
        }
    }

    // ── Builders (consume self) ──

    /// Configure a continuous numeric / temporal domain.
    ///
    /// `T` is anything that converts into a [`Value`] whose `as_number()`
    /// projection yields a finite f64. That covers `f64`, `f32`, `i32`,
    /// `i64`, and the temporal newtypes ([`Date`], [`DateTime`], [`Time`],
    /// [`Duration`]) — each projects to its canonical unit (days /
    /// microseconds). Non-numeric endpoints (`String`, `Bool`, `Color`,
    /// `Null`) panic at the call site since they have no continuous
    /// ordering.
    pub fn domain_continuous<T>(mut self, min: T, max: T) -> Self
    where
        T: Into<Value>,
    {
        let lo = endpoint_to_f64(min.into(), "domain_continuous: min");
        let hi = endpoint_to_f64(max.into(), "domain_continuous: max");
        self.input_range = Some(InputRange::Continuous { min: lo, max: hi });
        self
    }

    pub fn domain_discrete(mut self, values: impl IntoIterator<Item = Value>) -> Self {
        self.input_range = Some(InputRange::Discrete(values.into_iter().collect()));
        self
    }

    pub fn range_numbers(mut self, vs: impl IntoIterator<Item = f64>) -> Self {
        self.output_range = Some(OutputRange::Numbers(vs.into_iter().collect()));
        self
    }

    pub fn range_colors(mut self, vs: impl IntoIterator<Item = Color>) -> Self {
        self.output_range = Some(OutputRange::Colors(vs.into_iter().collect()));
        self
    }

    pub fn range_strings(mut self, vs: impl IntoIterator<Item = Arc<str>>) -> Self {
        self.output_range = Some(OutputRange::Strings(vs.into_iter().collect()));
        self
    }

    pub fn with_transform(mut self, t: TransformKind) -> Self {
        self.transform = Transform::of(t);
        self
    }

    // ── Mutators (`&mut self`; bump generation) ──

    pub fn set_domain_continuous<T>(&mut self, min: T, max: T)
    where
        T: Into<Value>,
    {
        let lo = endpoint_to_f64(min.into(), "set_domain_continuous: min");
        let hi = endpoint_to_f64(max.into(), "set_domain_continuous: max");
        self.input_range = Some(InputRange::Continuous { min: lo, max: hi });
        self.bump_generation();
    }

    pub fn set_domain_discrete(&mut self, values: Vec<Value>) {
        self.input_range = Some(InputRange::Discrete(values));
        self.bump_generation();
    }

    pub fn set_range_numbers(&mut self, vs: Vec<f64>) {
        self.output_range = Some(OutputRange::Numbers(vs));
        self.bump_generation();
    }

    pub fn set_range_colors(&mut self, vs: Vec<Color>) {
        self.output_range = Some(OutputRange::Colors(vs));
        self.bump_generation();
    }

    pub fn set_range_strings(&mut self, vs: Vec<Arc<str>>) {
        self.output_range = Some(OutputRange::Strings(vs));
        self.bump_generation();
    }

    pub fn set_transform(&mut self, t: TransformKind) {
        self.transform = Transform::of(t);
        self.bump_generation();
    }

    fn bump_generation(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    // ── Operations ──

    /// Map an input value to its scaled output.
    pub fn map(&self, input: &Value) -> Value {
        self.scale_type.map(input, self)
    }

    /// Like [`Self::map`] but additionally applies a band-fraction offset
    /// in the scale's band space. The offset is multiplied by the band
    /// width of the bin containing `input` (see
    /// [`ScaleTypeTrait::band_width_at`]) before being added to the
    /// nominal mapped fraction.
    ///
    /// - `band_offset` units: fraction of the input's own band width.
    ///   `0.0` is the band centre; `±0.5` reaches the band's left/right
    ///   edge. The offset isn't clamped — values outside `[-0.5, 0.5]`
    ///   extend past the band into neighbouring slots.
    /// - Continuous scales return `0.0` from `band_width_at`, so the
    ///   offset is a no-op there.
    /// - Non-numeric `map()` outputs (e.g. Color) ignore the offset and
    ///   pass through unchanged.
    ///
    /// This is the canonical entry point for geoms consuming `*_band`
    /// channels — moving the combining into `Scale` keeps the output
    /// resize-invariant (it's a panel fraction, not pixels) and lets
    /// scales with non-uniform bands (e.g. [`ScaleTypeKind::Binned`]
    /// with unequal-width bins) compute the correct width per row.
    pub fn map_with_offset(&self, input: &Value, band_offset: f64) -> Value {
        let base = self.scale_type.map(input, self);
        if band_offset == 0.0 {
            return base;
        }
        match base {
            Value::Number(f) => {
                let bw = self.scale_type.band_width_at(self, input);
                Value::Number(f + band_offset * bw)
            }
            other => other,
        }
    }

    /// Tick / category positions in **input** space. `n` is a target for
    /// continuous scales; discrete / ordinal ignore it and return every
    /// domain entry.
    pub fn breaks(&self, n: usize) -> Vec<Value> {
        self.scale_type.breaks(self, n)
    }

    /// Format a value as its tick label. Numeric values use
    /// `format!("{n}")`; temporal variants render compact YYYY-MM-DD /
    /// HH:MM:SS strings (UTC). Strings pass through; everything else
    /// uses [`std::fmt::Debug`].
    pub fn format(&self, v: &Value) -> String {
        format_value(v)
    }

    /// Band width as a fraction of the panel (in `[0, 1]`). Continuous
    /// scales return `0.0`; discrete-family scales return `1.0 / n_bands`.
    pub fn band_width(&self) -> f64 {
        self.scale_type.band_width(self)
    }

    // ── Accessors ──

    pub fn scale_type(&self) -> &ScaleType {
        &self.scale_type
    }

    pub fn transform(&self) -> &Transform {
        &self.transform
    }

    pub fn input_range(&self) -> Option<&InputRange> {
        self.input_range.as_ref()
    }

    pub fn output_range(&self) -> Option<&OutputRange> {
        self.output_range.as_ref()
    }

    /// Monotonic counter incremented on every mutation. Used by v1.5
    /// scaled-output caching to invalidate per-channel caches; v1
    /// increments it but doesn't consult it.
    #[allow(dead_code)] // wired through builder/mutators; consumed in v1.5
    pub(crate) fn generation(&self) -> u64 {
        self.generation.get()
    }
}

impl std::fmt::Debug for Scale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scale")
            .field("scale_type", &self.scale_type)
            .field("transform", &self.transform)
            .field("input_range", &self.input_range)
            .field("output_range", &self.output_range)
            .field("generation", &self.generation.get())
            .finish()
    }
}

// ─── Format helper ───────────────────────────────────────────────────────────

/// Best-effort tick-label formatter.
///
/// - `Number(n)` → `format!("{n}")` (Rust's f64 Display; strips trailing
///   zeros for clean values).
/// - `Date(d)` → `YYYY-MM-DD`.
/// - `DateTime(us)` → `YYYY-MM-DD HH:MM:SS` (UTC).
/// - `Time(us)` → `HH:MM:SS.fff` (or `HH:MM:SS` if sub-second is 0).
/// - `Duration(us)` → compact `Hh Mm Ss` or `MM:SS` depending on magnitude.
/// - `String(s)` → `s`.
/// - Others → debug-formatted.
fn format_value(v: &Value) -> String {
    use crate::plot::value::Date as DateNew;
    match v {
        Value::Number(n) => format!("{n}"),
        Value::String(s) => (**s).to_string(),
        Value::Bool(b) => format!("{b}"),
        Value::Null => "NA".to_string(),
        Value::Color(c) => format!("{c:?}"),
        Value::Date(d) => {
            let (y, m, dd) = DateNew::from_days(*d).to_ymd();
            format!("{y:04}-{m:02}-{dd:02}")
        }
        Value::DateTime(us) => {
            let dt = crate::plot::value::DateTime::from_micros(*us);
            let (date, time_us) = dt.split();
            let (y, m, dd) = date.to_ymd();
            let (h, mi, s, _us) = split_time_micros(time_us);
            format!("{y:04}-{m:02}-{dd:02} {h:02}:{mi:02}:{s:02}")
        }
        Value::Time(us) => {
            let (h, mi, s, sub) = split_time_micros(*us);
            if sub == 0 {
                format!("{h:02}:{mi:02}:{s:02}")
            } else {
                let millis = sub / 1000;
                format!("{h:02}:{mi:02}:{s:02}.{millis:03}")
            }
        }
        Value::Duration(us) => {
            let neg = *us < 0;
            let mut abs = us.unsigned_abs();
            let micros = (abs % 1_000_000) as u32;
            abs /= 1_000_000;
            let seconds = (abs % 60) as u32;
            abs /= 60;
            let minutes = (abs % 60) as u32;
            abs /= 60;
            let hours = abs;
            let sign = if neg { "-" } else { "" };
            if hours > 0 {
                format!("{sign}{hours}h {minutes:02}m {seconds:02}s")
            } else if minutes > 0 {
                format!("{sign}{minutes}m {seconds:02}s")
            } else if micros == 0 {
                format!("{sign}{seconds}s")
            } else {
                let millis = micros / 1000;
                format!("{sign}{seconds}.{millis:03}s")
            }
        }
    }
}

/// Project a continuous-domain endpoint to its canonical f64. Accepts
/// numeric and temporal `Value` variants; panics for other variants since
/// they have no continuous ordering.
fn endpoint_to_f64(v: Value, ctx: &str) -> f64 {
    match v.as_number() {
        Some(n) => n,
        None => panic!("{ctx}: expected a numeric or temporal value, got {v:?}"),
    }
}

/// Split microseconds-since-midnight into (hour, minute, second, sub_us).
fn split_time_micros(us: i64) -> (u8, u8, u8, u32) {
    let us = us.rem_euclid(86_400_000_000);
    let micros_of_sec = (us % 1_000_000) as u32;
    let total_secs = us / 1_000_000;
    let s = (total_secs % 60) as u8;
    let total_mins = total_secs / 60;
    let mi = (total_mins % 60) as u8;
    let h = ((total_mins / 60) % 24) as u8;
    (h, mi, s, micros_of_sec)
}

// ─── ScaleRegistry ───────────────────────────────────────────────────────────

use std::collections::HashMap;

/// Named registry of scales — the single source of truth for scale state
/// in a [`PlotComposition`](crate::plot) orchestrator. Plots reference
/// scales by name through their channel bindings; the registry owns the
/// `Scale` instances.
///
/// In v1 this is a thin `HashMap` wrapper; v1.5 will add per-scale
/// generation-tracking helpers for cache invalidation. The orchestrator
/// (Phase 7) owns the registry and exposes mutators that flip dirty
/// flags; v1 callers building one by hand (e.g. for tests or
/// orchestrator-free renders) can use the [`Self::insert`] /
/// [`Self::remove`] surface directly.
#[derive(Default, Clone, Debug)]
pub struct ScaleRegistry {
    scales: HashMap<String, Scale>,
}

impl ScaleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a scale under `name`, replacing any previous entry.
    pub fn insert(&mut self, name: impl Into<String>, scale: Scale) {
        self.scales.insert(name.into(), scale);
    }

    /// Chainable variant of [`Self::insert`].
    pub fn with(mut self, name: impl Into<String>, scale: Scale) -> Self {
        self.insert(name, scale);
        self
    }

    /// Remove a scale by name. Returns the removed scale if present.
    pub fn remove(&mut self, name: &str) -> Option<Scale> {
        self.scales.remove(name)
    }

    /// Read a scale by name.
    pub fn get(&self, name: &str) -> Option<&Scale> {
        self.scales.get(name)
    }

    /// Mutable read by name — used by the orchestrator's `update_scale`.
    #[allow(dead_code)] // wired through PlotComposition in Phase 7
    pub(crate) fn get_mut(&mut self, name: &str) -> Option<&mut Scale> {
        self.scales.get_mut(name)
    }

    /// Iterate `(name, scale)` pairs. Order is unspecified.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Scale)> + '_ {
        self.scales.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Iterate just the registered names. Order is unspecified.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.scales.keys().map(|s| s.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.scales.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.scales.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scales.is_empty()
    }
}

// ─── Free-function constructors (re-exported as `plot::scale::*`) ────────────

/// Continuous scale over a closed domain. `T` is anything that converts
/// into a [`Value`] whose `as_number()` projection yields a finite f64 —
/// `f64`, `f32`, `i32`, `i64`, and the temporal newtypes
/// ([`Date`](crate::plot::value::Date),
/// [`DateTime`](crate::plot::value::DateTime),
/// [`Time`](crate::plot::value::Time),
/// [`Duration`](crate::plot::value::Duration)). Temporal endpoints project
/// to their canonical f64 unit (days / microseconds) and the tick
/// formatter renders them in calendar form.
///
/// ```ignore
/// scale::continuous(0.0 ..= 100.0)
/// scale::continuous(Date::from_ymd(2024,1,1) ..= Date::from_ymd(2024,12,31))
/// ```
pub fn continuous<T>(domain: RangeInclusive<T>) -> Scale
where
    T: Into<Value> + Copy,
{
    Scale::new(ScaleType::continuous()).domain_continuous(*domain.start(), *domain.end())
}

/// Discrete scale over an explicit list of category values.
pub fn discrete(domain: impl IntoIterator<Item = Value>) -> Scale {
    Scale::new(ScaleType::discrete()).domain_discrete(domain)
}

/// Binned continuous scale. `domain` is the overall range (numeric or
/// temporal, same `T: Into<Value>` story as [`continuous`]); `edges` is
/// the list of bin boundaries in the projected f64 space (must be
/// strictly increasing, length ≥ 2). The bin count is `edges.len() - 1`.
pub fn binned<T>(domain: RangeInclusive<T>, edges: Vec<f64>) -> Scale
where
    T: Into<Value> + Copy,
{
    Scale::new(ScaleType::binned())
        .domain_continuous(*domain.start(), *domain.end())
        .range_numbers(edges)
}

/// Identity scale — input passes through unchanged.
pub fn identity() -> Scale {
    Scale::new(ScaleType::identity())
}

/// Ordinal scale over an ordered category list. The output range is
/// interpolated when set (see [`ScaleTypeKind::Ordinal`] for semantics) —
/// numeric and color gradients both work via `.range_numbers(...)` /
/// `.range_colors(...)`. Returns an unconfigured scale (no output range);
/// chain `.range_*(…)` to define the gradient.
pub fn ordinal(domain: impl IntoIterator<Item = impl Into<Value>>) -> Scale {
    Scale::new(ScaleType::ordinal()).domain_discrete(domain.into_iter().map(Into::into))
}

/// Convenience: build a continuous scale whose domain is set from the
/// numeric extent of a `DataColumn`. Useful for quick "auto-fit" cases
/// where the user hasn't specified a domain explicitly.
///
/// Returns an unconfigured continuous scale if the column is non-numeric
/// or empty.
pub fn continuous_from_data(col: &DataColumn) -> Scale {
    let scale = Scale::new(ScaleType::continuous());
    if col.is_empty() {
        return scale;
    }
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for i in 0..col.len() {
        if let Some(n) = col.get(i).as_number() {
            if n.is_finite() {
                lo = lo.min(n);
                hi = hi.max(n);
            }
        }
    }
    if lo.is_finite() && hi.is_finite() && lo <= hi {
        scale.domain_continuous(lo, hi)
    } else {
        scale
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64, msg: &str) {
        assert!((a - b).abs() < tol, "{msg}: {a} ≠ {b}");
    }

    // ── Continuous ──

    #[test]
    fn continuous_map_normalised() {
        let s = continuous(0.0..=10.0);
        // No output range → returns [0, 1] fraction.
        approx(
            s.map(&Value::Number(0.0)).as_number().unwrap(),
            0.0,
            1e-12,
            "lo",
        );
        approx(
            s.map(&Value::Number(5.0)).as_number().unwrap(),
            0.5,
            1e-12,
            "mid",
        );
        approx(
            s.map(&Value::Number(10.0)).as_number().unwrap(),
            1.0,
            1e-12,
            "hi",
        );
    }

    #[test]
    fn continuous_extrapolates_outside_domain() {
        // No OOB strategy in v1 — input below/above domain extrapolates.
        let s = continuous(0.0..=10.0);
        approx(
            s.map(&Value::Number(-5.0)).as_number().unwrap(),
            -0.5,
            1e-12,
            "below",
        );
        approx(
            s.map(&Value::Number(15.0)).as_number().unwrap(),
            1.5,
            1e-12,
            "above",
        );
    }

    #[test]
    fn continuous_with_numeric_range() {
        let s = continuous(0.0..=1.0).range_numbers([2.0, 12.0]);
        approx(
            s.map(&Value::Number(0.0)).as_number().unwrap(),
            2.0,
            1e-12,
            "lo",
        );
        approx(
            s.map(&Value::Number(0.5)).as_number().unwrap(),
            7.0,
            1e-12,
            "mid",
        );
        approx(
            s.map(&Value::Number(1.0)).as_number().unwrap(),
            12.0,
            1e-12,
            "hi",
        );
    }

    #[test]
    fn continuous_with_degenerate_domain() {
        // domain_continuous(5, 5) — span is zero. Should not divide by zero;
        // map collapses to 0.0 (or the lower output bound, if a range is set).
        let s = continuous(5.0..=5.0);
        assert_eq!(s.map(&Value::Number(5.0)).as_number(), Some(0.0));
    }

    #[test]
    fn continuous_non_numeric_input_returns_null() {
        let s = continuous(0.0..=10.0);
        assert!(s.map(&Value::Null).is_null());
        assert!(s.map(&Value::String("nope".into())).is_null());
    }

    #[test]
    fn continuous_breaks_use_extended() {
        let s = continuous(0.0..=10.0);
        let bs = s.breaks(5);
        // Same shape as extended_breaks(0, 10, 5).
        assert!(bs.len() >= 4 && bs.len() <= 7);
        assert!(bs.first().unwrap().key_eq(&Value::Number(0.0)));
        assert!(bs.last().unwrap().key_eq(&Value::Number(10.0)));
    }

    // ── Discrete / Ordinal ──

    #[test]
    fn discrete_band_centres_no_range() {
        let s = discrete(["a", "b", "c"].into_iter().map(Into::into));
        approx(
            s.map(&Value::from("a")).as_number().unwrap(),
            1.0 / 6.0,
            1e-12,
            "a",
        );
        approx(
            s.map(&Value::from("b")).as_number().unwrap(),
            0.5,
            1e-12,
            "b",
        );
        approx(
            s.map(&Value::from("c")).as_number().unwrap(),
            5.0 / 6.0,
            1e-12,
            "c",
        );
    }

    #[test]
    fn discrete_unknown_category_is_null() {
        let s = discrete(["a", "b"].into_iter().map(Into::into));
        assert!(s.map(&Value::from("missing")).is_null());
    }

    #[test]
    fn discrete_band_width() {
        let s = discrete(["a", "b", "c", "d"].into_iter().map(Into::into));
        approx(s.band_width(), 0.25, 1e-12, "1/4");
    }

    #[test]
    fn ordinal_color_round_trip() {
        let red = Color::new([1.0, 0.0, 0.0, 1.0]);
        let green = Color::new([0.0, 1.0, 0.0, 1.0]);
        let blue = Color::new([0.0, 0.0, 1.0, 1.0]);
        let s = ordinal(["A", "B", "C"]).range_colors([red, green, blue]);
        assert_eq!(s.map(&Value::from("A")).as_color(), Some(red));
        assert_eq!(s.map(&Value::from("B")).as_color(), Some(green));
        assert_eq!(s.map(&Value::from("C")).as_color(), Some(blue));
        assert!(s.map(&Value::from("D")).is_null());
    }

    #[test]
    fn ordinal_with_numeric_range_returns_pt() {
        // No dedicated `ordinal_size` — users compose via the builder.
        let s = Scale::new(ScaleType::ordinal())
            .domain_discrete(["S", "M", "L"].into_iter().map(Into::into))
            .range_numbers([4.0, 8.0, 12.0]);
        assert_eq!(s.map(&Value::from("S")).as_number(), Some(4.0));
        assert_eq!(s.map(&Value::from("L")).as_number(), Some(12.0));
    }

    #[test]
    fn ordinal_color_interpolates_when_stops_lt_levels() {
        // 4 ordered levels mapped through a 2-stop gradient → endpoints
        // hit exactly; intermediate levels land at interpolated points.
        let red = Color::new([1.0, 0.0, 0.0, 1.0]);
        let blue = Color::new([0.0, 0.0, 1.0, 1.0]);
        let s = ordinal(["L1", "L2", "L3", "L4"]).range_colors([red, blue]);
        // L1 → t=0 → red
        assert_eq!(s.map(&Value::from("L1")).as_color(), Some(red));
        // L4 → t=1 → blue
        assert_eq!(s.map(&Value::from("L4")).as_color(), Some(blue));
        // L2 → t=1/3 → ~33% along red→blue gradient
        let c2 = s.map(&Value::from("L2")).as_color().unwrap();
        approx(c2.components[0] as f64, 2.0 / 3.0, 1e-5, "L2.r");
        approx(c2.components[2] as f64, 1.0 / 3.0, 1e-5, "L2.b");
        // L3 → t=2/3 → ~67% along red→blue gradient
        let c3 = s.map(&Value::from("L3")).as_color().unwrap();
        approx(c3.components[0] as f64, 1.0 / 3.0, 1e-5, "L3.r");
        approx(c3.components[2] as f64, 2.0 / 3.0, 1e-5, "L3.b");
    }

    #[test]
    fn ordinal_numeric_interpolates_when_stops_lt_levels() {
        let s = Scale::new(ScaleType::ordinal())
            .domain_discrete(["A", "B", "C", "D", "E"].into_iter().map(Into::into))
            .range_numbers([2.0, 10.0]);
        // 5 levels, 2 stops → A=2, B=4, C=6, D=8, E=10.
        approx(
            s.map(&Value::from("A")).as_number().unwrap(),
            2.0,
            1e-12,
            "A",
        );
        approx(
            s.map(&Value::from("B")).as_number().unwrap(),
            4.0,
            1e-12,
            "B",
        );
        approx(
            s.map(&Value::from("C")).as_number().unwrap(),
            6.0,
            1e-12,
            "C",
        );
        approx(
            s.map(&Value::from("D")).as_number().unwrap(),
            8.0,
            1e-12,
            "D",
        );
        approx(
            s.map(&Value::from("E")).as_number().unwrap(),
            10.0,
            1e-12,
            "E",
        );
    }

    #[test]
    fn continuous_with_color_range() {
        // Continuous now supports color interpolation via the shared
        // engine. (Previously this returned Null.)
        let red = Color::new([1.0, 0.0, 0.0, 1.0]);
        let blue = Color::new([0.0, 0.0, 1.0, 1.0]);
        let s = continuous(0.0..=10.0).range_colors([red, blue]);
        assert_eq!(s.map(&Value::Number(0.0)).as_color(), Some(red));
        assert_eq!(s.map(&Value::Number(10.0)).as_color(), Some(blue));
        let mid = s.map(&Value::Number(5.0)).as_color().unwrap();
        approx(mid.components[0] as f64, 0.5, 1e-5, "mid.r");
        approx(mid.components[2] as f64, 0.5, 1e-5, "mid.b");
    }

    #[test]
    fn continuous_piecewise_three_stops() {
        // [2, 8, 12] with 3 stops → segment 0 is [2, 8] over t∈[0, 0.5];
        // segment 1 is [8, 12] over t∈[0.5, 1].
        let s = continuous(0.0..=1.0).range_numbers([2.0, 8.0, 12.0]);
        approx(
            s.map(&Value::Number(0.0)).as_number().unwrap(),
            2.0,
            1e-12,
            "0",
        );
        approx(
            s.map(&Value::Number(0.25)).as_number().unwrap(),
            5.0,
            1e-12,
            "0.25",
        );
        approx(
            s.map(&Value::Number(0.5)).as_number().unwrap(),
            8.0,
            1e-12,
            "0.5",
        );
        approx(
            s.map(&Value::Number(0.75)).as_number().unwrap(),
            10.0,
            1e-12,
            "0.75",
        );
        approx(
            s.map(&Value::Number(1.0)).as_number().unwrap(),
            12.0,
            1e-12,
            "1",
        );
    }

    #[test]
    fn ordinal_with_matched_stops_is_one_to_one() {
        // Domain and output range of the same length → ordinal coincides
        // with discrete-style one-to-one mapping.
        let s = Scale::new(ScaleType::ordinal())
            .domain_discrete(["S", "M", "L"].into_iter().map(Into::into))
            .range_numbers([4.0, 8.0, 12.0]);
        assert_eq!(s.map(&Value::from("S")).as_number(), Some(4.0));
        assert_eq!(s.map(&Value::from("M")).as_number(), Some(8.0));
        assert_eq!(s.map(&Value::from("L")).as_number(), Some(12.0));
    }

    #[test]
    fn discrete_breaks_return_domain() {
        let s = discrete(["a", "b", "c"].into_iter().map(Into::into));
        let bs = s.breaks(0);
        assert_eq!(bs.len(), 3);
        assert!(bs[0].key_eq(&Value::from("a")));
        assert!(bs[2].key_eq(&Value::from("c")));
    }

    // ── Binned ──

    #[test]
    fn binned_map() {
        // Domain [0, 10] with edges [0, 2, 5, 10] → three bins of
        // widths 2, 3, 5. Each value snaps to its bin's centre,
        // projected proportionally onto the panel:
        //   bin 0 centre = 1   → 1 / 10 = 0.1
        //   bin 1 centre = 3.5 → 3.5 / 10 = 0.35
        //   bin 2 centre = 7.5 → 7.5 / 10 = 0.75
        let s = binned(0.0..=10.0, vec![0.0, 2.0, 5.0, 10.0]);
        approx(
            s.map(&Value::Number(1.0)).as_number().unwrap(),
            0.1,
            1e-12,
            "bin 0",
        );
        approx(
            s.map(&Value::Number(3.0)).as_number().unwrap(),
            0.35,
            1e-12,
            "bin 1",
        );
        approx(
            s.map(&Value::Number(8.0)).as_number().unwrap(),
            0.75,
            1e-12,
            "bin 2",
        );
        // Boundary: 2.0 → bin 1 centre = 0.35.
        approx(
            s.map(&Value::Number(2.0)).as_number().unwrap(),
            0.35,
            1e-12,
            "boundary",
        );
        // Top edge: 10.0 → bin 2 centre = 0.75 (right-closed).
        approx(
            s.map(&Value::Number(10.0)).as_number().unwrap(),
            0.75,
            1e-12,
            "top",
        );
    }

    #[test]
    fn binned_band_width_at_per_bin() {
        // Edges [0, 2, 5, 10] → bin widths 2, 3, 5 → fractions 0.2, 0.3, 0.5.
        let s = binned(0.0..=10.0, vec![0.0, 2.0, 5.0, 10.0]);
        approx(
            s.scale_type().band_width_at(&s, &Value::Number(1.0)),
            0.2,
            1e-12,
            "bin 0 width",
        );
        approx(
            s.scale_type().band_width_at(&s, &Value::Number(3.0)),
            0.3,
            1e-12,
            "bin 1 width",
        );
        approx(
            s.scale_type().band_width_at(&s, &Value::Number(8.0)),
            0.5,
            1e-12,
            "bin 2 width",
        );
    }

    #[test]
    fn binned_map_with_offset_uses_per_bin_width() {
        // Bin 1 [2, 5] has centre 3.5 (frac 0.35) and width 3 (frac 0.3).
        // Offset +0.5 → 0.35 + 0.5 * 0.3 = 0.5 (right edge of bin 1).
        let s = binned(0.0..=10.0, vec![0.0, 2.0, 5.0, 10.0]);
        approx(
            s.map_with_offset(&Value::Number(3.0), 0.5)
                .as_number()
                .unwrap(),
            0.5,
            1e-12,
            "bin 1 right edge",
        );
        // Bin 2 [5, 10] has centre 7.5 (frac 0.75) and width 5 (frac 0.5).
        // Offset -0.5 → 0.75 + (-0.5) * 0.5 = 0.5 (left edge of bin 2).
        approx(
            s.map_with_offset(&Value::Number(8.0), -0.5)
                .as_number()
                .unwrap(),
            0.5,
            1e-12,
            "bin 2 left edge",
        );
    }

    #[test]
    fn binned_out_of_range_is_null() {
        let s = binned(0.0..=10.0, vec![0.0, 5.0, 10.0]);
        assert!(s.map(&Value::Number(-1.0)).is_null());
        assert!(s.map(&Value::Number(11.0)).is_null());
    }

    // ── Identity ──

    #[test]
    fn identity_passes_through() {
        let s = identity();
        let c = Color::new([0.5, 0.5, 0.5, 1.0]);
        assert_eq!(s.map(&Value::Number(42.0)).as_number(), Some(42.0));
        assert_eq!(s.map(&Value::Color(c)).as_color(), Some(c));
        assert!(s.map(&Value::from("hi")).key_eq(&Value::from("hi")));
    }

    #[test]
    fn identity_passes_color_through() {
        let s = identity();
        let c = Color::new([0.25, 0.5, 0.75, 1.0]);
        assert_eq!(s.map(&Value::Color(c)).as_color(), Some(c));
    }

    // ── Temporal ──

    #[test]
    fn continuous_dates_maps_via_days() {
        use crate::plot::value::Date;
        // 2024-01-01 = day 19723; 2024-12-31 = day 20088. Range = 365 days.
        let s = continuous(Date::from_ymd(2024, 1, 1)..=Date::from_ymd(2024, 12, 31));
        let mid = Date::from_ymd(2024, 7, 1);
        let frac = s.map(&Value::Date(mid.to_days())).as_number().unwrap();
        assert!(frac > 0.0 && frac < 1.0, "mid-year frac was {frac}");
    }

    #[test]
    fn temporal_format_dates() {
        use crate::plot::value::Date;
        let s = continuous(Date::from_ymd(2024, 1, 1)..=Date::from_ymd(2024, 12, 31));
        assert_eq!(
            s.format(&Value::Date(Date::from_ymd(2024, 1, 15).to_days())),
            "2024-01-15"
        );
    }

    #[test]
    fn temporal_format_datetime() {
        use crate::plot::value::DateTime;
        let s = continuous(
            DateTime::from_ymd_hms_micros(2024, 1, 1, 0, 0, 0, 0)
                ..=DateTime::from_ymd_hms_micros(2024, 12, 31, 23, 59, 59, 0),
        );
        let dt = DateTime::from_ymd_hms_micros(2024, 6, 15, 12, 34, 56, 0);
        assert_eq!(
            s.format(&Value::DateTime(dt.to_micros())),
            "2024-06-15 12:34:56"
        );
    }

    #[test]
    fn temporal_format_time_sub_second() {
        use crate::plot::value::Time;
        let s = continuous(
            Time::from_hms_micros(0, 0, 0, 0)..=Time::from_hms_micros(23, 59, 59, 999_999),
        );
        let t = Time::from_hms_micros(7, 8, 9, 123_000);
        assert_eq!(s.format(&Value::Time(t.to_micros())), "07:08:09.123");
        let t_exact = Time::from_hms_micros(7, 8, 9, 0);
        assert_eq!(s.format(&Value::Time(t_exact.to_micros())), "07:08:09");
    }

    #[test]
    fn binned_accepts_temporal_domain() {
        use crate::plot::value::Date;
        // Bin year 2024 into quarters by day offset from the start.
        // Quarters are NOT equal-width in days (90, 91, 92, 92 ish), so
        // proportional Binned correctly puts each bin centre at the
        // actual midpoint of its calendar range — not at 1/8, 3/8, 5/8,
        // 7/8 of the year.
        let start = Date::from_ymd(2024, 1, 1);
        let end = Date::from_ymd(2024, 12, 31);
        let q1 = Date::from_ymd(2024, 4, 1).to_days() as f64;
        let q2 = Date::from_ymd(2024, 7, 1).to_days() as f64;
        let q3 = Date::from_ymd(2024, 10, 1).to_days() as f64;
        let s = binned(
            start..=end,
            vec![start.to_days() as f64, q1, q2, q3, end.to_days() as f64],
        );
        // A January date lands in bin 0 [start, Apr 1). Expected output:
        // (bin 0 centre - start) / (end - start). The centre is the
        // midpoint between start and q1.
        let start_f = start.to_days() as f64;
        let end_f = end.to_days() as f64;
        let span = end_f - start_f;
        let expected = ((start_f + q1) * 0.5 - start_f) / span;

        let jan = Date::from_ymd(2024, 1, 15).to_days() as f64;
        let frac = s.map(&Value::Date(jan as i32)).as_number().unwrap();
        approx(frac, expected, 1e-12, "jan in bin 0 (proportional)");
    }

    #[test]
    fn temporal_format_duration() {
        let s = identity(); // format_value doesn't depend on the scale type
        assert_eq!(
            s.format(&Value::Duration(
                3 * 3600 * 1_000_000 + 25 * 60 * 1_000_000 + 12 * 1_000_000
            )),
            "3h 25m 12s"
        );
        assert_eq!(s.format(&Value::Duration(-90 * 1_000_000)), "-1m 30s");
        assert_eq!(s.format(&Value::Duration(45 * 1_000_000)), "45s");
    }

    // ── Generation counter ──

    #[test]
    fn mutation_bumps_generation() {
        let mut s = continuous(0.0..=10.0);
        let g0 = s.generation();
        s.set_domain_continuous(0.0, 20.0);
        let g1 = s.generation();
        assert!(g1 > g0);
        s.set_range_numbers(vec![0.0, 1.0]);
        let g2 = s.generation();
        assert!(g2 > g1);
    }

    #[test]
    fn builder_chaining_does_not_bump_generation() {
        // Builder methods consume self; they're construction, not
        // mutation. The generation starts at 0 and stays 0 until a
        // `set_*` is called.
        let s = continuous(0.0..=10.0)
            .range_numbers([0.0, 1.0])
            .with_transform(TransformKind::Identity);
        assert_eq!(s.generation(), 0);
    }

    // ── Auto-fit from data ──

    #[test]
    fn continuous_from_data_fits_numeric_extent() {
        let col: DataColumn = vec![1.0_f64, 3.5, -2.0, 7.0].into();
        let s = continuous_from_data(&col);
        match s.input_range() {
            Some(InputRange::Continuous { min, max }) => {
                approx(*min, -2.0, 1e-12, "min");
                approx(*max, 7.0, 1e-12, "max");
            }
            _ => panic!("expected Continuous input range"),
        }
    }

    #[test]
    fn continuous_from_data_empty_unconfigured() {
        let col: DataColumn = DataColumn::F64(vec![]);
        let s = continuous_from_data(&col);
        assert!(s.input_range().is_none());
    }
}
