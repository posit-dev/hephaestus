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
//! ```
//! use hephaestus::scene::recording::RecordingScene;
//! use hephaestus::shape::{ShapeKind, ShapeRegistry};
//! use hephaestus::{Affine, Brush, FillRule, PickId, Point, SceneBuilder};
//! use hephaestus::color::rgb8;
//!
//! let registry = ShapeRegistry::with_builtins();
//! let shape = registry.get("circle").expect("builtin circle");
//! let mut sb = RecordingScene::new();
//! let (center, size) = (Point::new(50.0, 50.0), 8.0);
//! let brush: Brush = rgb8(200, 60, 60).into();
//!
//! let xform = Affine::translate(center.to_vec2()) * Affine::scale(size);
//! match shape.kind() {
//!     ShapeKind::Paths { paths, .. } => for sub in paths {
//!         sb.fill(FillRule::NonZero, xform, &brush, None, sub, PickId::Skip);
//!     },
//!     ShapeKind::Glyph { .. } => { /* emit a GlyphRun — see PointGeom */ }
//! }
//! ```
//!
//! **(B) Attached to a line endpoint** — e.g. an open arrowhead, or any shape
//! used as a stroke-only outline terminator where the line shouldn't pass
//! through the interior. The anchor lands on the placement point:
//!
//! ```
//! use hephaestus::shape::ShapeRegistry;
//! use hephaestus::{Affine, Point, Vec2};
//!
//! let registry = ShapeRegistry::with_builtins();
//! let shape = registry.get("circle").expect("builtin circle");
//! let (placement, direction, size) = (Point::new(50.0, 50.0), Vec2::new(1.0, 0.0), 8.0);
//!
//! let angle        = direction.atan2();
//! let rot          = Affine::rotate(angle);
//! let anchor_world = rot * (shape.anchor().to_vec2() * size).to_point();
//! let origin       = placement - anchor_world.to_vec2();
//! let xform        = Affine::translate(origin.to_vec2()) * rot * Affine::scale(size);
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
/// Two shapes are equal when they would draw the same mark: the same
/// subpaths and style, or the same glyph of the same face.
///
/// `bbox` is derived from the rest, so it takes no part. Comparing two
/// glyph-backed shapes can read both font files — see the note on
/// [`Font`]'s own `PartialEq` — so this is not for a hot path. What it
/// is for is deciding whether a registry entry differs from the built-in
/// of the same name, which is what lets a plot document write only the
/// shapes a caller actually customised.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    content: ShapeContent,
    anchor: Point,
    /// Computed once at construction: the linetype walk asks for it
    /// once per stamped marker, and for a path-backed shape it means
    /// flattening every subpath.
    bbox: crate::geometry::Rect,
}

#[derive(Debug, Clone, PartialEq)]
enum ShapeContent {
    Paths {
        paths: Vec<Path>,
        style: ShapeStyle,
    },
    Glyph {
        font: Font,
        glyph_id: u32,
        em_bbox: crate::geometry::Rect,
        em_origin: Point,
        /// What this glyph was resolved from, when it came through
        /// [`glyph_marker`](crate::text::glyph_marker). Carried so a
        /// consumer can re-resolve the glyph against its own fonts
        /// rather than trusting a face-specific id — see
        /// [`Shape::glyph_source`].
        source: Option<GlyphSource>,
    },
}

/// The text and style a glyph-backed [`Shape`] was resolved from.
///
/// A glyph id means nothing outside the face it came from, and a family
/// name resolves to different faces on different machines. The source
/// text and the style that selected the face are what travel: re-running
/// [`glyph_marker`](crate::text::glyph_marker) on them reproduces the
/// shape wherever the same characters can be shaped, which is the same
/// contract every other piece of text in this crate keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphSource {
    /// The characters that were shaped — usually one, but a ligating
    /// sequence such as a country-flag emoji is also one glyph.
    pub text: String,
    /// The style the face was selected with.
    pub style: crate::text::TextStyle,
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
        em_bbox: crate::geometry::Rect,
        em_origin: Point,
    },
}

impl Shape {
    /// Construct a path-backed shape from its subpaths, style hint, and anchor.
    pub fn new(paths: Vec<Path>, style: ShapeStyle, anchor: Point) -> Self {
        let bbox = paths_bounding_box(&paths);
        Self {
            content: ShapeContent::Paths { paths, style },
            anchor,
            bbox,
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
        em_bbox: crate::geometry::Rect,
        em_origin: Point,
        anchor: Point,
    ) -> Self {
        Self {
            content: ShapeContent::Glyph {
                font,
                glyph_id,
                em_bbox,
                em_origin,
                source: None,
            },
            anchor,
            bbox: em_bbox,
        }
    }

    /// Construct a glyph-backed shape that remembers what it was
    /// resolved from, so a consumer can rebuild it against its own
    /// fonts. [`glyph_marker`](crate::text::glyph_marker) is the caller.
    pub(crate) fn glyph_with_source(
        font: Font,
        glyph_id: u32,
        em_bbox: crate::geometry::Rect,
        em_origin: Point,
        anchor: Point,
        source: GlyphSource,
    ) -> Self {
        Self {
            content: ShapeContent::Glyph {
                font,
                glyph_id,
                em_bbox,
                em_origin,
                source: Some(source),
            },
            anchor,
            bbox: em_bbox,
        }
    }

    /// Set the shape's anchor, the point that lands on a placement
    /// point in mode (B). See the [module documentation](self).
    pub fn with_anchor(mut self, anchor: Point) -> Self {
        self.anchor = anchor;
        self
    }

    /// What a glyph-backed shape was resolved from, when it is known.
    ///
    /// `None` for a path-backed shape, and for a glyph shape built
    /// through [`Self::glyph`] with an already-resolved id — that
    /// caller has a face in hand and no source text to offer.
    pub fn glyph_source(&self) -> Option<&GlyphSource> {
        match &self.content {
            ShapeContent::Glyph { source, .. } => source.as_ref(),
            ShapeContent::Paths { .. } => None,
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
                ..
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
    pub fn bounding_box(&self) -> crate::geometry::Rect {
        self.bbox
    }
}

/// Union of every subpath's bounding box; `Rect::ZERO` for no paths.
fn paths_bounding_box(paths: &[Path]) -> crate::geometry::Rect {
    use crate::geometry::Shape as _;
    let mut iter = paths.iter().map(|p| p.bounding_box());
    match iter.next() {
        None => crate::geometry::Rect::ZERO,
        Some(first) => iter.fold(first, |acc, r| acc.union(r)),
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

    /// Shared registry holding every built-in shape, built once per
    /// process. Draw paths that only ever look up built-ins — rich-text
    /// block borders, linetype marker stamps with no user shapes — read
    /// from this instead of constructing a fresh registry per call.
    pub fn shared_builtins() -> &'static ShapeRegistry {
        static SHARED: std::sync::OnceLock<ShapeRegistry> = std::sync::OnceLock::new();
        SHARED.get_or_init(ShapeRegistry::with_builtins)
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
#[path = "shape_builtin.rs"]
pub mod builtin;

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
        let blob = crate::brush::Blob::new(std::sync::Arc::new(Vec::<u8>::new()));
        let font = Font::new(blob, 0);
        let em_bbox = crate::geometry::Rect::new(0.0, 0.0, 0.6, 1.0);
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
        // builtin circle is `crate::geometry::Circle::new(Point::ORIGIN, 0.8)` —
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
