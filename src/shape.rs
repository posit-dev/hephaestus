//! Named shape glyphs for scatterplot markers and line endpoint terminators.
//!
//! A [`Shape`] is one or more subpaths in normalized coordinates plus a
//! [`ShapeStyle`] hint (stroke vs fill) and an [`anchor`](Shape::anchor) point.
//! Built-in shapes are exposed as free functions in [`builtin`]; a
//! [`ShapeRegistry`] is a name-keyed in-memory map for runtime lookup and
//! user-registered custom shapes.
//!
//! Drawing is the caller's responsibility — this module only produces path
//! data. The caller composes the placement transform and issues
//! [`SceneBuilder::fill`](crate::scene::SceneBuilder::fill) /
//! [`SceneBuilder::stroke`](crate::scene::SceneBuilder::stroke) calls itself.
//!
//! # Two placement modes
//!
//! The same `Shape` supports two use patterns. The caller picks based on intent.
//!
//! **(A) Centered on a placement point** — e.g. a scatterplot marker on a data
//! point, or a filled terminator that should sit on a line endpoint and occlude
//! the line cap. The anchor is **ignored**:
//!
//! ```ignore
//! let xform = Affine::translate(center.to_vec2()) * Affine::scale(size);
//! match shape.kind() {
//!     ShapeKind::Paths { paths, .. } => for sub in paths {
//!         sb.fill(rule, xform, &brush, None, sub, pick);
//!     },
//!     ShapeKind::Glyph { .. } => { /* emit a GlyphRun — see PointGeom */ }
//! }
//! ```
//!
//! **(B) Attached to a line endpoint** — e.g. an open arrowhead, or any shape
//! used as a stroke-only outline terminator where the line shouldn't pass
//! through the interior. The anchor lands on the placement point:
//!
//! ```ignore
//! let angle        = direction.angle();
//! let rot          = Affine::rotate(angle);
//! let anchor_world = rot * (shape.anchor().to_vec2() * size);
//! let origin       = placement - anchor_world;
//! let xform        = Affine::translate(origin.to_vec2()) * rot * Affine::scale(size);
//! match shape.kind() { /* same dispatch as (A) */ }
//! ```
//!
//! Built-in anchors are chosen for mode (B): point shapes get a back-edge
//! anchor (e.g. `(-0.8, 0)` for `circle`) so a stroke-only outline terminator
//! joins the line cleanly. In mode (A) that anchor is simply not consulted.

use std::collections::HashMap;

use crate::geometry::Point;
use crate::path::Path;
use crate::scene::Font;

/// How a path-backed [`Shape`] is meant to be rendered.
///
/// Glyph-backed shapes (constructed via [`Shape::glyph`]) don't carry a
/// `ShapeStyle` — they're always filled with the resolved fill colour;
/// stroke channels have no effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShapeStyle {
    /// Open curves — only meaningful with a stroke. Subpaths are 2-point line
    /// segments (or polylines) with no `close_path`. Examples: `plus`, `cross`,
    /// `arrow-open`, `arrow-bar`.
    Stroke,
    /// Closed polygons — meaningful as a fill. Subpaths end with `close_path`.
    /// May also be stroked for an outline. Examples: `circle`, `square`,
    /// `arrow-closed`, `arrow-dot`.
    Fill,
}

/// A scale- and orientation-free glyph expressed either as one or more
/// subpaths or as a single font-glyph.
///
/// See the [module documentation](self) for the two placement modes and the
/// anchor convention. Path and glyph variants are exposed via
/// [`Shape::kind`] returning a [`ShapeKind`].
#[derive(Debug, Clone)]
pub struct Shape {
    content: ShapeContent,
    anchor: Point,
}

#[derive(Debug, Clone)]
enum ShapeContent {
    Paths {
        paths: Vec<Path>,
        style: ShapeStyle,
    },
    Glyph {
        font: Font,
        glyph_id: u32,
        em_bbox: kurbo::Rect,
        em_origin: Point,
    },
}

/// Borrowed view of a [`Shape`]'s contents — returned by [`Shape::kind`].
///
/// `Paths` is the classic case: a list of vector subpaths plus a fill/stroke
/// hint. `Glyph` is a single positioned font glyph: caller emits a
/// [`crate::scene::GlyphRun`] using `font` / `glyph_id` and centres the
/// glyph at the placement point using `em_bbox` + `em_origin` (em-space;
/// multiply by the desired font-size in pixels at draw time). Marker
/// shapes are required to be a single glyph — multi-codepoint inputs
/// (e.g. flag emoji like 🇩🇰) are accepted at construction so long as the
/// resolved font ligates them to one composite glyph.
#[derive(Debug, Clone, Copy)]
pub enum ShapeKind<'a> {
    Paths {
        paths: &'a [Path],
        style: ShapeStyle,
    },
    Glyph {
        font: &'a Font,
        glyph_id: u32,
        em_bbox: kurbo::Rect,
        em_origin: Point,
    },
}

