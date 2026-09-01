//! Paths and graphics state as content-stream operators.
//!
//! Every drawing operator is preceded by the complete set of
//! graphics-state operators it depends on — nothing is inherited
//! between primitives. [`SceneBuilder`](crate::scene::SceneBuilder) has
//! no current-transform or current-brush state, so the emitter has none
//! either. It costs a few dozen redundant bytes per primitive, which
//! flate removes almost entirely, and it buys immunity from every
//! `q`/`Q`-nesting bug a state cache would introduce.

use super::writer::num;
use crate::blend::Mix;
use crate::geometry::{Affine, Point};
use crate::path::{FillRule, Path};
use crate::stroke::{Cap, Join, Stroke};

/// Append `path` as path-construction operators.
///
/// Returns false when the path held nothing to write, and sets
/// `non_finite` when a coordinate had to be written as zero.
///
/// PDF has no quadratic segment, so a `QuadTo` is elevated to a cubic —
/// the one place the scene's path vocabulary and PDF's disagree.
pub(crate) fn write_path(
    out: &mut String,
    path: &Path,
    decimals: u8,
    non_finite: &mut bool,
) -> bool {
    use crate::path::PathEl::*;
    let mut wrote = false;
    let mut current = Point::ZERO;
    let mut start = Point::ZERO;
    for el in path.elements() {
        match el {
            MoveTo(p) => {
                pt(out, *p, decimals, non_finite);
                out.push_str("m\n");
                current = *p;
                start = *p;
                wrote = true;
            }
            LineTo(p) => {
                pt(out, *p, decimals, non_finite);
                out.push_str("l\n");
                current = *p;
            }
            QuadTo(c, p) => {
                // Degree elevation: c1 = p0 + 2/3 (c - p0),
                // c2 = p1 + 2/3 (c - p1).
                let c1 = current + (*c - current) * (2.0 / 3.0);
                let c2 = *p + (*c - *p) * (2.0 / 3.0);
                pt(out, c1, decimals, non_finite);
                pt(out, c2, decimals, non_finite);
                pt(out, *p, decimals, non_finite);
                out.push_str("c\n");
                current = *p;
            }
            CurveTo(a, b, p) => {
                pt(out, *a, decimals, non_finite);
                pt(out, *b, decimals, non_finite);
                pt(out, *p, decimals, non_finite);
                out.push_str("c\n");
                current = *p;
            }
            ClosePath => {
                out.push_str("h\n");
                current = start;
            }
        }
    }
    wrote
}

/// Append one point's two operands, each followed by a space.
fn pt(out: &mut String, p: Point, decimals: u8, non_finite: &mut bool) {
    if !p.x.is_finite() || !p.y.is_finite() {
        *non_finite = true;
    }
    num(out, p.x, decimals);
    out.push(' ');
    num(out, p.y, decimals);
    out.push(' ');
}

/// Append `a b c d e f cm` unconditionally.
///
/// Unlike [`cm`](super::writer::cm) this writes the identity too: an
/// image XObject and a `sh` shading are placed *by* their matrix, so
/// omitting it would leave the unit square where the page put it.
pub(crate) fn write_placement(out: &mut String, a: Affine, decimals: u8) {
    super::writer::matrix(out, a, decimals);
    out.push_str("cm\n");
}

/// Append `x y w h re`, PDF's rectangle shorthand.
pub(crate) fn write_rect(out: &mut String, x: f64, y: f64, w: f64, h: f64, decimals: u8) {
    for v in [x, y, w, h] {
        num(out, v, decimals);
        out.push(' ');
    }
    out.push_str("re\n");
}

/// The painting operator for a fill under `rule`.
pub(crate) fn fill_op(rule: FillRule) -> &'static str {
    match rule {
        FillRule::NonZero => "f\n",
        FillRule::EvenOdd => "f*\n",
    }
}

/// The painting operator for a combined fill and stroke under `rule`.
pub(crate) fn fill_stroke_op(rule: FillRule) -> &'static str {
    match rule {
        FillRule::NonZero => "B\n",
        FillRule::EvenOdd => "B*\n",
    }
}

/// The clipping operators for `rule`, ending the path without painting.
pub(crate) fn clip_op(rule: FillRule) -> &'static str {
    match rule {
        FillRule::NonZero => "W n\n",
        FillRule::EvenOdd => "W* n\n",
    }
}

/// Append every pen parameter `stroke` carries.
///
/// All five are written unconditionally, per the module's
/// stateless-operator rule. Returns true when the pen set different
/// start and end caps, which PDF cannot express — it has one cap
/// setting, and the start cap is used.
pub(crate) fn write_stroke_state(out: &mut String, stroke: &Stroke, decimals: u8) -> bool {
    num(out, stroke.width, decimals);
    out.push_str(" w\n");
    let cap = match stroke.start_cap {
        Cap::Butt => '0',
        Cap::Round => '1',
        Cap::Square => '2',
    };
    out.push(cap);
    out.push_str(" J\n");
    let join = match stroke.join {
        Join::Miter => '0',
        Join::Round => '1',
        Join::Bevel => '2',
    };
    out.push(join);
    out.push_str(" j\n");
    num(out, stroke.miter_limit, decimals);
    out.push_str(" M\n");
    // A zero-length dash array is a PDF error in some viewers, and a
    // pattern summing to zero would divide by zero in the dash phase.
    if stroke.dash_pattern.is_empty() || stroke.dash_pattern.iter().sum::<f64>() <= 0.0 {
        out.push_str("[] 0 d\n");
    } else {
        out.push('[');
        for (i, v) in stroke.dash_pattern.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            num(out, *v, decimals);
        }
        out.push_str("] ");
        num(out, stroke.dash_offset, decimals);
        out.push_str(" d\n");
    }
    stroke.start_cap != stroke.end_cap
}

