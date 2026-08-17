//! High-level plot composition layout.
//!
//! Stacks on top of [`crate::layout`] to provide a patchwork-style model
//! where every plot is the same 13×16 anatomical grid (see [`anatomy::Slot`])
//! and composed plots automatically align by anatomical position.
//!
//! Construction is id-addressed: every [`Patch`] is created with a string id,
//! and resolved rects are looked up via
//! [`CompositionLayout::get(id, region)`](CompositionLayout::get) — flat
//! across any nesting depth.

pub mod anatomy;
mod build;
#[allow(clippy::module_inception)]
mod composition;
mod layout_result;
mod patch;

#[cfg(test)]
mod tests;

pub use anatomy::{
    Slot, MARGIN_BOTTOM_ROW, MARGIN_LEFT_COL, MARGIN_RIGHT_COL, MARGIN_TOP_ROW, PADDING_BOTTOM_ROW,
    PADDING_LEFT_COL, PADDING_RIGHT_COL, PADDING_TOP_ROW, PANEL_COL, PANEL_ROW, PLOT_BOTTOM,
    PLOT_LEFT, PLOT_RIGHT, PLOT_TOP, TABLE_COLS, TABLE_ROWS,
};

pub use composition::{beside, grid, spacer, stack, wrap, Composition, CompositionError, Element};
pub use layout_result::{CompositionLayout, Region};
pub use patch::{Patch, PatchPlacement, Span};

pub(crate) use composition::CompositionPlacement;

pub(crate) const TABLE_COLS_U16: u16 = TABLE_COLS as u16;
pub(crate) const TABLE_ROWS_U16: u16 = TABLE_ROWS as u16;
