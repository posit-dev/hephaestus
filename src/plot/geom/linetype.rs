//! Linetype constructors — re-exported from [`crate::linetype`], where
//! the pattern vocabulary and its arc-length renderer live so the text
//! layer can express block borders as linetypes without depending on
//! the plot layer.
//!
//! ```ignore
//! use hephaestus::plot::geom::linetype::{self, dash, gap, marker, pattern};
//!
//! linetype::solid();    // []
//! linetype::dashed();   // [Dash(8), Gap(4)]
//! linetype::dotted();   // [Dash(2), Gap(3)]
//! linetype::dashdot();  // [Dash(8), Gap(3), Dash(2), Gap(3)]
//!
//! // Mixed marker + dash pattern: 5pt dash, 3pt gap, circle marker,
//! // 5pt gap, repeat.
//! pattern([dash(5.0), gap(3.0), marker("circle"), gap(5.0)]);
//! ```

pub use crate::linetype::{
    check_pattern, dash, dashdot, dashed, dotted, gap, is_marker_free, marker, pattern, solid,
    strip_markers, to_kurbo_dashes, try_pattern, validate_pattern, LinetypeStep, PatternError,
};