impl Shape {
    /// Construct a path-backed shape from its subpaths, style hint, and anchor.
    pub fn new(paths: Vec<Path>, style: ShapeStyle, anchor: Point) -> Self {
        Self {
            content: ShapeContent::Paths { paths, style },
            anchor,
        }
    }

    /// Construct a glyph-backed shape from a resolved single glyph.
    ///
    /// `glyph_id` is the glyph index in `font`. `em_bbox` is the visual
    /// bounding box at unit em size; the drawing code uses
    /// `em_bbox.height()` for linetype-marker sizing
    /// (`scale = linewidth_px / em_bbox.height()`) and `em_bbox.center()`
    /// to centre the marker at the placement point. `em_origin` is the
    /// glyph's parley origin in the same em-frame (typically near the
    /// bottom-left for Latin glyphs because parley records the baseline
    /// and advance origin); drawing logic applies
    /// `translate(em_origin - em_bbox.center())` to centre the visible
    /// extent on the placement point.
    pub fn glyph(
        font: Font,
        glyph_id: u32,
        em_bbox: kurbo::Rect,
        em_origin: Point,
        anchor: Point,
    ) -> Self {
        Self {
            content: ShapeContent::Glyph {
                font,
                glyph_id,
                em_bbox,
                em_origin,
            },
            anchor,
        }
    }

    /// Borrowed view of the shape's contents — match this in draw code.
    pub fn kind(&self) -> ShapeKind<'_> {
        match &self.content {
            ShapeContent::Paths { paths, style } => ShapeKind::Paths {
                paths,
                style: *style,
            },
            ShapeContent::Glyph {
                font,
                glyph_id,
                em_bbox,
                em_origin,
            } => ShapeKind::Glyph {
                font,
                glyph_id: *glyph_id,
                em_bbox: *em_bbox,
                em_origin: *em_origin,
            },
        }
    }

    /// Point in the shape's local frame that aligns with the placement point
    /// in mode (B). See the [module documentation](self) for placement math.
    /// In mode (A) the anchor is not consulted.
    pub fn anchor(&self) -> Point {
        self.anchor
    }

    /// Bounding box of the shape in its local frame.
    ///
    /// For path-backed shapes this is the union of every subpath's bounding
    /// box; for glyph-backed shapes it's the stored `em_bbox`. Empty path
    /// shapes return `Rect::ZERO`.
    ///
    /// Used by callers that need to size the shape against a known extent
    /// (e.g. linetype markers scaling so the local y-extent matches the
    /// line's linewidth).
    pub fn bounding_box(&self) -> kurbo::Rect {
        match &self.content {
            ShapeContent::Paths { paths, .. } => {
                use kurbo::Shape as _;
                let mut iter = paths.iter().map(|p| p.bounding_box());
                match iter.next() {
                    None => kurbo::Rect::ZERO,
                    Some(first) => iter.fold(first, |acc, r| acc.union(r)),
                }
            }
            ShapeContent::Glyph { em_bbox, .. } => *em_bbox,
        }
    }
}

/// In-memory map from name to [`Shape`].
///
/// Typical usage: build once at setup with [`Self::with_builtins`], optionally
/// register user shapes via [`Self::insert`], then pull out `&Shape` references
/// for the draw path. The registry itself is not threaded through every call —
/// only the looked-up `&Shape` references are.
#[derive(Debug, Default, Clone)]
pub struct ShapeRegistry {
    shapes: HashMap<String, Shape>,
}

impl ShapeRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a registry pre-populated with every built-in shape (see
    /// [`builtin::NAMES`]).
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        for &name in builtin::NAMES {
            let s = builtin::lookup(name).expect("known built-in");
            r.shapes.insert(name.to_string(), s);
        }
        r
    }

    /// Insert a shape under the given name. Returns the previous shape if one
    /// existed.
    pub fn insert(&mut self, name: impl Into<String>, shape: Shape) -> Option<Shape> {
        self.shapes.insert(name.into(), shape)
    }

    /// Look up a shape by name.
    pub fn get(&self, name: &str) -> Option<&Shape> {
        self.shapes.get(name)
    }

    /// Whether a shape with the given name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.shapes.contains_key(name)
    }

    /// Remove and return the shape with the given name, if any.
    pub fn remove(&mut self, name: &str) -> Option<Shape> {
        self.shapes.remove(name)
    }

    /// Iterate over the registered shape names. Order is unspecified.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.shapes.keys().map(|s| s.as_str())
    }

    /// Number of registered shapes.
    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    /// Whether the registry has no entries.
    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }
}

