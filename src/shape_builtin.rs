//! The built-in marker and terminator shapes.
//!
//! Each constructor returns a fresh [`Shape`] in the unit reference
//! frame every geom scales from; [`crate::shape::builtin::NAMES`] is the canonical list, and
//! [`ShapeRegistry::with_builtins`](crate::shape::ShapeRegistry::with_builtins)
//! registers all of them at once.

use crate::geometry::Point;
use crate::path::Path;
use crate::shape::{Shape, ShapeStyle};
use std::f64::consts::{FRAC_PI_2, PI, SQRT_2};

/// Outer radius of the builtin `circle` in shape-local coordinates.
/// Every other closed builtin is area-matched to a circle of this
/// radius. Chrome that mirrors the marker (e.g. the legend's Point
/// key fallback) references this constant so legend and panel
/// markers can't drift.
pub const REFERENCE_RADIUS: f64 = 0.8;

fn to_points(points: &[(f64, f64)]) -> Vec<Point> {
    points.iter().map(|&(x, y)| Point::new(x, y)).collect()
}

fn polygon(points: &[(f64, f64)]) -> Path {
    crate::primitives::polygon_path(&to_points(points))
}

fn polyline(points: &[(f64, f64)]) -> Path {
    crate::primitives::polyline_path(&to_points(points))
}

fn segment(a: (f64, f64), b: (f64, f64)) -> Path {
    crate::primitives::segment(a.into(), b.into())
}

fn fill_one(path: Path, anchor: Point) -> Shape {
    Shape::new(vec![path], ShapeStyle::Fill, anchor)
}

fn fill_many(paths: Vec<Path>, anchor: Point) -> Shape {
    Shape::new(paths, ShapeStyle::Fill, anchor)
}

fn stroke(paths: Vec<Path>, anchor: Point) -> Shape {
    Shape::new(paths, ShapeStyle::Stroke, anchor)
}

// -------- point shapes (ported from posit-dev/ggsql) --------

/// Bezier circle at [`REFERENCE_RADIUS`]. Area ≈ 2.01 — reference for
/// area-equalization of the other closed point shapes.
pub fn circle() -> Shape {
    use crate::geometry::Shape as _;
    let path = crate::geometry::Circle::new(Point::ORIGIN, REFERENCE_RADIUS).to_path(0.01);
    fill_one(path, Point::new(-REFERENCE_RADIUS, 0.0))
}

/// Square, half-side 0.71. Area-matched to [`circle`].
pub fn square() -> Shape {
    let s = 0.71;
    fill_one(
        polygon(&[(-s, -s), (s, -s), (s, s), (-s, s)]),
        Point::new(-s, 0.0),
    )
}

/// Diamond (square rotated 45°), half-diagonal 0.89. Area-matched.
pub fn diamond() -> Shape {
    let d = 0.89;
    fill_one(
        polygon(&[(0.0, -d), (d, 0.0), (0.0, d), (-d, 0.0)]),
        Point::new(-d, 0.0),
    )
}

/// Triangle pointing up, circumradius 0.92.
pub fn triangle_up() -> Shape {
    let r = 0.92;
    let h = r * 0.75;
    fill_one(polygon(&[(0.0, -r), (r, h), (-r, h)]), Point::new(-r, 0.0))
}

/// Triangle pointing down, circumradius 0.92.
pub fn triangle_down() -> Shape {
    let r = 0.92;
    let h = r * 0.75;
    fill_one(polygon(&[(-r, -h), (r, -h), (0.0, r)]), Point::new(-r, 0.0))
}

/// 5-point star, outer radius 0.95, inner 0.38.
pub fn star() -> Shape {
    let outer = 0.95;
    let inner = outer * 0.4;
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let angle = -FRAC_PI_2 + PI * (i as f64) / 5.0;
        let r = if i % 2 == 0 { outer } else { inner };
        pts.push((r * angle.cos(), r * angle.sin()));
    }
    fill_one(polygon(&pts), Point::new(-outer, 0.0))
}

