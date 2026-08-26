//! Base64, for data URLs.
//!
//! Hand-rolled rather than pulled in: it is a lookup table and some bit
//! shifting, and `document/` and the `geom-*` parsers set the precedent
//! that this crate writes its own small codecs rather than growing its
//! dependency tree for convenience.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Append `bytes` to `out` as standard base64 with padding.
///
/// Writes straight into the output rather than returning a `String`:
/// the payload is an encoded image, so it can be megabytes, and there
/// is no reason to hold two copies.
pub(crate) fn encode_into(bytes: &[u8], out: &mut String) {
    out.reserve(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for c in &mut chunks {
        let n = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        for shift in [18, 12, 6, 0] {
            out.push(ALPHABET[((n >> shift) & 0x3f) as usize] as char);
        }
    }
    match chunks.remainder() {
        [a] => {
            let n = u32::from(*a) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push_str("==");
        }
        [a, b] => {
            let n = (u32::from(*a) << 16) | (u32::from(*b) << 8);
            for shift in [18, 12, 6] {
                out.push(ALPHABET[((n >> shift) & 0x3f) as usize] as char);
            }
            out.push('=');
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(b: &[u8]) -> String {
        let mut s = String::new();
        encode_into(b, &mut s);
        s
    }

    #[test]
    fn matches_the_rfc_test_vectors() {
        // RFC 4648 section 10 — including every padding case.
        assert_eq!(enc(b""), "");
        assert_eq!(enc(b"f"), "Zg==");
        assert_eq!(enc(b"fo"), "Zm8=");
        assert_eq!(enc(b"foo"), "Zm9v");
        assert_eq!(enc(b"foob"), "Zm9vYg==");
        assert_eq!(enc(b"fooba"), "Zm9vYmE=");
        assert_eq!(enc(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn covers_the_whole_alphabet_including_the_high_bytes() {
        let all: Vec<u8> = (0u8..=255).collect();
        let s = enc(&all);
        assert!(s.contains('+') && s.contains('/'), "{s}");
        assert_eq!(s.len(), 344);
        assert!(s.ends_with('='));
    }
}
