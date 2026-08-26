//! `BezPath` to an SVG `d` attribute.

use super::writer::num;
use crate::path::Path;

/// Append `path`'s `d` data, returning false when there was nothing to
/// write.
///
/// The scene's path vocabulary is kurbo's — moveto, lineto, quadto,
/// curveto, closepath, and deliberately no conics — which is exactly
/// SVG's `M L Q C Z`, so the mapping is one-to-one with nothing to
/// approximate.
///
/// Relative commands and the `H` / `V` / `S` / `T` shorthands are not
/// emitted. They would save perhaps a sixth of the path bytes and cost a
/// class of subtle bugs; revisit against a file that is measurably too
/// big.
pub(crate) fn write_d(out: &mut String, path: &Path, decimals: u8) -> bool {
    use crate::path::PathEl::*;
    let mut wrote = false;
    for el in path.elements() {
        match el {
            MoveTo(p) => {
                out.push('M');
                num(out, p.x, decimals);
                out.push(' ');
                num(out, p.y, decimals);
                wrote = true;
            }
            LineTo(p) => {
                out.push('L');
                num(out, p.x, decimals);
                out.push(' ');
                num(out, p.y, decimals);
            }
            QuadTo(a, b) => {
                out.push('Q');
                for (i, p) in [a, b].iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    num(out, p.x, decimals);
                    out.push(' ');
                    num(out, p.y, decimals);
                }
            }
            CurveTo(a, b, c) => {
                out.push('C');
                for (i, p) in [a, b, c].iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    num(out, p.x, decimals);
                    out.push(' ');
                    num(out, p.y, decimals);
                }
            }
            ClosePath => out.push('Z'),
        }
    }
    wrote
}

/// Serialize `path` on its own. Convenience over [`write_d`] for callers
/// building a `<defs>` entry rather than appending to the body.
pub(crate) fn to_d(path: &Path, decimals: u8) -> String {
    let mut s = String::new();
    write_d(&mut s, path, decimals);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Point, Rect, Shape};

    #[test]
    fn every_element_kind_maps_to_its_svg_command() {
        let mut p = Path::new();
        p.move_to(Point::new(0.0, 0.0));
        p.line_to(Point::new(10.0, 0.0));
        p.quad_to(Point::new(15.0, 5.0), Point::new(10.0, 10.0));
        p.curve_to(
            Point::new(8.0, 12.0),
            Point::new(2.0, 12.0),
            Point::new(0.0, 10.0),
        );
        p.close_path();
        assert_eq!(
            to_d(&p, 3),
            "M0 0L10 0Q15 5 10 10C8 12 2 12 0 10Z",
            "commands run together with no separator before a letter"
        );
    }

    #[test]
    fn an_empty_path_reports_that_it_wrote_nothing() {
        let mut s = String::new();
        assert!(!write_d(&mut s, &Path::new(), 3));
        assert!(s.is_empty());
    }

    #[test]
    fn a_rect_round_trips_through_the_shape_conversion() {
        let r = Rect::new(1.0, 2.0, 4.0, 6.0);
        let path: Path = Shape::to_path(&r, 0.1);
        let d = to_d(&path, 3);
        assert!(d.starts_with('M'), "d must begin with a moveto: {d:?}");
        assert!(d.ends_with('Z'));
    }
}
