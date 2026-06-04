//! Runtime-typed scalar values and columnar containers shared across the
//! plot module.
//!
//! Two parallel types live here:
//!
//! - [`Value`] — a tagged scalar (Number, String, Color, Date, …). Scales,
//!   diff lookups, and tick formatters operate on Values.
//! - [`DataColumn`] — a typed columnar container (`Vec<T>` per variant) used
//!   by geom channels. The match-on-variant happens **once per column** at
//!   the top of a draw loop, so the inner per-row code reads typed slices
//!   directly. [`DataColumn::get`] converts back to a `Value` for the
//!   non-hot paths (diff, axis ticks, chrome).
//!
//! Temporal units match Arrow defaults:
//! - [`Date`]     = days since 1970-01-01 (Arrow Date32).
//! - [`DateTime`] = microseconds since 1970-01-01T00:00:00Z (Arrow
//!   Timestamp(Microsecond, UTC)).
//! - [`Time`]     = microseconds since midnight (Arrow Time64(Microsecond)).
//! - [`Duration`] = signed microseconds (Arrow Duration(Microsecond)).
//!
//! The temporal newtypes are `repr(transparent)` so a `Vec<Date>` and a
//! `Vec<i32>` are layout-identical; we convert between them at the
//! [`DataColumn`] / [`Value`] boundary.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::color::Color;

// ─── Temporal newtypes ───────────────────────────────────────────────────────

/// A date — days since 1970-01-01 (Arrow Date32 semantics).
///
/// Constructors:
/// - [`Date::from_days`] — raw days.
/// - [`Date::from_ymd`] — year/month/day (proleptic Gregorian).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Date(pub i32);

impl Date {
    /// Construct from raw days since 1970-01-01.
    pub const fn from_days(days: i32) -> Self {
        Date(days)
    }

    /// Days since 1970-01-01.
    pub const fn to_days(self) -> i32 {
        self.0
    }

    /// Construct from proleptic Gregorian year/month/day.
    ///
    /// Uses the algorithm from
    /// <https://howardhinnant.github.io/date_algorithms.html>. Months
    /// outside `1..=12` and days outside `1..=31` are accepted as the
    /// algorithm rolls them over; pass valid calendar inputs if you want
    /// deterministic results.
    pub fn from_ymd(year: i32, month: u8, day: u8) -> Self {
        Date(days_from_civil(year, month, day))
    }

    /// Proleptic Gregorian year/month/day.
    pub fn to_ymd(self) -> (i32, u8, u8) {
        civil_from_days(self.0)
    }
}

/// A timestamp — microseconds since 1970-01-01T00:00:00Z (UTC), Arrow
/// `Timestamp(Microsecond, UTC)` semantics.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct DateTime(pub i64);

impl DateTime {
    /// Raw microseconds since 1970-01-01T00:00:00Z.
    pub const fn from_micros(us: i64) -> Self {
        DateTime(us)
    }

    pub const fn to_micros(self) -> i64 {
        self.0
    }

    /// Construct from calendar parts. Hours `0..=23`, minutes / seconds
    /// `0..=59`, microseconds `0..=999_999`. The date part follows
    /// [`Date::from_ymd`]'s tolerance.
    pub fn from_ymd_hms_micros(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        micros: u32,
    ) -> Self {
        let days = days_from_civil(year, month, day) as i64;
        let time_us = (hour as i64) * 3_600_000_000
            + (minute as i64) * 60_000_000
            + (second as i64) * 1_000_000
            + (micros as i64);
        DateTime(days * 86_400_000_000 + time_us)
    }

    /// Split into `(Date, micros_since_midnight)`. Microsecond field is
    /// always `0..86_400_000_000`.
    pub fn split(self) -> (Date, i64) {
        let day_us = 86_400_000_000_i64;
        let mut days = self.0.div_euclid(day_us);
        let mut us = self.0.rem_euclid(day_us);
        // `rem_euclid` already normalises us into [0, day_us), so days is
        // the floor — no further correction needed.
        if us < 0 {
            us += day_us;
            days -= 1;
        }
        (Date(days as i32), us)
    }
}

/// A time-of-day — microseconds since midnight (Arrow Time64(Microsecond)).
///
/// Values are conventionally in `0..86_400_000_000` but the type itself is
/// not range-restricted.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Time(pub i64);

impl Time {
    pub const fn from_micros(us: i64) -> Self {
        Time(us)
    }

    pub const fn to_micros(self) -> i64 {
        self.0
    }

    /// Construct from hour/minute/second/microsecond components.
    pub fn from_hms_micros(hour: u8, minute: u8, second: u8, micros: u32) -> Self {
        Time(
            (hour as i64) * 3_600_000_000
                + (minute as i64) * 60_000_000
                + (second as i64) * 1_000_000
                + (micros as i64),
        )
    }
}

