//! Brushes to SVG paint: solid colors, and gradients as `<defs>`
//! entries referenced by `url(#…)`.

use super::defs::{DefKind, Defs};
use super::writer::{is_identity, num, transform_attr};
use super::{SvgWarning, Warnings};
use crate::brush::{Brush, Extend, Gradient, GradientKind, ImageBrush};
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
        Brush::Image(ib) => {
            image_pattern(ib, brush_transform, defs, doc_prefix, decimals, warnings)
        }
        Brush::Pattern(p) => {
            vector_pattern(p, brush_transform, defs, doc_prefix, decimals, warnings)
        }
    }
}

/// Intern a vector fill pattern as a `<pattern>` element and return the
/// `url(#…)` that references it.
///
/// The tile viewport is `spacing` wide and high, anchored at
/// `tile_rect`'s origin, with `patternUnits="userSpaceOnUse"` so tile
/// coordinates live in the referencing element's user space — the same
/// contract the gradient emitters use. `pattern.transform` and
/// `brush_transform` compose into `patternTransform`. Content is clipped
/// to the viewport by the renderer, which is exact when the tile rect
/// and the spacing rect coincide and trims overlapping tiles otherwise.
fn vector_pattern(
    p: &crate::brush::FillPattern,
    brush_transform: Option<Affine>,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) -> Paint {
    use crate::brush::PatternElement;
    let p = crate::pattern::bound(p);
    let dec = decimals;
    let mut body = String::from("<pattern patternUnits=\"userSpaceOnUse\" x=\"");
    num(&mut body, p.tile_rect.x0, dec);
    body.push_str("\" y=\"");
    num(&mut body, p.tile_rect.y0, dec);
    body.push_str("\" width=\"");
    num(&mut body, p.spacing.x, dec);
    body.push_str("\" height=\"");
    num(&mut body, p.spacing.y, dec);
    body.push('"');
    let pt = brush_transform.unwrap_or(Affine::IDENTITY) * p.transform;
    if !is_identity(pt) {
        body.push_str(" patternTransform=\"matrix(");
        for v in pt.as_coeffs() {
            num(&mut body, v, dec);
            body.push(' ');
        }
        body.pop();
        body.push_str(")\"");
    }
    body.push('>');
    let tile_h = p.tile_rect.height();
    for el in &p.elements {
        match el {
            PatternElement::Path { path, fill, stroke } => {
                path_el(
                    &mut body,
                    path,
                    fill.as_ref(),
                    stroke.as_deref(),
                    defs,
                    doc_prefix,
                    dec,
                    warnings,
                );
            }
            PatternElement::Shape {
                name,
                at,
                size,
                paint,
            } => shape_el(
                &mut body,
                name,
                *at,
                size * tile_h,
                paint,
                defs,
                doc_prefix,
                dec,
                warnings,
            ),
            PatternElement::Text {
                content,
                at,
                size,
                family,
                paint,
            } => text_el(
                &mut body,
                content,
                *at,
                size * tile_h,
                family.as_deref(),
                paint,
                defs,
                doc_prefix,
                dec,
                warnings,
            ),
        }
    }
    body.push_str("</pattern>");
    let id = defs.intern(DefKind::Pattern, &body, doc_prefix);
    Paint {
        value: format!("url(#{id})"),
        opacity: None,
    }
}

/// An element's paint, resolved like any other brush except that a
/// nested pattern is skipped — one level of `<pattern>` is enough.
fn element_paint(
    b: &Brush,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) -> Option<Paint> {
    match b {
        Brush::Solid(c) => Some(solid(*c)),
        Brush::Gradient(g) => Some(gradient(g, None, defs, doc_prefix, decimals, warnings)),
        Brush::Image(ib) => Some(image_pattern(
            ib, None, defs, doc_prefix, decimals, warnings,
        )),
        Brush::Pattern(_) => {
            warnings.note(SvgWarning::UnsupportedPatternElement);
            None
        }
    }
}

