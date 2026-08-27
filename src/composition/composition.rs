//! The composition grid: [`Composition`], the [`Element`] tree it holds,
//! [`CompositionError`], and the free combinators that build compositions.

use crate::geometry::Size;
use crate::layout::{Axis, Cell, Extent, Inset, Placement, Track};

use super::build::{
    build_composition_grid, build_single_patch, element_contains_patch_id, inset_is_zero,
    BuildState,
};
use super::{CompositionLayout, Patch, PatchPlacement, Slot, Span};

/// A grid of [`Element`]s of size `rows × cols`. Per-panel-column widths and
/// per-panel-row heights default to `Fr(1.0)`; override with
/// [`Composition::widths`] / [`Composition::heights`].
///
/// Construct with [`beside`], [`stack`], [`grid`], or
/// [`Composition::empty`] + [`Composition::place`] for spans.
///
/// Nested compositions are supported: an [`Element::Composition`] placed in
/// a cell is simplified to the same canonical 13×16 anatomical block as a
/// plain patch, with the inner composition's panel band collapsed into the
/// outer block's panel cell and the inner border plots' chrome propagated
/// to the outer block's chrome slots.
pub struct Composition {
    pub(super) placements: Vec<CompositionPlacement>,
    pub(super) cols: usize,
    pub(super) rows: usize,
    pub(super) widths: Vec<Track>,
    pub(super) heights: Vec<Track>,
    /// Optional id for addressing chrome rects via
    /// [`CompositionLayout::get`]. Set with [`Composition::id`].
    /// `None` ⇒ chrome rects are placed but not retrievable by id.
    pub(super) id: Option<String>,
    /// Composition-level chrome slots (Title, Caption, axis titles, …).
    /// When non-empty, the composition is treated as a "simplified plot":
    /// its facets fill the panel cell of a canonical 13×16 anatomical
    /// block, and these chrome slots sit at the canonical positions
    /// surrounding it. Mirrors patchwork's `plot_annotation()`.
    pub(super) chrome: Vec<PatchPlacement>,
    /// When chrome is present, applies an aspect-ratio lock to the panel
    /// cell (which contains the facets). Same wrapping as
    /// [`Patch::aspect`].
    pub(super) aspect: Option<(f64, f64)>,
    /// Outer margin around the simplified canonical block. Only applied
    /// when chrome is present.
    pub(super) margin: Inset,
    /// Inner padding inside the simplified canonical block. Only applied
    /// when chrome is present.
    pub(super) padding: Inset,
    /// First construction error, if any. Builders record here instead
    /// of panicking, so [`Composition::try_solve`] reports every
    /// failure mode rather than only the ones that survive to solve
    /// time; [`Composition::solve`] panics on it.
    pub(super) error: Option<CompositionError>,
}

pub(crate) struct CompositionPlacement {
    /// 1-indexed top-left cell within the composition.
    pub(super) row: u16,
    pub(super) col: u16,
    pub(super) span: Span,
    pub(super) element: Element,
}

/// Either a [`Patch`] or a (nested) [`Composition`].
//
// `Patch` carries the per-side margin + padding `Inset`s (6 `Option<Extent>`
// each), so the `Patch` variant is ~ 400 bytes heavier than `Composition`.
// Acceptable given the small number of `Element` values typically
// constructed (one per patch in a composition); boxing margin/padding inside
// `Patch` would add allocations on every construction.
#[allow(clippy::large_enum_variant)]
pub enum Element {
    Patch(Patch),
    Composition(Composition),
}

impl From<Patch> for Element {
    fn from(p: Patch) -> Self {
        Element::Patch(p)
    }
}

impl From<Composition> for Element {
    fn from(c: Composition) -> Self {
        Element::Composition(c)
    }
}

impl Composition {
    /// Build an empty `rows × cols` composition filled with anonymous
    /// spacers. Drop elements into specific cells with [`Self::place`].
    pub fn empty(rows: usize, cols: usize) -> Composition {
        let error = (rows < 1 || cols < 1).then_some(CompositionError::Degenerate { rows, cols });
        // Clamp so the rest of the builder chain still has a coherent
        // shape to record further errors against.
        let (rows, cols) = (rows.max(1), cols.max(1));
        Composition {
            placements: Vec::new(),
            cols,
            rows,
            widths: vec![Track::Fr(1.0); cols],
            heights: vec![Track::Fr(1.0); rows],
            id: None,
            chrome: Vec::new(),
            aspect: None,
            margin: Inset::default(),
            padding: Inset::default(),
            error,
        }
    }

