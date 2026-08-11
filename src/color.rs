//! Color types. Re-exports `peniko::Color` (which itself wraps the `color` crate)
//! and provides a couple of ergonomic constructors.

use peniko::color::{AlphaColor, Oklab};

pub use peniko::Color;

/// The space a color interpolation walks through.
///
/// Two colors define a straight line in whichever space they're
/// interpolated in, and the space decides what the midpoint looks like.
/// [`ColorSpace::Oklab`] is perceptually uniform, so a ramp reads as an
/// even progression of lightness and hue; [`ColorSpace::Srgb`] mixes the
/// encoded channel values directly, which is cheaper but darkens and
/// desaturates through the middle of a hue transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorSpace {
    /// Perceptually uniform rectangular space. The default for data-driven
    /// color: equal steps in the domain produce equal-looking steps of
    /// color.
    #[default]
    Oklab,
    /// Gamma-encoded sRGB — componentwise interpolation of the values as
    /// stored. Reproduces the arithmetic of a plain channel lerp, which is
    /// what palette derivations expressed as channel fractions expect.
    Srgb,
}

/// sRGB color from 0..=1 floats.
pub fn rgb(r: f32, g: f32, b: f32) -> Color {
    Color::new([r, g, b, 1.0])
}

/// sRGB color with alpha, 0..=1 floats.
pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color::new([r, g, b, a])
}

/// sRGB color from 0..=255 bytes (alpha = 255).
pub fn rgb8(r: u8, g: u8, b: u8) -> Color {
    rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

/// Linear interpolation between two colors through `space`. `t = 0.0`
/// returns `a`, `t = 1.0` returns `b`.
///
/// Alpha interpolates linearly and un-premultiplied in either space, so
/// switching spaces changes the color path and nothing else. Values of
/// `t` outside `[0, 1]` extrapolate. The [`ColorSpace::Oklab`] result is
/// clamped back into the sRGB gamut, since a straight line between two
/// in-gamut colors can leave it.
pub fn lerp_color(a: Color, b: Color, t: f64, space: ColorSpace) -> Color {
    let t = t as f32;
    match space {
        ColorSpace::Srgb => {
            let [ar, ag, ab, aa] = a.components;
            let [br, bg, bb, ba] = b.components;
            Color::new([
                ar + t * (br - ar),
                ag + t * (bg - ag),
                ab + t * (bb - ab),
                aa + t * (ba - aa),
            ])
        }
        ColorSpace::Oklab => {
            // Landing exactly on a stop returns it untouched — a ramp's
            // endpoints, and its interior stops, have to reproduce the
            // colors they were given rather than a round-trip of them.
            if t == 0.0 || a == b {
                return a;
            }
            if t == 1.0 {
                return b;
            }
            let [al, aa_, ab_, aalpha] = a.convert::<Oklab>().components;
            let [bl, ba_, bb_, balpha] = b.convert::<Oklab>().components;
            let mixed = AlphaColor::<Oklab>::new([
                al + t * (bl - al),
                aa_ + t * (ba_ - aa_),
                ab_ + t * (bb_ - ab_),
                aalpha + t * (balpha - aalpha),
            ]);
            let [r, g, bch, alpha] = mixed.convert::<peniko::color::Srgb>().components;
            Color::new([
                r.clamp(0.0, 1.0),
                g.clamp(0.0, 1.0),
                bch.clamp(0.0, 1.0),
                alpha.clamp(0.0, 1.0),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Color, b: Color, tol: f32) -> bool {
        a.components
            .iter()
            .zip(b.components.iter())
            .all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn endpoints_are_exact_in_both_spaces() {
        let a = rgb8(255, 0, 0);
        let b = rgb8(0, 0, 255);
        for space in [ColorSpace::Srgb, ColorSpace::Oklab] {
            assert!(approx(lerp_color(a, b, 0.0, space), a, 1e-3));
            assert!(approx(lerp_color(a, b, 1.0, space), b, 1e-3));
        }
    }

    #[test]
    fn srgb_midpoint_is_the_channel_average() {
        let mid = lerp_color(
            rgb(0.0, 0.0, 0.0),
            rgb(1.0, 0.5, 0.25),
            0.5,
            ColorSpace::Srgb,
        );
        assert!(approx(mid, rgb(0.5, 0.25, 0.125), 1e-6));
    }

    #[test]
    fn oklab_midpoint_is_lighter_than_the_srgb_one() {
        // The classic red→blue case: mixing the encoded channels drops
        // through a dark purple, while Oklab holds the lightness of the
        // two ends.
        let a = rgb8(255, 0, 0);
        let b = rgb8(0, 0, 255);
        let ok = lerp_color(a, b, 0.5, ColorSpace::Oklab);
        let srgb = lerp_color(a, b, 0.5, ColorSpace::Srgb);
        let lightness = |c: Color| c.convert::<Oklab>().components[0];
        assert!(
            lightness(ok) > lightness(srgb) + 0.05,
            "oklab {:?} should hold more lightness than srgb {:?}",
            ok,
            srgb
        );
    }

    #[test]
    fn oklab_interpolates_gray_without_a_hue_cast() {
        let mid = lerp_color(
            rgb(0.0, 0.0, 0.0),
            rgb(1.0, 1.0, 1.0),
            0.5,
            ColorSpace::Oklab,
        );
        let [r, g, b, _] = mid.components;
        assert!((r - g).abs() < 1e-3 && (g - b).abs() < 1e-3);
    }

    #[test]
    fn alpha_interpolates_the_same_way_in_both_spaces() {
        let a = rgba(1.0, 0.0, 0.0, 1.0);
        let b = rgba(0.0, 0.0, 1.0, 0.0);
        for space in [ColorSpace::Srgb, ColorSpace::Oklab] {
            let mid = lerp_color(a, b, 0.25, space);
            assert!((mid.components[3] - 0.75).abs() < 1e-3);
        }
    }

    #[test]
    fn oklab_results_stay_in_gamut_under_extrapolation() {
        let out = lerp_color(rgb8(255, 0, 0), rgb8(0, 0, 255), 2.5, ColorSpace::Oklab);
        assert!(out.components.iter().all(|c| (0.0..=1.0).contains(c)));
    }

    #[test]
    fn default_space_is_oklab() {
        assert_eq!(ColorSpace::default(), ColorSpace::Oklab);
    }
}