/// Emit one tile path as a `<path>`, filled and/or stroked.
#[allow(clippy::too_many_arguments)]
fn path_el(
    out: &mut String,
    path: &crate::path::Path,
    fill: Option<&crate::brush::PatternPaint>,
    stroke: Option<&(crate::stroke::Stroke, crate::brush::PatternPaint)>,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) {
    use crate::brush::PatternPaint;
    let d = super::path::to_d(path, decimals);
    if d.is_empty() {
        return;
    }
    out.push_str("<path d=\"");
    out.push_str(&d);
    out.push('"');
    let fill_paint = match fill {
        Some(PatternPaint::Fixed(b)) => element_paint(b, defs, doc_prefix, decimals, warnings),
        // Bound patterns carry `Fixed` only; anything else is a skip.
        _ => None,
    };
    match (fill, fill_paint) {
        (Some(_), Some(paint)) => {
            super::write_fill_attrs_to(out, &paint, crate::path::FillRule::NonZero, decimals)
        }
        (Some(_), None) => out.push_str(" fill=\"none\""),
        (None, _) => out.push_str(" fill=\"none\""),
    }
    if let Some((style, PatternPaint::Fixed(b))) = stroke {
        if let Some(paint) = element_paint(b, defs, doc_prefix, decimals, warnings) {
            super::write_stroke_attrs_to(out, style, &paint, decimals, warnings);
        }
    }
    out.push_str("/>");
}

/// Emit a text element as a real `<text>`, keeping the pattern editable:
/// the family is named rather than resolved, so the viewer shapes it.
/// `at` is the baseline origin, which is exactly what SVG's `y` is, and
/// `size` is the em size in tile units, which is what `font-size` takes.
#[allow(clippy::too_many_arguments)]
fn text_el(
    out: &mut String,
    content: &str,
    at: crate::geometry::Point,
    size: f64,
    family: Option<&str>,
    paint: &crate::brush::PatternPaint,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) {
    let crate::brush::PatternPaint::Fixed(b) = paint else {
        return;
    };
    let Some(p) = element_paint(b, defs, doc_prefix, decimals, warnings) else {
        return;
    };
    out.push_str("<text x=\"");
    num(out, at.x, decimals);
    out.push_str("\" y=\"");
    num(out, at.y, decimals);
    out.push_str("\" font-size=\"");
    num(out, size, decimals);
    out.push('"');
    if let Some(f) = family {
        out.push_str(" font-family=\"");
        super::writer::escape_attr(out, f);
        out.push('"');
    }
    out.push_str(" fill=\"");
    out.push_str(&p.value);
    out.push('"');
    if let Some(a) = p.opacity {
        out.push_str(" fill-opacity=\"");
        num(out, a as f64, decimals.max(3));
        out.push('"');
    }
    out.push('>');
    super::writer::escape_text(out, content);
    out.push_str("</text>");
}

