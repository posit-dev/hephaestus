//! Stroke parameters. Re-exports `kurbo::{Cap, Join, Stroke}`.
//!
//! `kurbo::Stroke` already matches the intersection of backends for the fields
//! we want: width, caps, joins, miter limit, dash pattern, dash offset.
//! Stroke alignment (inside/outside) and variable-width strokes are not
//! supported by Vello and are intentionally not exposed.

pub use kurbo::{Cap, Join, Stroke};