/// X — two diagonal strokes through the origin.
pub fn cross() -> Shape {
    let c = 0.8 / SQRT_2;
    stroke(
        vec![segment((-c, -c), (c, c)), segment((-c, c), (c, -c))],
        Point::new(-c, 0.0),
    )
}

/// + — two axis-aligned strokes through the origin.
pub fn plus() -> Shape {
    stroke(
        vec![
            segment((-0.8, 0.0), (0.8, 0.0)),
            segment((0.0, -0.8), (0.0, 0.8)),
        ],
        Point::new(-0.8, 0.0),
    )
}

/// Asterisk — three line segments at 60° increments.
pub fn asterisk() -> Shape {
    let r: f64 = 0.8;
    let paths = (0..3)
        .map(|i| {
            let angle = (i as f64) * PI / 3.0;
            let (sin, cos) = angle.sin_cos();
            segment((-r * cos, -r * sin), (r * cos, r * sin))
        })
        .collect();
    stroke(paths, Point::new(-r, 0.0))
}

/// Two triangles meeting at the origin (left and right).
pub fn bowtie() -> Shape {
    fill_many(
        vec![
            polygon(&[(-0.8, -0.8), (0.0, 0.0), (-0.8, 0.8)]),
            polygon(&[(0.8, -0.8), (0.0, 0.0), (0.8, 0.8)]),
        ],
        Point::new(-0.8, 0.0),
    )
}

/// Horizontal line segment from `(-0.8, 0)` to `(0.8, 0)`.
pub fn hline() -> Shape {
    stroke(
        vec![segment((-0.8, 0.0), (0.8, 0.0))],
        Point::new(-0.8, 0.0),
    )
}

/// Vertical line segment from `(0, -0.8)` to `(0, 0.8)`.
pub fn vline() -> Shape {
    stroke(vec![segment((0.0, -0.8), (0.0, 0.8))], Point::ORIGIN)
}

/// Square divided into 4 triangles pointing inward (composite — 4 subpaths).
pub fn square_cross() -> Shape {
    let s = 0.71;
    let g = 0.12;
    fill_many(
        vec![
            polygon(&[(-s + g, -s), (s - g, -s), (0.0, -g)]),
            polygon(&[(s, -s + g), (s, s - g), (g, 0.0)]),
            polygon(&[(s - g, s), (-s + g, s), (0.0, g)]),
            polygon(&[(-s, s - g), (-s, -s + g), (-g, 0.0)]),
        ],
        Point::new(-s, 0.0),
    )
}

/// Circle divided into 4 quarter pieces by a `+`-shaped gap (composite — 4 subpaths).
pub fn circle_plus() -> Shape {
    let r: f64 = 0.8;
    let g: f64 = 0.12 / SQRT_2;
    let n = 8;
    let edge = (r * r - g * g).sqrt();
    let start_angle = (g / r).asin();
    let end_angle = FRAC_PI_2 - start_angle;
    let mut paths = Vec::with_capacity(4);
    for q in 0..4 {
        let base_angle = (q as f64) * FRAC_PI_2;
        let mut pts: Vec<(f64, f64)> = Vec::new();
        pts.push(match q {
            0 => (g, g),
            1 => (-g, g),
            2 => (-g, -g),
            _ => (g, -g),
        });
        pts.push(match q {
            0 => (edge, g),
            1 => (-g, edge),
            2 => (-edge, -g),
            _ => (g, -edge),
        });
        let arc_start = base_angle + start_angle;
        let arc_span = end_angle - start_angle;
        for i in 0..=n {
            let t = (i as f64) / (n as f64);
            let angle = arc_start + t * arc_span;
            pts.push((r * angle.cos(), r * angle.sin()));
        }
        pts.push(match q {
            0 => (g, edge),
            1 => (-edge, g),
            2 => (-g, -edge),
            _ => (edge, -g),
        });
        paths.push(polygon(&pts));
    }
    fill_many(paths, Point::new(-r, 0.0))
}

