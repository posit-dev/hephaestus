//! The built-in marker and terminator shapes.
//!
//! Geometry and metadata are defined once in [`zygos::patterns::shape`]
//! and converted to this crate's [`Shape`] on lookup — the shape
//! vocabulary is shared with other zygos consumers, so there is one
//! definition to fix. What stays here is what zygos doesn't have: the
//! [`Shape`] type itself (glyph-backed shapes, [`Shape::with_anchor`],
//! the cached bounding box) and
//! [`ShapeRegistry`](crate::shape::ShapeRegistry).
//!
//! Each constructor returns a fresh [`Shape`] in the unit reference
//! frame every geom scales from; [`NAMES`] is the canonical list, and
//! [`ShapeRegistry::with_builtins`](crate::shape::ShapeRegistry::with_builtins)
//! registers all of them at once.

use crate::shape::{Shape, ShapeStyle};
use zygos::patterns::shape as shared;

/// Outer radius of the builtin `circle` in shape-local coordinates.
/// Every other closed builtin is area-matched to a circle of this
/// radius. Chrome that mirrors the marker (e.g. the legend's Point
/// key fallback) references this constant so legend and panel
/// markers can't drift.
pub const REFERENCE_RADIUS: f64 = shared::REFERENCE_RADIUS;

/// Canonical names of every built-in shape, in registration order.
pub const NAMES: &[&str] = shared::NAMES;

/// Convert the shared shape definition to this crate's [`Shape`].
fn convert(s: shared::Shape) -> Shape {
    let style = match s.style() {
        shared::ShapeStyle::Fill => ShapeStyle::Fill,
        shared::ShapeStyle::Stroke => ShapeStyle::Stroke,
    };
    Shape::new(s.paths().to_vec(), style, s.anchor())
}

/// Construct the built-in shape registered under `name`. Returns
/// `None` if `name` doesn't match any entry in [`NAMES`]. Matching is
/// exact — the shared lookup is case-insensitive, which the registry
/// contract doesn't want.
pub(super) fn lookup(name: &str) -> Option<Shape> {
    if !NAMES.contains(&name) {
        return None;
    }
    shared::builtin_shape(name).map(convert)
}

macro_rules! builtin {
    ($(#[$m:meta])* $f:ident, $name:literal) => {
        $(#[$m])*
        pub fn $f() -> Shape {
            convert(shared::builtin_shape($name).expect("built-in shape"))
        }
    };
}

builtin!(
    /// Bezier circle at [`REFERENCE_RADIUS`]. Area ≈ 2.01 — reference for
    /// area-equalization of the other closed point shapes.
    circle, "circle"
);
builtin!(
    /// Square, half-side 0.71. Area-matched to [`circle`].
    square, "square"
);
builtin!(
    /// Diamond (square rotated 45°), half-diagonal 0.89. Area-matched.
    diamond, "diamond"
);
builtin!(
    /// Triangle pointing up, circumradius 0.92.
    triangle_up, "triangle-up"
);
builtin!(
    /// Triangle pointing down, circumradius 0.92.
    triangle_down, "triangle-down"
);
builtin!(
    /// 5-point star, outer radius 0.95, inner 0.38.
    star, "star"
);
builtin!(
    /// X — two diagonal strokes through the origin.
    cross, "cross"
);
builtin!(
    /// + — two axis-aligned strokes through the origin.
    plus, "plus"
);
builtin!(
    /// Asterisk — three line segments at 60° increments.
    asterisk, "asterisk"
);
builtin!(
    /// Two triangles meeting at the origin (left and right).
    bowtie, "bowtie"
);
builtin!(
    /// Horizontal line segment from `(-0.8, 0)` to `(0.8, 0)`.
    hline, "hline"
);
builtin!(
    /// Vertical line segment from `(0, -0.8)` to `(0, 0.8)`.
    vline, "vline"
);
builtin!(
    /// Square divided into 4 triangles pointing inward (composite — 4 subpaths).
    square_cross, "square-cross"
);
builtin!(
    /// Circle divided into 4 quarter pieces by a `+`-shaped gap (composite — 4 subpaths).
    circle_plus, "circle-plus"
);
builtin!(
    /// Square divided into 4 corner squares by a `+`-shaped gap (composite — 4 subpaths).
    square_plus, "square-plus"
);
builtin!(
    /// Open V: two strokes meeting at the tip. Anchor at the tip.
    arrow_open, "arrow-open"
);
builtin!(
    /// Filled isoceles triangle. Anchor at the back of the body.
    arrow_closed, "arrow-closed"
);
builtin!(
    /// TikZ-style stealth: concave-back filled triangle. Anchor at notch apex.
    arrow_stealth, "arrow-stealth"
);
builtin!(
    /// LaTeX `\to`-style: slightly concave-back filled triangle.
    arrow_latex, "arrow-latex"
);
builtin!(
    /// Narrow filled triangle (~5:1 aspect).
    arrow_thin, "arrow-thin"
);
builtin!(
    /// Asymmetric barb / half-arrow (top half only).
    arrow_wedge, "arrow-wedge"
);
builtin!(
    /// Two strokes opening outward — classic fletching/tail look.
    arrow_fishtail, "arrow-fishtail"
);
builtin!(
    /// Wider-angle Y.
    arrow_fork, "arrow-fork"
);
builtin!(
    /// Stylised fletching — three chevrons along the shaft (6 subpaths).
    arrow_feather, "arrow-feather"
);
builtin!(
    /// Perpendicular bar.
    arrow_bar, "arrow-bar"
);
builtin!(
    /// Bar with two right-angle returns (`[`-shape).
    arrow_bracket, "arrow-bracket"
);
builtin!(
    /// Perpendicular X.
    arrow_cross, "arrow-cross"
);
builtin!(
    /// Small filled circle terminator.
    arrow_dot, "arrow-dot"
);
builtin!(
    /// Small filled square terminator.
    arrow_square, "arrow-square"
);
builtin!(
    /// Small filled diamond terminator.
    arrow_diamond, "arrow-diamond"
);
