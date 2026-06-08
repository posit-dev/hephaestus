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
