//! The document's font registry and the `<style>` block it produces.
//!
//! Naming a family on a `<text>` element is not the same as delivering
//! it. Two mechanisms, both optional, neither guessing:
//!
//! - a Google Fonts `@import` for families this process actually
//!   resolved through `fetch_google_font`;
//! - `@font-face` with the face bytes base64'd, when the caller asks.
//!
//! Whatever happens, every `<text>` still names its full family chain
//! plus a generic tail, because an import can fail and a substituted
//! face is far better than none.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::scene::Font;
use crate::style_vocab::{FontSpec, FontStyleKind};

/// One `@font-face` declaration's identity: CSS selects a face by
/// family, weight, style *and* width.
///
/// Width belongs here for two reasons. Two rules with identical
/// descriptors are not two faces to CSS — the later one simply wins, so
/// a document holding both the condensed and the normal cut of one
/// family at one weight would render every label in whichever came
/// last. And an element asking for `font-stretch:condensed` cannot be
/// answered by a declaration that never says which width it is.
#[derive(PartialEq)]
struct FaceKey {
    family: String,
    weight: u16,
    italic: bool,
    /// CSS width ratio — 1.0 is normal.
    width: f32,
}

// `f32` rules out deriving these, and a `BTreeMap` key needs a total
// order. A width is never NaN, so ordering it totally is honest.
impl Eq for FaceKey {}

impl Ord for FaceKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.family
            .cmp(&other.family)
            .then_with(|| self.weight.cmp(&other.weight))
            .then_with(|| self.italic.cmp(&other.italic))
            .then_with(|| self.width.total_cmp(&other.width))
    }
}

impl PartialOrd for FaceKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// What was seen for one key.
#[derive(Default)]
struct FaceUse {
    /// Distinct faces, by blob id so the same file is embedded once.
    blobs: BTreeMap<u64, Font>,
}

/// Fonts referenced by a document.
///
/// A `BTreeMap` rather than a `HashMap`: the `<style>` block's ordering
/// has to be deterministic, because two renders of one scene must
/// produce identical bytes.
#[derive(Default)]
pub(crate) struct FontRegistry {
    faces: BTreeMap<FaceKey, FaceUse>,
}

impl FontRegistry {
    /// Note that `spec` was drawn with `font`.
    pub(crate) fn note(&mut self, spec: &FontSpec, font: &Font) {
        // What a `@font-face` has to declare is the family of the face
        // that was actually *resolved*, not the one that was asked for:
        // a chain ending in a generic named nothing, and a named family
        // that was missing resolved to something else. That name lives
        // in the face's own `name` table.
        let Some(family) = resolved_family(font).or_else(|| first_named(spec)) else {
            return;
        };
        let key = FaceKey {
            family,
            weight: spec.weight,
            italic: matches!(
                spec.style,
                FontStyleKind::Italic | FontStyleKind::Oblique(_)
            ),
            width: spec.width,
        };
        let entry = self.faces.entry(key).or_default();
        // Comparing `Font` by equality would byte-compare megabytes on
        // a miss, which its own docs warn against; the blob id is the
        // identity that matters here.
        entry
            .blobs
            .entry(font.data().data.id())
            .or_insert_with(|| font.clone());
    }

    /// Forget everything, for a new frame.
    pub(crate) fn clear(&mut self) {
        self.faces.clear();
    }

    /// The stylesheet for this document, or `None` when it would be
    /// empty.
    ///
    /// `embed` inlines the face bytes; otherwise only an `@import` for
    /// Google-resolvable families is written. `@import` must be the
    /// first rule in a stylesheet, so it goes first.
    pub(crate) fn stylesheet(&self, embed: bool, decimals: u8) -> Option<String> {
        let mut css = String::new();
        let google = google_families();
        let wanted: Vec<&FaceKey> = self
            .faces
            .keys()
            .filter(|k| google.iter().any(|g| g.eq_ignore_ascii_case(&k.family)))
            .collect();
        if !wanted.is_empty() {
            css.push_str("@import url('");
            css.push_str(&css2_url(&wanted));
            css.push_str("');\n");
        }
        if embed {
            for (key, use_) in &self.faces {
                for font in use_.blobs.values() {
                    if let Some(decl) = font_face(key, font, decimals) {
                        css.push_str(&decl);
                    }
                }
            }
        }
        (!css.is_empty()).then_some(css)
    }
}

