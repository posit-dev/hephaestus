//! Conversions from our restricted enums to peniko equivalents.

use crate::blend::{BlendMode, Compose, Mix};
use crate::brush::Sampling;
use crate::path::FillRule;

pub(super) fn fill_rule(rule: FillRule) -> peniko::Fill {
    match rule {
        FillRule::NonZero => peniko::Fill::NonZero,
        FillRule::EvenOdd => peniko::Fill::EvenOdd,
    }
}

pub(super) fn blend_mode(mode: BlendMode) -> peniko::BlendMode {
    peniko::BlendMode {
        mix: mix(mode.mix),
        compose: compose(mode.compose),
    }
}

fn mix(m: Mix) -> peniko::Mix {
    match m {
        Mix::Normal => peniko::Mix::Normal,
        Mix::Multiply => peniko::Mix::Multiply,
        Mix::Screen => peniko::Mix::Screen,
        Mix::Overlay => peniko::Mix::Overlay,
        Mix::Darken => peniko::Mix::Darken,
        Mix::Lighten => peniko::Mix::Lighten,
        Mix::ColorDodge => peniko::Mix::ColorDodge,
        Mix::ColorBurn => peniko::Mix::ColorBurn,
        Mix::HardLight => peniko::Mix::HardLight,
        Mix::SoftLight => peniko::Mix::SoftLight,
        Mix::Difference => peniko::Mix::Difference,
        Mix::Exclusion => peniko::Mix::Exclusion,
        Mix::Hue => peniko::Mix::Hue,
        Mix::Saturation => peniko::Mix::Saturation,
        Mix::Color => peniko::Mix::Color,
        Mix::Luminosity => peniko::Mix::Luminosity,
    }
}

fn compose(c: Compose) -> peniko::Compose {
    match c {
        Compose::Clear => peniko::Compose::Clear,
        Compose::Copy => peniko::Compose::Copy,
        Compose::Dest => peniko::Compose::Dest,
        Compose::SrcOver => peniko::Compose::SrcOver,
        Compose::DestOver => peniko::Compose::DestOver,
        Compose::SrcIn => peniko::Compose::SrcIn,
        Compose::DestIn => peniko::Compose::DestIn,
        Compose::SrcOut => peniko::Compose::SrcOut,
        Compose::DestOut => peniko::Compose::DestOut,
        Compose::SrcAtop => peniko::Compose::SrcAtop,
        Compose::DestAtop => peniko::Compose::DestAtop,
        Compose::Xor => peniko::Compose::Xor,
        Compose::Plus => peniko::Compose::Plus,
    }
}

pub(super) fn sampling_to_quality(s: Sampling) -> peniko::ImageQuality {
    match s {
        Sampling::Nearest => peniko::ImageQuality::Low,
        Sampling::Bilinear => peniko::ImageQuality::Medium,
    }
}
