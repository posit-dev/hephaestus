//! Geometry primitives. Re-exports from `kurbo` through our own module path so
//! downstream code never references `kurbo::` directly.

pub use kurbo::{Affine, Point, Rect, Size, Vec2};