/// True when a face can be inlined as `@font-face`.
///
/// A collection cannot: `@font-face` has no way to name a face inside
/// one, so a `ttcf` blob in a data URL loads in no browser. macOS
/// resolves `sans-serif` to a 2.4 MB collection, so this is the
/// ordinary case rather than an exotic one.
pub(crate) fn is_embeddable(font: &Font) -> bool {
    sfnt_format(font.data().data.as_ref()).is_some()
}

/// The family name a face calls itself, read from its `name` table.
///
/// The shaper does not surface this — it resolves a request to a face
/// and hands back the blob — so it is read back off the bytes.
pub(crate) fn resolved_family(font: &Font) -> Option<String> {
    use skrifa::{FontRef, MetadataProvider};
    let data = font.data();
    let font_ref = FontRef::from_index(data.data.as_ref(), data.index).ok()?;
    font_ref
        .localized_strings(skrifa::string::StringId::FAMILY_NAME)
        .english_or_first()
        .map(|s| s.chars().collect())
}

/// The first explicitly named family in a chain.
fn first_named(spec: &FontSpec) -> Option<String> {
    spec.families.iter().find_map(|f| match f {
        crate::style_vocab::FontFamilyEntry::Named(n) => Some(n.clone()),
        _ => None,
    })
}

/// Families this process resolved through Google Fonts.
#[cfg(feature = "google-fonts")]
fn google_families() -> Vec<String> {
    crate::text::google_fonts::google_fetched_families()
}

/// Without the lookup there is nothing that could have been fetched,
/// so nothing is claimed.
#[cfg(not(feature = "google-fonts"))]
fn google_families() -> Vec<String> {
    Vec::new()
}

/// A CSS2 API URL covering every wanted family and its axes.
///
/// Axis names go in alphabetical order and their value tuples in
/// ascending order, which is the form the API documents. An axis every
/// face of a family agrees on is left out entirely, so the ordinary
/// single-weight request stays `wght@400`.
fn css2_url(keys: &[&FaceKey]) -> String {
    // Tuples ordered as the axis names are — ital, wdth, wght — so
    // sorting them sorts the URL. Width in per mille rather than as the
    // ratio, because a float cannot order or deduplicate.
    let mut by_family: BTreeMap<&str, Vec<(bool, u32, u16)>> = BTreeMap::new();
    for k in keys {
        by_family.entry(k.family.as_str()).or_default().push((
            k.italic,
            width_per_mille(k.width),
            k.weight,
        ));
    }
    let mut url = String::from("https://fonts.googleapis.com/css2");
    for (i, (family, mut axes)) in by_family.into_iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str("family=");
        url.push_str(&family.replace(' ', "+"));
        axes.sort_unstable();
        axes.dedup();
        let any_italic = axes.iter().any(|(ital, _, _)| *ital);
        let any_width = axes.iter().any(|(_, width, _)| *width != NORMAL_PER_MILLE);
        url.push(':');
        let mut names: Vec<&str> = Vec::with_capacity(3);
        if any_italic {
            names.push("ital");
        }
        if any_width {
            names.push("wdth");
        }
        names.push("wght");
        url.push_str(&names.join(","));
        url.push('@');
        let tuples: Vec<String> = axes
            .iter()
            .map(|(ital, width, weight)| {
                let mut t = String::new();
                if any_italic {
                    t.push_str(&u8::from(*ital).to_string());
                    t.push(',');
                }
                if any_width {
                    t.push_str(&per_mille_percent(*width));
                    t.push(',');
                }
                t.push_str(&weight.to_string());
                t
            })
            .collect();
        url.push_str(&tuples.join(";"));
    }
    // Without this the text is invisible while the face loads.
    url.push_str("&display=swap");
    url
}

