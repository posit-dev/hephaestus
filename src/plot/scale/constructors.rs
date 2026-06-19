//! Free-function constructors for the common `Scale` shapes
//! (`continuous`, `discrete`, `binned`, `identity`, `temporal`,
//! `ordinal`, `continuous_from_data`).
//!
//! Each builds a [`Scale`] preconfigured for one [`ScaleTypeKind`] and
//! returns it for further chaining. Re-exported from
//! [`crate::plot::scale`] so existing call paths
//! (`scale::continuous(...)`) keep resolving.

use std::ops::RangeInclusive;

use crate::scales::value::{DataColumn, Value};

use super::{Scale, ScaleTypeKind, TemporalUnit};

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

/// Calendar-aware temporal scale. `T` must convert to a temporal
/// [`Value`] variant ([`Date`](crate::scales::value::Date),
/// [`DateTime`](crate::scales::value::DateTime),
/// [`Time`](crate::scales::value::Time), or
/// [`Duration`](crate::scales::value::Duration)) — the variant of the
/// `domain.start()` value selects the calendar unit. Endpoints project
/// to f64 days / microseconds for storage; breaks come back as
/// `Value::Date / Value::DateTime / Value::Time / Value::Duration`
/// aligned to year / quarter / month / week / day / hour / minute /
/// second boundaries.
///
/// Use `scale::continuous(date_start..=date_end)` for the legacy
/// behaviour (numeric breaks); `scale::temporal(...)` is the opt-in for
/// calendar awareness.
///
/// ```ignore
/// scale::temporal(Date::from_ymd(2024, 1, 1) ..= Date::from_ymd(2024, 12, 31))
/// scale::temporal(DateTime::from_ymd_hms_micros(2024, 1, 1, 0, 0, 0, 0)
///     ..= DateTime::from_ymd_hms_micros(2025, 1, 1, 0, 0, 0, 0))
/// ```
///
/// Panics if `domain.start()` is not a temporal value.
pub fn temporal<T>(domain: RangeInclusive<T>) -> Scale
where
    T: Into<Value> + Copy,
{
    let start: Value = (*domain.start()).into();
    let unit = match &start {
        Value::Date(_) => TemporalUnit::Date,
        Value::DateTime(_) => TemporalUnit::DateTime,
        Value::Time(_) => TemporalUnit::Time,
        Value::Duration(_) => TemporalUnit::Duration,
        other => panic!(
            "scale::temporal: expected a temporal value (Date / DateTime / Time / Duration), got {other:?}"
        ),
    };
    Scale::new(ScaleTypeKind::Temporal(unit)).domain_continuous(*domain.start(), *domain.end())
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
