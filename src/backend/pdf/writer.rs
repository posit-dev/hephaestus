//! Number formatting, name and string escaping, and the indirect-object
//! table a PDF file is made of.
//!
//! Every coordinate in the document goes through [`num`], so its rules
//! are the document's rules; every object in the file goes through
//! [`Objects`], so the xref table is correct by construction rather
//! than by remembering to update it.

use crate::geometry::Affine;

/// Decimals a matrix's linear part is written to.
///
/// Higher than the coordinate precision on purpose. A scale factor
/// rounded to three decimals is a 0.1% error, which over a 1000 px span
/// is a visible pixel of drift; a translation rounded the same way moves
/// by a thousandth of a pixel and nobody can tell.
pub(crate) const MATRIX_DECIMALS: u8 = 6;

/// Decimals a `DeviceRGB` component is written to.
///
/// Finer than the eight bits every rasterizer in the crate produces, so
/// nothing is lost, and short enough that a color operator stays one
/// line.
pub(crate) const COLOR_DECIMALS: u8 = 4;

/// Append `v` to `out` with at most `decimals` decimal places.
///
/// Trailing zeros and a trailing point are trimmed, and `-0` normalizes
/// to `0`, so the common integral coordinate costs one character rather
/// than five. PDF real numbers may not use exponent notation at all
/// (ISO 32000-1 §7.3.3); Rust's `Display` for `f64` never produces it —
/// only `Debug` does — so the rule that matters is never to `{:?}` a
/// coordinate.
///
/// A non-finite value writes `0`. NaN coordinates do reach a scene from
/// a degenerate scale, and a literal `NaN` in a content stream makes the
/// page unrenderable — one wrong pixel is recoverable, an unreadable
/// file is not. Callers that care report it as a warning.
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
            out.push_str(&i.to_string());
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

