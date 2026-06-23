//! Google Fonts auto-lookup. Gated behind the `text-google-fonts` feature.
//!
//! On cache miss, hits the Google Fonts CSS2 API, downloads each
//! face's TTF blob, writes the blobs to the platform cache directory,
//! and registers them with the global font context via
//! [`crate::text::register_font_bytes`]. Cache hits register directly
//! from disk via [`crate::text::register_font_dir`].

use std::io;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use directories::ProjectDirs;

use crate::text::{register_font_bytes, register_font_dir};

const GOOGLE_CSS_API: &str = "https://fonts.googleapis.com/css2";
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Error type returned by [`fetch_google_font`].
#[derive(thiserror::Error, Debug)]
pub enum GoogleFontError {
    /// Couldn't determine a platform cache directory for this user.
    #[error("could not resolve a platform cache directory")]
    NoCacheDir,
    /// Network transport / HTTP error from the Google Fonts API.
    #[error("google fonts network error: {0}")]
    Network(#[from] Box<ureq::Error>),
    /// Local I/O error reading or writing the cache directory.
    #[error("google fonts io error: {0}")]
    Io(#[from] io::Error),
    /// The Google Fonts CSS response did not contain any `url(...)`
    /// entries — typically means the family name was misspelled or
    /// is not on Google Fonts.
    #[error("no font URLs found in Google Fonts response for family {0:?}")]
    NoFontUrls(String),
}

impl From<ureq::Error> for GoogleFontError {
    fn from(e: ureq::Error) -> Self {
        GoogleFontError::Network(Box::new(e))
    }
}

/// Fetch a Google Fonts family by name. On a cold cache, hits the
/// Google Fonts CSS2 API, downloads each TTF face, writes them to the
/// platform cache directory, and registers them with the global font
/// context. On a warm cache, registers directly from disk. Returns
/// the number of font faces registered.
///
/// Subsequent [`crate::text::TextRun::new`] calls referencing the
/// family by name pick up the newly-registered faces.
///
/// `family` is the human-readable Google Fonts family name (e.g.
/// `"Inter"`, `"Open Sans"`). Names are case-sensitive against the
/// Google Fonts catalogue. Multi-word names use literal spaces.
pub fn fetch_google_font(family: &str) -> Result<usize, GoogleFontError> {
    let family_dir = google_font_cache_dir()?.join(family_slug(family));
    if family_dir.is_dir() {
        let n = register_font_dir(&family_dir)?;
        if n > 0 {
            return Ok(n);
        }
    }
    let css = http_get_string(&family_css_url(family))?;
    let urls = parse_font_urls(&css);
    if urls.is_empty() {
        return Err(GoogleFontError::NoFontUrls(family.to_string()));
    }
    std::fs::create_dir_all(&family_dir)?;
    let mut total = 0;
    for (i, url) in urls.iter().enumerate() {
        let bytes = http_get_bytes(url)?;
        let file = family_dir.join(face_filename(i, url));
        std::fs::write(&file, &bytes)?;
        total += register_font_bytes(bytes);
    }
    Ok(total)
}

/// Resolved on-disk cache directory for Google-Fonts-sourced font
/// files. One subdirectory per family slug; each holds the family's
/// downloaded TTF blobs. Public so callers that want to pre-warm the
/// cache (offline prep, CI fixtures) can write directly to it.
pub fn google_font_cache_dir() -> Result<PathBuf, GoogleFontError> {
    let dirs =
        ProjectDirs::from("org", "hephaestus", "hephaestus").ok_or(GoogleFontError::NoCacheDir)?;
    Ok(dirs.cache_dir().join("fonts").join("google"))
}

fn family_slug(family: &str) -> String {
    family
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn family_css_url(family: &str) -> String {
    let encoded: String = family
        .chars()
        .map(|c| if c == ' ' { '+' } else { c })
        .collect();
    format!("{}?family={}", GOOGLE_CSS_API, encoded)
}

fn parse_font_urls(css: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut s = css;
    while let Some(start) = s.find("url(") {
        let after = &s[start + 4..];
        if let Some(end) = after.find(')') {
            let url = after[..end].trim().trim_matches(|c| c == '"' || c == '\'');
            out.push(url.to_string());
            s = &after[end..];
        } else {
            break;
        }
    }
    out
}

fn face_filename(i: usize, url: &str) -> String {
    let tail = url.rsplit('/').next().unwrap_or("");
    let no_query = tail.split('?').next().unwrap_or(tail);
    let ext = no_query
        .rfind('.')
        .map(|p| &no_query[p + 1..])
        .unwrap_or("");
    let ext_clean: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(4)
        .collect();
    let ext_final = if ext_clean.is_empty() {
        "ttf".to_string()
    } else {
        ext_clean
    };
    format!("face-{:03}.{}", i, ext_final)
}

/// Send no `User-Agent` header. Empirically, the Google Fonts CSS2
/// API serves direct TTF `src: url(...)` entries to clients without
/// a UA, and falls back to WOFF2 (which fontique can't ingest
/// directly) for any modern-browser UA. ureq does not add a default
/// User-Agent, so simply omitting `set("User-Agent", _)` keeps the
/// header out of the request.
fn http_get_string(url: &str) -> Result<String, GoogleFontError> {
    let resp = ureq::get(url)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()?;
    Ok(resp.into_string()?)
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>, GoogleFontError> {
    let resp = ureq::get(url)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_slug_normalises_spaces_and_case() {
        assert_eq!(family_slug("Open Sans"), "open-sans");
        assert_eq!(family_slug("Inter"), "inter");
        assert_eq!(family_slug("Noto Sans CJK JP"), "noto-sans-cjk-jp");
    }

    #[test]
    fn family_css_url_encodes_spaces_as_plus() {
        assert_eq!(
            family_css_url("Open Sans"),
            "https://fonts.googleapis.com/css2?family=Open+Sans"
        );
    }

    #[test]
    fn parse_font_urls_extracts_each_src_entry() {
        let css = r#"
            @font-face {
              font-family: 'Inter';
              src: url(https://fonts.gstatic.com/s/inter/v1/foo.ttf) format('truetype');
            }
            @font-face {
              font-family: 'Inter';
              src: url("https://fonts.gstatic.com/s/inter/v1/bar.ttf") format('truetype');
            }
        "#;
        let urls = parse_font_urls(css);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://fonts.gstatic.com/s/inter/v1/foo.ttf");
        assert_eq!(urls[1], "https://fonts.gstatic.com/s/inter/v1/bar.ttf");
    }

    /// Live network test against the actual Google Fonts CSS2 API.
    /// Hits the network — `#[ignore]`d by default. Run with
    /// `cargo test --features text-google-fonts -- --ignored
    /// google_fonts::tests::live_fetch_inter`.
    #[test]
    #[ignore]
    fn live_fetch_inter() {
        let n = fetch_google_font("Inter").expect("fetch");
        assert!(n > 0, "expected at least one face for Inter; got {n}");
    }

    #[test]
    fn face_filename_preserves_extension() {
        assert_eq!(face_filename(0, "https://x/y/a.ttf"), "face-000.ttf");
        assert_eq!(face_filename(1, "https://x/y/b.otf?v=2"), "face-001.otf");
        assert_eq!(face_filename(2, "https://x/y/nothing"), "face-002.ttf");
    }
}
