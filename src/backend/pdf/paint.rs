//! Brushes to PDF paint: color operators for a solid, and a shading
//! pattern for a gradient.
//!
//! Gradients go through *patterns* rather than the `sh` operator
//! because both a fill and a stroke can carry one, and `sh` cannot
//! stroke. One mechanism serves both.

use super::res::{ResKind, Resources};
use super::writer::{matrix, num, COLOR_DECIMALS};
use super::{PdfWarning, Warnings};
use crate::brush::{Brush, Extend, Gradient, GradientKind};
use crate::color::Color;
use crate::geometry::{Affine, Rect};

/// Where a paint is being resolved, geometrically.
///
/// Bundled because the three travel together and a shading pattern
/// needs all of them: two to place the ramp, and the third to size the
/// soft mask that carries an alpha ramp the shading itself cannot.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PaintSpace {
    /// The primitive's own transform.
    pub(crate) transform: Affine,
    /// Scene space to the open stream's default space, which is what a
    /// pattern `/Matrix` resolves against.
    pub(crate) pattern_base: Affine,
    /// Scene space to *default user space*, which is where a soft mask
    /// is set and so the space its form's content is read in. Equal to
    /// `pattern_base` on the page and to the page flip inside a form,
    /// where the two differ.
    pub(crate) page_base: Affine,
    /// The page rectangle in default user space, which is the mask
    /// form's box.
    pub(crate) page: Rect,
}

/// A resolved paint: the operators that select it, and its alpha.
#[derive(Clone)]
pub(crate) struct Paint {
    /// Operators setting the nonstroking color, e.g. `0.2 0.4 0.8 rg\n`
    /// or `/Pattern cs /P0 scn\n`.
    pub(crate) fill_ops: String,
    /// The same for the stroking color: `RG` / `CS` / `SCN`.
    pub(crate) stroke_ops: String,
    /// Constant alpha for `/ca` / `/CA`, or `None` when fully opaque.
    pub(crate) alpha: Option<f32>,
    /// A `/SMask` entry carrying an alpha ramp the shading itself
    /// cannot, or `None` when a constant alpha says everything.
    pub(crate) mask: Option<String>,
}

impl Paint {
    /// A paint that selects nothing and paints nothing.
    fn none() -> Self {
        Self {
            fill_ops: String::new(),
            stroke_ops: String::new(),
            alpha: Some(0.0),
            mask: None,
        }
    }
}

/// The `r g b` operands of a color, clamped into range.
pub(crate) fn write_components(out: &mut String, color: Color) {
    for c in &color.components[..3] {
        num(out, f64::from(*c).clamp(0.0, 1.0), COLOR_DECIMALS);
        out.push(' ');
    }
}

/// Color operators for a solid, and its alpha separately.
///
/// `DeviceRGB` is what every viewer treats as sRGB. Tagging an ICC
/// profile would be the next step up and buys a plot nothing.
pub(crate) fn solid(color: Color) -> Paint {
    let mut fill_ops = String::new();
    write_components(&mut fill_ops, color);
    let mut stroke_ops = fill_ops.clone();
    fill_ops.push_str("rg\n");
    stroke_ops.push_str("RG\n");
    let a = color.components[3];
    Paint {
        fill_ops,
        stroke_ops,
        alpha: (a < 1.0).then_some(a.clamp(0.0, 1.0)),
        mask: None,
    }
}

/// Resolve `brush` to a paint, interning a shading pattern when needed.
///
/// See [`PaintSpace`] for what the geometry arguments mean. The
/// primitive's own transform has to be carried by the pattern matrix
/// because the `cm` an ordinary fill emits does not move its gradient.
pub(crate) fn resolve(
    brush: &Brush,
    brush_transform: Option<Affine>,
    space: PaintSpace,
    res: &mut Resources,
    decimals: u8,
    warnings: &mut Warnings,
) -> Paint {
    match brush {
        Brush::Solid(c) => solid(*c),
        Brush::Gradient(g) => gradient(g, brush_transform, space, res, decimals, warnings),
        Brush::Image(_) => {
            // An image brush needs a tiling pattern; until that lands,
            // saying so beats painting the wrong color silently.
            warnings.note(PdfWarning::ImageBrushUnsupported);
            Paint::none()
        }
    }
}

