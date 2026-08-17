//! A single plot's anatomical grid: [`Patch`], its slot placements, and the
//! [`Span`] used to size them.

use crate::geometry::Size;
use crate::layout::{Cell, Extent, Inset, Placement};

use super::{CompositionLayout, Element, Slot};

/// A single plot's content laid out into the 13×16 anatomical grid.
///
/// Construct with [`Patch::new(id)`](Patch::new), drop content into named
/// [`Slot`]s with [`Patch::slot`], or into custom positions with
/// [`Patch::place_at`]. Lock the panel to an aspect ratio with
/// [`Patch::aspect`]. Solve directly or compose with [`beside`] / [`stack`] /
/// [`grid`] before solving.
///
/// [`beside`]: super::beside
/// [`stack`]: super::stack
/// [`grid`]: super::grid
pub struct Patch {
    /// `None` only for anonymous spacers — those don't expose addressable
    /// regions in the final [`CompositionLayout`].
    pub(super) id: Option<String>,
    pub(super) placements: Vec<PatchPlacement>,
    pub(super) aspect: Option<(f64, f64)>,
    /// Outermost-ring track sizes. The [`Slot::Background`] does not extend
    /// into these tracks. Defaults to `Inset::default()` (zero on every
    /// side). See [`Patch::margin`].
    pub(super) margin: Inset,
    /// Second-from-outermost-ring track sizes. The background covers the
    /// padding area; chrome (axes, title, legend) sits inside the padding.
    /// Defaults to `Inset::default()`. See [`Patch::padding`].
    pub(super) padding: Inset,
}

/// One slot placement inside a [`Patch`] — captures the anatomical
/// position, the region name (used for lookups in the resolved
/// layout), and the [`Cell`] whose measure drives sizing.
pub struct PatchPlacement {
    pub placement: Placement,
    pub region: String,
    pub cell: Cell,
}

impl Patch {
    /// Create a named patch. The id must be unique across all patches reachable
    /// from the root of a composition.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            placements: Vec::new(),
            aspect: None,
            margin: Inset::default(),
            padding: Inset::default(),
        }
    }

    /// Create an anonymous patch — used internally by [`spacer`]. Not
    /// addressable in [`CompositionLayout::get`].
    pub(super) fn anonymous() -> Self {
        Self {
            id: None,
            placements: Vec::new(),
            aspect: None,
            margin: Inset::default(),
            padding: Inset::default(),
        }
    }

    /// Place content into a named anatomical [`Slot`]. The slot's region name
    /// (e.g. `"axis_left_text"`) is used in [`CompositionLayout::get`] lookups.
    ///
    /// Multiple calls for the same `Slot` produce multiple
    /// placements; the layout solver rejects that as a duplicate id.
    /// Callers that need to merge contributions from multiple sources
    /// (e.g. the `PlotComposition` orchestrator when several plots
    /// share a patch) should harvest each source's placements
    /// independently via [`Self::into_placements`] and emit one
    /// merged cell per region — typically by wrapping the per-source
    /// measures in a [`MaxMergeMeasure`](crate::layout::MaxMergeMeasure).
    pub fn slot(mut self, s: Slot, cell: Cell) -> Self {
        let (r, c, rs, cs) = s.placement();
        self.placements.push(PatchPlacement {
            placement: Placement::at(r, c).span(rs, cs),
            region: s.name().to_string(),
            cell,
        });
        self
    }

    /// Consume this patch and yield its placements. Each placement
    /// is a `(placement, region, cell)` triple — the orchestrator
    /// uses this to harvest contributions from multiple plots,
    /// group by region, and re-emit one merged cell per region.
    pub fn into_placements(self) -> Vec<PatchPlacement> {
        self.placements
    }

    /// The patch's id, or `None` for anonymous spacers.
    pub fn patch_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Borrow this patch's aspect lock, if any.
    pub fn aspect_ratio(&self) -> Option<(f64, f64)> {
        self.aspect
    }

    /// Borrow this patch's outer margin inset.
    pub fn margin_inset(&self) -> &Inset {
        &self.margin
    }

    /// Borrow this patch's inner padding inset.
    pub fn padding_inset(&self) -> &Inset {
        &self.padding
    }

    /// Escape hatch: place content at a raw (1-indexed) `(row, col)` with an
    /// explicit span and an arbitrary region name. Looked up as
    /// `layout.get(patch_id, region)`.
    pub fn place_at(
        mut self,
        region: impl Into<String>,
        row: u16,
        col: u16,
        span: Span,
        cell: Cell,
    ) -> Self {
        self.placements.push(PatchPlacement {
            placement: Placement::at(row, col).span(span.rows, span.cols),
            region: region.into(),
            cell,
        });
        self
    }

    /// Lock the panel to an aspect ratio of `w:h`. The panel cell is wrapped
    /// in a `respect()`-locked sub-grid, isolated per patch.
    pub fn aspect(mut self, w: f64, h: f64) -> Self {
        self.aspect = Some((w, h));
        self
    }

    /// Per-side outer margin. Sizes the outermost ring tracks (row 1,
    /// `TABLE_ROWS`, col 1, `TABLE_COLS`) of this patch's anatomy. The
    /// [`Slot::Background`] does **not** extend into the margin, so when
    /// two patches are composed side-by-side the gap between their
    /// backgrounds equals `margin_a.right + margin_b.left`. Default is
    /// zero on every side.
    pub fn margin(mut self, inset: Inset) -> Self {
        self.margin = inset;
        self
    }

    /// Convenience: identical margin on every side.
    pub fn margin_all(self, length: Extent) -> Self {
        self.margin(Inset::all(length))
    }

    /// Per-side inner padding. Sizes the second-from-outer-ring tracks
    /// (row 2, `TABLE_ROWS - 1`, col 2, `TABLE_COLS - 1`). The
    /// [`Slot::Background`] covers the padding, but chrome (axes, title,
    /// legends) sits inside the padding — so padding is the breathing
    /// room between the background's edge and the start of chrome.
    /// Default is zero on every side.
    pub fn padding(mut self, inset: Inset) -> Self {
        self.padding = inset;
        self
    }

    /// Convenience: identical padding on every side.
    pub fn padding_all(self, length: Extent) -> Self {
        self.padding(Inset::all(length))
    }

    /// Solve this patch standalone in a `size`-sized viewport.
    pub fn solve(self, size: Size, dpi: f64) -> CompositionLayout {
        Element::Patch(self).solve(size, dpi)
    }
}

// ─── Span ────────────────────────────────────────────────────────────────────

/// A row × column span (1-indexed counts) used by [`Patch::place_at`] and
/// [`Composition::place`].
///
/// [`Composition::place`]: super::Composition::place
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub rows: u16,
    pub cols: u16,
}

impl Span {
    /// 1×1.
    pub fn cell() -> Self {
        Self { rows: 1, cols: 1 }
    }
    /// `r × 1`.
    pub fn rows(r: u16) -> Self {
        Self { rows: r, cols: 1 }
    }
    /// `1 × c`.
    pub fn cols(c: u16) -> Self {
        Self { rows: 1, cols: c }
    }
    /// `r × c`.
    pub fn rc(r: u16, c: u16) -> Self {
        Self { rows: r, cols: c }
    }
}
