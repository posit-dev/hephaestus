//! PDF output — aliases for the entry points in [`crate::backend::pdf`].
//!
//! Mirrors [`crate::svg`]: the backend is where the code lives, this is
//! where a caller looks for it.

pub use crate::backend::pdf::{
    encode_pdf, write_pdf, write_pdf_to, PdfConfig, PdfScene, PdfWarning,
};

#[cfg(test)]
mod tests {
    #[test]
    fn the_alias_module_reaches_the_backend() {
        let scene = super::PdfScene::new(crate::geometry::Size::new(10.0, 10.0), 96.0);
        assert!(super::encode_pdf(&scene).starts_with(b"%PDF-"));
    }
}
