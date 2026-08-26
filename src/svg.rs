//! SVG output — aliases for the entry points in [`crate::backend::svg`].
//!
//! Mirrors [`crate::png`]: the backend is where the code lives, this is
//! where a caller looks for it.

pub use crate::backend::svg::{
    encode_svg, write_svg, write_svg_to, SvgConfig, SvgScene, SvgUnits, SvgWarning, TextMode,
};

#[cfg(test)]
mod tests {
    #[test]
    fn the_alias_module_reaches_the_backend() {
        let scene = super::SvgScene::new(crate::geometry::Size::new(10.0, 10.0), 96.0);
        assert!(super::encode_svg(&scene).starts_with("<svg"));
    }
}