/// A signed duration — microseconds (Arrow Duration(Microsecond)).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Duration(pub i64);

impl Duration {
    pub const fn from_micros(us: i64) -> Self {
        Duration(us)
    }

    pub const fn to_micros(self) -> i64 {
        self.0
    }

    /// Construct from whole seconds. Out-of-range values saturate at
    /// `i64::{MIN,MAX}` rather than panicking.
    pub fn from_seconds(s: i64) -> Self {
        Duration(s.saturating_mul(1_000_000))
    }
}

// ─── Linetype steps ──────────────────────────────────────────────────────────

/// One step in a linetype pattern.
///
/// Patterns are even-length sequences where even-indexed entries are
/// `Dash` or `Marker` (something to draw at the cursor) and odd-indexed
/// entries are `Gap` (an unconditional advance). See
/// [`crate::plot::geom::linetype`] for constructors that enforce the
/// alternation.
///
/// `PartialEq` follows f64's IEEE semantics for `Dash` / `Gap`
/// (NaN ≠ NaN); use [`Value::key_eq`] / `key_hash` for the canonicalised
/// diff-friendly comparison.
#[derive(Clone, Debug, PartialEq)]
pub enum LinetypeStep {
    /// Stroke a segment of this length (in pt) along the line, then
    /// advance the cursor by the same amount.
    Dash(f64),
    /// Stamp the named shape at the current cursor. The marker is
    /// assumed to occupy `linewidth` pt of arc length so the next gap
    /// measures clear space starting from the marker's trailing edge.
    Marker(Arc<str>),
    /// Advance the cursor by this many pt without drawing.
    Gap(f64),
}

impl LinetypeStep {
    /// `true` if this is a `Marker` step.
    pub fn is_marker(&self) -> bool {
        matches!(self, LinetypeStep::Marker(_))
    }
}

// ─── Value ───────────────────────────────────────────────────────────────────

/// A tagged scalar — the lingua franca that scales, diffs, and tick
/// formatters share.
///
/// `Value` is *not* `Eq` / `Hash` directly because of `f64` semantics
/// (NaN ≠ NaN, distinct -0/0 bit patterns). Use [`Value::key_eq`] and
/// [`Value::key_hash`] for deterministic equality/hashing — they
/// canonicalise NaN and -0/0.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Number(f64),
    String(Arc<str>),
    Bool(bool),
    Color(Color),

    // Temporal family. Variants store raw integers (units per the type
    // docs at the top of the module); the matching newtypes flow through
    // `From` impls below for ergonomic input.
    Date(i32),
    DateTime(i64),
    Time(i64),
    Duration(i64),

    /// A linetype pattern — a sequence of [`LinetypeStep`] entries
    /// (Dash / Marker / Gap). Even-length, with even-indexed entries =
    /// Dash | Marker and odd-indexed = Gap; empty array = solid. Carried
    /// by `Arc<[LinetypeStep]>` so cloning is cheap when the same
    /// pattern repeats across many rows (the common case under
    /// categorical scaling).
    Linetype(Arc<[LinetypeStep]>),
}

impl Value {
    /// `true` if this value is the null sentinel.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// `true` if this value is `Value::Number(n)` where `n` is finite and
    /// not NaN, or any of the temporal variants (which project to a
    /// finite f64).
    pub fn is_finite(&self) -> bool {
        match self {
            Value::Number(n) => n.is_finite(),
            Value::Date(_) | Value::DateTime(_) | Value::Time(_) | Value::Duration(_) => true,
            _ => false,
        }
    }

    /// Project numeric and temporal variants to `f64`. Temporal projections:
    /// - `Date`     → days as f64
    /// - `DateTime` → microseconds as f64
    /// - `Time`     → microseconds as f64
    /// - `Duration` → microseconds as f64
    ///
    /// Returns `None` for non-numeric, non-temporal variants.
    pub fn as_number(&self) -> Option<f64> {
        match *self {
            Value::Number(n) => Some(n),
            Value::Date(d) => Some(d as f64),
            Value::DateTime(us) => Some(us as f64),
            Value::Time(us) => Some(us as f64),
            Value::Duration(us) => Some(us as f64),
            _ => None,
        }
    }

    /// Same as [`as_number`](Self::as_number) but specifically intended for
    /// the temporal-aware code paths. Returns `None` for non-temporal,
    /// non-numeric variants. Kept separate from `as_number` so the call
    /// site documents intent.
    pub fn as_temporal_f64(&self) -> Option<f64> {
        self.as_number()
    }

