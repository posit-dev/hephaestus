//! Brushes to SVG paint: solid colors, and gradients as `<defs>`
//! entries referenced by `url(#…)`.

use super::defs::{DefKind, Defs};
use super::writer::{num, transform_attr};
use super::{SvgWarning, Warnings};
use crate::brush::{Brush, Extend, Gradient, GradientKind};
use crate::color::Color;
use crate::geometry::Affine;

/// A resolved paint plus the opacity that goes with it.
pub(crate) struct Paint {
    /// What to put in `fill` or `stroke`.
    pub value: String,
    /// Companion `*-opacity`, or `None` when fully opaque.
    pub opacity: Option<f32>,
}

/// `#rrggbb` for a color, and its alpha separately.
///
/// Not CSS `rgba()`: SVG 1.1 does not accept Color 4 function syntax,
/// whereas `#rrggbb` plus a separate `*-opacity` is understood
/// everywhere. Eight bits loses nothing — every rasteriser in the crate
/// produces eight-bit output anyway.
pub(crate) fn solid(color: Color) -> Paint {
    let [r, g, b, a] = color.to_rgba8().to_u8_array();
    Paint {
        value: format!("#{r:02x}{g:02x}{b:02x}"),
        opacity: (a != 255).then(|| a as f32 / 255.0),
    }
}

/// Resolve `brush` to a paint, interning a gradient in `defs` when
/// needed.
///
/// `brush_transform` becomes `gradientTransform` and is deliberately
/// *not* composed with the element's own transform: the element's
/// transform already establishes the user space that
/// `gradientUnits="userSpaceOnUse"` resolves against, so composing would
/// apply it twice.
pub(crate) fn resolve(
    brush: &Brush,
    brush_transform: Option<Affine>,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) -> Paint {
    match brush {
        Brush::Solid(c) => solid(*c),
        Brush::Gradient(g) => gradient(g, brush_transform, defs, doc_prefix, decimals, warnings),
        Brush::Image(_) => {
            // An image brush needs a `<pattern>`; until that lands,
            // saying so beats painting the wrong color silently.
            warnings.note(SvgWarning::ImageBrushUnsupported);
            Paint {
                value: "none".into(),
                opacity: None,
            }
        }
    }
}

/// Intern a gradient and return the `url(#…)` that references it.
fn gradient(
    g: &Gradient,
    brush_transform: Option<Affine>,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) -> Paint {
    let mut body = String::new();
    let kind = match g.kind {
        GradientKind::Linear(pos) => {
            body.push_str("<linearGradient");
            attrs(
                &mut body,
                decimals,
                &[
                    ("x1", pos.start.x),
                    ("y1", pos.start.y),
                    ("x2", pos.end.x),
                    ("y2", pos.end.y),
                ],
            );
            DefKind::LinearGradient
        }
        GradientKind::Radial(pos) => {
            body.push_str("<radialGradient");
            attrs(
                &mut body,
                decimals,
                &[
                    ("cx", pos.end_center.x),
                    ("cy", pos.end_center.y),
                    ("r", pos.end_radius as f64),
                    ("fx", pos.start_center.x),
                    ("fy", pos.start_center.y),
                ],
            );
            if pos.start_radius != 0.0 {
                // `fr` is SVG 2. Browsers and resvg honour it; an
                // SVG-1.1-only consumer drops it and draws a
                // point-focus gradient instead.
                attrs(&mut body, decimals, &[("fr", pos.start_radius as f64)]);
                warnings.note(SvgWarning::RadialFocalRadius);
            }
            DefKind::RadialGradient
        }
        GradientKind::Sweep(_) => {
            // SVG has no conic paint server in either version, and CSS
            // `conic-gradient()` is a background image rather than a
            // paint. A flat fill from the middle of the ramp is closer
            // to right than a hole where the shape was.
            warnings.note(SvgWarning::SweepGradient);
            let mid = g
                .stops
                .iter()
                .min_by(|a, b| (a.offset - 0.5).abs().total_cmp(&(b.offset - 0.5).abs()))
                .map(|s| s.color.to_alpha_color())
                .unwrap_or(Color::BLACK);
            return solid(mid);
        }
    };
    body.push_str(" gradientUnits=\"userSpaceOnUse\"");
    if let Some(spread) = spread_method(g.extend) {
        body.push_str(" spreadMethod=\"");
        body.push_str(spread);
        body.push('"');
    }
    if let Some(t) = brush_transform {
        let mut attr = String::new();
        transform_attr(&mut attr, t, decimals);
        // `transform_attr` writes ` transform="…"`; the gradient
        // spelling differs only in the attribute name.
        body.push_str(&attr.replacen(" transform=", " gradientTransform=", 1));
    }
    body.push('>');
    for stop in g.stops.iter() {
        let c = stop.color.to_alpha_color();
        let p = solid(c);
        body.push_str("<stop offset=\"");
        num(&mut body, stop.offset as f64, decimals);
        body.push_str("\" stop-color=\"");
        body.push_str(&p.value);
        body.push('"');
        if let Some(a) = p.opacity {
            body.push_str(" stop-opacity=\"");
            num(&mut body, a as f64, 3);
            body.push('"');
        }
        body.push_str("/>");
    }
    body.push_str(match kind {
        DefKind::LinearGradient => "</linearGradient>",
        _ => "</radialGradient>",
    });
    let id = defs.intern(kind, &body, doc_prefix);
    Paint {
        value: format!("url(#{id})"),
        opacity: None,
    }
}