/// True when `a` is close enough to the identity to omit its operator.
pub(crate) fn is_identity(a: Affine) -> bool {
    a.as_coeffs() == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

/// Append `a b c d e f cm`, or nothing when `a` is the identity.
///
/// kurbo's coefficient order is PDF's matrix order already — both mean
/// `x' = ax + cy + e`, `y' = bx + dy + f` — so no transposition is
/// involved.
pub(crate) fn cm(out: &mut String, a: Affine, decimals: u8) {
    if is_identity(a) {
        return;
    }
    matrix(out, a, decimals);
    out.push_str("cm\n");
}

/// Append a matrix's six operands, each followed by a space.
///
/// Unconditional, unlike [`cm`] — a `/Matrix` array or a `Tm` operator
/// needs the identity spelled out.
pub(crate) fn matrix(out: &mut String, a: Affine, decimals: u8) {
    for (i, v) in a.as_coeffs().iter().enumerate() {
        // The linear part carries the tighter precision, for the reason
        // `MATRIX_DECIMALS` states.
        num(out, *v, if i < 4 { MATRIX_DECIMALS } else { decimals });
        out.push(' ');
    }
}

/// Append a literal string in parentheses, escaping what PDF reserves.
///
/// `\`, `(` and `)` are backslash-escaped; anything outside printable
/// ASCII goes out as a three-digit octal escape, which keeps the whole
/// file ASCII and so keeps a byte offset equal to a character offset.
pub(crate) fn pdf_string(out: &mut String, s: &str) {
    out.push('(');
    for b in s.bytes() {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push(')');
}

/// Append a hex string, which is how binary data reaches a dictionary.
pub(crate) fn pdf_hex(out: &mut String, bytes: &[u8]) {
    out.push('<');
    for b in bytes {
        out.push_str(&format!("{b:02X}"));
    }
    out.push('>');
}

/// An indirect object's number. Objects are numbered from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ref(u32);

impl Ref {
    /// Append `N 0 R`, the way one object names another.
    pub(crate) fn write(self, out: &mut String) {
        out.push_str(&self.0.to_string());
        out.push_str(" 0 R");
    }

    /// The reference as `N 0 R`.
    pub(crate) fn to_ref_string(self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }
}

/// Accumulates indirect objects and the byte offsets an xref table
/// needs.
///
/// Object numbers are handed out by [`alloc`](Self::alloc) before their
/// bodies exist, because a dictionary routinely names an object written
/// later — a `/Page` names its `/Contents`.
pub(crate) struct Objects {
    out: Vec<u8>,
    /// `offsets[i]` is the byte offset of object `i + 1`, or `None`
    /// until it is written.
    offsets: Vec<Option<usize>>,
}

impl Objects {
    /// Start a document, writing the header.
    pub(crate) fn new() -> Self {
        // The high-byte comment on the second line is what tells a
        // transfer agent the file is binary rather than text.
        let mut out = Vec::with_capacity(4096);
        out.extend_from_slice(b"%PDF-1.7\n");
        out.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
        Self {
            out,
            offsets: Vec::new(),
        }
    }

    /// Reserve an object number without writing anything yet.
    pub(crate) fn alloc(&mut self) -> Ref {
        self.offsets.push(None);
        Ref(self.offsets.len() as u32)
    }

    /// Write `r` as a plain object whose body is `body`.
    ///
    /// `body` is the object's complete value — a dictionary including
    /// its `<<` and `>>`, or an array, or a number.
    pub(crate) fn object(&mut self, r: Ref, body: &str) {
        self.begin(r);
        self.out.extend_from_slice(body.as_bytes());
        self.out.extend_from_slice(b"\nendobj\n");
    }

    /// Write `r` as a stream object.
    ///
    /// `dict` is the stream dictionary's entries *without* the
    /// enclosing `<<` `>>`; `/Length` and, when compressed, `/Filter`
    /// are appended here. `/Length` is always a direct integer — the
    /// payload is in hand before the dictionary is written, so there is
    /// no reason to defer it.
    pub(crate) fn stream(&mut self, r: Ref, dict: &str, payload: &[u8], compress: bool) {
        let body = if compress {
            deflate(payload)
        } else {
            payload.to_vec()
        };
        self.begin(r);
        self.out.extend_from_slice(b"<< ");
        self.out.extend_from_slice(dict.as_bytes());
        if !dict.is_empty() && !dict.ends_with(' ') {
            self.out.push(b' ');
        }
        if compress {
            self.out.extend_from_slice(b"/Filter /FlateDecode ");
        }
        self.out
            .extend_from_slice(format!("/Length {} >>\nstream\n", body.len()).as_bytes());
        self.out.extend_from_slice(&body);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
    }

    /// Begin `r`'s body, recording where it starts.
    fn begin(&mut self, r: Ref) {
        let i = r.0 as usize - 1;
        self.offsets[i] = Some(self.out.len());
        self.out
            .extend_from_slice(format!("{} 0 obj\n", r.0).as_bytes());
    }

    /// Write the xref table and trailer, and return the finished file.
    ///
    /// No `/ID` and no `/Info`: both would carry a timestamp or a
    /// random value, and their absence is what makes two encodes of one
    /// scene byte-identical.
    pub(crate) fn finish(mut self, root: Ref) -> Vec<u8> {
        let start = self.out.len();
        let count = self.offsets.len() + 1;
        self.out
            .extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
        // Entries are exactly 20 bytes: ten digits, space, five digits,
        // space, the type letter, space, newline. A viewer seeks by
        // multiplying, so a byte either way makes the file unreadable.
        self.out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &self.offsets {
            let o = offset.unwrap_or(0);
            self.out
                .extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        let mut trailer = String::from("trailer\n<< /Size ");
        trailer.push_str(&count.to_string());
        trailer.push_str(" /Root ");
        trailer.push_str(&root.to_ref_string());
        trailer.push_str(" >>\nstartxref\n");
        trailer.push_str(&start.to_string());
        trailer.push_str("\n%%EOF\n");
        self.out.extend_from_slice(trailer.as_bytes());
        self.out
    }
}

/// zlib-compress `data`, which is what `/Filter /FlateDecode` means.
///
/// zlib (RFC 1950) with its two-byte header, not raw deflate.
pub(crate) fn deflate(data: &[u8]) -> Vec<u8> {
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).expect("writing to a Vec cannot fail");
    e.finish().expect("finishing into a Vec cannot fail")
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
        assert_eq!(n(1.23456), "1.235");
        assert_eq!(n(-0.0001), "0");
    }

    #[test]
    fn non_finite_numbers_become_zero_rather_than_an_unreadable_file() {
        assert_eq!(n(f64::NAN), "0");
        assert_eq!(n(f64::INFINITY), "0");
        assert_eq!(n(f64::NEG_INFINITY), "0");
    }

    /// kurbo's coefficient order must be PDF's, or every transformed
    /// primitive on the page is wrong in a way that looks plausible.
    #[test]
    fn affine_coefficients_are_in_pdf_matrix_order() {
        let a = Affine::new([2.0, 3.0, 5.0, 7.0, 11.0, 13.0]);
        let mut s = String::new();
        cm(&mut s, a, 3);
        assert_eq!(s, "2 3 5 7 11 13 cm\n");
        // And the mapping it claims: x' = ax + cy + e, y' = bx + dy + f.
        let p = a * crate::geometry::Point::new(1.0, 1.0);
        assert_eq!(p.x, 2.0 + 5.0 + 11.0);
        assert_eq!(p.y, 3.0 + 7.0 + 13.0);
    }

    #[test]
    fn the_identity_earns_no_operator() {
        let mut s = String::new();
        cm(&mut s, Affine::IDENTITY, 3);
        assert_eq!(s, "");
    }

    #[test]
    fn strings_escape_delimiters_and_high_bytes() {
        let mut s = String::new();
        pdf_string(&mut s, "a(b)c\\d");
        assert_eq!(s, "(a\\(b\\)c\\\\d)");

        let mut s = String::new();
        pdf_string(&mut s, "é");
        assert_eq!(s, "(\\303\\251)", "UTF-8 bytes, each octal-escaped");
    }

    /// Byte offset of the first occurrence of `needle` in `haystack`.
    ///
    /// Byte offsets, not char offsets: the header's binary comment
    /// makes the file invalid UTF-8, and an xref entry names a byte.
    fn find(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("needle is present")
    }

    #[test]
    fn xref_entries_are_exactly_twenty_bytes() {
        let mut o = Objects::new();
        let a = o.alloc();
        let b = o.alloc();
        o.object(a, "<< /Type /Catalog /Pages 2 0 R >>");
        o.object(b, "<< /Type /Pages /Kids [] /Count 0 >>");
        let file = o.finish(a);
        let start = find(&file, b"xref\n") + "xref\n0 3\n".len();
        // Three entries: the free head plus one per object.
        for i in 0..3 {
            let entry = &file[start + i * 20..start + (i + 1) * 20];
            assert_eq!(entry.len(), 20);
            assert_eq!(entry[19], b'\n');
            assert_eq!(entry[18], b' ');
        }
    }

    #[test]
    fn every_offset_lands_on_its_object_header() {
        let mut o = Objects::new();
        let a = o.alloc();
        let b = o.alloc();
        o.object(a, "<< /Type /Catalog >>");
        o.stream(b, "/Type /Whatever", b"hello", false);
        let file = o.finish(a);
        let start = find(&file, b"xref\n") + "xref\n0 3\n".len();
        for (i, expected) in [b"1 0 obj".as_slice(), b"2 0 obj".as_slice()]
            .iter()
            .enumerate()
        {
            let entry = &file[start + (i + 1) * 20..start + (i + 2) * 20];
            let offset: usize = std::str::from_utf8(&entry[..10]).unwrap().parse().unwrap();
            assert!(
                file[offset..].starts_with(expected),
                "object {} does not start at {offset}",
                i + 1
            );
        }
    }

    #[test]
    fn the_startxref_offset_points_at_the_table() {
        let mut o = Objects::new();
        let a = o.alloc();
        o.object(a, "<< /Type /Catalog >>");
        let file = o.finish(a);
        let tail = find(&file, b"startxref\n") + "startxref\n".len();
        let end = tail + file[tail..].iter().position(|b| *b == b'\n').unwrap();
        let offset: usize = std::str::from_utf8(&file[tail..end])
            .unwrap()
            .parse()
            .unwrap();
        assert!(file[offset..].starts_with(b"xref\n"));
    }

    #[test]
    fn a_stream_length_matches_its_payload() {
        let mut o = Objects::new();
        let a = o.alloc();
        o.stream(a, "", b"0123456789", false);
        let file = o.finish(a);
        let text = String::from_utf8_lossy(&file).into_owned();
        assert!(text.contains("/Length 10 >>\nstream\n0123456789\nendstream"));
    }

    #[test]
    fn deflate_round_trips_through_a_zlib_reader() {
        use std::io::Read;
        let data = b"the quick brown fox jumps over the lazy dog".repeat(8);
        let packed = deflate(&data);
        let mut back = Vec::new();
        flate2::read::ZlibDecoder::new(&packed[..])
            .read_to_end(&mut back)
            .expect("zlib stream");
        assert_eq!(back, data);
    }
}
