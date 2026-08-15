//! [`Length`] — a numeric measurement that's either an absolute pt
//! value or a relative multiplier against a parent's resolved length —
//! and the four-sided [`Margin`] container over it.
//!
//! Both live in [`crate::style_vocab`] so the text layer can resolve
//! relative sizes without depending on the plot layer; this module
//! re-exports them under their theme-facing path.

pub use crate::style_vocab::{pt, rel, Length, Margin};