/// The `/BM` name for a mix function.
///
/// The enum is exactly PDF's blend-mode name set, so this is a spelling
/// change and nothing more.
pub(crate) fn bm_name(mix: Mix) -> &'static str {
    match mix {
        Mix::Normal => "Normal",
        Mix::Multiply => "Multiply",
        Mix::Screen => "Screen",
        Mix::Overlay => "Overlay",
        Mix::Darken => "Darken",
        Mix::Lighten => "Lighten",
        Mix::ColorDodge => "ColorDodge",
        Mix::ColorBurn => "ColorBurn",
        Mix::HardLight => "HardLight",
        Mix::SoftLight => "SoftLight",
        Mix::Difference => "Difference",
        Mix::Exclusion => "Exclusion",
        Mix::Hue => "Hue",
        Mix::Saturation => "Saturation",
        Mix::Color => "Color",
        Mix::Luminosity => "Luminosity",
    }
}

/// What an open layer does when it is popped.
#[derive(Clone, Copy)]
pub(crate) enum LayerFrame {
    /// `q` … `Q` in the enclosing stream.
    Simple,
    /// A transparency group with its own content stream, painted with
    /// these once popped.
    Group {
        /// Blend mode the group composites with.
        blend: crate::blend::BlendMode,
        /// Constant alpha applied to the group as a whole.
        alpha: f32,
    },
}

/// One content stream under construction.
#[derive(Clone)]
pub(crate) struct Target {
    /// The operator text. ASCII throughout, so a `String` is the right
    /// type.
    pub(crate) content: String,
    /// Maps scene space to this stream's default space, which is what a
    /// pattern `/Matrix` resolves against: the page flip on the page,
    /// the identity inside a form.
    pub(crate) pattern_base: Affine,
}

impl Target {
    /// A stream whose default space is `pattern_base`.
    pub(crate) fn new(pattern_base: Affine) -> Self {
        Self {
            content: String::new(),
            pattern_base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit(path: &Path) -> String {
        let mut s = String::new();
        let mut nf = false;
        write_path(&mut s, path, 3, &mut nf);
        s
    }

    #[test]
    fn every_element_kind_maps_to_its_operator() {
        let mut p = Path::new();
        p.move_to(Point::new(0.0, 0.0));
        p.line_to(Point::new(10.0, 0.0));
        p.curve_to(
            Point::new(8.0, 12.0),
            Point::new(2.0, 12.0),
            Point::new(0.0, 10.0),
        );
        p.close_path();
        assert_eq!(emit(&p), "0 0 m\n10 0 l\n8 12 2 12 0 10 c\nh\n");
    }

    /// A quadratic elevated to a cubic must pass through the same
    /// points, or every glyph-derived path drifts.
    #[test]
    fn a_quadratic_elevates_to_the_equivalent_cubic() {
        let mut p = Path::new();
        p.move_to(Point::new(0.0, 0.0));
        p.quad_to(Point::new(6.0, 12.0), Point::new(12.0, 0.0));
        assert_eq!(emit(&p), "0 0 m\n4 8 8 8 12 0 c\n");
    }

    #[test]
    fn an_empty_path_reports_that_it_wrote_nothing() {
        let mut s = String::new();
        let mut nf = false;
        assert!(!write_path(&mut s, &Path::new(), 3, &mut nf));
        assert!(s.is_empty());
    }

    #[test]
    fn a_non_finite_coordinate_is_written_as_zero_and_reported() {
        let mut p = Path::new();
        p.move_to(Point::new(f64::NAN, 3.0));
        let mut s = String::new();
        let mut nf = false;
        write_path(&mut s, &p, 3, &mut nf);
        assert_eq!(s, "0 3 m\n");
        assert!(nf);
    }

    #[test]
    fn an_empty_dash_pattern_writes_the_solid_form() {
        let mut s = String::new();
        write_stroke_state(&mut s, &Stroke::new(2.0), 3);
        assert!(s.contains("2 w\n"), "{s}");
        assert!(s.contains("[] 0 d\n"), "{s}");
    }

    #[test]
    fn asymmetric_caps_are_reported() {
        let stroke = Stroke::new(1.0)
            .with_start_cap(Cap::Round)
            .with_end_cap(Cap::Butt);
        let mut s = String::new();
        assert!(write_stroke_state(&mut s, &stroke, 3));
        assert!(s.contains("1 J\n"), "the start cap is the one used: {s}");
    }
}