/// `spreadMethod`, or `None` for the `pad` default.
fn spread_method(extend: Extend) -> Option<&'static str> {
    match extend {
        Extend::Pad => None,
        Extend::Repeat => Some("repeat"),
        Extend::Reflect => Some("reflect"),
    }
}

/// Append a run of `name="number"` attributes.
fn attrs(out: &mut String, decimals: u8, pairs: &[(&str, f64)]) {
    for (name, v) in pairs {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        num(out, *v, decimals);
        out.push('"');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solid_color_is_hex_with_alpha_kept_separate() {
        let p = solid(Color::from_rgba8(0x3c, 0x78, 0xc8, 255));
        assert_eq!(p.value, "#3c78c8");
        assert_eq!(
            p.opacity, None,
            "an opaque color earns no opacity attribute"
        );

        let p = solid(Color::from_rgba8(0, 0, 0, 128));
        assert_eq!(p.value, "#000000");
        assert!((p.opacity.unwrap() - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn a_linear_gradient_is_interned_and_referenced() {
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        let g =
            Gradient::new_linear((0.0, 0.0), (10.0, 0.0)).with_stops([Color::BLACK, Color::WHITE]);
        let p = resolve(
            &Brush::Gradient(g.clone()),
            None,
            &mut defs,
            "",
            3,
            &mut warnings,
        );
        assert_eq!(p.value, "url(#lg0)");
        let mut out = String::new();
        defs.write(&mut out, None);
        assert!(out.contains("gradientUnits=\"userSpaceOnUse\""), "{out}");
        assert!(out.contains("stop-color=\"#000000\""), "{out}");

        // The same gradient again reuses the definition.
        let q = resolve(&Brush::Gradient(g), None, &mut defs, "", 3, &mut warnings);
        assert_eq!(q.value, "url(#lg0)");
    }

    #[test]
    fn a_sweep_gradient_degrades_to_a_flat_fill_and_says_so() {
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        let g = Gradient::new_sweep((0.0, 0.0), 0.0, std::f32::consts::TAU)
            .with_stops([Color::BLACK, Color::WHITE]);
        let p = resolve(&Brush::Gradient(g), None, &mut defs, "", 3, &mut warnings);
        assert!(
            p.value.starts_with('#'),
            "a solid color, not a url: {}",
            p.value
        );
        assert!(warnings.contains(&SvgWarning::SweepGradient));
    }
}
