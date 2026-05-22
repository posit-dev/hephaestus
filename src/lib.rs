//! `hephaestus` — backend-agnostic 2D scene renderer for data visualization.
//!
//! The public API is the [`SceneBuilder`](scene::SceneBuilder) trait (what plot
//! code calls) plus the [`Renderer`](backend::Renderer) trait (what produces
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
pub mod layout;
pub mod path;
pub mod pick;
pub mod plot;
pub mod primitives;
pub mod scene;
pub mod shape;
pub mod stroke;

#[cfg(feature = "png")]
pub mod png;

#[cfg(feature = "text")]
pub mod text;

// Curated re-exports of the most commonly used types.
pub use blend::{BlendMode, Compose, Mix};
pub use brush::{Brush, Sampling};
pub use color::Color;
pub use geometry::{Affine, Point, Rect, Size, Vec2};
pub use layout::{
    Cell, CellId, Grid, Inset, Layout, Length, Measure, Node, Placement, Track, WidthHint,
};
pub use path::{FillRule, Path};
pub use pick::PickId;
pub use primitives::{
    annular_wedge, arc, circle, clip_polyline, ellipse, offset_polygon, path_to_rings, polygon,
    polyline, rect, regular_polygon, regular_polygon_vertices, round_corners, round_path_corners,
    rounded_rect, segment, wedge, CornerRounding, EndClip, PolygonOptions, PolylineOptions,
};
pub use scene::SceneBuilder;
pub use shape::{Shape, ShapeRegistry, ShapeStyle};
pub use stroke::Stroke;

pub use backend::{BackendError, Renderer};
