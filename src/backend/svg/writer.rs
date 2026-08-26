//! Number formatting, XML escaping, and transform serialization.
//!
//! Small enough to be unremarkable, and precise enough to be worth
//! keeping in one place: every coordinate in the document goes through
//! [`num`], so its rules are the document's rules.

use crate::geometry::Affine;

/// Decimals a matrix's linear part is written to.
///
/// Higher than the coordinate precision on purpose. A scale factor
/// rounded to three decimals is a 0.1% error, which over a 1000 px span
/// is a visible pixel of drift; a translation rounded the same way moves
/// by a thousandth of a pixel and nobody can tell.
const MATRIX_DECIMALS: u8 = 6;

/// Append `v` to `out` with at most `decimals` decimal places.
///
/// Trailing zeros and a trailing point are trimmed, and `-0` normalizes
/// to `0`, so the common integral coordinate costs one character rather
/// than five. Rust's `Display` for `f64` never produces exponent form —
/// only `Debug` does — so the SVG-invalid `1e-7` cannot arise here; the
/// rule that matters is never to `{:?}` a coordinate.
///
/// A non-finite value writes `0`. NaN coordinates do reach a scene from
/// a degenerate scale, and a literal `NaN` in an attribute makes the
/// whole document unparseable — one wrong pixel is recoverable, an
/// unreadable file is not. Callers that care report it as a warning.
pub(crate) fn num(out: &mut String, v: f64, decimals: u8) {
    if !v.is_finite() {
        out.push('0');
        return;
    }
    // Integral fast path — most plot coordinates land here after
    // rounding, and it skips the formatting machinery entirely.
    if v.fract() == 0.0 && v.abs() < 1e15 {
        let i = v as i64;
        if i == 0 {
            out.push('0');
        } else {
            out.push_str(itoa(i).as_str());
        }
        return;
    }
    let s = format!("{:.*}", decimals as usize, v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" || s == "-" {
        out.push('0');
    } else {
        out.push_str(s);
    }
}

/// Integer to string without pulling in a formatting dependency.
fn itoa(v: i64) -> String {
    v.to_string()
}

/// True when `a` is close enough to the identity to omit its attribute.
pub(crate) fn is_identity(a: Affine) -> bool {
    let c = a.as_coeffs();
    c == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

/// Append `transform="…"` for `a`, or nothing when it is the identity.
///
/// kurbo's coefficient order is SVG's `matrix(a b c d e f)` order
/// already — both mean `x' = ax + cy + e`, `y' = bx + dy + f` — so no
/// transposition is involved. A pure translation is written as
/// `translate(…)`, which is shorter and reads better when someone opens
/// the file.
pub(crate) fn transform_attr(out: &mut String, a: Affine, decimals: u8) {
    if is_identity(a) {
        return;
    }
    let c = a.as_coeffs();
    out.push_str(" transform=\"");
    if c[0] == 1.0 && c[1] == 0.0 && c[2] == 0.0 && c[3] == 1.0 {
        out.push_str("translate(");
        num(out, c[4], decimals);
        if c[5] != 0.0 {
            out.push(' ');
            num(out, c[5], decimals);
        }
        out.push(')');
    } else {
        out.push_str("matrix(");
        for (i, v) in c.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            // The linear part carries the tighter precision.
            let d = if i < 4 { MATRIX_DECIMALS } else { decimals };
            num(out, *v, d);
        }
        out.push(')');
    }
    out.push('"');
}

/// Append `s` as XML element content, escaping what XML reserves and
/// dropping what it forbids.
///
/// C0 controls other than tab, newline and carriage return, plus the
/// two non-characters, are illegal in XML 1.0 *even written as numeric
/// references*, so a label carrying one would produce a file no parser
/// accepts. Dropping them is the only option that still yields a
/// document.
pub(crate) fn escape_text(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c if is_xml_illegal(c) => {}
            c => out.push(c),
        }
    }
}