    pub fn as_color(&self) -> Option<Color> {
        if let Value::Color(c) = *self {
            Some(c)
        } else {
            None
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(s) = self {
            Some(s)
        } else {
            None
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        if let Value::Bool(b) = *self {
            Some(b)
        } else {
            None
        }
    }

    /// Access the underlying linetype pattern, if this value is a
    /// `Value::Linetype`. Returns `None` for every other variant.
    pub fn as_linetype(&self) -> Option<&[LinetypeStep]> {
        if let Value::Linetype(p) = self {
            Some(p)
        } else {
            None
        }
    }

    /// Deterministic equality for diff/lookup keys. Two `Value::Number`
    /// NaNs compare equal; positive and negative zero compare equal.
    /// `Value::Linetype` compares element-wise via
    /// [`linetype_step_key_eq`].
    pub fn key_eq(&self, other: &Value) -> bool {
        use Value::*;
        match (self, other) {
            (Null, Null) => true,
            (Number(a), Number(b)) => canonical_f64_bits(*a) == canonical_f64_bits(*b),
            (String(a), String(b)) => **a == **b,
            (Bool(a), Bool(b)) => a == b,
            (Color(a), Color(b)) => *a == *b,
            (Date(a), Date(b)) => a == b,
            (DateTime(a), DateTime(b)) => a == b,
            (Time(a), Time(b)) => a == b,
            (Duration(a), Duration(b)) => a == b,
            (Linetype(a), Linetype(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(x, y)| linetype_step_key_eq(x, y))
            }
            _ => false,
        }
    }

    /// Deterministic hash for diff/lookup keys. The variant tag is mixed
    /// in so a `Number(1.0)` and a `Date(1)` produce different hashes
    /// even though they project to the same f64.
    pub fn key_hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Null => {}
            Value::Number(n) => canonical_f64_bits(*n).hash(state),
            Value::String(s) => (**s).hash(state),
            Value::Bool(b) => b.hash(state),
            Value::Color(c) => {
                // peniko::Color doesn't impl Hash; canonicalise on its
                // four f32 components.
                let [r, g, b, a] = c.components;
                canonical_f32_bits(r).hash(state);
                canonical_f32_bits(g).hash(state);
                canonical_f32_bits(b).hash(state);
                canonical_f32_bits(a).hash(state);
            }
            Value::Date(d) => d.hash(state),
            Value::DateTime(us) => us.hash(state),
            Value::Time(us) => us.hash(state),
            Value::Duration(us) => us.hash(state),
            Value::Linetype(p) => {
                (p.len() as u64).hash(state);
                for step in p.iter() {
                    linetype_step_key_hash(step, state);
                }
            }
        }
    }
}

/// Element-wise equality for [`LinetypeStep`] entries. NaN-safe / -0
/// canonicalised for the numeric variants; marker-name comparison is
/// byte-exact via the `Arc<str>` payload.
pub(crate) fn linetype_step_key_eq(a: &LinetypeStep, b: &LinetypeStep) -> bool {
    use LinetypeStep::*;
    match (a, b) {
        (Dash(x), Dash(y)) | (Gap(x), Gap(y)) => canonical_f64_bits(*x) == canonical_f64_bits(*y),
        (Marker(x), Marker(y)) => **x == **y,
        _ => false,
    }
}

/// Deterministic hash for one [`LinetypeStep`]. Mirrors
/// [`linetype_step_key_eq`].
pub(crate) fn linetype_step_key_hash<H: Hasher>(step: &LinetypeStep, state: &mut H) {
    std::mem::discriminant(step).hash(state);
    match step {
        LinetypeStep::Dash(f) | LinetypeStep::Gap(f) => canonical_f64_bits(*f).hash(state),
        LinetypeStep::Marker(s) => (**s).hash(state),
    }
}

// `From<T> for Value` for the common scalar inputs.

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Number(v)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Number(v as f64)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Number(v as f64)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        // Round-trips exactly for i64 values in [-2^53, 2^53]; loses
        // precision past that. Callers that need wider integer ranges
        // should construct `Value::DateTime` or similar temporal variants
        // directly.
        Value::Number(v as f64)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<&'static str> for Value {
    fn from(v: &'static str) -> Self {
        Value::String(Arc::from(v))
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(Arc::from(v))
    }
}
impl From<Arc<str>> for Value {
    fn from(v: Arc<str>) -> Self {
        Value::String(v)
    }
}
impl From<Color> for Value {
    fn from(v: Color) -> Self {
        Value::Color(v)
    }
}

// Temporal newtypes flow through their dedicated variants.
impl From<Date> for Value {
    fn from(v: Date) -> Self {
        Value::Date(v.0)
    }
}
impl From<DateTime> for Value {
    fn from(v: DateTime) -> Self {
        Value::DateTime(v.0)
    }
}
impl From<Time> for Value {
    fn from(v: Time) -> Self {
        Value::Time(v.0)
    }
}
impl From<Duration> for Value {
    fn from(v: Duration) -> Self {
        Value::Duration(v.0)
    }
}

// ─── DataColumn ──────────────────────────────────────────────────────────────