/// A normal width, in the per-mille the URL builder orders widths by.
const NORMAL_PER_MILLE: u32 = 1000;

/// A width ratio as per mille, so it can be ordered and deduplicated.
fn width_per_mille(width: f32) -> u32 {
    (width * NORMAL_PER_MILLE as f32).round() as u32
}

/// Per mille as the percentage the `wdth` axis is expressed in, with no
/// trailing zero — `875` is `87.5`, the width `semi-condensed` names.
fn per_mille_percent(per_mille: u32) -> String {
    let whole = per_mille / 10;
    match per_mille % 10 {
        0 => whole.to_string(),
        frac => format!("{whole}.{frac}"),
    }
}

/// An `@font-face` embedding one face, or `None` when it cannot be
/// embedded.
fn font_face(key: &FaceKey, font: &Font, decimals: u8) -> Option<String> {
    let data = font.data();
    let bytes = data.data.as_ref();
    let format = sfnt_format(bytes)?;
    let mut css = String::from("@font-face{font-family:'");
    css.push_str(&key.family.replace('\'', "\\'"));
    css.push_str("';font-style:");
    css.push_str(if key.italic { "italic" } else { "normal" });
    css.push_str(";font-weight:");
    css.push_str(&key.weight.to_string());
    if key.width != 1.0 {
        css.push_str(";font-stretch:");
        super::text::write_stretch_value(&mut css, key.width, decimals);
    }
    css.push_str(";src:url(data:font/");
    css.push_str(format.0);
    css.push_str(";base64,");
    super::base64::encode_into(bytes, &mut css);
    css.push_str(") format('");
    css.push_str(format.1);
    css.push_str("');}\n");
    Some(css)
}

