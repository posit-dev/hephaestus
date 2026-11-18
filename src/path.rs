//! Path representation. `Path` is `kurbo::BezPath`, which natively supports
//! moveto/lineto/quadto/curveto/closepath (no conic Beziers — intentional, to
//! match the Vello ∩ Blend2D intersection).

pub use kurbo::{BezPath as Path, PathEl};

use crate::geometry::Rect;

/// The bounding box of `path`'s control points — loose for curves, which
/// is what a conservative coverage test wants.
///
/// Free function rather than a method because `Path` is a re-exported
/// kurbo type; the trait that provides this upstream (`kurbo::Shape`)
/// stays inside the wrapper modules.
pub fn bounding_box(path: &Path) -> Rect {
    kurbo::Shape::bounding_box(path)
}

/// Fill rule. Both Vello and Blend2D support both rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}
