//! Hephaestus's `Scale` bundle, `ScaleRegistry`, and the ggplot-style
//! free-function constructors (`scale::continuous(...)`,
//! `scale::ordinal(...)`, etc.).
//!
//! The algorithms themselves (map, breaks, band width, transform forward
//! / inverse) live in [`crate::scales`] as plain enums + free functions.
//! `Scale` is a thin bundle that holds the configured pieces and exposes
//! convenience methods that match on the enum tags and delegate.
//!
//! This file is **hephaestus-only** — `Scale`, `ScaleRegistry`, and the
//! constructors don't ship in the lift-ready scales crate. Consumers of
//! that crate roll their own bundle and call the free functions
//! directly.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::color::Color;
use crate::scales::value::{DataColumn, LinetypeStep, Value};

// Re-export scales-crate items so legacy `crate::plot::scale::*` paths
// continue to resolve. The submodules (`breaks`, `transform`, etc.) are
// re-exported wholesale; selected free functions and types are pulled to
// the top level.
pub use crate::scales::{
    binned_band_width, binned_band_width_at, binned_breaks, binned_map, breaks, chrome,
    continuous_breaks, continuous_map, continuous_minor_breaks, discrete_band_width,
    discrete_breaks, discrete_map, extended_breaks, identity_map, input, linear_breaks,
    linear_minor_breaks_between, log_minor_breaks, log_pretty_breaks, ordinal_map, output,
    scale_type, sqrt_breaks, symlog_breaks, symlog_minor_breaks, transform,
    transform_allowed_domain, transform_forward, transform_inverse, value, AxisSide, InputRange,
    LegendSide, OutputRange, ScaleTypeKind, Transform, TransformKind, DEFAULT_BREAK_COUNT,
};

// Axis / legend chrome renderers live in src/plot/chrome/ — re-exported
// here so legacy `crate::plot::scale::axis::*` paths keep working.
#[cfg(feature = "text")]
pub use crate::plot::chrome::{axis, legend};

// ─── Scale ───────────────────────────────────────────────────────────────────

/// A configurable value mapper. Bundles a [`ScaleTypeKind`] with optional
/// input/output ranges, a [`Transform`], and a monotonic generation
/// counter for invalidating downstream caches.
///
/// `Scale` is hephaestus's aggregate — the lift-ready `scales` crate
/// exposes only the underlying enums + free functions. The methods on
/// `Scale` (`map`, `breaks`, `band_width`, …) match-dispatch on the
/// scale type and delegate to those free functions.
#[derive(Clone)]
pub struct Scale {
    scale_type: ScaleTypeKind,
    transform: Transform,
    input_range: Option<InputRange>,
    output_range: Option<OutputRange>,
    /// Bumped on every mutation. Plumbed for per-channel cache
    /// invalidation; not currently consulted by the draw path.
    generation: Cell<u64>,
}

