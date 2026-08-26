//! `<defs>` accumulation: id allocation, dedup, and the shared
//! `<style>` slot.
//!
//! Ids are allocated in first-use order and entries are emitted in id
//! order, never by iterating a hash map. Two renders of one scene have
//! to produce byte-identical documents, and hash iteration order is the
//! usual way that quietly stops being true.

use std::collections::HashMap;

/// What kind of definition an id names. The prefix keeps ids readable
/// and keeps the kinds from colliding as the emitter grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefKind {
    /// A `<clipPath>`.
    Clip,
    /// A `<linearGradient>`.
    LinearGradient,
    /// A `<radialGradient>`.
    RadialGradient,
    /// A `<pattern>` wrapping an image brush.
    #[allow(dead_code)] // used once image brushes land
    Pattern,
    /// An `<image>` referenced by `<use>`.
    #[allow(dead_code)] // used once raster images land
    Image,
}

impl DefKind {
    /// Short tag distinguishing this kind's ids from the others'.
    fn prefix(self) -> &'static str {
        match self {
            DefKind::Clip => "c",
            DefKind::LinearGradient => "lg",
            DefKind::RadialGradient => "rg",
            DefKind::Pattern => "pt",
            DefKind::Image => "im",
        }
    }
}

/// One interned definition.
struct Entry {
    /// The id, already including the document prefix. Read by the
    /// duplicate-id assertions in tests.
    #[cfg_attr(not(test), allow(dead_code))]
    id: String,
    /// Element serialization, with `id="…"` inserted.
    body: String,
}

/// Accumulated `<defs>` content.
#[derive(Default)]
pub(crate) struct Defs {
    entries: Vec<Entry>,
    /// Body text (as written, before an id was inserted) to the id it
    /// was given. Keying on the serialization makes dedup exact by
    /// construction — two definitions that serialize identically render
    /// identically, and nothing else can. It also sidesteps the brush
    /// types being neither `Hash` nor `Eq`.
    seen: HashMap<String, String>,
    next: u32,
}

impl Defs {
    /// Intern one definition and return the id to reference it by.
    ///
    /// `body` is the element's full serialization *without* an `id`
    /// attribute, which this inserts. A body seen before returns the id
    /// it already has rather than defining it twice.
    pub(crate) fn intern(&mut self, kind: DefKind, body: &str, doc_prefix: &str) -> String {
        if let Some(id) = self.seen.get(body) {
            return id.clone();
        }
        let id = format!("{doc_prefix}{}{}", kind.prefix(), self.next);
        self.next += 1;
        self.seen.insert(body.to_string(), id.clone());
        self.entries.push(Entry {
            id: id.clone(),
            body: insert_id(body, &id),
        });
        id
    }

    /// True when no definition was interned.
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every id defined, in emission order. For tests asserting that
    /// nothing is defined twice.
    #[cfg(test)]
    pub(crate) fn ids(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.id.as_str()).collect()
    }

    /// Write the whole `<defs>` block, or nothing when there is
    /// neither a definition nor a stylesheet.
    ///
    /// The stylesheet is passed in rather than accumulated because it
    /// is derived from the whole document — which fonts were used —
    /// and so is only known once drawing is done.
    pub(crate) fn write(&self, out: &mut String, style: Option<&str>) {
        if self.is_empty() && style.is_none() {
            return;
        }
        out.push_str("<defs>");
        if let Some(css) = style {
            // CDATA rather than escaping: a CSS payload carries `&` in
            // every query string and `>` in every child selector, and
            // wrapping settles all of that at once. `@import` has to be
            // the first rule, which is why the stylesheet leads.
            out.push_str("<style type=\"text/css\"><![CDATA[");
            out.push_str(css);
            out.push_str("]]></style>");
        }
        for entry in &self.entries {
            out.push_str(&entry.body);
        }
        out.push_str("</defs>");
    }

    /// Reset for a new frame.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.seen.clear();
        self.next = 0;
    }
}

/// Put `id="…"` into an element serialization written without one,
/// immediately after the tag name.
fn insert_id(element: &str, id: &str) -> String {
    let cut = element
        .char_indices()
        .skip(1) // past '<'
        .find(|(_, c)| c.is_whitespace() || *c == '>' || *c == '/')
        .map(|(i, _)| i)
        .unwrap_or(element.len());
    format!("{} id=\"{}\"{}", &element[..cut], id, &element[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identical_body_is_defined_once_and_shares_its_id() {
        let mut d = Defs::default();
        let a = d.intern(DefKind::Clip, "<clipPath><path d=\"M0 0\"/></clipPath>", "");
        let b = d.intern(DefKind::Clip, "<clipPath><path d=\"M0 0\"/></clipPath>", "");
        assert_eq!(a, b);
        assert_eq!(d.ids().len(), 1, "one definition, not two");
    }

    #[test]
    fn ids_are_allocated_in_first_use_order_across_kinds() {
        let mut d = Defs::default();
        assert_eq!(d.intern(DefKind::Clip, "<clipPath/>", ""), "c0");
        assert_eq!(
            d.intern(DefKind::LinearGradient, "<linearGradient/>", ""),
            "lg1"
        );
        assert_eq!(
            d.intern(DefKind::Clip, "<clipPath><a/></clipPath>", ""),
            "c2"
        );
    }

    #[test]
    fn the_document_prefix_keeps_two_inlined_svgs_apart() {
        let mut d = Defs::default();
        assert_eq!(d.intern(DefKind::Clip, "<clipPath/>", "p1-"), "p1-c0");
    }

    #[test]
    fn the_id_lands_just_after_the_tag_name() {
        assert_eq!(insert_id("<clipPath/>", "c0"), "<clipPath id=\"c0\"/>");
        assert_eq!(
            insert_id("<linearGradient x1=\"0\"/>", "lg3"),
            "<linearGradient id=\"lg3\" x1=\"0\"/>"
        );
        assert_eq!(
            insert_id("<clipPath><path/></clipPath>", "c1"),
            "<clipPath id=\"c1\"><path/></clipPath>"
        );
    }

    #[test]
    fn clearing_restarts_id_allocation() {
        let mut d = Defs::default();
        d.intern(DefKind::Clip, "<clipPath/>", "");
        d.clear();
        assert!(d.is_empty());
        assert_eq!(d.intern(DefKind::Clip, "<clipPath/>", ""), "c0");
    }

    #[test]
    fn a_stylesheet_is_wrapped_in_cdata_and_written_first() {
        let mut d = Defs::default();
        d.intern(DefKind::Clip, "<clipPath/>", "");
        let mut out = String::new();
        d.write(&mut out, Some("@import url('x?a=1&b=2');"));
        assert!(out.starts_with("<defs><style type=\"text/css\"><![CDATA[@import"));
        assert!(
            out.find("]]></style>").unwrap() < out.find("<clipPath").unwrap(),
            "the stylesheet precedes the definitions"
        );
    }
}