/// Intern a gradient as a shading pattern and return the operators that
/// select it.
fn gradient(
    g: &Gradient,
    brush_transform: Option<Affine>,
    space: PaintSpace,
    res: &mut Resources,
    decimals: u8,
    warnings: &mut Warnings,
) -> Paint {
    let stops = normalized_stops(g);
    // The shading type and its coordinates, shared by the color
    // shading and the gray one a soft mask paints.
    let mut coords = String::new();
    let kind = match g.kind {
        GradientKind::Linear(pos) => {
            for v in [pos.start.x, pos.start.y, pos.end.x, pos.end.y] {
                num(&mut coords, v, decimals);
                coords.push(' ');
            }
            2
        }
        GradientKind::Radial(pos) => {
            // `ShadingType 3` takes a non-zero start radius natively,
            // so there is nothing to degrade here — unlike SVG, whose
            // `fr` is a version-2 attribute.
            for v in [
                pos.start_center.x,
                pos.start_center.y,
                f64::from(pos.start_radius),
                pos.end_center.x,
                pos.end_center.y,
                f64::from(pos.end_radius),
            ] {
                num(&mut coords, v, decimals);
                coords.push(' ');
            }
            3
        }
        GradientKind::Sweep(_) => {
            // A `ShadingType 1` function-based shading with a
            // `FunctionType 4` calculator computing `atan2` would
            // express it, and is not worth a PostScript interpreter's
            // worth of output. A flat fill from the middle of the ramp
            // is closer to right than a hole where the shape was.
            warnings.note(PdfWarning::SweepGradient);
            return solid(mid_stop(&stops));
        }
    };
    coords.pop();
    if g.extend != Extend::Pad {
        // PDF has no repeating shading in any version. This is a
        // degradation SVG does not have.
        warnings.note(PdfWarning::UnsupportedExtend);
    }

    let shading = shading_dict(kind, &coords, &stops, Ramp::Rgb);
    let mut pattern = String::from("<< /Type /Pattern /PatternType 2 /Matrix [");
    let m = space.pattern_base * space.transform * brush_transform.unwrap_or(Affine::IDENTITY);
    matrix(&mut pattern, m, decimals);
    pattern.pop();
    pattern.push_str("] /Shading ");
    pattern.push_str(&shading);
    pattern.push_str(" >>");
    let name = res.intern(ResKind::Pattern, &pattern);

    // Stops that agree about alpha need only a constant; stops that
    // disagree need a mask, because a shading function produces color
    // and not alpha.
    let (alpha, mask) = match uniform_alpha(&stops) {
        Some(a) => ((a < 1.0).then_some(a), None),
        None => (
            None,
            Some(alpha_mask(
                &shading_dict(kind, &coords, &stops, Ramp::Alpha),
                space.page_base * space.transform * brush_transform.unwrap_or(Affine::IDENTITY),
                space.page,
                res,
                decimals,
            )),
        ),
    };

    Paint {
        fill_ops: format!("/Pattern cs /{name} scn\n"),
        stroke_ops: format!("/Pattern CS /{name} SCN\n"),
        alpha,
        mask,
    }
}

/// A shading dictionary over `coords`, in whichever channel `ramp`
/// names.
fn shading_dict(kind: u8, coords: &str, stops: &[(f32, Color)], ramp: Ramp) -> String {
    let mut out = format!(
        "<< /ShadingType {kind} /ColorSpace {} /Coords [{coords}] /Function ",
        match ramp {
            Ramp::Rgb => "/DeviceRGB",
            Ramp::Alpha => "/DeviceGray",
        }
    );
    ramp_function(&mut out, stops, ramp);
    out.push_str(" /Extend [true true] >>");
    out
}

