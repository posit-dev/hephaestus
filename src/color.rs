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