/// Typed columnar container — each variant holds a contiguous `Vec<T>`.
///
/// Geoms store one `DataColumn` per channel. The hot draw loop matches on
/// the variant **once** at the top, then reads the typed slice directly,
/// so per-row code stays monomorphic. [`DataColumn::get`] converts back to
/// a [`Value`] for the cold paths (diff key lookup, axis tick formatting,
/// chrome rendering).
#[derive(Clone, Debug)]
pub enum DataColumn {
    F64(Vec<f64>),
    F32(Vec<f32>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    Bool(Vec<bool>),
    String(Vec<Arc<str>>),
    Color(Vec<Color>),

    Date(Vec<i32>),
    DateTime(Vec<i64>),
    Time(Vec<i64>),
    Duration(Vec<i64>),

    /// Column of linetype patterns — one `Arc<[LinetypeStep]>` per row.
    /// Even-length per row; empty array = solid. The `Arc` lets distinct
    /// rows share a backing buffer when a column is constructed by
    /// replicating a small set of patterns (the typical case under
    /// categorical scaling).
    Linetype(Vec<Arc<[LinetypeStep]>>),
}

impl DataColumn {
    /// Number of elements in the column.
    pub fn len(&self) -> usize {
        match self {
            DataColumn::F64(v) => v.len(),
            DataColumn::F32(v) => v.len(),
            DataColumn::I32(v) => v.len(),
            DataColumn::I64(v) => v.len(),
            DataColumn::Bool(v) => v.len(),
            DataColumn::String(v) => v.len(),
            DataColumn::Color(v) => v.len(),
            DataColumn::Date(v) => v.len(),
            DataColumn::DateTime(v) => v.len(),
            DataColumn::Time(v) => v.len(),
            DataColumn::Duration(v) => v.len(),
            DataColumn::Linetype(v) => v.len(),
        }
    }

    /// `true` if the column has zero elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// O(1) read at index `i`, converted to a [`Value`].
    ///
    /// Panics if `i` is out of bounds — by convention the caller has
    /// already iterated `0..len()`.
    pub fn get(&self, i: usize) -> Value {
        match self {
            DataColumn::F64(v) => Value::Number(v[i]),
            DataColumn::F32(v) => Value::Number(v[i] as f64),
            DataColumn::I32(v) => Value::Number(v[i] as f64),
            DataColumn::I64(v) => Value::Number(v[i] as f64),
            DataColumn::Bool(v) => Value::Bool(v[i]),
            DataColumn::String(v) => Value::String(v[i].clone()),
            DataColumn::Color(v) => Value::Color(v[i]),
            DataColumn::Date(v) => Value::Date(v[i]),
            DataColumn::DateTime(v) => Value::DateTime(v[i]),
            DataColumn::Time(v) => Value::Time(v[i]),
            DataColumn::Duration(v) => Value::Duration(v[i]),
            DataColumn::Linetype(v) => Value::Linetype(v[i].clone()),
        }
    }

    /// Hash the i-th element using the deterministic [`Value::key_hash`]
    /// strategy. Used by [`diff_columns`](crate::plot::diff) building a
    /// key index without materialising every cell as a `Value`.
    #[allow(unused)] // wired up in Phase 4 (diff)
    pub(crate) fn key_hash_at<H: Hasher>(&self, i: usize, state: &mut H) {
        // Cheaper specialisation per variant — avoids the temporary
        // `Value::clone` of `get(i).key_hash(...)` for owned variants
        // (`String`).
        match self {
            DataColumn::String(v) => {
                let s: &Arc<str> = &v[i];
                let value_discr = std::mem::discriminant(&Value::String(s.clone()));
                value_discr.hash(state);
                (**s).hash(state);
            }
            _ => {
                let v = self.get(i);
                v.key_hash(state);
            }
        }
    }

