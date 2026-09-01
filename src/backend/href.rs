//! The link-destination allow-list shared by the vector backends.

/// True when a link destination is safe to put in a document's link.
///
/// Markdown here is layered over arbitrary user strings, and a
/// `javascript:` destination is a script-injection vector — in an
/// inlined SVG directly, and in a PDF through a viewer that honours
/// scripted actions. Anything not on the allowed list falls back to
/// plain styled text.
pub(crate) fn safe_href(url: &str) -> bool {
    let trimmed = url.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with('/') {
        return true;
    }
    match trimmed.split_once(':') {
        None => true, // relative
        Some((scheme, _)) => {
            matches!(
                scheme.trim().to_ascii_lowercase().as_str(),
                "http" | "https" | "mailto"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_navigable_schemes_are_allowed() {
        assert!(safe_href("https://example.com"));
        assert!(safe_href("http://example.com"));
        assert!(safe_href("mailto:a@b.c"));
        assert!(safe_href("#anchor"));
        assert!(safe_href("relative/path"));
        assert!(!safe_href("javascript:alert(1)"));
        assert!(!safe_href("  JavaScript:alert(1)"));
        assert!(!safe_href("data:text/html,<script>"));
    }
}
