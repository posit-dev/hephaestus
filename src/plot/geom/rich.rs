//! Rich-text plumbing shared by the text geoms.
//!
//! A geom's `"text_stroke"` / `"text_linewidth"` channels outline every
//! glyph, but the rich draw pass takes its outline from the style sheet
//! rather than from a draw-time argument. Folding a row's outline onto
//! the sheet's root selector is what bridges the two, and the derived
//! sheet has to keep one identity across frames — [`RichShapeCache`]
//! keys on the sheet's `Arc` pointer, so a fresh sheet per frame would
//! miss every time.
//!
//! [`RichShapeCache`]: crate::text::rich::RichShapeCache

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::color::Color;
use crate::geometry::{Affine, Vec2};
use crate::plot::theme::ThemeColor;
use crate::text::rich::{pt as rich_pt, RichTextStyleSheet, StyleDelta as RichStyleDelta};

/// Sheets derived from a base one by folding a row's outline onto its
/// root selector, keyed by `(base sheet identity, colour, width)`.
#[derive(Default)]
pub(crate) struct OutlineSheets {
    sheets: RefCell<HashMap<(usize, u128, u64), Arc<RichTextStyleSheet>>>,
}

impl OutlineSheets {
    /// An empty cache.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Drop every derived sheet.
    pub(crate) fn clear(&self) {
        self.sheets.borrow_mut().clear();
    }

    /// `base` with `stroke` and `width_pt` folded onto its `base`
    /// selector so every span inherits the halo — a span that sets its
    /// own `text_stroke` still wins. Returns `base` itself when the row
    /// resolved no outline colour.
    pub(crate) fn resolve(
        &self,
        base: &Arc<RichTextStyleSheet>,
        stroke: Option<Color>,
        width_pt: f64,
    ) -> Arc<RichTextStyleSheet> {
        let Some(color) = stroke else {
            return Arc::clone(base);
        };
        let [r, g, b, a] = color.components;
        let color_bits = (r.to_bits() as u128) << 96
            | (g.to_bits() as u128) << 64
            | (b.to_bits() as u128) << 32
            | a.to_bits() as u128;
        let key = (Arc::as_ptr(base) as usize, color_bits, width_pt.to_bits());
        let mut sheets = self.sheets.borrow_mut();
        Arc::clone(sheets.entry(key).or_insert_with(|| {
            let mut derived = (**base).clone();
            let root = derived.get("base").cloned().unwrap_or_default();
            derived.set(
                "base",
                RichStyleDelta {
                    text_stroke: Some(ThemeColor::Fixed(color)),
                    text_stroke_width: Some(rich_pt(width_pt)),
                    ..root
                },
            );
            Arc::new(derived)
        }))
    }
}

impl std::fmt::Debug for OutlineSheets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlineSheets")
            .field("len", &self.sheets.borrow().len())
            .finish()
    }
}

/// Re-order a panel-space `transform` for [`draw_rich_text`] drawing at
/// `(x, y)`.
///
/// [`draw_text`] bakes the draw position into glyph coordinates and
/// applies the caller's transform to the result, so a rotation about a
/// panel-space pivot lands as written. [`draw_rich_text`] composes the
/// other way round — `translate(x, y) * transform` — which applies the
/// transform in layout-local coordinates, pivoting about a point that
/// is not in that frame. Conjugating by the translation restores the
/// panel-space ordering, leaving the identity untouched.
///
/// [`draw_text`]: crate::text::draw_text
/// [`draw_rich_text`]: crate::text::rich::draw_rich_text
pub(crate) fn panel_space_transform(transform: Affine, x: f64, y: f64) -> Affine {
    if transform == Affine::IDENTITY {
        return transform;
    }
    Affine::translate(Vec2::new(-x, -y)) * transform * Affine::translate(Vec2::new(x, y))
}
