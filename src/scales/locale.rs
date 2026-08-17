//! [`Locale`] — locale-specific formatting hints consumed by tick-label
//! formatters.
//!
//! `Locale` carries decimal / grouping separators for numeric output and
//! short / long month / day names for temporal output, plus AM / PM
//! strings and the first day of the week. The default formatter and any
//! user-supplied closure both receive `&Locale` alongside the `Value` so
//! formatting can adapt without per-scale configuration.

/// First day of the week. Independent of the [`Locale::month_long`] /
/// [`Locale::day_long`] arrays so consumers can pick the calendar
/// rendering convention separately from the language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weekday {
    /// Calendars start on Monday (most of Europe, ISO 8601).
    Monday,
    /// Calendars start on Sunday (US convention).
    Sunday,
}

/// Locale-specific formatting hints. Used by [`crate::scales`] formatter
/// helpers and threaded into the user-supplied closure on
/// [`crate::plot::scale::Scale::with_format`].
///
/// `'static` string arrays keep the type trivially `Copy`-friendly and
/// avoid heap allocation for the small built-in set
/// ([`Self::EN_US`], [`Self::DE_DE`], [`Self::FR_FR`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Locale {
    /// Decimal mark for numeric output. `'.'` for English, `','` for
    /// most of Continental Europe.
    pub decimal: char,
    /// Thousands separator for grouped numeric output. `None`
    /// suppresses grouping. The default formatter does *not* insert
    /// grouping for tick labels (axis ticks read cleanly without it);
    /// user formatters can opt in by checking this field.
    pub grouping: Option<char>,
    /// Three-letter month abbreviations, January through December.
    /// Read by user formatters; the default one renders ISO dates.
    pub month_short: [&'static str; 12],
    /// Full month names, January through December.
    pub month_long: [&'static str; 12],
    /// Three-letter day abbreviations, Monday through Sunday.
    pub day_short: [&'static str; 7],
    /// Full day names, Monday through Sunday.
    pub day_long: [&'static str; 7],
    /// Morning marker — typically `"AM"` or `"am"`.
    pub am: &'static str,
    /// Evening marker — typically `"PM"` or `"pm"`.
    pub pm: &'static str,
    /// Which day calendars / week-aligned breaks start on.
    pub first_dow: Weekday,
}

impl Locale {
    /// US English. The crate's default.
    pub const EN_US: Locale = Locale {
        decimal: '.',
        grouping: Some(','),
        month_short: [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ],
        month_long: [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ],
        day_short: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
        day_long: [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ],
        am: "AM",
        pm: "PM",
        first_dow: Weekday::Sunday,
    };

    /// German (Germany).
    pub const DE_DE: Locale = Locale {
        decimal: ',',
        grouping: Some('.'),
        month_short: [
            "Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez",
        ],
        month_long: [
            "Januar",
            "Februar",
            "März",
            "April",
            "Mai",
            "Juni",
            "Juli",
            "August",
            "September",
            "Oktober",
            "November",
            "Dezember",
        ],
        day_short: ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"],
        day_long: [
            "Montag",
            "Dienstag",
            "Mittwoch",
            "Donnerstag",
            "Freitag",
            "Samstag",
            "Sonntag",
        ],
        am: "vorm.",
        pm: "nachm.",
        first_dow: Weekday::Monday,
    };

    /// French (France).
    pub const FR_FR: Locale = Locale {
        decimal: ',',
        grouping: Some(' '),
        month_short: [
            "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.",
            "nov.", "déc.",
        ],
        month_long: [
            "janvier",
            "février",
            "mars",
            "avril",
            "mai",
            "juin",
            "juillet",
            "août",
            "septembre",
            "octobre",
            "novembre",
            "décembre",
        ],
        day_short: ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."],
        day_long: [
            "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
        ],
        am: "AM",
        pm: "PM",
        first_dow: Weekday::Monday,
    };
}

impl Default for Locale {
    /// US English ([`Self::EN_US`]).
    fn default() -> Self {
        Self::EN_US
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILT_INS: [(&str, Locale); 3] = [
        ("EN_US", Locale::EN_US),
        ("DE_DE", Locale::DE_DE),
        ("FR_FR", Locale::FR_FR),
    ];

    /// Drop a trailing abbreviation dot so short and long names can be
    /// compared as prefixes (`"janv."` → `"janv"`).
    fn undotted(s: &str) -> &str {
        s.strip_suffix('.').unwrap_or(s)
    }

    #[test]
    fn default_locale_is_us_english() {
        assert_eq!(Locale::default(), Locale::EN_US);
    }

    #[test]
    fn separators_never_collide_within_a_locale() {
        // A grouped number is unreadable when the same character both
        // groups the thousands and marks the decimal.
        for (name, loc) in BUILT_INS {
            if let Some(grouping) = loc.grouping {
                assert_ne!(
                    grouping, loc.decimal,
                    "{name} uses '{grouping}' for both grouping and decimal"
                );
            }
        }
    }

    #[test]
    fn calendar_names_are_populated_and_distinct() {
        for (name, loc) in BUILT_INS {
            for (label, names) in [
                ("month_short", &loc.month_short[..]),
                ("month_long", &loc.month_long[..]),
                ("day_short", &loc.day_short[..]),
                ("day_long", &loc.day_long[..]),
            ] {
                for entry in names {
                    assert!(!entry.trim().is_empty(), "{name}.{label} has a blank entry");
                }
                let mut seen: Vec<&str> = names.to_vec();
                seen.sort_unstable();
                let before = seen.len();
                seen.dedup();
                assert_eq!(
                    seen.len(),
                    before,
                    "{name}.{label} repeats a name — the table is misaligned"
                );
            }
        }
    }

    #[test]
    fn short_calendar_names_abbreviate_their_long_counterparts() {
        // Catches an off-by-one or a transposition between the short and
        // long tables: entry `i` of each pair names the same month / day.
        for (name, loc) in BUILT_INS {
            for i in 0..12 {
                let (short, long) = (undotted(loc.month_short[i]), loc.month_long[i]);
                assert!(
                    long.to_lowercase().starts_with(&short.to_lowercase()),
                    "{name}: month {i} short \"{short}\" does not abbreviate \"{long}\""
                );
            }
            for i in 0..7 {
                let (short, long) = (undotted(loc.day_short[i]), loc.day_long[i]);
                assert!(
                    long.to_lowercase().starts_with(&short.to_lowercase()),
                    "{name}: day {i} short \"{short}\" does not abbreviate \"{long}\""
                );
            }
        }
    }

    #[test]
    fn day_tables_run_monday_first_independently_of_the_first_day_of_week() {
        // The arrays are indexed Monday-through-Sunday whatever calendar
        // convention `first_dow` states.
        assert_eq!(Locale::EN_US.day_long[0], "Monday");
        assert_eq!(Locale::EN_US.day_long[6], "Sunday");
        assert_eq!(Locale::DE_DE.day_long[0], "Montag");
        assert_eq!(Locale::FR_FR.day_long[0], "lundi");
        assert_eq!(Locale::EN_US.first_dow, Weekday::Sunday);
        assert_eq!(Locale::DE_DE.first_dow, Weekday::Monday);
    }
}
