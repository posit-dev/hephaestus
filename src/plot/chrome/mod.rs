//! Hephaestus-side rendering of axis and legend chrome.
//!
//! Scales are pure value mappers that live in [`crate::scales`]; this
//! module provides their visual rendering against
//! [`SceneBuilder`](crate::scene::SceneBuilder). Gated on `feature =
//! "text"` because the renderers consume `TextRun`.

#[cfg(feature = "text")]
pub mod axis;
#[cfg(feature = "text")]
pub mod legend;
#[cfg(feature = "text")]
pub(crate) mod linear_axis;
#[cfg(feature = "text")]
pub mod panel;
#[cfg(feature = "text")]
pub mod polar;
#[cfg(feature = "text")]
pub mod strip;