/// Emit a shape element as one `<path>` per subpath, stamped at `at`
/// scaled to `height`, in the pattern's own coordinates.
#[allow(clippy::too_many_arguments)]
fn shape_el(
    out: &mut String,
    name: &str,
    at: crate::geometry::Point,
    height: f64,
    paint: &crate::brush::PatternPaint,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) {
    use crate::shape::{ShapeKind, ShapeStyle};
    let crate::brush::PatternPaint::Fixed(b) = paint else {
        return;
    };
    let shapes = crate::shape::ShapeRegistry::with_builtins();
    let Some(shape) = shapes.get(name) else {
        warnings.note(SvgWarning::UnsupportedPatternElement);
        return;
    };
    let ShapeKind::Paths { paths, style } = shape.kind() else {
        warnings.note(SvgWarning::UnsupportedPatternElement);
        return;
    };
    let bbox = shape.bounding_box();
    if !(bbox.height() > 0.0 && height.is_finite() && height > 0.0) {
        return;
    }
    let scale = height / bbox.height();
    let anchor = shape.anchor();
    let xform = Affine::translate(at.to_vec2())
        * Affine::scale(scale)
        * Affine::translate((-anchor.x, -anchor.y));
    for path in paths {
        let d = super::path::to_d(path, decimals);
        if d.is_empty() {
            continue;
        }
        out.push_str("<path d=\"");
        out.push_str(&d);
        out.push('"');
        super::writer::transform_attr(out, xform, decimals);
        match style {
            ShapeStyle::Fill => {
                if let Some(paint) = element_paint(b, defs, doc_prefix, decimals, warnings) {
                    super::write_fill_attrs_to(
                        out,
                        &paint,
                        crate::path::FillRule::NonZero,
                        decimals,
                    );
                }
            }
            ShapeStyle::Stroke => {
                if let Some(paint) = element_paint(b, defs, doc_prefix, decimals, warnings) {
                    // The width lands scaled by `xform`; divide it back
                    // out so the stamp matches the replay backends.
                    let stroke = crate::stroke::Stroke::new(height * 0.125 / scale);
                    super::write_stroke_attrs_to(out, &stroke, &paint, decimals, warnings);
                }
            }
        }
        out.push_str("/>");
    }
}