/// Append `s` as an XML attribute value.
///
/// Attributes are always written with `"` delimiters, so an apostrophe
/// needs no escaping — and leaving it alone matters, because CSS quotes
/// font family names with one and `&apos;Open Sans&apos;` is a poor
/// thing to hand someone reading the file.
pub(crate) fn escape_attr(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if is_xml_illegal(c) => {}
            c => out.push(c),
        }
    }
}

/// Characters XML 1.0 cannot represent at all.
fn is_xml_illegal(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(v: f64) -> String {
        let mut s = String::new();
        num(&mut s, v, 3);
        s
    }

    #[test]
    fn numbers_never_use_exponent_form() {
        // The values that would tempt a naive formatter into `1e-7`.
        for v in [1e-7, 1e-30, -1e-9, 1e20, -1e18] {
            let s = n(v);
            assert!(
                !s.contains('e') && !s.contains('E'),
                "{v} formatted as {s:?}"
            );
        }
    }

    #[test]
    fn numbers_trim_and_normalize() {
        assert_eq!(n(0.0), "0");
        assert_eq!(n(-0.0), "0");
        assert_eq!(n(12.0), "12");
        assert_eq!(n(-3.0), "-3");
        assert_eq!(n(1.5), "1.5");
        assert_eq!(n(1.23456), "1.235");
        // Rounds to nothing rather than to "-0".
        assert_eq!(n(-0.0001), "0");
    }

    #[test]
    fn non_finite_numbers_become_zero_rather_than_an_unparseable_file() {
        assert_eq!(n(f64::NAN), "0");
        assert_eq!(n(f64::INFINITY), "0");
        assert_eq!(n(f64::NEG_INFINITY), "0");
    }

    /// kurbo's coefficient order must be SVG's, or every transformed
    /// element in the document is wrong in a way that looks plausible.
    #[test]
    fn affine_coefficients_are_in_svg_matrix_order() {
        let a = Affine::new([2.0, 3.0, 5.0, 7.0, 11.0, 13.0]);
        let mut s = String::new();
        transform_attr(&mut s, a, 3);
        assert_eq!(s, " transform=\"matrix(2 3 5 7 11 13)\"");
        // And the mapping it claims: x' = ax + cy + e, y' = bx + dy + f.
        let p = a * crate::geometry::Point::new(1.0, 1.0);
        assert_eq!(p.x, 2.0 + 5.0 + 11.0);
        assert_eq!(p.y, 3.0 + 7.0 + 13.0);
    }

    #[test]
    fn identity_and_translation_are_written_compactly() {
        let mut s = String::new();
        transform_attr(&mut s, Affine::IDENTITY, 3);
        assert_eq!(s, "", "the identity earns no attribute");

        let mut s = String::new();
        transform_attr(&mut s, Affine::translate((4.0, 0.0)), 3);
        assert_eq!(s, " transform=\"translate(4)\"");

        let mut s = String::new();
        transform_attr(&mut s, Affine::translate((4.0, 5.0)), 3);
        assert_eq!(s, " transform=\"translate(4 5)\"");
    }

    #[test]
    fn escaping_covers_content_and_attributes() {
        let mut s = String::new();
        escape_text(&mut s, "a<b>&c\"d");
        assert_eq!(s, "a&lt;b&gt;&amp;c\"d");

        let mut s = String::new();
        escape_attr(&mut s, "a<b>&c\"d'e");
        assert_eq!(
            s, "a&lt;b&gt;&amp;c&quot;d'e",
            "an apostrophe is legal inside a double-quoted attribute"
        );
    }

    #[test]
    fn characters_xml_cannot_represent_are_dropped() {
        let mut s = String::new();
        escape_text(&mut s, "a\u{0}b\u{1f}c\u{ffff}d");
        assert_eq!(s, "abcd");
        // The three controls XML does allow survive.
        let mut s = String::new();
        escape_text(&mut s, "a\tb\nc\rd");
        assert_eq!(s, "a\tb\nc\rd");
    }
}
