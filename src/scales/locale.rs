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