    /// Record `err` unless an earlier one is already pending — the
    /// first failure is the one that explains the rest.
    fn fail(mut self, err: CompositionError) -> Self {
        if self.error.is_none() {
            self.error = Some(err);
        }
        self
    }

    /// Set the composition's id for chrome lookups. Required if you
    /// want to retrieve chrome rects (Title, Caption, …) via
    /// [`CompositionLayout::get`]. The composition's id is independent
    /// of patch ids inside it.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Add a chrome slot to this composition. The composition becomes a
    /// "simplified plot" wrapping its facets in the canonical 13×16
    /// anatomical block; the slot lives at its canonical position
    /// around the panel band (which contains the facets).
    ///
    /// Useful for giving a faceted plot a shared title / subtitle /
    /// caption / axis title that spans all facets.
    ///
    /// [`Slot::Panel`] is accepted: it resolves to the rect the facets
    /// occupy, addressable as `(composition_id, "panel")`. Content
    /// placed there sits behind or over the facets — a shared panel
    /// background, a watermark, an annotation layer.
    pub fn slot(mut self, s: Slot, cell: Cell) -> Self {
        let (r, c, rs, cs) = s.placement();
        self.chrome.push(PatchPlacement {
            placement: Placement::at(r, c).span(rs, cs),
            region: s.name().to_string(),
            cell,
        });
        self
    }

    /// Escape hatch for composition-level chrome: place content at a
    /// raw 1-indexed `(row, col)` within the canonical 13×16 block,
    /// addressable as `(composition_id, region)`. Mirrors
    /// [`Patch::place_at`].
    ///
    /// The panel cell (row 9, col 7) may be covered; the facets sit in
    /// the same tracks, so content there shares their rect. A span
    /// reaching into the surrounding Auto chrome tracks sizes them, as
    /// any spanning placement does.
    pub fn place_at(
        mut self,
        region: impl Into<String>,
        row: u16,
        col: u16,
        span: Span,
        cell: Cell,
    ) -> Self {
        self.chrome.push(PatchPlacement {
            placement: Placement::at(row, col).span(span.rows, span.cols),
            region: region.into(),
            cell,
        });
        self
    }

    /// Lock every descendant's panel to an aspect ratio. The ratio
    /// cascades depth-first into patches and nested compositions that
    /// don't carry one of their own; a descendant with its own aspect
    /// keeps it and blocks propagation past that node. Same per-patch
    /// semantics as [`Patch::aspect`].
    pub fn aspect(mut self, w: f64, h: f64) -> Self {
        self.aspect = Some((w, h));
        self
    }

    /// Per-side outer margin around the whole composition. Setting it
    /// wraps the facets in a canonical block to carry the ring. Same
    /// semantics as [`Patch::margin`].
    pub fn margin(mut self, inset: Inset) -> Self {
        self.margin = inset;
        self
    }

    /// Convenience: identical margin on every side.
    pub fn margin_all(self, length: Extent) -> Self {
        self.margin(Inset::all(length))
    }

    /// Per-side inner padding between the composition's background edge
    /// and its chrome. Setting it wraps the facets in a canonical block
    /// to carry the ring. Same semantics as [`Patch::padding`].
    pub fn padding(mut self, inset: Inset) -> Self {
        self.padding = inset;
        self
    }

    /// Convenience: identical padding on every side.
    pub fn padding_all(self, length: Extent) -> Self {
        self.padding(Inset::all(length))
    }

    /// The composition's id, if set with [`Self::id`]. Composition-level
    /// chrome rects are keyed on `(id, region)`, so an unnamed
    /// composition's chrome is placed but not retrievable.
    pub fn composition_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Borrow the composition's aspect lock, if any.
    pub fn aspect_ratio(&self) -> Option<(f64, f64)> {
        self.aspect
    }