/// Build a luminosity soft mask painting `gray`, and return the
/// `/SMask` entry that selects it.
///
/// `matrix` maps the shading's own coordinates into **default user
/// space** — the same composition a shading pattern's `/Matrix`
/// carries — and `page` is the page rectangle in that space.
///
/// Both follow from the rule the caller has to honour: the mask must be
/// set while the CTM is the identity. A soft-mask group is evaluated in
/// the coordinate system in force when `gs` runs, so that pins the
/// group's space to default user space, and it is what keeps the mask
/// clear of the page-sized buffer a renderer composites it in. See the
/// module CLAUDE.md.
pub(crate) fn alpha_mask(
    gray: &str,
    matrix: Affine,
    page: Rect,
    res: &mut Resources,
    decimals: u8,
) -> String {
    let shading = res.intern(ResKind::Shading, gray);
    let mut content = String::new();
    super::content::write_placement(&mut content, matrix, decimals);
    content.push_str(&format!("/{shading} sh\n"));
    mask_form(content.into_bytes(), page, res, decimals)
}

/// Wrap `content` as a luminosity-group form and return the `/SMask`
/// entry naming it.
pub(crate) fn mask_form(content: Vec<u8>, page: Rect, res: &mut Resources, decimals: u8) -> String {
    let mut dict = String::from(
        "/Type /XObject /Subtype /Form /Group \
         << /S /Transparency /CS /DeviceGray /I true /K false >> /BBox [",
    );
    for v in [page.x0, page.y0, page.x1, page.y1] {
        num(&mut dict, v, decimals);
        dict.push(' ');
    }
    dict.pop();
    dict.push_str("] /Resources ");
    dict.push_str(super::res::RES_REF);
    let key = format!("smask:{dict}|{}", super::fnv1a(&content));
    let form = res.intern_stream(ResKind::XObject, &key, &dict, content, None);
    // `/G` names a form by object number rather than by resource name,
    // which is what the reference token is for.
    format!(
        "/SMask << /S /Luminosity /G {} /BC [0] >>",
        super::res::ref_token(&form)
    )
}

/// The stops of `g`, sorted, clamped and covering the whole domain.
fn normalized_stops(g: &Gradient) -> Vec<(f32, Color)> {
    let mut stops: Vec<(f32, Color)> = g
        .stops
        .iter()
        .map(|s| (s.offset.clamp(0.0, 1.0), s.color.to_alpha_color()))
        .collect();
    if stops.is_empty() {
        return vec![(0.0, Color::BLACK)];
    }
    // Stable, so equal offsets keep source order — which is what makes
    // a hard stop come out the right way round.
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    if stops[0].0 > 0.0 {
        stops.insert(0, (0.0, stops[0].1));
    }
    if stops[stops.len() - 1].0 < 1.0 {
        stops.push((1.0, stops[stops.len() - 1].1));
    }
    stops
}

/// The stop nearest the middle of the ramp, for the flat fallbacks.
fn mid_stop(stops: &[(f32, Color)]) -> Color {
    stops
        .iter()
        .min_by(|a, b| (a.0 - 0.5).abs().total_cmp(&(b.0 - 0.5).abs()))
        .map(|s| s.1)
        .unwrap_or(Color::BLACK)
}

/// One alpha for a whole ramp, or `None` when the stops disagree.
///
/// Disagreement is not a degradation: a PDF shading function produces
/// color and not alpha, so a varying ramp is carried by a luminosity
/// soft mask instead of by `/ca`.
pub(crate) fn uniform_alpha(stops: &[(f32, Color)]) -> Option<f32> {
    let first = stops.first().map(|s| s.1.components[3]).unwrap_or(1.0);
    stops
        .iter()
        .all(|s| s.1.components[3] == first)
        .then(|| first.clamp(0.0, 1.0))
}

/// Which components a ramp's `FunctionType 2` segments carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ramp {
    /// Three `DeviceRGB` components — the color a shading paints.
    Rgb,
    /// One `DeviceGray` component holding the stop's alpha, for the
    /// luminosity soft mask that carries what the color shading cannot.
    Alpha,
}