/// Intern an image brush as a `<pattern>` tiling its image, and return
/// the `url(#…)` that references it.
///
/// Only `Repeat` is exact: an SVG pattern always tiles, so a pad or
/// reflect sampler repeats instead and says so.
fn image_pattern(
    ib: &ImageBrush,
    brush_transform: Option<Affine>,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) -> Paint {
    let sampler = &ib.sampler;
    if sampler.x_extend != Extend::Repeat || sampler.y_extend != Extend::Repeat {
        warnings.note(SvgWarning::UnsupportedExtend);
    }
    let Some(id) = super::image::pattern(
        &ib.image,
        sampler.quality,
        brush_transform,
        defs,
        doc_prefix,
        decimals,
        warnings,
    ) else {
        return Paint {
            value: "none".into(),
            opacity: None,
        };
    };
    Paint {
        value: format!("url(#{id})"),
        opacity: (sampler.alpha < 1.0).then_some(sampler.alpha),
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
    fn a_vector_pattern_is_interned_with_its_tile_as_markup() {
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        let brush = Brush::Pattern(std::sync::Arc::new(
            crate::brush::fill_pattern("stripe", Brush::Solid(Color::BLACK)).unwrap(),
        ));
        let p = resolve(&brush, None, &mut defs, "", 3, &mut warnings);
        assert!(p.value.starts_with("url(#pt"), "{}", p.value);
        let mut out = String::new();
        defs.write(&mut out, None);
        assert!(out.contains("patternUnits=\"userSpaceOnUse\""), "{out}");
        // One stroked tile line: a path with no fill and a stroke.
        assert!(out.contains("fill=\"none\""), "{out}");
        assert!(out.contains("stroke=\"#000000\""), "{out}");
        // The stripe's 45° rotation lands in patternTransform.
        assert!(out.contains("patternTransform=\"matrix("), "{out}");
        assert!(!warnings.contains(&SvgWarning::UnsupportedPatternElement));
    }

    #[test]
    fn an_identical_pattern_shares_one_def() {
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        let brush = || {
            Brush::Pattern(std::sync::Arc::new(
                crate::brush::fill_pattern("crosshatch", Brush::Solid(Color::BLACK)).unwrap(),
            ))
        };
        let a = resolve(&brush(), None, &mut defs, "", 3, &mut warnings);
        let b = resolve(&brush(), None, &mut defs, "", 3, &mut warnings);
        assert_eq!(a.value, b.value);
    }

    #[test]
    fn a_text_element_becomes_a_text_element() {
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        let pattern = crate::brush::FillPattern::new(
            vec![crate::brush::PatternElement::Text {
                content: std::sync::Arc::from("ab"),
                at: crate::geometry::Point::new(0.5, 0.8),
                size: 0.8,
                family: Some(std::sync::Arc::from("serif")),
                paint: crate::brush::PatternPaint::Foreground,
            }],
            crate::geometry::Rect::new(0.0, 0.0, 1.0, 1.0),
            crate::geometry::Vec2::new(1.0, 1.0),
        );
        let brush = Brush::Pattern(std::sync::Arc::new(pattern));
        let p = resolve(&brush, None, &mut defs, "", 3, &mut warnings);
        assert!(p.value.starts_with("url(#"));
        let mut out = String::new();
        defs.write(&mut out, None);
        assert!(
            out.contains("<text x=\"0.5\" y=\"0.8\" font-size=\"0.8\" font-family=\"serif\" fill=\"#000000\">ab</text>"),
            "{out}"
        );
        assert!(!warnings.contains(&SvgWarning::UnsupportedPatternElement));
    }

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

    /// A 2×1 red/blue image to tile.
    #[cfg(feature = "png")]
    fn checker() -> ImageBrush {
        use crate::brush::{Blob, Image, ImageAlphaType, ImageFormat};
        ImageBrush::new(Image {
            data: Blob::from(vec![255, 0, 0, 255, 0, 0, 255, 255]),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 1,
        })
    }

    #[cfg(feature = "png")]
    #[test]
    fn a_repeating_image_brush_is_interned_as_a_pattern() {
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        let brush = Brush::Image(checker().with_extend(Extend::Repeat));
        let p = resolve(&brush, None, &mut defs, "", 3, &mut warnings);
        assert_eq!(p.value, "url(#pt0)");
        assert_eq!(p.opacity, None, "the default sampler is opaque");
        assert!(!warnings.contains(&SvgWarning::UnsupportedExtend));
        let mut out = String::new();
        defs.write(&mut out, None);
        assert!(
            out.contains(
                "<pattern id=\"pt0\" patternUnits=\"userSpaceOnUse\" width=\"2\" height=\"1\">"
            ),
            "{out}"
        );
        assert!(out.contains("data:image/png;base64,"), "{out}");

        // The same brush again reuses the definition.
        let q = resolve(&brush, None, &mut defs, "", 3, &mut warnings);
        assert_eq!(q.value, "url(#pt0)");
    }

    #[cfg(feature = "png")]
    #[test]
    fn a_non_repeating_image_brush_tiles_and_says_so() {
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        // The default sampler pads, which a pattern cannot express.
        let p = resolve(
            &Brush::Image(checker()),
            None,
            &mut defs,
            "",
            3,
            &mut warnings,
        );
        assert!(p.value.starts_with("url(#"), "{:?}", p.value);
        assert!(warnings.contains(&SvgWarning::UnsupportedExtend));
    }

    #[cfg(feature = "png")]
    #[test]
    fn an_image_brushs_alpha_becomes_the_paint_opacity() {
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        let brush = Brush::Image(checker().with_extend(Extend::Repeat).with_alpha(0.5));
        let p = resolve(&brush, None, &mut defs, "", 3, &mut warnings);
        assert_eq!(p.opacity, Some(0.5));
    }

    #[cfg(not(feature = "png"))]
    #[test]
    fn an_image_brush_without_a_png_encoder_draws_nothing_and_says_so() {
        use crate::brush::{Blob, Image, ImageAlphaType, ImageFormat};
        let mut defs = Defs::default();
        let mut warnings = Warnings::default();
        let brush = Brush::Image(ImageBrush::new(Image {
            data: Blob::from(vec![255, 0, 0, 255]),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        }));
        let p = resolve(&brush, None, &mut defs, "", 3, &mut warnings);
        assert_eq!(p.value, "none");
        assert!(warnings.contains(&SvgWarning::MissingPngFeature));
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