impl Scale {
    /// Build a fresh scale of the given type. Domain and range are unset
    /// until configured via the builder methods.
    pub fn new(scale_type: ScaleTypeKind) -> Self {
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
    /// `i64`, and the temporal newtypes ([`Date`](crate::scales::value::Date),
    /// [`DateTime`](crate::scales::value::DateTime),
    /// [`Time`](crate::scales::value::Time),
    /// [`Duration`](crate::scales::value::Duration)) — each projects to
    /// its canonical unit (days / microseconds). Non-numeric endpoints
    /// (`String`, `Bool`, `Color`, `Null`) panic at the call site since
    /// they have no continuous ordering.
    pub fn domain_continuous<T>(mut self, min: T, max: T) -> Self
    where
        T: Into<Value>,
    {
        let lo = endpoint_to_f64(min.into(), "domain_continuous: min");
        let hi = endpoint_to_f64(max.into(), "domain_continuous: max");
        self.input_range = Some(InputRange::Continuous { min: lo, max: hi });
        self
    }

    /// Configure a discrete domain — explicit ordered list of input
    /// values. Used by [`ScaleTypeKind::Discrete`] and
    /// [`ScaleTypeKind::Ordinal`].
    pub fn domain_discrete(mut self, values: impl IntoIterator<Item = Value>) -> Self {
        self.input_range = Some(InputRange::Discrete(values.into_iter().collect()));
        self
    }

    /// Configure a numeric output range (pt for absolute sizes;
    /// unitless otherwise).
    pub fn range_numbers(mut self, vs: impl IntoIterator<Item = f64>) -> Self {
        self.output_range = Some(OutputRange::Numbers(vs.into_iter().collect()));
        self
    }

    /// Configure a colour output range.
    pub fn range_colors(mut self, vs: impl IntoIterator<Item = Color>) -> Self {
        self.output_range = Some(OutputRange::Colors(vs.into_iter().collect()));
        self
    }

    /// Configure a string output range.
    pub fn range_strings(mut self, vs: impl IntoIterator<Item = Arc<str>>) -> Self {
        self.output_range = Some(OutputRange::Strings(vs.into_iter().collect()));
        self
    }

    /// Configure a linetype output range. Each entry is a
    /// [`LinetypeStep`] pattern (alternating Dash|Marker and Gap; empty =
    /// solid). Pairs naturally with the named helpers in
    /// [`crate::plot::geom::linetype`].
    pub fn range_linetypes(mut self, vs: impl IntoIterator<Item = Arc<[LinetypeStep]>>) -> Self {
        self.output_range = Some(OutputRange::Linetypes(vs.into_iter().collect()));
        self
    }

    /// Configure the scale's [`Transform`]. Currently only
    /// [`TransformKind::Identity`] is implemented.
    pub fn with_transform(mut self, t: TransformKind) -> Self {
        self.transform = Transform::of(t);
        self
    }

    // ── Mutators (`&mut self`; bump generation) ──

    /// Replace the continuous domain in place. Bumps the generation
    /// counter.
    pub fn set_domain_continuous<T>(&mut self, min: T, max: T)
    where
        T: Into<Value>,
    {
        let lo = endpoint_to_f64(min.into(), "set_domain_continuous: min");
        let hi = endpoint_to_f64(max.into(), "set_domain_continuous: max");
        self.input_range = Some(InputRange::Continuous { min: lo, max: hi });
        self.bump_generation();
    }

    /// Replace the discrete domain in place. Bumps the generation
    /// counter.
    pub fn set_domain_discrete(&mut self, values: Vec<Value>) {
        self.input_range = Some(InputRange::Discrete(values));
        self.bump_generation();
    }

    /// Replace the numeric output range in place. Bumps the generation
    /// counter.
    pub fn set_range_numbers(&mut self, vs: Vec<f64>) {
        self.output_range = Some(OutputRange::Numbers(vs));
        self.bump_generation();
    }

    /// Replace the colour output range in place. Bumps the generation
    /// counter.
    pub fn set_range_colors(&mut self, vs: Vec<Color>) {
        self.output_range = Some(OutputRange::Colors(vs));
        self.bump_generation();
    }

    /// Replace the string output range in place. Bumps the generation
    /// counter.
    pub fn set_range_strings(&mut self, vs: Vec<Arc<str>>) {
        self.output_range = Some(OutputRange::Strings(vs));
        self.bump_generation();
    }

    /// Replace the linetype output range in place. Bumps the generation
    /// counter.
    pub fn set_range_linetypes(&mut self, vs: Vec<Arc<[LinetypeStep]>>) {
        self.output_range = Some(OutputRange::Linetypes(vs));
        self.bump_generation();
    }

    /// Replace the transform in place. Bumps the generation counter.
    pub fn set_transform(&mut self, t: TransformKind) {
        self.transform = Transform::of(t);
        self.bump_generation();
    }

    fn bump_generation(&mut self) {
        self.generation.set(self.generation.get() + 1);
    }

    // ── Operations ──

    /// Map an input value to its scaled output. Dispatches on
    /// [`Self::scale_type_kind`] into the matching free function from
    /// [`crate::scales`].
    pub fn map(&self, input: &Value) -> Value {
        match self.scale_type {
            ScaleTypeKind::Continuous => continuous_map(
                input,
                self.input_range.as_ref(),
                self.output_range.as_ref(),
                &self.transform,
            ),
            ScaleTypeKind::Discrete => {
                discrete_map(input, self.input_range.as_ref(), self.output_range.as_ref())
            }
            ScaleTypeKind::Ordinal => {
                ordinal_map(input, self.input_range.as_ref(), self.output_range.as_ref())
            }
            ScaleTypeKind::Binned => {
                binned_map(input, self.input_range.as_ref(), self.output_range.as_ref())
            }
            ScaleTypeKind::Identity => identity_map(input),
        }
    }

    /// Like [`Self::map`] but additionally applies a band-fraction offset
    /// in the scale's band space. The offset is multiplied by the band
    /// width of the bin containing `input` (see
    /// [`Self::band_width_at`]) before being added to the nominal mapped
    /// fraction.
    ///
    /// - `band_offset` units: fraction of the input's own band width.
    ///   `0.0` is the band centre; `±0.5` reaches the band's left/right
    ///   edge. The offset isn't clamped — values outside `[-0.5, 0.5]`
    ///   extend past the band into neighbouring slots.
    /// - Continuous scales return `0.0` from `band_width_at`, so the
    ///   offset is a no-op there.
    /// - Non-numeric `map()` outputs (e.g. Color) ignore the offset and
    ///   pass through unchanged.
    pub fn map_with_offset(&self, input: &Value, band_offset: f64) -> Value {
        let base = self.map(input);
        if band_offset == 0.0 {
            return base;
        }
        match base {
            Value::Number(f) => {
                let bw = self.band_width_at(input);
                Value::Number(f + band_offset * bw)
            }
            other => other,
        }
    }

    /// Tick / category positions in **input** space. `n` is a target for
    /// continuous scales; discrete / ordinal ignore it and return every
    /// domain entry.
    pub fn breaks(&self, n: usize) -> Vec<Value> {
        match self.scale_type {
            ScaleTypeKind::Continuous => {
                continuous_breaks(self.input_range.as_ref(), &self.transform, n)
            }
            ScaleTypeKind::Discrete | ScaleTypeKind::Ordinal => {
                discrete_breaks(self.input_range.as_ref())
            }
            ScaleTypeKind::Binned => binned_breaks(self.output_range.as_ref()),
            ScaleTypeKind::Identity => Vec::new(),
        }
    }

    /// Minor (sub-tick) positions in input space. Empty for non-continuous
    /// scale types. For continuous scales the algorithm is transform-
    /// aware: log scales emit geometric 2..9 between decades; sqrt /
    /// identity / etc. emit one midpoint between consecutive majors.
    pub fn minor_breaks(&self, n: usize) -> Vec<Value> {
        match self.scale_type {
            ScaleTypeKind::Continuous => {
                let majors = self.breaks(n);
                continuous_minor_breaks(self.input_range.as_ref(), &self.transform, &majors)
            }
            _ => Vec::new(),
        }
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
        match self.scale_type {
            ScaleTypeKind::Continuous => 0.0,
            ScaleTypeKind::Discrete | ScaleTypeKind::Ordinal => {
                discrete_band_width(self.input_range.as_ref())
            }
            ScaleTypeKind::Binned => binned_band_width(self.output_range.as_ref()),
            ScaleTypeKind::Identity => 0.0,
        }
    }

    /// Width (as panel fraction) of the band containing `input`. For
    /// uniform-band scales this matches [`Self::band_width`]; for
    /// [`ScaleTypeKind::Binned`] with non-uniform widths it returns the
    /// specific bin's width. Used by [`Self::map_with_offset`].
    pub fn band_width_at(&self, input: &Value) -> f64 {
        match self.scale_type {
            ScaleTypeKind::Binned => {
                binned_band_width_at(input, self.input_range.as_ref(), self.output_range.as_ref())
            }
            _ => self.band_width(),
        }
    }

    // ── Accessors ──

    /// Discriminator for this scale's family.
    pub fn scale_type_kind(&self) -> ScaleTypeKind {
        self.scale_type
    }

    /// Borrow the [`Transform`].
    pub fn transform(&self) -> &Transform {
        &self.transform
    }

    /// Borrow the configured input domain, if any.
    pub fn input_range(&self) -> Option<&InputRange> {
        self.input_range.as_ref()
    }

    /// Borrow the configured output range, if any.
    pub fn output_range(&self) -> Option<&OutputRange> {
        self.output_range.as_ref()
    }

    /// Monotonic counter incremented on every mutation. Plumbed for
    /// downstream scaled-output caching to invalidate per-channel
    /// caches without comparing values; not currently consulted.
    #[allow(dead_code)] // wired through builder/mutators; consumers TBD
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
    use crate::scales::value::Date;
    match v {
        Value::Number(n) => format!("{n}"),
        Value::String(s) => (**s).to_string(),
        Value::Bool(b) => format!("{b}"),
        Value::Null => "NA".to_string(),
        Value::Color(c) => format!("{c:?}"),
        Value::Date(d) => {
            let (y, m, dd) = Date::from_days(*d).to_ymd();
            format!("{y:04}-{m:02}-{dd:02}")
        }
        Value::DateTime(us) => {
            let dt = crate::scales::value::DateTime::from_micros(*us);
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
        Value::Linetype(p) => {
            if p.is_empty() {
                "solid".to_string()
            } else {
                let parts: Vec<String> = p
                    .iter()
                    .map(|s| match s {
                        LinetypeStep::Dash(f) => format!("dash({f})"),
                        LinetypeStep::Gap(f) => format!("gap({f})"),
                        LinetypeStep::Marker(name) => format!("marker({name:?})"),
                    })
                    .collect();
                format!("[{}]", parts.join(", "))
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

/// Named registry of scales — the single source of truth for scale state
/// in a [`PlotComposition`](crate::plot) orchestrator. Plots reference
/// scales by name through their channel bindings; the registry owns the
/// `Scale` instances.
///
/// A thin `HashMap` wrapper. The orchestrator owns the registry and
/// exposes mutators that flip dirty flags; callers building one by hand
/// (e.g. for tests or orchestrator-free renders) can use the
/// [`Self::insert`] / [`Self::remove`] surface directly.
#[derive(Default, Clone, Debug)]
pub struct ScaleRegistry {
    scales: HashMap<String, Scale>,
}

impl ScaleRegistry {
    /// Empty registry — no scales registered.
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

    /// True if a scale with `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.scales.contains_key(name)
    }

    /// Number of registered scales.
    pub fn len(&self) -> usize {
        self.scales.len()
    }

    /// True when no scales are registered.
    pub fn is_empty(&self) -> bool {
        self.scales.is_empty()
    }
}

// ─── Free-function constructors ──────────────────────────────────────────────

/// Continuous scale over a closed domain. `T` is anything that converts
/// into a [`Value`] whose `as_number()` projection yields a finite f64 —
/// `f64`, `f32`, `i32`, `i64`, and the temporal newtypes ([`Date`],
/// [`DateTime`], [`Time`], [`Duration`]).
///
/// ```ignore
/// scale::continuous(0.0 ..= 100.0)
/// scale::continuous(Date::from_ymd(2024,1,1) ..= Date::from_ymd(2024,12,31))
/// ```
pub fn continuous<T>(domain: RangeInclusive<T>) -> Scale
where
    T: Into<Value> + Copy,
{
    Scale::new(ScaleTypeKind::Continuous).domain_continuous(*domain.start(), *domain.end())
}

/// Discrete scale over an explicit list of category values.
pub fn discrete(domain: impl IntoIterator<Item = Value>) -> Scale {
    Scale::new(ScaleTypeKind::Discrete).domain_discrete(domain)
}

/// Binned continuous scale. `domain` is the overall range; `edges` is the
/// list of bin boundaries (strictly increasing, length ≥ 2). The bin
/// count is `edges.len() - 1`.
pub fn binned<T>(domain: RangeInclusive<T>, edges: Vec<f64>) -> Scale
where
    T: Into<Value> + Copy,
{
    Scale::new(ScaleTypeKind::Binned)
        .domain_continuous(*domain.start(), *domain.end())
        .range_numbers(edges)
}

/// Identity scale — input passes through unchanged.
pub fn identity() -> Scale {
    Scale::new(ScaleTypeKind::Identity)
}

/// Ordinal scale over an ordered category list. The output range is
/// interpolated when set (see [`ScaleTypeKind::Ordinal`] for semantics).
pub fn ordinal(domain: impl IntoIterator<Item = impl Into<Value>>) -> Scale {
    Scale::new(ScaleTypeKind::Ordinal).domain_discrete(domain.into_iter().map(Into::into))
}

/// Convenience: build a continuous scale whose domain is set from the
/// numeric extent of a `DataColumn`. Useful for quick "auto-fit" cases
/// where the user hasn't specified a domain explicitly.
///
/// Returns an unconfigured continuous scale if the column is non-numeric
/// or empty.
pub fn continuous_from_data(col: &DataColumn) -> Scale {
    let scale = Scale::new(ScaleTypeKind::Continuous);
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
    use crate::scales::value::{Date, DateTime, Time};

    fn approx(a: f64, b: f64, tol: f64, msg: &str) {
        assert!((a - b).abs() < tol, "{msg}: {a} ≠ {b}");
    }

    // ── Continuous ──

    #[test]
    fn continuous_map_normalised() {
        let s = continuous(0.0..=10.0);
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
        let s = Scale::new(ScaleTypeKind::Ordinal)
            .domain_discrete(["S", "M", "L"].into_iter().map(Into::into))
            .range_numbers([4.0, 8.0, 12.0]);
        assert_eq!(s.map(&Value::from("S")).as_number(), Some(4.0));
        assert_eq!(s.map(&Value::from("L")).as_number(), Some(12.0));
    }

    fn lt_dash_gap(d: f64, g: f64) -> Arc<[LinetypeStep]> {
        Arc::from(vec![LinetypeStep::Dash(d), LinetypeStep::Gap(g)])
    }

    fn lt_solid() -> Arc<[LinetypeStep]> {
        Arc::from(Vec::<LinetypeStep>::new())
    }

    #[test]
    fn discrete_with_linetype_range_steps_by_index() {
        let solid = lt_solid();
        let dashed = lt_dash_gap(8.0, 4.0);
        let dotted = lt_dash_gap(2.0, 3.0);
        let s = discrete(["A", "B", "C"].into_iter().map(Into::into)).range_linetypes([
            solid.clone(),
            dashed.clone(),
            dotted.clone(),
        ]);
        assert!(s
            .map(&Value::from("A"))
            .key_eq(&Value::Linetype(solid.clone())));
        assert!(s
            .map(&Value::from("B"))
            .key_eq(&Value::Linetype(dashed.clone())));
        assert!(s.map(&Value::from("C")).key_eq(&Value::Linetype(dotted)));
        assert!(s.map(&Value::from("D")).is_null());
    }

    #[test]
    fn ordinal_with_linetype_range_steps_by_nearest_index() {
        let solid = lt_solid();
        let dashed = lt_dash_gap(8.0, 4.0);
        let s = ordinal(["L1", "L2", "L3", "L4"]).range_linetypes([solid.clone(), dashed.clone()]);
        assert!(s
            .map(&Value::from("L1"))
            .key_eq(&Value::Linetype(solid.clone())));
        assert!(s.map(&Value::from("L2")).key_eq(&Value::Linetype(solid)));
        assert!(s
            .map(&Value::from("L3"))
            .key_eq(&Value::Linetype(dashed.clone())));
        assert!(s.map(&Value::from("L4")).key_eq(&Value::Linetype(dashed)));
    }

    #[test]
    fn continuous_with_linetype_range_steps() {
        let solid = lt_solid();
        let dashed = lt_dash_gap(8.0, 4.0);
        let dotted = lt_dash_gap(2.0, 3.0);
        let s =
            continuous(0.0..=10.0).range_linetypes([solid.clone(), dashed.clone(), dotted.clone()]);
        assert!(s
            .map(&Value::Number(0.0))
            .key_eq(&Value::Linetype(solid.clone())));
        assert!(s.map(&Value::Number(5.0)).key_eq(&Value::Linetype(dashed)));
        assert!(s.map(&Value::Number(10.0)).key_eq(&Value::Linetype(dotted)));
    }

    #[test]
    fn ordinal_color_interpolates_when_stops_lt_levels() {
        let red = Color::new([1.0, 0.0, 0.0, 1.0]);
        let blue = Color::new([0.0, 0.0, 1.0, 1.0]);
        let s = ordinal(["L1", "L2", "L3", "L4"]).range_colors([red, blue]);
        assert_eq!(s.map(&Value::from("L1")).as_color(), Some(red));
        assert_eq!(s.map(&Value::from("L4")).as_color(), Some(blue));
        let c2 = s.map(&Value::from("L2")).as_color().unwrap();
        approx(c2.components[0] as f64, 2.0 / 3.0, 1e-5, "L2.r");
        approx(c2.components[2] as f64, 1.0 / 3.0, 1e-5, "L2.b");
        let c3 = s.map(&Value::from("L3")).as_color().unwrap();
        approx(c3.components[0] as f64, 1.0 / 3.0, 1e-5, "L3.r");
        approx(c3.components[2] as f64, 2.0 / 3.0, 1e-5, "L3.b");
    }

    #[test]
    fn ordinal_numeric_interpolates_when_stops_lt_levels() {
        let s = Scale::new(ScaleTypeKind::Ordinal)
            .domain_discrete(["A", "B", "C", "D", "E"].into_iter().map(Into::into))
            .range_numbers([2.0, 10.0]);
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
        let s = Scale::new(ScaleTypeKind::Ordinal)
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
    fn binned_map_proportional() {
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
        approx(
            s.map(&Value::Number(2.0)).as_number().unwrap(),
            0.35,
            1e-12,
            "boundary",
        );
        approx(
            s.map(&Value::Number(10.0)).as_number().unwrap(),
            0.75,
            1e-12,
            "top",
        );
    }

    #[test]
    fn binned_band_width_at_per_bin() {
        let s = binned(0.0..=10.0, vec![0.0, 2.0, 5.0, 10.0]);
        approx(s.band_width_at(&Value::Number(1.0)), 0.2, 1e-12, "bin 0");
        approx(s.band_width_at(&Value::Number(3.0)), 0.3, 1e-12, "bin 1");
        approx(s.band_width_at(&Value::Number(8.0)), 0.5, 1e-12, "bin 2");
    }

    #[test]
    fn binned_map_with_offset_uses_per_bin_width() {
        let s = binned(0.0..=10.0, vec![0.0, 2.0, 5.0, 10.0]);
        approx(
            s.map_with_offset(&Value::Number(3.0), 0.5)
                .as_number()
                .unwrap(),
            0.5,
            1e-12,
            "bin 1 right edge",
        );
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
        let s = continuous(Date::from_ymd(2024, 1, 1)..=Date::from_ymd(2024, 12, 31));
        let mid = Date::from_ymd(2024, 7, 1);
        let frac = s.map(&Value::Date(mid.to_days())).as_number().unwrap();
        assert!(frac > 0.0 && frac < 1.0, "mid-year frac was {frac}");
    }

    #[test]
    fn temporal_format_dates() {
        let s = continuous(Date::from_ymd(2024, 1, 1)..=Date::from_ymd(2024, 12, 31));
        assert_eq!(
            s.format(&Value::Date(Date::from_ymd(2024, 1, 15).to_days())),
            "2024-01-15"
        );
    }

    #[test]
    fn temporal_format_datetime() {
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
        let start = Date::from_ymd(2024, 1, 1);
        let end = Date::from_ymd(2024, 12, 31);
        let q1 = Date::from_ymd(2024, 4, 1).to_days() as f64;
        let q2 = Date::from_ymd(2024, 7, 1).to_days() as f64;
        let q3 = Date::from_ymd(2024, 10, 1).to_days() as f64;
        let s = binned(
            start..=end,
            vec![start.to_days() as f64, q1, q2, q3, end.to_days() as f64],
        );
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
        let s = identity();
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

    // ── Transform-aware breaks (E.1) ──

    #[test]
    fn log10_scale_breaks_emit_decade_powers() {
        let s = continuous(1.0..=1000.0).with_transform(TransformKind::Log10);
        let bs = s.breaks(5);
        let nums: Vec<f64> = bs.iter().filter_map(|v| v.as_number()).collect();
        for v in [1.0, 10.0, 100.0, 1000.0] {
            assert!(nums.contains(&v), "{nums:?} missing {v}");
        }
    }

    #[test]
    fn log10_scale_minor_breaks_emit_2_to_9() {
        let s = continuous(1.0..=10.0).with_transform(TransformKind::Log10);
        let m = s.minor_breaks(5);
        let nums: Vec<f64> = m.iter().filter_map(|v| v.as_number()).collect();
        for v in [2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0] {
            assert!(nums.contains(&v), "{nums:?} missing {v}");
        }
    }

    #[test]
    fn log10_scale_maps_decade_to_normalised_third() {
        // Log10 maps 1, 10, 100, 1000 to 0, 1/3, 2/3, 1 in normalised
        // panel space.
        let s = continuous(1.0..=1000.0).with_transform(TransformKind::Log10);
        approx(
            s.map(&Value::Number(1.0)).as_number().unwrap(),
            0.0,
            1e-9,
            "1",
        );
        approx(
            s.map(&Value::Number(10.0)).as_number().unwrap(),
            1.0 / 3.0,
            1e-9,
            "10",
        );
        approx(
            s.map(&Value::Number(100.0)).as_number().unwrap(),
            2.0 / 3.0,
            1e-9,
            "100",
        );
        approx(
            s.map(&Value::Number(1000.0)).as_number().unwrap(),
            1.0,
            1e-9,
            "1000",
        );
    }

    #[test]
    fn sqrt_scale_compresses_high_values() {
        let s = continuous(0.0..=100.0).with_transform(TransformKind::Sqrt);
        // Sqrt(50) / Sqrt(100) ≈ 0.707, not 0.5 like linear.
        approx(
            s.map(&Value::Number(50.0)).as_number().unwrap(),
            (50f64.sqrt()) / 10.0,
            1e-9,
            "50",
        );
    }

    #[test]
    fn sqrt_scale_minor_breaks_are_linear_midpoints() {
        let s = continuous(0.0..=100.0).with_transform(TransformKind::Sqrt);
        let m = s.minor_breaks(5);
        // Identity / sqrt / other use the linear midpoint algorithm: one
        // minor per consecutive-major interval.
        let majors = s.breaks(5);
        if majors.len() >= 2 {
            assert_eq!(m.len(), majors.len() - 1);
        }
    }

    #[test]
    fn identity_transform_breaks_match_extended() {
        // Default transform (Identity) on a continuous scale still uses
        // the Wilkinson Extended algorithm — no behavioural change from
        // E.0.
        let s = continuous(0.0..=10.0);
        let bs = s.breaks(5);
        let nums: Vec<f64> = bs.iter().filter_map(|v| v.as_number()).collect();
        // Should include 0 and 10, with evenly-spaced steps in between.
        assert!(nums.first() == Some(&0.0));
        assert!(nums.last() == Some(&10.0));
    }

    #[test]
    fn identity_transform_minor_breaks_are_midpoints() {
        let s = continuous(0.0..=10.0);
        let majors = s.breaks(5);
        let minors = s.minor_breaks(5);
        if majors.len() >= 2 {
            assert_eq!(minors.len(), majors.len() - 1);
        }
    }

    #[test]
    fn asinh_scale_handles_negative_domain() {
        let s = continuous(-10.0..=10.0).with_transform(TransformKind::Asinh);
        // map(0) should be the midpoint.
        approx(
            s.map(&Value::Number(0.0)).as_number().unwrap(),
            0.5,
            1e-9,
            "asinh midpoint",
        );
        // Negative values map to fractions < 0.5; positive > 0.5.
        assert!(s.map(&Value::Number(-1.0)).as_number().unwrap() < 0.5);
        assert!(s.map(&Value::Number(1.0)).as_number().unwrap() > 0.5);
    }

    #[test]
    fn discrete_scale_has_no_minor_breaks() {
        let s = discrete(["a", "b", "c"].into_iter().map(Into::into));
        assert!(s.minor_breaks(5).is_empty());
    }

    #[test]
    fn pseudo_log10_can_be_constructed() {
        // Previously panicked.
        let s = continuous(0.1..=1000.0).with_transform(TransformKind::PseudoLog10);
        let bs = s.breaks(5);
        assert!(!bs.is_empty());
    }
}