/// Append the `/Function` a color ramp needs.
///
/// A `FunctionType 3` stitching a chain of `FunctionType 2`
/// exponentials, or the single exponential alone when one segment
/// covers the domain. Functions may be direct objects, so this costs no
/// indirect object of its own.
///
/// `stops` must be sorted and cover `[0, 1]`; a pair with equal offsets
/// is dropped, which is exactly how a hard stop is expressed and what
/// guarantees `/Bounds` comes out strictly increasing.
pub(crate) fn ramp_function(out: &mut String, stops: &[(f32, Color)], ramp: Ramp) {
    let mut segments: Vec<(f32, f32, Color, Color)> = Vec::new();
    for pair in stops.windows(2) {
        let (o0, c0) = pair[0];
        let (o1, c1) = pair[1];
        if o1 > o0 {
            segments.push((o0, o1, c0, c1));
        }
    }
    if segments.is_empty() {
        // Every stop at one offset: a constant function over the whole
        // domain.
        let c = stops.last().map(|s| s.1).unwrap_or(Color::BLACK);
        exponential(out, c, c, ramp);
        return;
    }
    if segments.len() == 1 {
        exponential(out, segments[0].2, segments[0].3, ramp);
        return;
    }
    out.push_str("<< /FunctionType 3 /Domain [0 1] /Functions [");
    for (_, _, c0, c1) in &segments {
        exponential(out, *c0, *c1, ramp);
        out.push(' ');
    }
    out.push_str("] /Bounds [");
    for (i, seg) in segments.iter().take(segments.len() - 1).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        num(out, f64::from(seg.1), 6);
    }
    out.push_str("] /Encode [");
    for i in 0..segments.len() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str("0 1");
    }
    out.push_str("] >>");
}

/// One linear segment of a ramp as a `FunctionType 2`.
fn exponential(out: &mut String, c0: Color, c1: Color, ramp: Ramp) {
    out.push_str("<< /FunctionType 2 /Domain [0 1] /C0 [");
    write_ramp_components(out, c0, ramp);
    out.push_str("] /C1 [");
    write_ramp_components(out, c1, ramp);
    out.push_str("] /N 1 >>");
}

/// Append one endpoint's components, without a trailing space.
fn write_ramp_components(out: &mut String, c: Color, ramp: Ramp) {
    match ramp {
        Ramp::Rgb => {
            write_components(out, c);
            out.pop();
        }
        // Luminosity reads the gray value straight back out, so the
        // stop's alpha *is* the component.
        Ramp::Alpha => num(
            out,
            f64::from(c.components[3]).clamp(0.0, 1.0),
            COLOR_DECIMALS,
        ),
    }
}

