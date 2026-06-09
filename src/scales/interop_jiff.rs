//! `jiff` ↔ hephaestus newtype interop.
//!
//! Gated on the `jiff` cargo feature. Provides bidirectional `From`
//! impls for all four temporal types.
//!
//! - [`Date`] ↔ [`jiff::civil::Date`] via year/month/day extraction.
//! - [`DateTime`] ↔ [`jiff::Timestamp`] via μs since the Unix epoch.
//! - [`Time`] ↔ [`jiff::civil::Time`] via ns since midnight.
//! - [`Duration`] ↔ [`jiff::SignedDuration`] via μs.
//!
//! Note: jiff distinguishes `Span` (calendar-aware, variable lengths)
//! from `SignedDuration` (fixed-width ns). We map our μs-based
//! [`Duration`] to `SignedDuration` since the lengths are fixed; users
//! who want calendar-aware spans should keep them in jiff and only
//! cross the boundary when a concrete μs value is needed.

use super::value::{Date, DateTime, Duration, Time};

// ─── Date ↔ jiff::civil::Date ───────────────────────────────────────────────

impl From<jiff::civil::Date> for Date {
    fn from(d: jiff::civil::Date) -> Self {
        Date::from_ymd(d.year() as i32, d.month() as u8, d.day() as u8)
    }
}

impl From<Date> for jiff::civil::Date {
    fn from(d: Date) -> Self {
        let (y, m, dd) = d.to_ymd();
        jiff::civil::Date::new(y as i16, m as i8, dd as i8)
            .unwrap_or_else(|_| jiff::civil::date(1970, 1, 1))
    }
}

// ─── DateTime ↔ jiff::Timestamp ─────────────────────────────────────────────

impl From<jiff::Timestamp> for DateTime {
    fn from(ts: jiff::Timestamp) -> Self {
        DateTime(ts.as_microsecond())
    }
}

impl From<DateTime> for jiff::Timestamp {
    fn from(dt: DateTime) -> Self {
        jiff::Timestamp::from_microsecond(dt.0).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
    }
}

// ─── Time ↔ jiff::civil::Time ───────────────────────────────────────────────

impl From<jiff::civil::Time> for Time {
    fn from(t: jiff::civil::Time) -> Self {
        let h = t.hour() as i64;
        let m = t.minute() as i64;
        let s = t.second() as i64;
        let sub_ns = t.subsec_nanosecond() as i64;
        Time(h * 3_600_000_000_000 + m * 60_000_000_000 + s * 1_000_000_000 + sub_ns)
    }
}

impl From<Time> for jiff::civil::Time {
    fn from(t: Time) -> Self {
        let ns = t.0.rem_euclid(86_400_000_000_000);
        let total_secs = ns / 1_000_000_000;
        let sub_ns = (ns % 1_000_000_000) as i32;
        let s = (total_secs % 60) as i8;
        let total_mins = total_secs / 60;
        let m = (total_mins % 60) as i8;
        let h = ((total_mins / 60) % 24) as i8;
        jiff::civil::Time::new(h, m, s, sub_ns).unwrap_or(jiff::civil::Time::midnight())
    }
}

// ─── Duration ↔ jiff::SignedDuration ────────────────────────────────────────

impl From<jiff::SignedDuration> for Duration {
    fn from(d: jiff::SignedDuration) -> Self {
        // jiff::SignedDuration is split into (secs: i64, subsec_nanos: i32).
        // Compose to μs with saturating arithmetic.
        let secs_us = d.as_secs().saturating_mul(1_000_000);
        let sub_us = (d.subsec_nanos() / 1_000) as i64;
        Duration(secs_us.saturating_add(sub_us))
    }
}

impl From<Duration> for jiff::SignedDuration {
    fn from(d: Duration) -> Self {
        let secs = d.0 / 1_000_000;
        let sub_us = (d.0 % 1_000_000) as i32;
        let sub_ns = sub_us * 1_000;
        // SignedDuration::new takes (secs, subsec_nanos: i32).
        jiff::SignedDuration::new(secs, sub_ns)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_round_trip_epoch_anchored() {
        let h = Date::from_ymd(2024, 6, 15);
        let j: jiff::civil::Date = h.into();
        assert_eq!(j.year(), 2024);
        assert_eq!(j.month(), 6);
        assert_eq!(j.day(), 15);
        let back: Date = j.into();
        assert_eq!(back, h);
    }

    #[test]
    fn date_epoch_maps_to_zero() {
        let h: Date = jiff::civil::date(1970, 1, 1).into();
        assert_eq!(h.to_days(), 0);
    }

    #[test]
    fn datetime_round_trip_via_timestamp() {
        let h = DateTime::from_ymd_hms_micros(2024, 6, 15, 12, 34, 56, 789_012);
        let j: jiff::Timestamp = h.into();
        let back: DateTime = j.into();
        assert_eq!(back, h);
    }

    #[test]
    fn time_round_trip_ns_precision() {
        let h = Time::from_hms_nanos(7, 8, 9, 123_456_789);
        let j: jiff::civil::Time = h.into();
        let back: Time = j.into();
        assert_eq!(back, h);
    }

    #[test]
    fn duration_round_trip_micros() {
        let h = Duration::from_micros(1_500_000);
        let j: jiff::SignedDuration = h.into();
        let back: Duration = j.into();
        assert_eq!(back, h);
    }
}
