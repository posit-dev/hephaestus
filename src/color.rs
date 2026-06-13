//! Color types. Re-exports `peniko::Color` (which itself wraps the `color` crate)
//! and provides a couple of ergonomic constructors.

pub use peniko::Color;

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

/// Componentwise linear interpolation between two colors in sRGB
/// space. `t = 0.0` returns `a`, `t = 1.0` returns `b`; values outside
/// `[0, 1]` extrapolate. The fourth component (alpha) is interpolated
/// the same way.
pub fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let t = t as f32;
    let [ar, ag, ab, aa] = a.components;
    let [br, bg, bb, ba] = b.components;
    Color::new([
        ar + t * (br - ar),
        ag + t * (bg - ag),
        ab + t * (bb - ab),
        aa + t * (ba - aa),
    ])
}