/// MIME subtype and CSS `format()` keyword for a font blob.
///
/// A collection is refused: `@font-face` cannot address a face inside
/// one, so a `ttcf` blob in a data URL loads in no browser. macOS
/// resolves `sans-serif` to a 2.4 MB collection, so this is the common
/// case rather than an exotic one.
fn sfnt_format(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    match bytes.get(..4)? {
        [0x00, 0x01, 0x00, 0x00] | b"true" => Some(("ttf", "truetype")),
        b"OTTO" => Some(("otf", "opentype")),
        b"wOF2" => Some(("woff2", "woff2")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blob whose first four bytes are all `sfnt_format` reads, so a
    /// declaration can be built without a real face on the machine.
    fn fake_face() -> Font {
        let bytes = vec![0x00, 0x01, 0x00, 0x00, 0xff];
        Font::new(crate::brush::Blob::new(std::sync::Arc::new(bytes)), 0)
    }

    fn spec_of(family: &str, width: f32) -> FontSpec {
        FontSpec {
            families: vec![crate::style_vocab::FontFamilyEntry::Named(family.into())],
            weight: 400,
            style: FontStyleKind::Normal,
            width,
            tracking: 0.0,
            size_pt: 12.0,
            features: Vec::new(),
            variations: Vec::new(),
        }
    }

    #[test]
    fn two_widths_of_one_family_are_two_declarations() {
        let mut reg = FontRegistry::default();
        let face = fake_face();
        reg.note(&spec_of("Inter", 1.0), &face);
        reg.note(&spec_of("Inter", 0.75), &face);
        let css = reg.stylesheet(true, 3).expect("a stylesheet");
        // Sharing a key would mean one declaration, and CSS would then
        // serve the surviving cut for both widths.
        assert_eq!(css.matches("@font-face").count(), 2, "{css}");
        assert!(css.contains("font-stretch:condensed"), "{css}");
    }

    #[test]
    fn a_normal_width_leaves_the_descriptor_off() {
        let mut reg = FontRegistry::default();
        reg.note(&spec_of("Inter", 1.0), &fake_face());
        let css = reg.stylesheet(true, 3).expect("a stylesheet");
        // `normal` is what the descriptor already means unstated.
        assert!(!css.contains("font-stretch"), "{css}");
    }

    #[test]
    fn a_width_between_the_keywords_is_a_percentage_descriptor() {
        let mut reg = FontRegistry::default();
        reg.note(&spec_of("Inter", 0.8), &fake_face());
        let css = reg.stylesheet(true, 3).expect("a stylesheet");
        assert!(css.contains("font-stretch:80%"), "{css}");
    }

    fn key(family: &str, weight: u16, italic: bool) -> FaceKey {
        key_w(family, weight, italic, 1.0)
    }

    fn key_w(family: &str, weight: u16, italic: bool, width: f32) -> FaceKey {
        FaceKey {
            family: family.into(),
            weight,
            italic,
            width,
        }
    }

    #[test]
    fn a_css2_url_collects_every_weight_of_a_family() {
        let keys = [key("Inter", 400, false), key("Inter", 700, false)];
        let refs: Vec<&FaceKey> = keys.iter().collect();
        let url = css2_url(&refs);
        assert!(url.contains("family=Inter:wght@400;700"), "{url}");
        assert!(url.ends_with("&display=swap"), "{url}");
    }

    #[test]
    fn italics_switch_the_url_to_the_two_axis_form() {
        let keys = [key("Inter", 400, false), key("Inter", 400, true)];
        let refs: Vec<&FaceKey> = keys.iter().collect();
        let url = css2_url(&refs);
        assert!(url.contains("ital,wght@0,400;1,400"), "{url}");
    }

    #[test]
    fn a_condensed_cut_asks_for_the_width_axis() {
        let keys = [key("Inter", 400, false), key_w("Inter", 400, false, 0.75)];
        let refs: Vec<&FaceKey> = keys.iter().collect();
        let url = css2_url(&refs);
        // Without the axis the import delivers the normal cut and the
        // element's `font-stretch` has nothing to select.
        assert!(
            url.contains("family=Inter:wdth,wght@75,400;100,400"),
            "{url}"
        );
    }

    #[test]
    fn italic_and_width_together_use_the_three_axis_form() {
        let keys = [
            key_w("Inter", 400, false, 0.75),
            key_w("Inter", 700, true, 1.0),
        ];
        let refs: Vec<&FaceKey> = keys.iter().collect();
        let url = css2_url(&refs);
        assert!(
            url.contains("family=Inter:ital,wdth,wght@0,75,400;1,100,700"),
            "{url}"
        );
    }

    #[test]
    fn a_width_between_the_keywords_keeps_its_decimal() {
        let keys = [key_w("Inter", 400, false, 0.875)];
        let refs: Vec<&FaceKey> = keys.iter().collect();
        assert!(
            css2_url(&refs).contains("wdth,wght@87.5,400"),
            "{:?}",
            css2_url(&refs)
        );
    }

    #[test]
    fn a_multi_word_family_is_url_encoded() {
        let keys = [key("Open Sans", 400, false)];
        let refs: Vec<&FaceKey> = keys.iter().collect();
        assert!(
            css2_url(&refs).contains("family=Open+Sans"),
            "{:?}",
            css2_url(&refs)
        );
    }

    #[test]
    fn a_font_collection_is_refused_rather_than_embedded_unusably() {
        assert_eq!(sfnt_format(b"ttcf\x00\x01\x00\x00"), None);
        assert_eq!(
            sfnt_format(&[0x00, 0x01, 0x00, 0x00]),
            Some(("ttf", "truetype"))
        );
        assert_eq!(sfnt_format(b"OTTO"), Some(("otf", "opentype")));
    }
}