/// Built-in shape constructors and the canonical list of names.
///
/// Each constructor returns a fresh [`Shape`]. To get all of them at once,
/// use [`ShapeRegistry::with_builtins`].
pub mod builtin {
    use super::{Path, Point, Shape, ShapeStyle};
    use std::f64::consts::{FRAC_PI_2, PI, SQRT_2};

    /// Outer radius of the builtin `circle` in shape-local coordinates.
    /// Every other closed builtin is area-matched to a circle of this
    /// radius. Chrome that mirrors the marker (e.g. the legend's Point
    /// key fallback) references this constant so legend and panel
    /// markers can't drift.
    pub const REFERENCE_RADIUS: f64 = 0.8;

    fn polygon(points: &[(f64, f64)]) -> Path {
        let mut p = Path::new();
        let (x0, y0) = points[0];
        p.move_to((x0, y0));
        for &(x, y) in &points[1..] {
            p.line_to((x, y));
        }
        p.close_path();
        p
    }

    fn polyline(points: &[(f64, f64)]) -> Path {
        let mut p = Path::new();
        let (x0, y0) = points[0];
        p.move_to((x0, y0));
        for &(x, y) in &points[1..] {
            p.line_to((x, y));
        }
        p
    }

    fn segment(a: (f64, f64), b: (f64, f64)) -> Path {
        let mut p = Path::new();
        p.move_to(a);
        p.line_to(b);
        p
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
        use kurbo::Shape as _;
        let path = kurbo::Circle::new(Point::ORIGIN, REFERENCE_RADIUS).to_path(0.01);
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
        use kurbo::Shape as _;
        fill_one(
            kurbo::Circle::new(Point::ORIGIN, 0.3).to_path(0.01),
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
    /// `None` if `name` doesn't match any entry in [`NAMES`].
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::PathEl;

    #[test]
    fn with_builtins_has_all_names() {
        let r = ShapeRegistry::with_builtins();
        assert_eq!(r.len(), builtin::NAMES.len());
        for name in builtin::NAMES {
            assert!(r.get(name).is_some(), "missing {name}");
        }
    }

    #[test]
    fn fill_shapes_are_closed() {
        let r = ShapeRegistry::with_builtins();
        let fill_names = [
            "circle",
            "square",
            "diamond",
            "triangle-up",
            "triangle-down",
            "star",
            "bowtie",
            "square-cross",
            "circle-plus",
            "square-plus",
            "arrow-closed",
            "arrow-stealth",
            "arrow-latex",
            "arrow-thin",
            "arrow-wedge",
            "arrow-dot",
            "arrow-square",
            "arrow-diamond",
        ];
        for name in fill_names {
            let s = r.get(name).expect(name);
            let ShapeKind::Paths { paths, style } = s.kind() else {
                panic!("{name}: expected Paths variant");
            };
            assert_eq!(style, ShapeStyle::Fill, "{name}");
            for sub in paths {
                let last = sub.elements().last().expect("non-empty path");
                assert!(
                    matches!(last, PathEl::ClosePath),
                    "{name} subpath not closed",
                );
            }
        }
    }

    #[test]
    fn stroke_shapes_are_open() {
        let r = ShapeRegistry::with_builtins();
        let stroke_names = [
            "cross",
            "plus",
            "asterisk",
            "hline",
            "vline",
            "arrow-open",
            "arrow-fishtail",
            "arrow-fork",
            "arrow-feather",
            "arrow-bar",
            "arrow-bracket",
            "arrow-cross",
        ];
        for name in stroke_names {
            let s = r.get(name).expect(name);
            let ShapeKind::Paths { paths, style } = s.kind() else {
                panic!("{name}: expected Paths variant");
            };
            assert_eq!(style, ShapeStyle::Stroke, "{name}");
            for sub in paths {
                let last = sub.elements().last().expect("non-empty path");
                assert!(
                    !matches!(last, PathEl::ClosePath),
                    "{name} subpath unexpectedly closed",
                );
            }
        }
    }

    #[test]
    fn anchor_conventions() {
        let r = ShapeRegistry::with_builtins();
        let eps = 1e-9;
        assert!((r.get("circle").unwrap().anchor().x - (-0.8)).abs() < eps);
        assert!((r.get("square").unwrap().anchor().x - (-0.71)).abs() < eps);
        assert!((r.get("diamond").unwrap().anchor().x - (-0.89)).abs() < eps);
        assert_eq!(r.get("vline").unwrap().anchor(), Point::ORIGIN);
        assert!((r.get("arrow-closed").unwrap().anchor().x - (-1.0)).abs() < eps);
        assert!((r.get("arrow-stealth").unwrap().anchor().x - (-0.4)).abs() < eps);
        let origin_names = [
            "arrow-open",
            "arrow-fishtail",
            "arrow-fork",
            "arrow-feather",
            "arrow-bar",
            "arrow-bracket",
            "arrow-cross",
            "arrow-dot",
            "arrow-square",
            "arrow-diamond",
        ];
        for name in origin_names {
            assert_eq!(r.get(name).unwrap().anchor(), Point::ORIGIN, "{name}");
        }
    }

    #[test]
    fn insert_remove_roundtrip() {
        let mut r = ShapeRegistry::new();
        assert!(r.is_empty());
        assert!(r.insert("custom", builtin::circle()).is_none());
        assert!(r.contains("custom"));
        assert_eq!(r.len(), 1);
        let prev = r.insert("custom", builtin::square()).expect("prev shape");
        let ShapeKind::Paths { style, .. } = prev.kind() else {
            panic!("expected Paths variant");
        };
        assert_eq!(style, ShapeStyle::Fill);
        assert_eq!(r.len(), 1);
        assert!(r.remove("custom").is_some());
        assert!(r.is_empty());
        assert!(!r.contains("custom"));
    }

    #[test]
    fn paths_start_with_moveto() {
        let r = ShapeRegistry::with_builtins();
        for name in builtin::NAMES {
            let s = r.get(name).expect(name);
            let ShapeKind::Paths { paths, .. } = s.kind() else {
                panic!("{name}: expected Paths variant");
            };
            for sub in paths {
                let first = sub.elements().first().expect("non-empty path");
                assert!(matches!(first, PathEl::MoveTo(_)), "{name} missing MoveTo",);
            }
        }
    }

    #[test]
    fn glyph_shape_roundtrips_via_kind() {
        // Construct a glyph shape with a synthetic font blob; the only thing
        // we exercise here is the wrapping/unwrapping. Drawing semantics are
        // tested in PointGeom / resolve.rs tests.
        let blob = peniko::Blob::new(std::sync::Arc::new(Vec::<u8>::new()));
        let font = Font::new(blob, 0);
        let em_bbox = kurbo::Rect::new(0.0, 0.0, 0.6, 1.0);
        let em_origin = Point::new(0.05, 0.8);
        let anchor = Point::new(-0.5, 0.0);
        let s = Shape::glyph(font, 42, em_bbox, em_origin, anchor);

        assert_eq!(s.anchor(), anchor);
        assert_eq!(s.bounding_box(), em_bbox);
        match s.kind() {
            ShapeKind::Glyph {
                glyph_id,
                em_bbox: b,
                em_origin: o,
                ..
            } => {
                assert_eq!(glyph_id, 42);
                assert_eq!(b, em_bbox);
                assert_eq!(o, em_origin);
            }
            _ => panic!("expected Glyph variant"),
        }
    }

    #[test]
    fn circle_bounding_box_has_expected_extent() {
        // builtin circle is `kurbo::Circle::new(Point::ORIGIN, 0.8)` —
        // bbox should be approximately (-0.8, -0.8) -> (0.8, 0.8),
        // i.e. width = height = 1.6.
        let r = ShapeRegistry::with_builtins();
        let circle = r.get("circle").expect("circle");
        let bbox = circle.bounding_box();
        assert!((bbox.width() - 1.6).abs() < 0.05);
        assert!((bbox.height() - 1.6).abs() < 0.05);
    }

    #[test]
    fn square_bounding_box_has_expected_extent() {
        // builtin square is half-side 0.71 → bbox 1.42 × 1.42.
        let r = ShapeRegistry::with_builtins();
        let square = r.get("square").expect("square");
        let bbox = square.bounding_box();
        assert!((bbox.width() - 1.42).abs() < 1e-9);
        assert!((bbox.height() - 1.42).abs() < 1e-9);
    }
}