    /// Deterministic equality between `self[i]` and `other[j]`. Variants
    /// must match (a `Date` column is never equal to an `I32` column);
    /// mismatch returns `false`.
    #[allow(unused)] // wired up in Phase 4 (diff)
    pub(crate) fn key_eq_at(&self, i: usize, other: &DataColumn, j: usize) -> bool {
        // Specialise on `(self, other)` variant pairs to avoid the
        // `Value::clone` round-trip in the common typed cases.
        use DataColumn::*;
        match (self, other) {
            (F64(a), F64(b)) => canonical_f64_bits(a[i]) == canonical_f64_bits(b[j]),
            (F32(a), F32(b)) => canonical_f32_bits(a[i]) == canonical_f32_bits(b[j]),
            (I32(a), I32(b)) => a[i] == b[j],
            (I64(a), I64(b)) => a[i] == b[j],
            (Bool(a), Bool(b)) => a[i] == b[j],
            (String(a), String(b)) => *a[i] == *b[j],
            (Color(a), Color(b)) => a[i] == b[j],
            (Date(a), Date(b)) => a[i] == b[j],
            (DateTime(a), DateTime(b)) => a[i] == b[j],
            (Time(a), Time(b)) => a[i] == b[j],
            (Duration(a), Duration(b)) => a[i] == b[j],
            (Linetype(a), Linetype(b)) => {
                let pa = &a[i];
                let pb = &b[j];
                pa.len() == pb.len()
                    && pa
                        .iter()
                        .zip(pb.iter())
                        .all(|(x, y)| linetype_step_key_eq(x, y))
            }
            _ => false,
        }
    }
}

// `Vec<T> -> DataColumn` From impls — one per supported variant. The
// builder methods on geoms accept `impl Into<DataColumn>` so user code
// stays terse:
//
//     PointGeom::builder().x(vec![1.0, 2.0, 3.0])  // -> DataColumn::F64

impl From<Vec<f64>> for DataColumn {
    fn from(v: Vec<f64>) -> Self {
        DataColumn::F64(v)
    }
}
impl From<Vec<f32>> for DataColumn {
    fn from(v: Vec<f32>) -> Self {
        DataColumn::F32(v)
    }
}
impl From<Vec<i32>> for DataColumn {
    fn from(v: Vec<i32>) -> Self {
        DataColumn::I32(v)
    }
}
impl From<Vec<i64>> for DataColumn {
    fn from(v: Vec<i64>) -> Self {
        DataColumn::I64(v)
    }
}
impl From<Vec<bool>> for DataColumn {
    fn from(v: Vec<bool>) -> Self {
        DataColumn::Bool(v)
    }
}
impl From<Vec<&'static str>> for DataColumn {
    fn from(v: Vec<&'static str>) -> Self {
        DataColumn::String(v.into_iter().map(Arc::from).collect())
    }
}
impl From<Vec<String>> for DataColumn {
    fn from(v: Vec<String>) -> Self {
        DataColumn::String(v.into_iter().map(Arc::from).collect())
    }
}
impl From<Vec<Arc<str>>> for DataColumn {
    fn from(v: Vec<Arc<str>>) -> Self {
        DataColumn::String(v)
    }
}
impl From<Vec<Color>> for DataColumn {
    fn from(v: Vec<Color>) -> Self {
        DataColumn::Color(v)
    }
}

// Temporal column conversions go through the newtype wrappers to keep
// units explicit at the call site.
impl From<Vec<Date>> for DataColumn {
    fn from(v: Vec<Date>) -> Self {
        DataColumn::Date(v.into_iter().map(|d| d.0).collect())
    }
}
impl From<Vec<DateTime>> for DataColumn {
    fn from(v: Vec<DateTime>) -> Self {
        DataColumn::DateTime(v.into_iter().map(|d| d.0).collect())
    }
}
impl From<Vec<Time>> for DataColumn {
    fn from(v: Vec<Time>) -> Self {
        DataColumn::Time(v.into_iter().map(|d| d.0).collect())
    }
}
impl From<Vec<Duration>> for DataColumn {
    fn from(v: Vec<Duration>) -> Self {
        DataColumn::Duration(v.into_iter().map(|d| d.0).collect())
    }
}

// `Range<i64>` -> DataColumn::I64. Used by geom builders to synthesise
// positional key columns: `keys: (0..n as i64).into()`.
impl From<std::ops::Range<i64>> for DataColumn {
    fn from(r: std::ops::Range<i64>) -> Self {
        DataColumn::I64(r.collect())
    }
}

// `Vec<Arc<[LinetypeStep]>>` -> DataColumn::Linetype. There is
// intentionally no `From<Vec<f64>>` for the Linetype variant — that
// would collide with the existing numeric column conversion. Callers
// construct linetype columns either via the named helpers in
// `crate::plot::geom::linetype` or by passing pre-built
// `Arc<[LinetypeStep]>` entries.
impl From<Vec<Arc<[LinetypeStep]>>> for DataColumn {
    fn from(v: Vec<Arc<[LinetypeStep]>>) -> Self {
        DataColumn::Linetype(v)
    }
}

// ─── Canonicalisation helpers ────────────────────────────────────────────────

/// Canonicalise an `f64` bit pattern for deterministic equality / hashing:
/// every NaN maps to a single canonical NaN; `-0.0` maps to `0.0`.
fn canonical_f64_bits(n: f64) -> u64 {
    if n.is_nan() {
        f64::NAN.to_bits()
    } else if n == 0.0 {
        0u64
    } else {
        n.to_bits()
    }
}

fn canonical_f32_bits(n: f32) -> u32 {
    if n.is_nan() {
        f32::NAN.to_bits()
    } else if n == 0.0 {
        0u32
    } else {
        n.to_bits()
    }
}

