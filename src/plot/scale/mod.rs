//! Scale primitives — value/range types, transforms, tick algorithms.
//!
//! Phase 1 ships the leaf primitives ([`OutputRange`], [`InputRange`],
//! [`Transform`] + [`TransformKind`], [`extended_breaks`]). The concrete
//! [`Scale`] struct and the [`ScaleTypeTrait`] family arrive in Phase 2.

pub mod breaks;
pub mod input;
pub mod output;
pub mod transform;

pub use breaks::extended_breaks;
pub use input::InputRange;
pub use output::OutputRange;
pub use transform::{Transform, TransformKind, TransformTrait};
