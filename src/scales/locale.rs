//! [`Locale`] — the locale a plot's labels should be formatted for.
//!
//! `Locale` is a tag and nothing else. It names a locale; it does not
//! describe one. Deciding that `ar-EG` means Arabic-Indic digits, a
//! Saturday week start and `مارس` for March takes a CLDR-sized table,
//! and that table belongs with the code that formats labels rather than
//! with the renderer that draws them.
//!
//! So the tag rides along: it is carried on [`Theme`](crate::plot::Theme),
//! handed to every label formatter beside the value being formatted, and
//! written into a plot document so a consumer rebuilding the plot knows
//! which locale it was for. What a formatter does with it is the
//! formatter's business — the built-in one ignores it and renders ASCII
//! digits and ISO dates.

use std::borrow::Cow;

/// The locale a plot's labels are formatted for, as a tag.
///
/// Carried verbatim: whatever string goes in comes back out, so two
/// spellings of the same locale (`"ar-EG"` and `"ar_EG"`) are different
/// `Locale`s. Canonicalizing a tag needs a table of the rules, which is
/// the thing this type exists to avoid holding.
///
/// The tag is a [`Cow`], so the built-in constants are compile-time
/// values that cost no allocation while a tag obtained at runtime owns
/// itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Locale {
    tag: Cow<'static, str>,
}

impl Locale {
    /// US English.
    pub const EN_US: Locale = Locale::from_static("en-US");
    /// German (Germany).
    pub const DE_DE: Locale = Locale::from_static("de-DE");
    /// French (France).
    pub const FR_FR: Locale = Locale::from_static("fr-FR");

    /// A locale from a tag known at compile time.
    pub const fn from_static(tag: &'static str) -> Self {
        Self {
            tag: Cow::Borrowed(tag),
        }
    }

    /// The tag, as it was supplied.
    pub fn tag(&self) -> &str {
        &self.tag
    }
}

impl Default for Locale {
    /// US English ([`Self::EN_US`]).
    fn default() -> Self {
        Self::EN_US
    }
}

impl From<String> for Locale {
    fn from(tag: String) -> Self {
        Self {
            tag: Cow::Owned(tag),
        }
    }
}

impl From<&str> for Locale {
    fn from(tag: &str) -> Self {
        Self {
            tag: Cow::Owned(tag.to_string()),
        }
    }
}

impl std::fmt::Display for Locale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_locale_is_us_english() {
        assert_eq!(Locale::default(), Locale::EN_US);
        assert_eq!(Locale::EN_US.tag(), "en-US");
    }

    #[test]
    fn a_built_in_tag_borrows() {
        // The constants are compile-time values; naming one must not
        // allocate.
        assert!(matches!(Locale::DE_DE.tag, Cow::Borrowed(_)));
    }

    #[test]
    fn a_tag_obtained_at_runtime_owns_itself() {
        let from_data = format!("{}-{}", "ar", "EG");
        let loc = Locale::from(from_data);
        assert_eq!(loc.tag(), "ar-EG");
        assert!(matches!(loc.tag, Cow::Owned(_)));
    }

    #[test]
    fn a_tag_is_carried_verbatim() {
        // No canonicalization: the two spellings stay distinct, which is
        // what "a tag and nothing more" costs.
        assert_ne!(Locale::from("ar_EG"), Locale::from("ar-EG"));
        assert_eq!(Locale::from("ar_EG").tag(), "ar_EG");
    }

    #[test]
    fn display_is_the_tag() {
        assert_eq!(Locale::FR_FR.to_string(), "fr-FR");
    }
}
