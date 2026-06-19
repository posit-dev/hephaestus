//! High-level, plot-centric API layered on top of the low-level
//! [`SceneBuilder`](crate::scene::SceneBuilder) + [`composition`](crate::composition)
//! surfaces. The architecture is shaped so new geoms / scales /
//! projections drop in additively.
//!
//! The canonical user-facing surface is
//! [`PlotComposition`](composition::PlotComposition). It owns a
//! [`Composition`](crate::composition::Composition) template, a named
//! [`ScaleRegistry`], and a `HashMap<String, Plot>` of attached plots;
//! `view.render(scene, size, dpi)` is the single entry point for
//! rendering. Mutations flow through closures
//! (`view.update_scale("time", |s| …)`, `view.update_plot("price",
//! |p| …)`) so the orchestrator can keep dirty-tracking accurate.
//!
//! Two plots that bind the same scale name share a single mutation site —
//! `view.update_scale("time", |s| s.set_domain_continuous(0.0, 50.0))`
//! updates every plot whose `"x"` (or any other channel) is bound to
//! `"time"`.
//!
//! Power users that want to drive the solve/draw cycle by hand can use
//! the lower-level [`Plot`] primitives directly with an explicit
//! [`ScaleRegistry`]. See `Plot::wire` / `Plot::draw_chrome_into` /
//! `Plot::draw_panel_into`.

#[cfg(feature = "text")]
pub mod chrome;
pub mod composition;
pub mod diff;
pub mod geom;
#[allow(clippy::module_inception)]
pub mod plot;
pub mod projection;
pub mod scale;
pub mod theme;
pub mod value;

pub use composition::{PlotComposition, ValidationIssue};
pub use diff::{diff_columns, diff_positional, KeyIndex};
pub use geom::{
    linetype, BSplineGeom, BuildableGeom, Channel, ChannelDecl, EllipseGeom, ExpectedOutput, Geom,
    GeomBuilder, GeomContext, GeometryGeom, Keys, LineGeom, PointGeom, PolygonGeom, Raw, RectGeom,
    RibbonBSplineGeom, RibbonGeom, ScaleResolver, SegmentGeom, WedgeGeom,
};
#[cfg(feature = "text")]
pub use geom::{TextFitGeom, TextGeom, TextPathGeom};
pub use plot::{GeomId, Plot};
pub use projection::{ChromeStrategy, CustomProjection, Projection};
pub use scale::{
    AxisSide, InputRange, LegendSide, OutputRange, Scale, ScaleRegistry, ScaleTypeKind, Transform,
    TransformKind,
};
pub use value::{DataColumn, Date, DateTime, Duration, LinetypeStep, Time, Value};
