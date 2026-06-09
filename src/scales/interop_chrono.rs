//! `chrono` ↔ hephaestus newtype interop.
//!
//! Gated on the `chrono` cargo feature. Provides bidirectional `From`
//! impls for all four temporal types.
//!
//! - [`Date`] ↔ [`chrono::NaiveDate`] via the 1970-01-01 epoch.
//! - [`DateTime`] ↔ [`chrono::DateTime`]`<chrono::Utc>` via μs since
//!   the Unix epoch.
//! - [`Time`] ↔ [`chrono::NaiveTime`] via ns since midnight.
//! - [`Duration`] ↔ [`chrono::Duration`] via μs.
//!
//! Out-of-range / unrepresentable conversions saturate (e.g. a
//! [`Duration`] whose μs count overflows `chrono::Duration::max_value`)
//! rather than panicking, matching chrono's own `from_*_opt` defaults.

use chrono::{DateTime as ChronoDateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};

use super::value::{Date, DateTime, Duration, Time};

// ─── Date ↔ NaiveDate ───────────────────────────────────────────────────────

impl From<NaiveDate> for Date {
    fn from(d: NaiveDate) -> Self {
        // num_days_from_ce: days since year 1 CE. 1970-01-01 is day
        // 719_163. Subtract to get our days-since-epoch convention.
        Date(d.num_days_from_ce() - 719_163)
    }
}

impl From<Date> for NaiveDate {
    fn from(d: Date) -> Self {
        let (y, m, dd) = d.to_ymd();
        NaiveDate::from_ymd_opt(y, m as u32, dd as u32).unwrap_or(NaiveDate::MIN)
    }
}

// ─── DateTime ↔ DateTime<Utc> ───────────────────────────────────────────────

impl From<ChronoDateTime<Utc>> for DateTime {
    fn from(dt: ChronoDateTime<Utc>) -> Self {
        DateTime(dt.timestamp_micros())
    }
}

impl From<DateTime> for ChronoDateTime<Utc> {
    fn from(dt: DateTime) -> Self {
        // chrono's from_timestamp_micros returns Option; saturate to
        // UTC epoch on out-of-range (i.e. > ±292,000 years from epoch).
        ChronoDateTime::<Utc>::from_timestamp_micros(dt.0)
            .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap())
    }
}

// ─── Time ↔ NaiveTime ───────────────────────────────────────────────────────

impl From<NaiveTime> for Time {
    fn from(t: NaiveTime) -> Self {
        let secs = t.num_seconds_from_midnight() as i64;
        let sub_ns = t.nanosecond() as i64;
        Time(secs * 1_000_000_000 + sub_ns)
    }
}

impl From<Time> for NaiveTime {
    fn from(t: Time) -> Self {
        let ns = t.0.rem_euclid(86_400_000_000_000);
        let secs = (ns / 1_000_000_000) as u32;
        let sub_ns = (ns % 1_000_000_000) as u32;
        NaiveTime::from_num_seconds_from_midnight_opt(secs, sub_ns).unwrap_or(NaiveTime::MIN)
    }
}

// ─── Duration ↔ chrono::Duration ────────────────────────────────────────────

impl From<chrono::Duration> for Duration {
    fn from(d: chrono::Duration) -> Self {
        // chrono::Duration::num_microseconds returns Option (overflow on
        // very large spans). Saturate at i64 range.
        Duration(d.num_microseconds().unwrap_or_else(|| {
            if d.num_seconds() >= 0 {
                i64::MAX
            } else {
                i64::MIN
            }
        }))
    }
}

impl From<Duration> for chrono::Duration {
    fn from(d: Duration) -> Self {
        chrono::Duration::microseconds(d.0)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_round_trip_epoch_anchored() {
        let h = Date::from_ymd(2024, 6, 15);
        let c: NaiveDate = h.into();
        assert_eq!(c, NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
        let back: Date = c.into();
        assert_eq!(back, h);
    }

    #[test]
    fn date_round_trip_pre_epoch() {
        let h = Date::from_ymd(1969, 7, 20);
        let c: NaiveDate = h.into();
        let back: Date = c.into();
        assert_eq!(back, h);
    }

    #[test]
    fn datetime_round_trip_utc() {
        let h = DateTime::from_ymd_hms_micros(2024, 6, 15, 12, 34, 56, 789_012);
        let c: ChronoDateTime<Utc> = h.into();
        let back: DateTime = c.into();
        assert_eq!(back, h);
    }

    #[test]
    fn time_round_trip_ns_precision() {
        let h = Time::from_hms_nanos(7, 8, 9, 123_456_789);
        let c: NaiveTime = h.into();
        let back: Time = c.into();
        assert_eq!(back, h);
    }

    #[test]
    fn duration_round_trip_micros() {
        let h = Duration::from_micros(1_500_000);
        let c: chrono::Duration = h.into();
        assert_eq!(c.num_microseconds(), Some(1_500_000));
        let back: Duration = c.into();
        assert_eq!(back, h);
    }

    #[test]
    fn date_epoch_maps_to_zero() {
        let h: Date = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap().into();
        assert_eq!(h.to_days(), 0);
    }
}