// ─── Civil / day arithmetic (Howard Hinnant) ─────────────────────────────────
//
// References: https://howardhinnant.github.io/date_algorithms.html
//
// Self-contained: no chrono / time-rs dep. The proleptic Gregorian calendar
// is the same one Arrow uses; this round-trips bit-for-bit with Arrow
// Date32 / Timestamp(Microsecond, UTC).

fn days_from_civil(year: i32, month: u8, day: u8) -> i32 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32; // 0..=399
    let m = month as i32;
    let d = day as i32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as u32 + 2) / 5 + d as u32 - 1;
    // doy is in [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    (era * 146097 + doe as i32) - 719468
}

fn civil_from_days(days: i32) -> (i32, u8, u8) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i32 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let y_out = if m <= 2 { y + 1 } else { y };
    (y_out, m, d)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Value round-trips ──

    #[test]
    fn value_from_f64() {
        let v: Value = 1.5_f64.into();
        assert_eq!(v.as_number(), Some(1.5));
    }

    #[test]
    fn value_from_i32() {
        let v: Value = 42_i32.into();
        assert_eq!(v.as_number(), Some(42.0));
    }

    #[test]
    fn value_from_bool() {
        let v: Value = true.into();
        assert_eq!(v.as_bool(), Some(true));
        assert_eq!(v.as_number(), None);
    }

    #[test]
    fn value_from_static_str() {
        let v: Value = "hello".into();
        assert_eq!(v.as_str(), Some("hello"));
    }

    #[test]
    fn value_from_string() {
        let v: Value = String::from("world").into();
        assert_eq!(v.as_str(), Some("world"));
    }

    #[test]
    fn value_from_color() {
        let c = Color::new([1.0, 0.5, 0.25, 1.0]);
        let v: Value = c.into();
        assert_eq!(v.as_color(), Some(c));
    }

    // ── Temporal round-trips ──

    #[test]
    fn date_ymd_round_trip() {
        let cases = [
            (2024, 1, 1),
            (2024, 12, 31),
            (1970, 1, 1),
            (1969, 12, 31),
            (2000, 2, 29),
            (1900, 3, 1),
            (-1, 12, 31),
        ];
        for (y, m, d) in cases {
            let date = Date::from_ymd(y, m, d);
            assert_eq!(date.to_ymd(), (y, m, d), "round-trip {y}-{m}-{d}");
        }
    }

    #[test]
    fn date_epoch_is_zero() {
        assert_eq!(Date::from_ymd(1970, 1, 1).to_days(), 0);
    }

    #[test]
    fn date_known_offsets() {
        // 2024-01-01 is day 19723 since epoch. (Verified against
        // independent sources.)
        assert_eq!(Date::from_ymd(2024, 1, 1).to_days(), 19723);
        // 1969-12-31 is day -1.
        assert_eq!(Date::from_ymd(1969, 12, 31).to_days(), -1);
    }

    #[test]
    fn date_value_round_trip() {
        let d = Date::from_ymd(2024, 6, 15);
        let v: Value = d.into();
        match v {
            Value::Date(days) => assert_eq!(days, d.to_days()),
            _ => panic!("expected Value::Date"),
        }
    }

    #[test]
    fn datetime_split() {
        // 2024-01-01T12:34:56.789012Z
        let dt = DateTime::from_ymd_hms_micros(2024, 1, 1, 12, 34, 56, 789012);
        let (date, us) = dt.split();
        assert_eq!(date, Date::from_ymd(2024, 1, 1));
        let expected_us = 12 * 3_600_000_000_i64 + 34 * 60_000_000 + 56 * 1_000_000 + 789012;
        assert_eq!(us, expected_us);
    }

    #[test]
    fn datetime_pre_epoch_split() {
        // 1969-06-15T06:00:00Z — exercises the negative-microseconds path.
        let dt = DateTime::from_ymd_hms_micros(1969, 6, 15, 6, 0, 0, 0);
        let (date, us) = dt.split();
        assert_eq!(date, Date::from_ymd(1969, 6, 15));
        assert_eq!(us, 6 * 3_600_000_000_i64);
    }

    #[test]
    fn time_round_trip() {
        let t = Time::from_hms_micros(23, 59, 59, 999_999);
        assert_eq!(t.to_micros(), 86_399_999_999);
    }

    #[test]
    fn duration_from_seconds_saturates() {
        // i64::MAX seconds × 1_000_000 overflows; should saturate.
        let d = Duration::from_seconds(i64::MAX);
        assert_eq!(d.to_micros(), i64::MAX);
    }

    // ── Temporal projection to f64 ──

    #[test]
    fn temporal_as_temporal_f64() {
        assert_eq!(Value::Date(100).as_temporal_f64(), Some(100.0));
        assert_eq!(Value::DateTime(123_456).as_temporal_f64(), Some(123_456.0));
        assert_eq!(Value::Time(42).as_temporal_f64(), Some(42.0));
        assert_eq!(Value::Duration(-7).as_temporal_f64(), Some(-7.0));
    }

    #[test]
    fn null_is_not_finite() {
        assert!(!Value::Null.is_finite());
        assert!(Value::Number(1.0).is_finite());
        assert!(!Value::Number(f64::NAN).is_finite());
        assert!(!Value::Number(f64::INFINITY).is_finite());
        assert!(Value::Date(0).is_finite());
    }

    // ── key_eq / key_hash ──

    fn hash_of(v: &Value) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        let mut h = DefaultHasher::new();
        v.key_hash(&mut h);
        h.finish()
    }

    #[test]
    fn key_eq_nan_equals_nan() {
        let a = Value::Number(f64::NAN);
        let b = Value::Number(f64::NAN);
        assert!(a.key_eq(&b));
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn key_eq_minus_zero_equals_zero() {
        let a = Value::Number(-0.0);
        let b = Value::Number(0.0);
        assert!(a.key_eq(&b));
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn key_eq_distinguishes_variants() {
        // A Number(1.0) and a Date(1) project to the same f64 but must
        // not collide as diff keys.
        let n = Value::Number(1.0);
        let d = Value::Date(1);
        assert!(!n.key_eq(&d));
        // Hashes generally differ (variant discriminant is mixed in);
        // strictly speaking hashes could collide, but the discriminant
        // tag guarantees the comparison is correct.
        assert!(!n.key_eq(&d));
    }

    #[test]
    fn key_eq_distinguishes_strings() {
        let a: Value = "abc".into();
        let b: Value = "abc".into();
        let c: Value = "xyz".into();
        assert!(a.key_eq(&b));
        assert!(!a.key_eq(&c));
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    // ── DataColumn ──

    #[test]
    fn datacolumn_from_vec_f64() {
        let col: DataColumn = vec![1.0_f64, 2.0, 3.0].into();
        assert!(matches!(col, DataColumn::F64(_)));
        assert_eq!(col.len(), 3);
        assert!(!col.is_empty());
    }

    #[test]
    fn datacolumn_get_f64() {
        let col: DataColumn = vec![1.0_f64, 2.0].into();
        assert!(col.get(0).key_eq(&Value::Number(1.0)));
        assert!(col.get(1).key_eq(&Value::Number(2.0)));
    }

    #[test]
    fn datacolumn_get_i32_projects_to_number() {
        let col: DataColumn = vec![42_i32].into();
        assert!(col.get(0).key_eq(&Value::Number(42.0)));
    }

    #[test]
    fn datacolumn_get_color() {
        let c = Color::new([0.1, 0.2, 0.3, 1.0]);
        let col: DataColumn = vec![c].into();
        assert!(matches!(col, DataColumn::Color(_)));
        assert_eq!(col.get(0).as_color(), Some(c));
    }

    #[test]
    fn datacolumn_from_vec_date() {
        let col: DataColumn = vec![Date::from_ymd(2024, 1, 1), Date::from_ymd(2024, 1, 2)].into();
        assert!(matches!(col, DataColumn::Date(_)));
        assert_eq!(col.len(), 2);
        if let DataColumn::Date(v) = &col {
            assert_eq!(v[0], 19723);
            assert_eq!(v[1], 19724);
        }
    }

    #[test]
    fn datacolumn_get_datetime() {
        let dt = DateTime::from_ymd_hms_micros(2024, 1, 1, 0, 0, 0, 0);
        let col: DataColumn = vec![dt].into();
        match col.get(0) {
            Value::DateTime(us) => assert_eq!(us, dt.0),
            _ => panic!("expected Value::DateTime"),
        }
    }

    #[test]
    fn datacolumn_from_string_collects() {
        let col: DataColumn = vec![String::from("a"), String::from("b")].into();
        assert!(matches!(col, DataColumn::String(_)));
        assert_eq!(col.get(0).as_str(), Some("a"));
        assert_eq!(col.get(1).as_str(), Some("b"));
    }

    #[test]
    fn datacolumn_from_static_strs() {
        let col: DataColumn = vec!["alpha", "beta"].into();
        assert!(matches!(col, DataColumn::String(_)));
        assert_eq!(col.get(1).as_str(), Some("beta"));
    }

    #[test]
    fn datacolumn_from_range() {
        let col: DataColumn = (0_i64..5).into();
        assert!(matches!(col, DataColumn::I64(_)));
        assert_eq!(col.len(), 5);
        assert!(col.get(0).key_eq(&Value::Number(0.0)));
        assert!(col.get(4).key_eq(&Value::Number(4.0)));
    }

    #[test]
    fn datacolumn_key_eq_at_same_variant() {
        let a: DataColumn = vec![1.0_f64, 2.0, 3.0].into();
        let b: DataColumn = vec![3.0_f64, 1.0].into();
        assert!(a.key_eq_at(0, &b, 1)); // 1.0 == 1.0
        assert!(a.key_eq_at(2, &b, 0)); // 3.0 == 3.0
        assert!(!a.key_eq_at(0, &b, 0)); // 1.0 != 3.0
    }

    #[test]
    fn datacolumn_key_eq_at_mismatched_variant_is_false() {
        let a: DataColumn = vec![1_i32].into();
        let b: DataColumn = vec![Date::from_days(1)].into();
        // Same numeric projection (1), but different variants — must not
        // compare equal as diff keys.
        assert!(!a.key_eq_at(0, &b, 0));
    }

    // ── Linetype value / column ──

    fn dash_gap(d: f64, g: f64) -> Arc<[LinetypeStep]> {
        Arc::from(vec![LinetypeStep::Dash(d), LinetypeStep::Gap(g)])
    }

    #[test]
    fn value_linetype_round_trip() {
        let p = dash_gap(8.0, 4.0);
        let v = Value::Linetype(p.clone());
        let steps = v.as_linetype().expect("linetype");
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], LinetypeStep::Dash(d) if (d - 8.0).abs() < 1e-12));
        assert!(matches!(steps[1], LinetypeStep::Gap(g) if (g - 4.0).abs() < 1e-12));
        assert!(v.as_number().is_none());
        assert!(v.as_color().is_none());
        assert!(v.as_str().is_none());
        assert!(!v.is_finite());
    }

    #[test]
    fn value_linetype_key_eq_element_wise() {
        let a = Value::Linetype(dash_gap(8.0, 4.0));
        let b = Value::Linetype(dash_gap(8.0, 4.0));
        let c = Value::Linetype(Arc::from(vec![
            LinetypeStep::Dash(8.0),
            LinetypeStep::Gap(4.0),
            LinetypeStep::Dash(1.0),
            LinetypeStep::Gap(2.0),
        ]));
        let d = Value::Linetype(dash_gap(6.0, 4.0));
        assert!(a.key_eq(&b));
        assert!(!a.key_eq(&c));
        assert!(!a.key_eq(&d));
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn value_linetype_marker_key_eq() {
        let a = Value::Linetype(Arc::from(vec![
            LinetypeStep::Marker(Arc::from("circle")),
            LinetypeStep::Gap(5.0),
        ]));
        let b = Value::Linetype(Arc::from(vec![
            LinetypeStep::Marker(Arc::from("circle")),
            LinetypeStep::Gap(5.0),
        ]));
        let c = Value::Linetype(Arc::from(vec![
            LinetypeStep::Marker(Arc::from("square")),
            LinetypeStep::Gap(5.0),
        ]));
        assert!(a.key_eq(&b));
        assert!(!a.key_eq(&c));
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn value_linetype_empty_is_solid() {
        let solid = Value::Linetype(Arc::from(Vec::<LinetypeStep>::new()));
        assert_eq!(solid.as_linetype().map(|p| p.len()), Some(0));
    }

    #[test]
    fn datacolumn_linetype_get() {
        let col: DataColumn =
            vec![dash_gap(8.0, 4.0), Arc::from(Vec::<LinetypeStep>::new())].into();
        assert!(matches!(col, DataColumn::Linetype(_)));
        assert_eq!(col.len(), 2);
        assert!(col.get(0).key_eq(&Value::Linetype(dash_gap(8.0, 4.0))));
        assert!(col
            .get(1)
            .key_eq(&Value::Linetype(Arc::from(Vec::<LinetypeStep>::new()))));
    }

    #[test]
    fn datacolumn_linetype_key_eq_at() {
        let a: DataColumn = vec![dash_gap(8.0, 4.0)].into();
        let b: DataColumn = vec![dash_gap(8.0, 4.0)].into();
        let c: DataColumn = vec![dash_gap(2.0, 3.0)].into();
        assert!(a.key_eq_at(0, &b, 0));
        assert!(!a.key_eq_at(0, &c, 0));
    }

    #[test]
    fn datacolumn_key_hash_at_strings() {
        let a: DataColumn = vec![String::from("foo")].into();
        let b: DataColumn = vec![String::from("foo")].into();
        let c: DataColumn = vec![String::from("bar")].into();
        use std::collections::hash_map::DefaultHasher;

        let mut h_a = DefaultHasher::new();
        a.key_hash_at(0, &mut h_a);
        let mut h_b = DefaultHasher::new();
        b.key_hash_at(0, &mut h_b);
        let mut h_c = DefaultHasher::new();
        c.key_hash_at(0, &mut h_c);

        assert_eq!(h_a.finish(), h_b.finish());
        assert_ne!(h_a.finish(), h_c.finish());
    }
}
