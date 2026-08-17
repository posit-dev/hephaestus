//! Hephaestus-side rendering of axis and legend chrome.
//!
//! Scales are pure value mappers that live in [`crate::scales`]; this
//! module provides their visual rendering against
//! [`SceneBuilder`](crate::scene::SceneBuilder).

pub mod axis;
pub mod legend;
pub(crate) mod linear_axis;
pub mod panel;
pub mod polar;
pub mod strip;
pub(crate) mod text;

/// The font size a chrome slot's `Length::Rel` text size resolves
/// against — `theme.text.size_pt`, or the crate default when the theme
/// leaves it unset.
///
/// Every text-bearing chrome slot has to resolve its relative size
/// against the same parent, otherwise raising `theme.text.size_pt`
/// scales some slots and pins others.
pub(crate) fn root_text_pt(theme: &crate::plot::theme::Theme) -> f64 {
    use crate::plot::theme::DEFAULT_TEXT_SIZE_PT;
    theme
        .text
        .size_pt
        .map(|l| l.resolve(DEFAULT_TEXT_SIZE_PT))
        .unwrap_or(DEFAULT_TEXT_SIZE_PT)
}
