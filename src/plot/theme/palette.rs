//! Semantic colour palette + [`ThemeColor`] references.
//!
//! Both live in [`crate::style_vocab`] so the text layer can resolve
//! palette colours without depending on the plot layer; this module
//! re-exports them under their theme-facing path.

pub use crate::style_vocab::{Palette, ThemeColor};
