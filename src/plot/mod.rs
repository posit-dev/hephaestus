//! High-level, plot-centric API layered on top of the low-level
//! [`SceneBuilder`](crate::scene::SceneBuilder) + [`composition`](crate::composition)
//! surfaces. v1 ships **point geom only**; the surrounding architecture is
//! shaped so other geoms / scales / projections drop in additively.
//!
//! The canonical user-facing surface is
//! [`PlotComposition`](composition::PlotComposition) (added in a later
//! phase). It owns a `Composition` template, a named [`ScaleRegistry`], and
//! a `HashMap<String, Plot>` of attached plots; `view.render(...)` is the
//! single entry point for rendering.
//!
//! Phase 1 ships the value and scale primitives only. Subsequent phases
//! layer scales, diff, geoms, and the orchestrator on top.

pub mod diff;
pub mod geom;
pub mod scale;
pub mod value;

pub use diff::{diff_columns, diff_positional, KeyIndex};
pub use geom::{
    BuildableGeom, Channel, ChannelDecl, ExpectedOutput, Geom, GeomBuilder, GeomContext, Keys,
    PointGeom, ScaleResolver,
};
pub use value::{DataColumn, Date, DateTime, Duration, Time, Value};
