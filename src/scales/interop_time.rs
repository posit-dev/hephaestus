//! `time` (time-rs) ↔ hephaestus newtype interop.
//!
//! Gated on the `time` cargo feature. Provides bidirectional `From`
//! impls for all four temporal types.
//!
//! - [`Date`] ↔ [`time::Date`] via Julian day arithmetic
//!   (julian day of 1970-01-01 = 2440588).
//! - [`DateTime`] ↔ [`time::OffsetDateTime`] via μs since the Unix
//!   epoch (UTC offset).
//! - [`Time`] ↔ [`time::Time`] via ns since midnight.
//! - [`Duration`] ↔ [`time::Duration`] via μs.

use super::value::{Date, DateTime, Duration, Time};

/// Julian day number for 1970-01-01 (proleptic Gregorian).
const UNIX_EPOCH_JULIAN_DAY: i32 = 2_440_588;

// ─── Date ↔ time::Date ──────────────────────────────────────────────────────

impl From<time::Date> for Date {
    fn from(d: time::Date) -> Self {
        Date(d.to_julian_day() - UNIX_EPOCH_JULIAN_DAY)
    }
}

impl From<Date> for time::Date {
    fn from(d: Date) -> Self {
        // time::Date::from_julian_day returns Result; saturate to
        // Date::MIN / MAX on out-of-range.
        time::Date::from_julian_day(d.0 + UNIX_EPOCH_JULIAN_DAY).unwrap_or(time::Date::MIN)
    }
}

// ─── DateTime ↔ OffsetDateTime ──────────────────────────────────────────────

impl From<time::OffsetDateTime> for DateTime {
    fn from(dt: time::OffsetDateTime) -> Self {
        // unix_timestamp_nanos returns i128; we store μs.
        DateTime((dt.unix_timestamp_nanos() / 1_000) as i64)
    }
}

impl From<DateTime> for time::OffsetDateTime {
    fn from(dt: DateTime) -> Self {
        time::OffsetDateTime::from_unix_timestamp_nanos((dt.0 as i128) * 1_000)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
    }
}

// ─── Time ↔ time::Time ──────────────────────────────────────────────────────

impl From<time::Time> for Time {
    fn from(t: time::Time) -> Self {
        let (h, m, s, ns) = t.as_hms_nano();
        Time(
            (h as i64) * 3_600_000_000_000
                + (m as i64) * 60_000_000_000
                + (s as i64) * 1_000_000_000
                + ns as i64,
        )
    }
}

impl From<Time> for time::Time {
    fn from(t: Time) -> Self {
        let ns = t.0.rem_euclid(86_400_000_000_000);
        let total_secs = ns / 1_000_000_000;
        let sub_ns = (ns % 1_000_000_000) as u32;
        let s = (total_secs % 60) as u8;
        let total_mins = total_secs / 60;
        let m = (total_mins % 60) as u8;
        let h = ((total_mins / 60) % 24) as u8;
        time::Time::from_hms_nano(h, m, s, sub_ns).unwrap_or(time::Time::MIDNIGHT)
    }
}

// ─── Duration ↔ time::Duration ──────────────────────────────────────────────

impl From<time::Duration> for Duration {
    fn from(d: time::Duration) -> Self {
        // whole_microseconds returns i128; saturate to i64.
        let us = d.whole_microseconds();
        let clamped = if us > i64::MAX as i128 {
            i64::MAX
        } else if us < i64::MIN as i128 {
            i64::MIN
        } else {
            us as i64
        };
        Duration(clamped)
    }
}

impl From<Duration> for time::Duration {
    fn from(d: Duration) -> Self {
        time::Duration::microseconds(d.0)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_round_trip_epoch_anchored() {
        let h = Date::from_ymd(2024, 6, 15);
        let t: time::Date = h.into();
        assert_eq!(t.year(), 2024);
        assert_eq!(t.month() as u8, 6);
        assert_eq!(t.day(), 15);
        let back: Date = t.into();
        assert_eq!(back, h);
    }

    #[test]
    fn date_epoch_maps_to_zero() {
        let epoch = time::Date::from_calendar_date(1970, time::Month::January, 1).unwrap();
        let h: Date = epoch.into();
        assert_eq!(h.to_days(), 0);
    }

    #[test]
    fn datetime_round_trip_utc() {
        let h = DateTime::from_ymd_hms_micros(2024, 6, 15, 12, 34, 56, 789_012);
        let t: time::OffsetDateTime = h.into();
        let back: DateTime = t.into();
        assert_eq!(back, h);
    }

    #[test]
    fn time_round_trip_ns_precision() {
        let h = Time::from_hms_nanos(7, 8, 9, 123_456_789);
        let t: time::Time = h.into();
        let back: Time = t.into();
        assert_eq!(back, h);
    }

    #[test]
    fn duration_round_trip_micros() {
        let h = Duration::from_micros(1_500_000);
        let t: time::Duration = h.into();
        assert_eq!(t.whole_microseconds(), 1_500_000);
        let back: Duration = t.into();
        assert_eq!(back, h);
    }
}