/// Square divided into 4 corner squares by a `+`-shaped gap (composite — 4 subpaths).
pub fn square_plus() -> Shape {
    let s = 0.71;
    let g = 0.12 / SQRT_2;
    fill_many(
        vec![
            polygon(&[(-s, -s), (-g, -s), (-g, -g), (-s, -g)]),
            polygon(&[(g, -s), (s, -s), (s, -g), (g, -g)]),
            polygon(&[(g, g), (s, g), (s, s), (g, s)]),
            polygon(&[(-s, g), (-g, g), (-g, s), (-s, s)]),
        ],
        Point::new(-s, 0.0),
    )
}

// -------- pointed arrowheads (tip at origin, body in -x) --------

/// Open V: two strokes meeting at the tip. Anchor at the tip.
pub fn arrow_open() -> Shape {
    stroke(
        vec![
            segment((-1.0, 0.5), (0.0, 0.0)),
            segment((0.0, 0.0), (-1.0, -0.5)),
        ],
        Point::ORIGIN,
    )
}

/// Filled isoceles triangle. Anchor at the back of the body.
pub fn arrow_closed() -> Shape {
    fill_one(
        polygon(&[(0.0, 0.0), (-1.0, 0.5), (-1.0, -0.5)]),
        Point::new(-1.0, 0.0),
    )
}

/// TikZ-style stealth: concave-back filled triangle. Anchor at notch apex.
pub fn arrow_stealth() -> Shape {
    fill_one(
        polygon(&[(0.0, 0.0), (-1.0, 0.5), (-0.4, 0.0), (-1.0, -0.5)]),
        Point::new(-0.4, 0.0),
    )
}

/// LaTeX `\to`-style: slightly concave-back filled triangle.
pub fn arrow_latex() -> Shape {
    fill_one(
        polygon(&[(0.0, 0.0), (-1.0, 0.35), (-0.6, 0.0), (-1.0, -0.35)]),
        Point::new(-0.6, 0.0),
    )
}

/// Narrow filled triangle (~5:1 aspect).
pub fn arrow_thin() -> Shape {
    fill_one(
        polygon(&[(0.0, 0.0), (-1.0, 0.2), (-1.0, -0.2)]),
        Point::new(-1.0, 0.0),
    )
}

/// Asymmetric barb / half-arrow (top half only).
pub fn arrow_wedge() -> Shape {
    fill_one(
        polygon(&[(0.0, 0.0), (-1.0, 0.5), (-1.0, 0.0)]),
        Point::new(-1.0, 0.0),
    )
}

// -------- tail-style (open, opens away from line) --------

/// Two strokes opening outward — classic fletching/tail look.
pub fn arrow_fishtail() -> Shape {
    stroke(
        vec![
            segment((0.0, 0.0), (-1.0, 0.5)),
            segment((0.0, 0.0), (-1.0, -0.5)),
        ],
        Point::ORIGIN,
    )
}

/// Wider-angle Y.
pub fn arrow_fork() -> Shape {
    stroke(
        vec![
            segment((0.0, 0.0), (-0.7, 0.7)),
            segment((0.0, 0.0), (-0.7, -0.7)),
        ],
        Point::ORIGIN,
    )
}

/// Stylised fletching — three chevrons along the shaft (6 subpaths).
pub fn arrow_feather() -> Shape {
    let arm = 0.4;
    let halfh = 0.5;
    let offsets = [0.0, -0.3, -0.6];
    let mut paths = Vec::with_capacity(6);
    for &ox in &offsets {
        paths.push(segment((ox, 0.0), (ox - arm, halfh)));
        paths.push(segment((ox, 0.0), (ox - arm, -halfh)));
    }
    stroke(paths, Point::ORIGIN)
}

// -------- symmetric terminators --------

