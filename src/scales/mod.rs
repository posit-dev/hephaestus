//! Scale primitives — values, ranges, transforms, scale-type kinds, and
//! tick / break algorithms — as plain enums + free functions.
//!
//! This module is the **lift candidate**: nothing inside `src/scales/`
//! imports from `src/plot/*`, `src/scene/*`, `src/backend/*`,
//! `src/primitives/*`, or `src/text/*`. The contents are intended to be
//! lifted into their own `scales` crate when the API settles.
//!
//! ## Surface
//!
//! - [`Value`] / [`DataColumn`] / [`Date`] / [`DateTime`] / [`Time`] /
//!   [`Duration`] / [`LinetypeStep`] — the data scales operate on.
//! - [`InputRange`] / [`OutputRange`] — POD configuration types.
//! - [`Transform`] + [`TransformKind`] — value transforms (Identity in
//!   v1; Log / Sqrt / Asinh / PseudoLog land in Phase E.1).
//! - [`ScaleTypeKind`] — discriminator for the five scale families.
//! - [`AxisSide`] / [`LegendSide`] — placement enums.
//! - Free-function algorithms:
//!   - Per scale type: [`continuous_map`], [`discrete_map`],
//!     [`ordinal_map`], [`binned_map`], [`identity_map`].
//!   - Per scale type: [`continuous_breaks`], [`discrete_breaks`],
//!     [`binned_breaks`].
//!   - Band-width queries: [`discrete_band_width`],
//!     [`binned_band_width`], [`binned_band_width_at`].
//!   - Transform dispatch: [`transform_forward`], [`transform_inverse`],
//!     [`transform_allowed_domain`].
//!   - Tick selection: [`extended_breaks`], [`linear_breaks`].
//!
//! ## What's not here
//!
//! - No `Scale` aggregate struct or `ScaleRegistry`. Consumers bundle
//!   `(scale_type, transform, input_range, output_range)` as they see
//!   fit. Hephaestus's bundle lives at [`crate::plot::scale::Scale`].
//! - No rendering. Axes and legends are drawn by hephaestus's
//!   [`crate::plot::chrome`] against `SceneBuilder` + `TextRun`; future
//!   `scales`-crate consumers (e.g. ggsql) supply their own.

pub mod breaks;
pub mod chrome;
pub mod input;
pub mod output;
pub mod scale_type;
pub mod transform;
pub mod value;

pub use breaks::{extended_breaks, linear_breaks, DEFAULT_BREAK_COUNT};
pub use chrome::{AxisSide, LegendSide};
pub use input::InputRange;
pub use output::OutputRange;
pub use scale_type::{
    binned_band_width, binned_band_width_at, binned_breaks, binned_map, continuous_breaks,
    continuous_map, discrete_band_width, discrete_breaks, discrete_map, identity_map, ordinal_map,
    ScaleTypeKind,
};
pub use transform::{
    transform_allowed_domain, transform_forward, transform_inverse, Transform, TransformKind,
};
pub use value::{DataColumn, Date, DateTime, Duration, LinetypeStep, Time, Value};
