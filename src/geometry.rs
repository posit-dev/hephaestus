//! Geometry primitives. Re-exports from `kurbo` through our own module path so
//! downstream code never references `kurbo::` directly.
//!
//! The set is everything the crate actually consumes, so a future swap
//! stays a single-file change: the shape types the primitives module
//! builds paths from, the `Shape` trait their `to_path` / `bounding_box`
//! methods come from, and `flatten` for arc sampling.

pub use kurbo::{
    flatten, Affine, Arc, BezPath, Circle, CubicBez, Ellipse, Line, ParamCurve, ParamCurveArclen,
    ParamCurveDeriv, PathEl, PathSeg, Point, QuadBez, Rect, RoundedRect, Shape, Size, Vec2,
};