/// Perpendicular bar.
pub fn arrow_bar() -> Shape {
    stroke(vec![segment((0.0, -0.5), (0.0, 0.5))], Point::ORIGIN)
}

/// Bar with two right-angle returns (`[`-shape).
pub fn arrow_bracket() -> Shape {
    stroke(
        vec![polyline(&[
            (0.2, -0.5),
            (0.0, -0.5),
            (0.0, 0.5),
            (0.2, 0.5),
        ])],
        Point::ORIGIN,
    )
}

/// Perpendicular X.
pub fn arrow_cross() -> Shape {
    stroke(
        vec![
            segment((-0.5, -0.5), (0.5, 0.5)),
            segment((-0.5, 0.5), (0.5, -0.5)),
        ],
        Point::ORIGIN,
    )
}

/// Small filled circle terminator.
pub fn arrow_dot() -> Shape {
    use crate::geometry::Shape as _;
    fill_one(
        crate::geometry::Circle::new(Point::ORIGIN, 0.3).to_path(0.01),
        Point::ORIGIN,
    )
}

/// Small filled square terminator.
pub fn arrow_square() -> Shape {
    fill_one(
        polygon(&[(-0.3, -0.3), (0.3, -0.3), (0.3, 0.3), (-0.3, 0.3)]),
        Point::ORIGIN,
    )
}

/// Small filled diamond terminator.
pub fn arrow_diamond() -> Shape {
    fill_one(
        polygon(&[(0.0, -0.4), (0.4, 0.0), (0.0, 0.4), (-0.4, 0.0)]),
        Point::ORIGIN,
    )
}

/// Canonical names of every built-in shape, in registration order.
pub const NAMES: &[&str] = &[
    // Point shapes
    "circle",
    "square",
    "diamond",
    "triangle-up",
    "triangle-down",
    "star",
    "cross",
    "plus",
    "asterisk",
    "bowtie",
    "hline",
    "vline",
    "square-cross",
    "circle-plus",
    "square-plus",
    // Pointed arrowheads
    "arrow-open",
    "arrow-closed",
    "arrow-stealth",
    "arrow-latex",
    "arrow-thin",
    "arrow-wedge",
    // Tail-style
    "arrow-fishtail",
    "arrow-fork",
    "arrow-feather",
    // Symmetric terminators
    "arrow-bar",
    "arrow-bracket",
    "arrow-cross",
    "arrow-dot",
    "arrow-square",
    "arrow-diamond",
];

/// Construct the built-in shape registered under `name`. Returns
/// `None` if `name` doesn't match any entry in [`crate::shape::builtin::NAMES`].
pub(super) fn lookup(name: &str) -> Option<Shape> {
    Some(match name {
        "circle" => circle(),
        "square" => square(),
        "diamond" => diamond(),
        "triangle-up" => triangle_up(),
        "triangle-down" => triangle_down(),
        "star" => star(),
        "cross" => cross(),
        "plus" => plus(),
        "asterisk" => asterisk(),
        "bowtie" => bowtie(),
        "hline" => hline(),
        "vline" => vline(),
        "square-cross" => square_cross(),
        "circle-plus" => circle_plus(),
        "square-plus" => square_plus(),
        "arrow-open" => arrow_open(),
        "arrow-closed" => arrow_closed(),
        "arrow-stealth" => arrow_stealth(),
        "arrow-latex" => arrow_latex(),
        "arrow-thin" => arrow_thin(),
        "arrow-wedge" => arrow_wedge(),
        "arrow-fishtail" => arrow_fishtail(),
        "arrow-fork" => arrow_fork(),
        "arrow-feather" => arrow_feather(),
        "arrow-bar" => arrow_bar(),
        "arrow-bracket" => arrow_bracket(),
        "arrow-cross" => arrow_cross(),
        "arrow-dot" => arrow_dot(),
        "arrow-square" => arrow_square(),
        "arrow-diamond" => arrow_diamond(),
        _ => return None,
    })
}
