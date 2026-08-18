#![cfg_attr(docsrs, feature(doc_cfg))]
//! `hephaestus` — backend-agnostic 2D scene renderer for data visualization.
//!
//! The public API is the [`scene::SceneBuilder`] trait (what plot
//! code calls) plus the [`backend::Renderer`] trait (what produces
//! pixels). Backends slot in behind cargo features; the initial backend is
//! Vello (GPU compute via wgpu).
//!
//! The intersection of Vello and Blend2D capabilities defines the public
//! surface: no conic Beziers, no stroke alignment, no exotic blend modes, no
//! filter effects. Backend-specific extensions are not exposed.

pub mod backend;
pub mod blend;
pub mod brush;
pub mod color;
pub mod composition;
pub mod geometry;
#[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
pub mod image;
pub mod layout;
pub mod linetype;
pub mod mesh;
pub mod path;
pub mod pick;
pub mod plot;
pub mod primitives;
pub mod scales;
pub mod scene;
pub mod shape;
pub mod stroke;
pub mod style_vocab;

#[cfg(feature = "png")]
pub mod png;

pub mod text;

// Curated re-exports: the types a caller touches writing a
// hello-world against either API level. Anything more specialised is
// reached through its own module path.
pub use blend::{BlendMode, Compose, Mix};
pub use brush::{Brush, Sampling};
pub use color::{lerp_color, rgb, rgb8, rgba, Color, ColorSpace};
pub use geometry::{Affine, Point, Rect, Size, Vec2};
pub use layout::{
    Axis, Cell, CellId, Extent, Grid, Inset, Layout, Measure, Placement, Respect, Track, WidthHint,
};
pub use linetype::LinetypeStep;
pub use mesh::Mesh;
pub use path::{FillRule, Path};
pub use pick::PickId;
pub use primitives::{
    annular_wedge, arc, circle, clip_polyline, ellipse, offset_polygon, path_to_rings, polygon,
    polyline, rect, regular_polygon, regular_polygon_vertices, round_corners, round_path_corners,
    rounded_rect, segment, wedge, ArcLengthWalker, ArcSample, CornerRounding, EndClip,
    PolygonOptions, PolylineOptions, PolylineSampler, RibbonOptions, TrailingPolicy,
};
pub use scene::{Font, Glyph, GlyphRun, SceneBuilder};
pub use shape::{Shape, ShapeRegistry, ShapeStyle};
pub use stroke::Stroke;
pub use style_vocab::{HAlign, Length, Margin, Palette, ThemeColor, VAlign};

pub use backend::{BackendError, Renderer};

#[cfg(feature = "vello")]
pub use backend::WgpuRenderer;

/// Re-export of the `wgpu` crate version `hephaestus` is built against, so
/// callers integrating the GPU rendering path (see [`WgpuRenderer`]) can pin
/// to the exact types the backend expects.
#[cfg(feature = "vello")]
pub use wgpu;