    /// Borrow the composition's outer margin inset.
    pub fn margin_inset(&self) -> &Inset {
        &self.margin
    }

    /// Borrow the composition's inner padding inset.
    pub fn padding_inset(&self) -> &Inset {
        &self.padding
    }

    /// Does this composition need wrapping in a canonical block? True
    /// when it carries anything that block would hold: chrome cells, or
    /// a margin / padding ring. An aspect is not among them — it
    /// cascades into the descendants rather than locking a cell of the
    /// composition's own.
    pub(super) fn has_chrome(&self) -> bool {
        !self.chrome.is_empty() || !inset_is_zero(&self.margin) || !inset_is_zero(&self.padding)
    }

    /// Place an element at 1-indexed `(row, col)` covering `span.rows ×
    /// span.cols` cells. Re-placing into cells already covered by a previous
    /// placement is allowed — later calls overlay earlier ones.
    pub fn place(mut self, row: u16, col: u16, span: Span, element: impl Into<Element>) -> Self {
        if row < 1 || col < 1 {
            return self.fail(CompositionError::NotOneIndexed { row, col });
        }
        let end_row = (row + span.rows - 1) as usize;
        let end_col = (col + span.cols - 1) as usize;
        let (rows, cols) = (self.rows, self.cols);
        if end_row > rows {
            return self.fail(CompositionError::PlacementOverflow {
                axis: Axis::Height,
                end: end_row,
                available: rows,
            });
        }
        if end_col > cols {
            return self.fail(CompositionError::PlacementOverflow {
                axis: Axis::Width,
                end: end_col,
                available: cols,
            });
        }
        self.placements.push(CompositionPlacement {
            row,
            col,
            span,
            element: element.into(),
        });
        self
    }

    /// Override the per-panel-column tracks. `tracks.len()` must equal
    /// `self.cols`. Default is `Fr(1.0)` for every column.
    pub fn widths(mut self, tracks: Vec<Track>) -> Self {
        if tracks.len() != self.cols {
            let found = tracks.len();
            let expected = self.cols;
            return self.fail(CompositionError::TrackCountMismatch {
                axis: Axis::Width,
                expected,
                found,
            });
        }
        self.widths = tracks;
        self
    }

    /// Override the per-panel-row tracks. `tracks.len()` must equal
    /// `self.rows`. Default is `Fr(1.0)` for every row.
    pub fn heights(mut self, tracks: Vec<Track>) -> Self {
        if tracks.len() != self.rows {
            let found = tracks.len();
            let expected = self.rows;
            return self.fail(CompositionError::TrackCountMismatch {
                axis: Axis::Height,
                expected,
                found,
            });
        }
        self.heights = tracks;
        self
    }

    /// `true` if any patch reachable from this composition (including
    /// patches nested inside other patches' panels) has the given id.
    /// Walks the element tree; anonymous patches are skipped.
    pub fn contains_patch_id(&self, id: &str) -> bool {
        self.placements
            .iter()
            .any(|p| element_contains_patch_id(&p.element, id))
    }

    /// Number of composition columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Number of composition rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Per-column tracks (panel column sizing). Extent always equals
    /// [`Self::cols`]. Default `Fr(1.0)` per column unless the user set
    /// [`Self::widths`].
    pub fn widths_slice(&self) -> &[Track] {
        &self.widths
    }

    /// Per-row tracks (panel row sizing). Extent always equals
    /// [`Self::rows`].
    pub fn heights_slice(&self) -> &[Track] {
        &self.heights
    }

