//! Path representation. `Path` is `kurbo::BezPath`, which natively supports
//! moveto/lineto/quadto/curveto/closepath (no conic Beziers — intentional, to
//! match the Vello ∩ Blend2D intersection).

pub use kurbo::{BezPath as Path, PathEl};

/// Fill rule. Both Vello and Blend2D support both rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}