/// The `/ExtGState` body for a constant alpha, a blend mode and a soft
/// mask, or `None` when none of the three is needed.
pub(crate) fn ext_gstate(
    fill_alpha: Option<f32>,
    stroke_alpha: Option<f32>,
    blend: Option<crate::blend::Mix>,
    mask: Option<&str>,
) -> Option<String> {
    let blend = blend.filter(|m| *m != crate::blend::Mix::Normal);
    if fill_alpha.is_none() && stroke_alpha.is_none() && blend.is_none() && mask.is_none() {
        return None;
    }
    let mut body = String::from("<< /Type /ExtGState ");
    if let Some(a) = fill_alpha {
        body.push_str("/ca ");
        num(&mut body, f64::from(a), 4);
        body.push(' ');
    }
    if let Some(a) = stroke_alpha {
        body.push_str("/CA ");
        num(&mut body, f64::from(a), 4);
        body.push(' ');
    }
    if let Some(m) = blend {
        body.push_str("/BM /");
        body.push_str(super::content::bm_name(m));
        body.push(' ');
    }
    if let Some(m) = mask {
        body.push_str(m);
        body.push(' ');
    }
    body.push_str(">>");
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stops(v: &[(f32, Color)]) -> Vec<(f32, Color)> {
        v.to_vec()
    }

    fn space(transform: Affine) -> PaintSpace {
        PaintSpace {
            transform,
            pattern_base: Affine::IDENTITY,
            page_base: Affine::IDENTITY,
            page: Rect::new(0.0, 0.0, 100.0, 100.0),
        }
    }

    #[test]
    fn a_solid_color_writes_both_operator_cases() {
        let p = solid(Color::from_rgba8(0x3c, 0x78, 0xc8, 255));
        assert!(p.fill_ops.ends_with("rg\n"), "{}", p.fill_ops);
        assert!(p.stroke_ops.ends_with("RG\n"), "{}", p.stroke_ops);
        assert_eq!(p.alpha, None, "an opaque color earns no ExtGState");
    }

    #[test]
    fn one_segment_needs_no_stitching_wrapper() {
        let mut s = String::new();
        ramp_function(
            &mut s,
            &stops(&[(0.0, Color::BLACK), (1.0, Color::WHITE)]),
            Ramp::Rgb,
        );
        assert!(s.starts_with("<< /FunctionType 2"), "{s}");
        assert!(!s.contains("FunctionType 3"), "{s}");
    }

    #[test]
    fn three_stops_stitch_two_exponentials() {
        let mut s = String::new();
        ramp_function(
            &mut s,
            &stops(&[
                (0.0, Color::BLACK),
                (0.5, Color::from_rgba8(255, 0, 0, 255)),
                (1.0, Color::WHITE),
            ]),
            Ramp::Rgb,
        );
        assert!(s.starts_with("<< /FunctionType 3"), "{s}");
        assert!(s.contains("/Bounds [0.5]"), "{s}");
        assert!(s.contains("/Encode [0 1 0 1]"), "{s}");
        assert_eq!(s.matches("/FunctionType 2").count(), 2, "{s}");
    }

    /// A hard stop is two stops at one offset. Dropping the zero-width
    /// segment is what keeps `/Bounds` strictly increasing, which the
    /// spec requires and viewers enforce.
    #[test]
    fn a_hard_stop_produces_no_zero_width_segment() {
        let mut s = String::new();
        ramp_function(
            &mut s,
            &stops(&[
                (0.0, Color::BLACK),
                (0.5, Color::BLACK),
                (0.5, Color::WHITE),
                (1.0, Color::WHITE),
            ]),
            Ramp::Rgb,
        );
        assert!(s.contains("/Bounds [0.5]"), "{s}");
        assert_eq!(s.matches("/FunctionType 2").count(), 2, "{s}");
    }

    #[test]
    fn stops_are_sorted_clamped_and_extended_to_the_whole_domain() {
        let g = Gradient::new_linear((0.0, 0.0), (10.0, 0.0))
            .with_stops([Color::BLACK, Color::WHITE])
            .with_extend(Extend::Pad);
        let s = normalized_stops(&g);
        assert_eq!(s.first().unwrap().0, 0.0);
        assert_eq!(s.last().unwrap().0, 1.0);
    }

    #[test]
    fn a_gradient_interns_as_a_shading_pattern() {
        let mut res = Resources::default();
        let mut warnings = Warnings::default();
        let g =
            Gradient::new_linear((0.0, 0.0), (10.0, 0.0)).with_stops([Color::BLACK, Color::WHITE]);
        let p = resolve(
            &Brush::Gradient(g.clone()),
            None,
            space(Affine::IDENTITY),
            &mut res,
            3,
            &mut warnings,
        );
        assert_eq!(p.fill_ops, "/Pattern cs /P0 scn\n");
        assert_eq!(p.stroke_ops, "/Pattern CS /P0 SCN\n");

        // The same gradient under the same transform reuses it.
        let q = resolve(
            &Brush::Gradient(g.clone()),
            None,
            space(Affine::IDENTITY),
            &mut res,
            3,
            &mut warnings,
        );
        assert_eq!(q.fill_ops, "/Pattern cs /P0 scn\n");

        // Under a different transform it is a different pattern, which
        // is correct: the matrix carries the transform.
        let r = resolve(
            &Brush::Gradient(g),
            None,
            space(Affine::translate((5.0, 0.0))),
            &mut res,
            3,
            &mut warnings,
        );
        assert_eq!(r.fill_ops, "/Pattern cs /P1 scn\n");
    }

    #[test]
    fn a_sweep_gradient_degrades_to_a_flat_fill_and_says_so() {
        let mut res = Resources::default();
        let mut warnings = Warnings::default();
        let g = Gradient::new_sweep((0.0, 0.0), 0.0, std::f32::consts::TAU)
            .with_stops([Color::BLACK, Color::WHITE]);
        let p = resolve(
            &Brush::Gradient(g),
            None,
            space(Affine::IDENTITY),
            &mut res,
            3,
            &mut warnings,
        );
        assert!(p.fill_ops.ends_with("rg\n"), "{}", p.fill_ops);
        assert!(warnings.contains(&PdfWarning::SweepGradient));
    }

    #[test]
    fn a_repeating_gradient_is_padded_and_reported() {
        let mut res = Resources::default();
        let mut warnings = Warnings::default();
        let g = Gradient::new_linear((0.0, 0.0), (10.0, 0.0))
            .with_stops([Color::BLACK, Color::WHITE])
            .with_extend(Extend::Repeat);
        resolve(
            &Brush::Gradient(g),
            None,
            space(Affine::IDENTITY),
            &mut res,
            3,
            &mut warnings,
        );
        assert!(warnings.contains(&PdfWarning::UnsupportedExtend));
    }

    #[test]
    fn stops_agreeing_about_alpha_need_only_a_constant() {
        let a = uniform_alpha(&stops(&[
            (0.0, Color::from_rgba8(0, 0, 0, 128)),
            (1.0, Color::from_rgba8(255, 255, 255, 128)),
        ]));
        assert!((a.unwrap() - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn stops_disagreeing_about_alpha_have_no_single_constant() {
        assert!(uniform_alpha(&stops(&[
            (0.0, Color::from_rgba8(0, 0, 0, 0)),
            (1.0, Color::from_rgba8(0, 0, 0, 255)),
        ]))
        .is_none());
    }

    /// The gap this backend used to have: a gradient fading from
    /// transparent to opaque printed at a flat mid alpha.
    #[test]
    fn a_fading_gradient_earns_a_luminosity_soft_mask() {
        let mut res = Resources::default();
        let mut warnings = Warnings::default();
        let g = Gradient::new_linear((0.0, 0.0), (10.0, 0.0)).with_stops([
            Color::from_rgba8(0, 0, 255, 0),
            Color::from_rgba8(0, 0, 255, 255),
        ]);
        let p = resolve(
            &Brush::Gradient(g),
            None,
            space(Affine::IDENTITY),
            &mut res,
            3,
            &mut warnings,
        );
        assert!(p.alpha.is_none(), "the mask carries it, not a constant");
        let mask = p.mask.expect("a soft mask");
        assert!(mask.contains("/S /Luminosity"), "{mask}");
        assert!(mask.contains("/BC [0]"), "{mask}");
        assert!(
            warnings.0.is_empty(),
            "nothing degraded, so nothing is reported: {:?}",
            warnings.0
        );
    }

    /// The mask's ramp is the alpha channel, one `DeviceGray`
    /// component per stop rather than three colour ones.
    #[test]
    fn a_gray_ramp_carries_the_stop_alphas() {
        let mut s = String::new();
        ramp_function(
            &mut s,
            &stops(&[
                (0.0, Color::from_rgba8(255, 0, 0, 0)),
                (1.0, Color::from_rgba8(0, 255, 0, 255)),
            ]),
            Ramp::Alpha,
        );
        assert_eq!(
            s,
            "<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >>"
        );
    }

    #[test]
    fn an_opaque_gradient_needs_no_mask_at_all() {
        let mut res = Resources::default();
        let mut warnings = Warnings::default();
        let g =
            Gradient::new_linear((0.0, 0.0), (10.0, 0.0)).with_stops([Color::BLACK, Color::WHITE]);
        let p = resolve(
            &Brush::Gradient(g),
            None,
            space(Affine::IDENTITY),
            &mut res,
            3,
            &mut warnings,
        );
        assert!(p.mask.is_none());
        assert!(p.alpha.is_none());
    }
}