    /// Iterate `(row, col, span, &Element)` tuples for each placement.
    /// Used by orchestrators (e.g. plot's `PlotComposition`) that walk
    /// the composition tree to build a clone-friendly description.
    pub fn placements(&self) -> impl Iterator<Item = (u16, u16, Span, &Element)> + '_ {
        self.placements
            .iter()
            .map(|p| (p.row, p.col, p.span, &p.element))
    }

    /// Append a new column with `other` placed in the single row at
    /// position `(1, cols + 1)`. Requires `self.rows == 1`. For
    /// multi-row appends use [`Self::empty`] + [`Self::place`].
    ///
    /// Distinct from the free [`beside`] function, which builds a fresh
    /// 1×2 composition rather than growing this one.
    pub fn append_col(mut self, other: impl Into<Element>) -> Self {
        if self.rows != 1 {
            let extent = self.rows;
            return self.fail(CompositionError::NotAppendable {
                axis: Axis::Height,
                extent,
            });
        }
        self.cols += 1;
        self.widths.push(Track::Fr(1.0));
        self.placements.push(CompositionPlacement {
            row: 1,
            col: self.cols as u16,
            span: Span::cell(),
            element: other.into(),
        });
        self
    }

    /// Append a new row with `other` placed in the single column at
    /// position `(rows + 1, 1)`. Requires `self.cols == 1`.
    ///
    /// Distinct from the free [`stack`] function, which builds a fresh
    /// 2×1 composition rather than growing this one.
    pub fn append_row(mut self, other: impl Into<Element>) -> Self {
        if self.cols != 1 {
            let extent = self.cols;
            return self.fail(CompositionError::NotAppendable {
                axis: Axis::Width,
                extent,
            });
        }
        self.rows += 1;
        self.heights.push(Track::Fr(1.0));
        self.placements.push(CompositionPlacement {
            row: self.rows as u16,
            col: 1,
            span: Span::cell(),
            element: other.into(),
        });
        self
    }

    /// Solve the composition in a `size`-sized viewport.
    pub fn solve(self, size: Size, dpi: f64) -> CompositionLayout {
        Element::Composition(self).solve(size, dpi)
    }

    /// Like [`Self::solve`] but returns an error instead of panicking.
    ///
    /// Reports construction errors the builders recorded (a placement
    /// off the grid, a mismatched track list) as well as solve-time
    /// ones (duplicate patch ids),
    /// so this is the single entry point for validating a composition
    /// built from untrusted input.
    pub fn try_solve(self, size: Size, dpi: f64) -> Result<CompositionLayout, CompositionError> {
        Element::Composition(self).try_solve(size, dpi)
    }

    /// The first construction error recorded by the builders, if any.
    /// [`Self::try_solve`] surfaces the same thing; this reports it
    /// without solving.
    pub fn error(&self) -> Option<&CompositionError> {
        self.error.as_ref()
    }
}

impl Element {
    /// The first construction error anywhere in this element's tree.
    /// Walks nested compositions so an error recorded three levels
    /// down still reaches the caller.
    fn check_construction(&self) -> Result<(), CompositionError> {
        match self {
            Element::Patch(_) => Ok(()),
            Element::Composition(c) => {
                if let Some(e) = &c.error {
                    return Err(e.clone());
                }
                for p in &c.placements {
                    p.element.check_construction()?;
                }
                Ok(())
            }
        }
    }

    /// Solve this element as the root of a layout.
    ///
    /// # Panics
    ///
    /// On any [`CompositionError`] — a construction mistake the
    /// builders recorded, or a duplicate patch id. Use
    /// [`Self::try_solve`] to inspect it instead.
    pub fn solve(self, size: Size, dpi: f64) -> CompositionLayout {
        match self.try_solve(size, dpi) {
            Ok(layout) => layout,
            Err(e) => panic!("composition error: {e} — use try_solve to handle this"),
        }
    }

    /// Like [`Self::solve`] but returns errors instead of panicking.
    pub fn try_solve(self, size: Size, dpi: f64) -> Result<CompositionLayout, CompositionError> {
        self.check_construction()?;
        let mut state = BuildState::new();
        let root_id = state.alloc_id();
        let grid = match self {
            Element::Patch(p) => build_single_patch(p, root_id, &mut state)?,
            Element::Composition(c) => build_composition_grid(c, root_id, &mut state, None)?,
        };
        let layout = grid.solve(size, dpi);
        Ok(CompositionLayout {
            layout,
            regions: state.regions,
        })
    }
}

/// Errors produced by [`Composition::try_solve`].
#[derive(Debug, Clone)]
pub enum CompositionError {
    /// Two patches reachable from the root carry the same id.
    DuplicateId(String),
    /// [`Composition::empty`] was given a zero row or column count.
    Degenerate { rows: usize, cols: usize },
    /// Inert variant, kept so existing exhaustive matches compile. No
    /// builder produces it: a composition-level [`Slot::Panel`] is a
    /// valid placement.
    PanelSlot,
    /// Inert variant, kept so existing exhaustive matches compile. No
    /// builder produces it: covering the panel cell is a valid
    /// placement.
    PanelCovered { row: u16, col: u16 },
    /// A placement used a 0 row or column. Placements are 1-indexed.
    NotOneIndexed { row: u16, col: u16 },
    /// A placement reached past the composition's extent on `axis`.
    PlacementOverflow {
        axis: Axis,
        end: usize,
        available: usize,
    },
    /// An explicit track list didn't match the composition's extent on
    /// `axis`.
    TrackCountMismatch {
        axis: Axis,
        expected: usize,
        found: usize,
    },
    /// [`Composition::append_col`] / [`append_row`](Composition::append_row)
    /// require a single-row / single-column composition respectively.
    NotAppendable { axis: Axis, extent: usize },
    /// [`grid`] was given a cell count that isn't `rows * cols`.
    CellCountMismatch {
        rows: usize,
        cols: usize,
        found: usize,
    },
}

impl std::fmt::Display for CompositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompositionError::DuplicateId(id) => {
                write!(f, "duplicate patch id: {id:?}")
            }
            CompositionError::Degenerate { rows, cols } => {
                write!(f, "composition must be at least 1×1, got {rows}×{cols}")
            }
            CompositionError::PanelSlot => {
                write!(f, "composition-level panel slot")
            }
            CompositionError::PanelCovered { row, col } => {
                write!(f, "placement covering the panel cell ({row}, {col})")
            }
            CompositionError::NotOneIndexed { row, col } => {
                write!(f, "composition placement is 1-indexed, got ({row}, {col})")
            }
            CompositionError::PlacementOverflow {
                axis,
                end,
                available,
            } => write!(
                f,
                "placement reaches {axis:?} {end} but the composition has {available}"
            ),
            CompositionError::TrackCountMismatch {
                axis,
                expected,
                found,
            } => write!(
                f,
                "{axis:?} track list must have {expected} entries, got {found}"
            ),
            CompositionError::NotAppendable { axis, extent } => write!(
                f,
                "appending along {axis:?} requires a single-track composition, got {extent}"
            ),
            CompositionError::CellCountMismatch { rows, cols, found } => write!(
                f,
                "grid({rows}, {cols}) needs {} cells, got {found}",
                rows * cols
            ),
        }
    }
}

impl std::error::Error for CompositionError {}

// ─── Free-function combinators ───────────────────────────────────────────────

/// Place `a` and `b` side by side in a 1×2 composition.
pub fn beside(a: impl Into<Element>, b: impl Into<Element>) -> Composition {
    grid(1, 2, vec![a.into(), b.into()])
}

/// Stack `a` on top of `b` in a 2×1 composition.
pub fn stack(a: impl Into<Element>, b: impl Into<Element>) -> Composition {
    grid(2, 1, vec![a.into(), b.into()])
}

/// Build a `rows × cols` composition from `cells` in row-major order.
/// `cells.len()` must equal `rows * cols`.
pub fn grid(rows: usize, cols: usize, cells: Vec<Element>) -> Composition {
    let found = cells.len();
    let mut c = Composition::empty(rows, cols);
    if found != rows * cols {
        return c.fail(CompositionError::CellCountMismatch { rows, cols, found });
    }
    for (i, element) in cells.into_iter().enumerate() {
        let r = (i / cols) as u16 + 1;
        let col = (i % cols) as u16 + 1;
        c.placements.push(CompositionPlacement {
            row: r,
            col,
            span: Span::cell(),
            element,
        });
    }
    c
}

/// An anonymous spacer patch — empty, alignment-only, not addressable.
pub fn spacer() -> Patch {
    Patch::anonymous()
}

/// A patch wrapping `cell` in its Panel slot. Addressable as `(id, "panel")`.
pub fn wrap(id: impl Into<String>, cell: Cell) -> Patch {
    Patch::new(id).slot(Slot::Panel, cell)
}
