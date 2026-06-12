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

pub use anatomy::{
    Slot, MARGIN_BOTTOM_ROW, MARGIN_LEFT_COL, MARGIN_RIGHT_COL, MARGIN_TOP_ROW, PADDING_BOTTOM_ROW,
    PADDING_LEFT_COL, PADDING_RIGHT_COL, PADDING_TOP_ROW, PANEL_COL, PANEL_ROW, PLOT_BOTTOM,
    PLOT_LEFT, PLOT_RIGHT, PLOT_TOP, TABLE_COLS, TABLE_ROWS,
};

use crate::geometry::{Rect, Size};
use crate::layout::{Axis, Cell, CellId, Grid, Inset, Layout, Length, Placement, Track};
use std::collections::HashMap;

const TABLE_COLS_U16: u16 = TABLE_COLS as u16;
const TABLE_ROWS_U16: u16 = TABLE_ROWS as u16;

// ─── Patch ───────────────────────────────────────────────────────────────────

/// A single plot's content laid out into the 13×16 anatomical grid.
///
/// Construct with [`Patch::new(id)`](Patch::new), drop content into named
/// [`Slot`]s with [`Patch::slot`], or into custom positions with
/// [`Patch::place_at`]. Lock the panel to an aspect ratio with
/// [`Patch::aspect`]. Solve directly or compose with [`beside`] / [`stack`] /
/// [`grid`] before solving.
pub struct Patch {
    /// `None` only for anonymous spacers — those don't expose addressable
    /// regions in the final [`CompositionLayout`].
    id: Option<String>,
    placements: Vec<PatchPlacement>,
    aspect: Option<(f32, f32)>,
    /// Outermost-ring track sizes. The [`Slot::Background`] does not extend
    /// into these tracks. Defaults to `Inset::default()` (zero on every
    /// side). See [`Patch::margin`].
    margin: Inset,
    /// Second-from-outermost-ring track sizes. The background covers the
    /// padding area; chrome (axes, title, legend) sits inside the padding.
    /// Defaults to `Inset::default()`. See [`Patch::padding`].
    padding: Inset,
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

    /// The patch's id, or `None` for anonymous spacers.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Create an anonymous patch — used internally by [`spacer`]. Not
    /// addressable in [`CompositionLayout::get`].
    fn anonymous() -> Self {
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

    /// Borrow this patch's id, if any.
    pub fn patch_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Borrow this patch's aspect lock, if any.
    pub fn aspect_ratio(&self) -> Option<(f32, f32)> {
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
    pub fn aspect(mut self, w: f32, h: f32) -> Self {
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
    pub fn margin_all(self, length: Length) -> Self {
        self.margin(
            Inset::default()
                .left(length.clone())
                .right(length.clone())
                .top(length.clone())
                .bottom(length),
        )
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
    pub fn padding_all(self, length: Length) -> Self {
        self.padding(
            Inset::default()
                .left(length.clone())
                .right(length.clone())
                .top(length.clone())
                .bottom(length),
        )
    }

    /// Solve this patch standalone in a `size`-sized viewport.
    pub fn solve(self, size: Size, dpi: f64) -> CompositionLayout {
        Element::Patch(self).solve(size, dpi)
    }
}

// ─── Span ────────────────────────────────────────────────────────────────────

/// A row × column span (1-indexed counts) used by [`Patch::place_at`] and
/// [`Composition::place`].
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

// ─── Composition / Element ───────────────────────────────────────────────────

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
    placements: Vec<CompositionPlacement>,
    cols: usize,
    rows: usize,
    widths: Vec<Track>,
    heights: Vec<Track>,
    /// Optional id for addressing chrome rects via
    /// [`CompositionLayout::get`]. Set with [`Composition::id`].
    /// `None` ⇒ chrome rects are placed but not retrievable by id.
    id: Option<String>,
    /// Composition-level chrome slots (Title, Caption, axis titles, …).
    /// When non-empty, the composition is treated as a "simplified plot":
    /// its facets fill the panel cell of a canonical 13×16 anatomical
    /// block, and these chrome slots sit at the canonical positions
    /// surrounding it. Mirrors patchwork's `plot_annotation()`.
    chrome: Vec<PatchPlacement>,
    /// When chrome is present, applies an aspect-ratio lock to the panel
    /// cell (which contains the facets). Same wrapping as
    /// [`Patch::aspect`].
    aspect: Option<(f32, f32)>,
    /// Outer margin around the simplified canonical block. Only applied
    /// when chrome is present.
    margin: Inset,
    /// Inner padding inside the simplified canonical block. Only applied
    /// when chrome is present.
    padding: Inset,
}

struct CompositionPlacement {
    /// 1-indexed top-left cell within the composition.
    row: u16,
    col: u16,
    span: Span,
    element: Element,
}

/// Either a [`Patch`] or a (nested) [`Composition`].
//
// `Patch` carries the per-side margin + padding `Inset`s (6 `Option<Length>`
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
        assert!(rows >= 1 && cols >= 1, "composition must be at least 1×1");
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
        }
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
    /// Panics on [`Slot::Panel`] — the composition's facets fill the
    /// panel.
    pub fn slot(mut self, s: Slot, cell: Cell) -> Self {
        assert!(
            !matches!(s, Slot::Panel),
            "Composition::slot does not accept Slot::Panel; the composition's facets fill the panel"
        );
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
    /// Panics if `(row, col, span)` includes the canonical panel cell
    /// (row 9 col 7) — that cell is reserved for the composition's
    /// facets.
    pub fn place_at(
        mut self,
        region: impl Into<String>,
        row: u16,
        col: u16,
        span: Span,
        cell: Cell,
    ) -> Self {
        let end_row = row + span.rows - 1;
        let end_col = col + span.cols - 1;
        assert!(
            !(row <= PANEL_ROW && end_row >= PANEL_ROW && col <= PANEL_COL && end_col >= PANEL_COL),
            "Composition::place_at cannot cover the panel cell (row {PANEL_ROW}, col {PANEL_COL}); the facets fill it"
        );
        self.chrome.push(PatchPlacement {
            placement: Placement::at(row, col).span(span.rows, span.cols),
            region: region.into(),
            cell,
        });
        self
    }

    /// Lock the simplified plot's panel cell (which contains the
    /// facets) to an aspect ratio. Same semantics as [`Patch::aspect`].
    /// Only takes effect when the composition has chrome.
    pub fn aspect(mut self, w: f32, h: f32) -> Self {
        self.aspect = Some((w, h));
        self
    }

    /// Per-side outer margin for the simplified canonical block. Same
    /// semantics as [`Patch::margin`]. Only takes effect when the
    /// composition has chrome.
    pub fn margin(mut self, inset: Inset) -> Self {
        self.margin = inset;
        self
    }

    /// Convenience: identical margin on every side.
    pub fn margin_all(self, length: Length) -> Self {
        self.margin(
            Inset::default()
                .left(length.clone())
                .right(length.clone())
                .top(length.clone())
                .bottom(length),
        )
    }

    /// Per-side inner padding for the simplified canonical block. Same
    /// semantics as [`Patch::padding`]. Only takes effect when the
    /// composition has chrome.
    pub fn padding(mut self, inset: Inset) -> Self {
        self.padding = inset;
        self
    }

    /// Convenience: identical padding on every side.
    pub fn padding_all(self, length: Length) -> Self {
        self.padding(
            Inset::default()
                .left(length.clone())
                .right(length.clone())
                .top(length.clone())
                .bottom(length),
        )
    }

    /// Has any composition-level chrome been added?
    fn has_chrome(&self) -> bool {
        !self.chrome.is_empty()
            || self.aspect.is_some()
            || !inset_is_zero(&self.margin)
            || !inset_is_zero(&self.padding)
    }

    /// Place an element at 1-indexed `(row, col)` covering `span.rows ×
    /// span.cols` cells. Re-placing into cells already covered by a previous
    /// placement is allowed — later calls overlay earlier ones.
    pub fn place(mut self, row: u16, col: u16, span: Span, element: impl Into<Element>) -> Self {
        assert!(row >= 1 && col >= 1, "composition placement is 1-indexed");
        assert!(
            (row + span.rows - 1) as usize <= self.rows,
            "placement extends past composition row count"
        );
        assert!(
            (col + span.cols - 1) as usize <= self.cols,
            "placement extends past composition col count"
        );
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
        assert_eq!(
            tracks.len(),
            self.cols,
            "widths length must equal composition columns"
        );
        self.widths = tracks;
        self
    }

    /// Override the per-panel-row tracks. `tracks.len()` must equal
    /// `self.rows`. Default is `Fr(1.0)` for every row.
    pub fn heights(mut self, tracks: Vec<Track>) -> Self {
        assert_eq!(
            tracks.len(),
            self.rows,
            "heights length must equal composition rows"
        );
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

    /// Per-column tracks (panel column sizing). Length always equals
    /// [`Self::cols`]. Default `Fr(1.0)` per column unless the user set
    /// [`Self::widths`].
    pub fn widths_slice(&self) -> &[Track] {
        &self.widths
    }

    /// Per-row tracks (panel row sizing). Length always equals
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

    /// Append a new column with `other` placed in the single row at position
    /// `(1, cols + 1)`. Requires `self.rows == 1`. For multi-row appends use
    /// [`Self::empty`] + [`Self::place`].
    pub fn beside(mut self, other: impl Into<Element>) -> Self {
        assert_eq!(
            self.rows, 1,
            "Composition::beside requires a single-row composition; use empty() + place() instead"
        );
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

    /// Append a new row with `other` placed in the single column at position
    /// `(rows + 1, 1)`. Requires `self.cols == 1`.
    pub fn stack(mut self, other: impl Into<Element>) -> Self {
        assert_eq!(
            self.cols, 1,
            "Composition::stack requires a single-column composition; use empty() + place() instead"
        );
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

    /// Like [`Self::solve`] but returns an error instead of panicking on
    /// duplicate patch ids or unsupported nesting.
    pub fn try_solve(self, size: Size, dpi: f64) -> Result<CompositionLayout, CompositionError> {
        Element::Composition(self).try_solve(size, dpi)
    }
}

impl Element {
    /// Solve this element as the root of a layout.
    pub fn solve(self, size: Size, dpi: f64) -> CompositionLayout {
        self.try_solve(size, dpi)
            .expect("composition error — use try_solve to inspect (duplicate ids)")
    }

    /// Like [`Self::solve`] but returns errors instead of panicking.
    pub fn try_solve(self, size: Size, dpi: f64) -> Result<CompositionLayout, CompositionError> {
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

// ─── Build pipeline ──────────────────────────────────────────────────────────
//
// Each composition produces a uniform `rows · TABLE_ROWS × cols · TABLE_COLS`
// outer grid — one canonical 13×16 anatomical block per outer cell, no
// expansion for nested compositions. A nested composition is placed as a
// sub-`Grid` spanning the entire outer block (rows 1..16, cols 1..13). The
// inner composition's outer-facing chrome aligns with the outer block's
// chrome rows/cols via `Length::TrackOf` sizer cells on both sides of the
// boundary: forward sizers in the outer point at sub-Grid chrome tracks;
// back sizers in the sub point at outer chrome tracks. The fixed-point
// iteration over `TrackOf` references in the solver converges this
// bidirectional coupling in two or three iterations per nesting level.

struct BuildState {
    next_id: u64,
    regions: HashMap<(String, String), CellId>,
}

impl BuildState {
    /// Fresh state with `next_id = 1` and no registered regions.
    fn new() -> Self {
        Self {
            next_id: 1,
            regions: HashMap::new(),
        }
    }

    /// Allocate the next monotonic [`CellId`] and bump the counter.
    fn alloc_id(&mut self) -> CellId {
        let id = CellId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Allocate a fresh [`CellId`] and register `(patch_id, region)` so
    /// it can be looked up from the solved layout. Returns
    /// [`CompositionError::DuplicateId`] if `(patch_id, region)` is
    /// already registered.
    fn register_region(
        &mut self,
        patch_id: &Option<String>,
        region: &str,
    ) -> Result<CellId, CompositionError> {
        let cell_id = self.alloc_id();
        if let Some(pid) = patch_id {
            let key = (pid.clone(), region.to_string());
            if self.regions.contains_key(&key) {
                return Err(CompositionError::DuplicateId(format!("{pid}:{region}")));
            }
            self.regions.insert(key, cell_id);
        }
        Ok(cell_id)
    }
}

/// Couples a sub-composition's inner border tracks to the parent
/// composition's outer-block chrome tracks. Carried into
/// [`build_composition_grid`] when recursing into a nested composition.
struct ParentCoupling {
    parent_id: CellId,
    /// 0-based row of the parent outer block this nested composition sits in.
    parent_block_row: usize,
    /// 0-based column of the parent outer block.
    parent_block_col: usize,
    /// Number of parent outer-block rows spanned by this composition.
    parent_block_row_span: usize,
    /// Number of parent outer-block columns spanned.
    parent_block_col_span: usize,
}

/// Build a single-patch root: wrap one patch as a 1×1 composition's outer
/// block. Reuses the same emit_patch_into machinery for consistency.
fn build_single_patch(
    p: Patch,
    grid_id: CellId,
    state: &mut BuildState,
) -> Result<Grid, CompositionError> {
    let cols = patch_block_tracks(Track::Fr(1.0), Axis::Width);
    let rows = patch_block_tracks(Track::Fr(1.0), Axis::Height);
    let mut g = Grid::new(cols, rows).id(grid_id);
    emit_patch_into(&mut g, p, 0, 0, 1, 1, state, true, true)?;
    Ok(g)
}

fn inset_is_zero(inset: &Inset) -> bool {
    inset.left.is_none()
        && inset.right.is_none()
        && inset.top.is_none()
        && inset.bottom.is_none()
        && inset.width.is_none()
        && inset.height.is_none()
}

/// Recursively build a `Grid` for `c`. `parent` is `Some` when `c` is a
/// nested composition embedded in another composition's outer block; in
/// that case the function emits back-sizers binding `c`'s inner border
/// chrome tracks to the parent's outer chrome tracks. The caller pre-
/// allocates `grid_id` so it can reference the new grid via `TrackOf`.
fn build_composition_grid(
    mut c: Composition,
    grid_id: CellId,
    state: &mut BuildState,
    parent: Option<ParentCoupling>,
) -> Result<Grid, CompositionError> {
    // Composition::aspect propagates to descendants that don't carry their
    // own aspect. Cascading is recursive: a child Composition that just
    // received the propagated aspect propagates further when its own
    // `build_composition_grid` runs.
    if let Some(asp) = c.aspect.take() {
        propagate_aspect(&mut c.placements, asp);
    }
    if c.has_chrome() {
        return build_wrapped_composition(c, grid_id, state, parent);
    }
    let cols = composition_col_tracks(&c);
    let rows = composition_row_tracks(&c);
    let mut g = Grid::new(cols, rows).id(grid_id);

    // Count aspect-bearing placements per outer row / col so each
    // child's emit / nest path knows whether it's the sole aspect-
    // contributor on its row or col. When alone in a col, it can
    // safely encode its aspect into the col Fr weight; when alone
    // in a row, it encodes into the row Fr. Cells alone in both
    // default to encoding via the col axis. When multiple aspect
    // cells share a row OR col, neither Fr can carry the signal —
    // respect alone keeps the cell coupled. A nested composition
    // counts as aspect-bearing iff its own children resolve to a
    // determinate natural aspect (every leaf patch is locked).
    let mut aspect_per_row = vec![0u32; c.rows];
    let mut aspect_per_col = vec![0u32; c.cols];
    for cp in &c.placements {
        let has_aspect = match &cp.element {
            Element::Patch(p) => p.aspect.is_some(),
            Element::Composition(inner) => composition_natural_aspect(inner).is_some(),
        };
        if has_aspect {
            let r = (cp.row as usize).saturating_sub(1);
            let col = (cp.col as usize).saturating_sub(1);
            if r < c.rows {
                aspect_per_row[r] += 1;
            }
            if col < c.cols {
                aspect_per_col[col] += 1;
            }
        }
    }

    let placements = c.placements;
    for cp in placements {
        let block_row = (cp.row - 1) as usize;
        let block_col = (cp.col - 1) as usize;
        let block_row_span = cp.span.rows.max(1) as usize;
        let block_col_span = cp.span.cols.max(1) as usize;
        match cp.element {
            Element::Patch(p) => {
                let alone_in_col = aspect_per_col.get(block_col).copied().unwrap_or(0) == 1;
                let alone_in_row = aspect_per_row.get(block_row).copied().unwrap_or(0) == 1;
                emit_patch_into(
                    &mut g,
                    p,
                    block_row,
                    block_col,
                    block_row_span,
                    block_col_span,
                    state,
                    alone_in_col,
                    alone_in_row,
                )?;
            }
            Element::Composition(inner) => {
                let sub_rows = inner.rows;
                let sub_cols = inner.cols;
                // Snapshot the nested composition's natural aspect
                // *before* moving it into the recursive build — we
                // want the same alone-in-col / alone-in-row Fr
                // propagation that `emit_patch_into` does for
                // leaf patches, so a stacked column of aspect-
                // locked plots can broadcast its width up to its
                // sibling in the outer grid.
                let nested_aspect = composition_natural_aspect(&inner);
                let sub_id = state.alloc_id();
                let sub = build_composition_grid(
                    inner,
                    sub_id,
                    state,
                    Some(ParentCoupling {
                        parent_id: grid_id,
                        parent_block_row: block_row,
                        parent_block_col: block_col,
                        parent_block_row_span: block_row_span,
                        parent_block_col_span: block_col_span,
                    }),
                )?;
                let span_rows = (block_row_span * TABLE_ROWS) as u16;
                let span_cols = (block_col_span * TABLE_COLS) as u16;
                let start_row = (block_row * TABLE_ROWS) as u16 + 1;
                let start_col = (block_col * TABLE_COLS) as u16 + 1;
                g.place(
                    Placement::at(start_row, start_col).span(span_rows, span_cols),
                    sub,
                );
                emit_forward_sizers(
                    &mut g,
                    block_row,
                    block_col,
                    block_row_span,
                    block_col_span,
                    sub_id,
                    sub_rows,
                    sub_cols,
                );
                // Propagate the nested composition's aspect to the
                // outer block's panel cell (same axis-selection rule
                // `emit_patch_into` uses for leaf patches): the
                // panel-row × panel-col cell is the canonical
                // anchor for cross-grid respect.
                if let Some((aw, ah)) = nested_aspect {
                    let alone_in_col = aspect_per_col.get(block_col).copied().unwrap_or(0) == 1;
                    let alone_in_row = aspect_per_row.get(block_row).copied().unwrap_or(0) == 1;
                    if alone_in_col || alone_in_row {
                        let panel_row_0 = block_row * TABLE_ROWS + (PANEL_ROW - 1) as usize;
                        let panel_col_0 = block_col * TABLE_COLS + (PANEL_COL - 1) as usize;
                        install_respect_at(&mut g, panel_row_0, panel_col_0);
                        if alone_in_col {
                            let ratio = if ah.abs() < f64::EPSILON { aw } else { aw / ah };
                            set_fr_if_fr(&mut g.node.cols, panel_col_0, ratio as f32);
                            if alone_in_row {
                                set_fr_if_fr(&mut g.node.rows, panel_row_0, 1.0);
                            }
                        } else if alone_in_row {
                            let ratio = if aw.abs() < f64::EPSILON { ah } else { ah / aw };
                            set_fr_if_fr(&mut g.node.rows, panel_row_0, ratio as f32);
                        }
                    }
                }
            }
        }
    }

    if let Some(parent) = parent {
        emit_back_sizers(&mut g, &parent, c.rows, c.cols);
    }

    Ok(g)
}

/// Build a Composition that carries composition-level chrome (Title,
/// Caption, axis titles, …) as a single canonical 13×16 outer block.
/// The facets sub-Grid spans the **entire** wrapping block (rows 1..16,
/// cols 1..13), with forward and back sizers binding the inner border
/// facets' chrome tracks to the wrapping block's canonical chrome
/// tracks — same mechanism as nested-composition-in-a-cell.
///
/// Consequence: composition-level chrome (e.g.
/// `composition.slot(Slot::Title, …)`) shares canonical rows with the
/// inner border facets' own chrome. If both are populated for the same
/// anatomical slot, both rects resolve to the **same y range**, with
/// the composition chrome spanning the full plot-area width and the
/// per-facet chrome spanning a single facet's width. Intended — the
/// outer wider chrome visually sits behind the narrower per-facet
/// chrome at the same canonical row.
///
/// Mirrors patchwork's `simplify_gt.gtable_patchwork`: a 13×16
/// canonical anatomy whose chrome cols/rows are shared between the
/// wrapping annotation and the inner border facets.
fn build_wrapped_composition(
    mut c: Composition,
    grid_id: CellId,
    state: &mut BuildState,
    parent: Option<ParentCoupling>,
) -> Result<Grid, CompositionError> {
    // Extract chrome metadata; leave `c` as the bare facets composition.
    let chrome = std::mem::take(&mut c.chrome);
    let comp_id = c.id.take();
    // Aspect on a wrapped composition has no clean semantics yet — the
    // sub-Grid spans the entire wrapping block. Silently dropped.
    c.aspect = None;
    let margin = std::mem::take(&mut c.margin);
    let padding = std::mem::take(&mut c.padding);

    // Outer wrapping grid is a single canonical 13×16 block.
    let cols = patch_block_tracks(Track::Fr(1.0), Axis::Width);
    let rows = patch_block_tracks(Track::Fr(1.0), Axis::Height);
    let mut g = Grid::new(cols, rows).id(grid_id);

    // Emit chrome slots at canonical positions of this single block.
    for ch in chrome {
        let cell_id = state.register_region(&comp_id, &ch.region)?;
        let translated = translate_patch_placement(&ch.placement, 0, 0, 1, 1);
        g.place(translated, ch.cell.id(cell_id));
    }

    // Ring sizers for margin/padding.
    emit_ring_sizers(&mut g, 0, 0, 1, 1, &margin, &padding);

    // Build the chromeless facets sub-Grid, coupled to this wrapping
    // block via back sizers (so its inner border chrome tracks bind to
    // the wrapping block's canonical chrome tracks).
    let sub_rows = c.rows;
    let sub_cols = c.cols;
    let sub_id = state.alloc_id();
    let sub_parent = ParentCoupling {
        parent_id: grid_id,
        parent_block_row: 0,
        parent_block_col: 0,
        parent_block_row_span: 1,
        parent_block_col_span: 1,
    };
    let sub = build_composition_grid(c, sub_id, state, Some(sub_parent))?;

    // Place the sub-Grid spanning the entire wrapping block — same
    // semantics as a nested composition placed in a parent's outer
    // block. Forward sizers in the wrapping block read sub-Grid tracks.
    g.place(
        Placement::at(1, 1).span(TABLE_ROWS_U16, TABLE_COLS_U16),
        sub,
    );
    emit_forward_sizers(&mut g, 0, 0, 1, 1, sub_id, sub_rows, sub_cols);

    // Back sizers when THIS wrapping block is itself nested in another
    // composition's outer block.
    if let Some(parent) = parent {
        emit_back_sizers(&mut g, &parent, 1, 1);
    }

    Ok(g)
}

/// Outer block track pattern (13 cols): Auto everywhere except the panel
/// column, which is `panel`.
fn patch_block_tracks(panel: Track, axis: Axis) -> Vec<Track> {
    let (count, panel_idx) = match axis {
        Axis::Width => (TABLE_COLS_U16, PANEL_COL),
        Axis::Height => (TABLE_ROWS_U16, PANEL_ROW),
    };
    (1..=count)
        .map(|i| {
            if i == panel_idx {
                panel.clone()
            } else {
                Track::Auto
            }
        })
        .collect()
}

fn composition_col_tracks(c: &Composition) -> Vec<Track> {
    let mut out = Vec::with_capacity(c.cols * TABLE_COLS);
    for col in 0..c.cols {
        let panel = c.widths[col].clone();
        for i in 1..=TABLE_COLS_U16 {
            out.push(if i == PANEL_COL {
                panel.clone()
            } else {
                Track::Auto
            });
        }
    }
    out
}

fn composition_row_tracks(c: &Composition) -> Vec<Track> {
    let mut out = Vec::with_capacity(c.rows * TABLE_ROWS);
    for row in 0..c.rows {
        let panel = c.heights[row].clone();
        for r in 1..=TABLE_ROWS_U16 {
            out.push(if r == PANEL_ROW {
                panel.clone()
            } else {
                Track::Auto
            });
        }
    }
    out
}

/// Emit a patch's anatomical slots, margin/padding ring sizers, and
/// optional aspect-locked panel wrap into the outer grid at block
/// `(block_row, block_col)`, spanning `block_row_span × block_col_span`
/// outer blocks.
#[allow(clippy::too_many_arguments)]
fn emit_patch_into(
    g: &mut Grid,
    patch: Patch,
    block_row: usize,
    block_col: usize,
    block_row_span: usize,
    block_col_span: usize,
    state: &mut BuildState,
    alone_in_col: bool,
    alone_in_row: bool,
) -> Result<(), CompositionError> {
    let Patch {
        id,
        placements,
        aspect,
        margin,
        padding,
    } = patch;
    for p in placements {
        let cell_id = state.register_region(&id, &p.region)?;
        let translated = translate_patch_placement(
            &p.placement,
            block_row,
            block_col,
            block_row_span,
            block_col_span,
        );
        let is_panel = p.placement.row == PANEL_ROW
            && p.placement.col == PANEL_COL
            && p.placement.row_span <= 1
            && p.placement.col_span <= 1;
        g.place(translated.clone(), p.cell.id(cell_id));
        if let (Some((aw, ah)), true) = (aspect, is_panel) {
            // Adopting R `grid`'s selective-respect path: mark the outer
            // panel cell in the respect matrix and encode the aspect
            // ratio into whichever axis is free of conflict. When the
            // patch is alone in its outer column, the column Fr can
            // carry `aw/ah` and the row Fr stays canonical (1). When
            // the patch is alone in its row but shares its column with
            // siblings, the column Fr must stay 1 (other rows want it
            // too) and the row Fr encodes the aspect as `ah/aw`. When
            // it shares both axes, neither Fr can carry the signal —
            // respect alone couples the cell. Sibling unrespected Fr
            // tracks absorb the slack.
            let panel_row_0 = (translated.row as usize).saturating_sub(1);
            let panel_col_0 = (translated.col as usize).saturating_sub(1);
            install_respect_at(g, panel_row_0, panel_col_0);
            if alone_in_col {
                let ratio = if ah.abs() < f32::EPSILON { aw } else { aw / ah };
                set_fr_if_fr(&mut g.node.cols, panel_col_0, ratio);
                if alone_in_row {
                    set_fr_if_fr(&mut g.node.rows, panel_row_0, 1.0);
                }
            } else if alone_in_row {
                let ratio = if aw.abs() < f32::EPSILON { ah } else { ah / aw };
                set_fr_if_fr(&mut g.node.rows, panel_row_0, ratio);
            }
        }
    }
    emit_ring_sizers(
        g,
        block_row,
        block_col,
        block_row_span,
        block_col_span,
        &margin,
        &padding,
    );
    Ok(())
}

/// Push a composition's `aspect = Some((aw, ah))` down to immediate
/// children that don't already carry their own. Cascading to grandchildren
/// happens naturally when each child Composition's
/// [`build_composition_grid`] runs and propagates again from its own
/// (possibly just-received) aspect. A child with its own explicit aspect
/// wins and blocks further propagation past that node.
fn propagate_aspect(placements: &mut [CompositionPlacement], aspect: (f32, f32)) {
    for p in placements.iter_mut() {
        match &mut p.element {
            Element::Patch(patch) if patch.aspect.is_none() => {
                patch.aspect = Some(aspect);
            }
            Element::Composition(child) if child.aspect.is_none() => {
                child.aspect = Some(aspect);
            }
            _ => {}
        }
    }
}

/// Mark `(row, col)` (0-based) as respected on the outer grid. Allocates
/// a matrix sized to the current grid if one doesn't exist; preserves
/// previously-marked cells. If the grid was set to `Respect::All`, this
/// call leaves it as `All` (already respects every cell).
fn install_respect_at(g: &mut Grid, row: usize, col: usize) {
    let nrows = g.node.rows.len();
    let ncols = g.node.cols.len();
    if row >= nrows || col >= ncols {
        return;
    }
    use crate::layout::Respect;
    let m = match std::mem::replace(&mut g.node.respect, Respect::None) {
        Respect::All => {
            // All respected already; nothing to do.
            g.node.respect = Respect::All;
            return;
        }
        Respect::Matrix(mut m) => {
            if m.len() < nrows {
                m.resize_with(nrows, || vec![false; ncols]);
            }
            for row_v in m.iter_mut() {
                if row_v.len() < ncols {
                    row_v.resize(ncols, false);
                }
            }
            m
        }
        Respect::None => vec![vec![false; ncols]; nrows],
    };
    let mut m = m;
    m[row][col] = true;
    g.node.respect = Respect::Matrix(m);
}

/// If `tracks[idx]` is a `Track::Fr`, replace its weight with `f`. No-op
/// for Fixed/Auto tracks (the panel sized by an explicit constraint
/// shouldn't be overridden by aspect).
fn set_fr_if_fr(tracks: &mut [Track], idx: usize, f: f32) {
    if let Some(Track::Fr(w)) = tracks.get_mut(idx) {
        *w = f;
    }
}

/// Recursive natural aspect of `c` in `(width, height)` Fr units —
/// the shape the composition would naturally take if every contained
/// aspect-locked patch got its requested ratio.
///
/// Returns `None` when any contained patch lacks an aspect lock (the
/// composition then has no determinate natural shape). When every
/// row and column resolves, the result is suitable for the same
/// alone-in-col / alone-in-row Fr propagation that
/// [`emit_patch_into`] applies to leaf patches: a 4×1 stack of
/// fixed-aspect plots can broadcast its 1 : 3.357 demand up to its
/// sibling so the outer beside divides its column Fr by that ratio
/// instead of falling back to `1 : 1`.
///
/// Per-cell axis selection mirrors `emit_patch_into`: a cell alone
/// in its column contributes to col width as `aw / ah`; a cell
/// alone in its row but sharing its column contributes to row
/// height as `ah / aw`; cells alone in both default to the col
/// axis; cells sharing both leave their tracks at the canonical 1.
fn composition_natural_aspect(c: &Composition) -> Option<(f64, f64)> {
    if c.placements.is_empty() {
        return None;
    }
    let mut col_counts = vec![0u32; c.cols];
    let mut row_counts = vec![0u32; c.rows];
    let mut aspects: Vec<(usize, usize, (f64, f64))> = Vec::with_capacity(c.placements.len());
    for p in &c.placements {
        let aspect = match &p.element {
            Element::Patch(patch) => patch.aspect.map(|(w, h)| (w as f64, h as f64))?,
            Element::Composition(inner) => composition_natural_aspect(inner)?,
        };
        let r = (p.row as usize).saturating_sub(1);
        let col = (p.col as usize).saturating_sub(1);
        if r >= c.rows || col >= c.cols {
            continue;
        }
        col_counts[col] += 1;
        row_counts[r] += 1;
        aspects.push((r, col, aspect));
    }
    let mut col_w = vec![1.0_f64; c.cols];
    let mut row_h = vec![1.0_f64; c.rows];
    for (r, col, (aw, ah)) in aspects {
        let alone_in_col = col_counts[col] == 1;
        let alone_in_row = row_counts[r] == 1;
        if alone_in_col && ah > 0.0 {
            col_w[col] = aw / ah;
        } else if alone_in_row && aw > 0.0 {
            row_h[r] = ah / aw;
        }
    }
    let total_w: f64 = col_w.iter().sum();
    let total_h: f64 = row_h.iter().sum();
    if total_w > 0.0 && total_h > 0.0 {
        Some((total_w, total_h))
    } else {
        None
    }
}

/// Emit empty sizer cells at the four margin tracks and four padding
/// tracks of the outer block at `(block_row, block_col)`. Each cell uses
/// `Inset::width` / `Inset::height` to force the corresponding Auto track
/// to size to the requested length.
fn emit_ring_sizers(
    g: &mut Grid,
    block_row: usize,
    block_col: usize,
    block_row_span: usize,
    block_col_span: usize,
    margin: &Inset,
    padding: &Inset,
) {
    let end_block_row = block_row + block_row_span - 1;
    let end_block_col = block_col + block_col_span - 1;
    // Top/bottom ring rows live in the start/end block respectively.
    let row_sizers: [(u16, usize, &Option<Length>); 4] = [
        (MARGIN_TOP_ROW, block_row, &margin.top),
        (MARGIN_BOTTOM_ROW, end_block_row, &margin.bottom),
        (PADDING_TOP_ROW, block_row, &padding.top),
        (PADDING_BOTTOM_ROW, end_block_row, &padding.bottom),
    ];
    // Left/right ring cols similarly anchor to start/end block.
    let col_sizers: [(u16, usize, &Option<Length>); 4] = [
        (MARGIN_LEFT_COL, block_col, &margin.left),
        (MARGIN_RIGHT_COL, end_block_col, &margin.right),
        (PADDING_LEFT_COL, block_col, &padding.left),
        (PADDING_RIGHT_COL, end_block_col, &padding.right),
    ];

    for (anat_row, br, length) in row_sizers {
        if let Some(l) = length {
            let row = (br * TABLE_ROWS) as u16 + anat_row;
            let col = (block_col * TABLE_COLS) as u16 + PANEL_COL;
            g.place(
                Placement::at(row, col).inset(Inset::default().height(l.clone())),
                Cell::empty(),
            );
        }
    }
    for (anat_col, bc, length) in col_sizers {
        if let Some(l) = length {
            let row = (block_row * TABLE_ROWS) as u16 + PANEL_ROW;
            let col = (bc * TABLE_COLS) as u16 + anat_col;
            g.place(
                Placement::at(row, col).inset(Inset::default().width(l.clone())),
                Cell::empty(),
            );
        }
    }
}

/// Emit forward sizers in the OUTER grid at every chrome row/col of the
/// outer block `(block_row, block_col)`, referencing the sub-Grid's
/// inner border-block chrome tracks. Each sizer is a single-span
/// `Cell::empty()` whose `inset.height` / `inset.width` is a
/// `Length::track_of(sub_id, ...)` reference — the solver's fixed-point
/// iteration over `TrackOf` makes the outer Auto track grow to the
/// sub-Grid's resolved inner-border track size.
#[allow(clippy::too_many_arguments)]
fn emit_forward_sizers(
    g: &mut Grid,
    block_row: usize,
    block_col: usize,
    block_row_span: usize,
    block_col_span: usize,
    sub_id: CellId,
    sub_rows: usize,
    sub_cols: usize,
) {
    let end_block_row = block_row + block_row_span - 1;
    let end_block_col = block_col + block_col_span - 1;
    // Top chrome rows of the start block point at the inner TOP border
    // block (inner row 1) of the sub-Grid.
    // Bottom chrome rows of the end block point at the inner BOTTOM
    // border block (inner row sub_rows).
    for anat_r in (1u16..=8).chain(10..=16) {
        let (outer_block_row, sub_inner_row): (usize, u16) = if anat_r <= 8 {
            (block_row, anat_r) // inner top block (inner row 0), anat row r
        } else {
            (end_block_row, ((sub_rows - 1) * TABLE_ROWS) as u16 + anat_r)
        };
        let outer_row = (outer_block_row * TABLE_ROWS) as u16 + anat_r;
        let outer_col = (block_col * TABLE_COLS) as u16 + PANEL_COL;
        g.place(
            Placement::at(outer_row, outer_col).inset(Inset::default().height(Length::track_of(
                sub_id,
                Axis::Height,
                sub_inner_row,
            ))),
            Cell::empty(),
        );
    }
    for anat_c in (1u16..=6).chain(8..=13) {
        let (outer_block_col, sub_inner_col): (usize, u16) = if anat_c <= 6 {
            (block_col, anat_c)
        } else {
            (end_block_col, ((sub_cols - 1) * TABLE_COLS) as u16 + anat_c)
        };
        let outer_row = (block_row * TABLE_ROWS) as u16 + PANEL_ROW;
        let outer_col = (outer_block_col * TABLE_COLS) as u16 + anat_c;
        g.place(
            Placement::at(outer_row, outer_col).inset(Inset::default().width(Length::track_of(
                sub_id,
                Axis::Width,
                sub_inner_col,
            ))),
            Cell::empty(),
        );
    }
}

/// Emit back sizers in the SUB grid at every chrome row/col of the inner
/// border blocks, each referencing the parent's outer-block chrome track.
/// The bidirectional pair (forward + back) makes the two Auto tracks
/// converge to their pointwise max under the solver's TrackOf iteration.
fn emit_back_sizers(g: &mut Grid, parent: &ParentCoupling, sub_rows: usize, sub_cols: usize) {
    let pid = parent.parent_id;
    let p_start_row = parent.parent_block_row;
    let p_end_row = parent.parent_block_row + parent.parent_block_row_span - 1;
    let p_start_col = parent.parent_block_col;
    let p_end_col = parent.parent_block_col + parent.parent_block_col_span - 1;

    // For each inner column on the top border (inner row 0), sizer at
    // (anat r, inner-col-anchor) pointing at parent's start-block row r.
    // Symmetric for bottom border.
    for inner_c in 0..sub_cols {
        for anat_r in (1u16..=8).chain(10..=16) {
            let (inner_row_block, p_row_block): (usize, usize) = if anat_r <= 8 {
                (0, p_start_row)
            } else {
                (sub_rows - 1, p_end_row)
            };
            let sub_row = (inner_row_block * TABLE_ROWS) as u16 + anat_r;
            let sub_col = (inner_c * TABLE_COLS) as u16 + PANEL_COL;
            let parent_track = (p_row_block * TABLE_ROWS) as u16 + anat_r;
            g.place(
                Placement::at(sub_row, sub_col).inset(Inset::default().height(Length::track_of(
                    pid,
                    Axis::Height,
                    parent_track,
                ))),
                Cell::empty(),
            );
        }
    }
    // Left/right border for cols.
    for inner_r in 0..sub_rows {
        for anat_c in (1u16..=6).chain(8..=13) {
            let (inner_col_block, p_col_block): (usize, usize) = if anat_c <= 6 {
                (0, p_start_col)
            } else {
                (sub_cols - 1, p_end_col)
            };
            let sub_row = (inner_r * TABLE_ROWS) as u16 + PANEL_ROW;
            let sub_col = (inner_col_block * TABLE_COLS) as u16 + anat_c;
            let parent_track = (p_col_block * TABLE_COLS) as u16 + anat_c;
            g.place(
                Placement::at(sub_row, sub_col).inset(Inset::default().width(Length::track_of(
                    pid,
                    Axis::Width,
                    parent_track,
                ))),
                Cell::empty(),
            );
        }
    }
}

/// Translate a patch-local anatomy placement into outer-grid coordinates.
/// Anatomy cols `1..=PANEL_COL` left-anchor to the start block; cols
/// `PANEL_COL+1..=TABLE_COLS` right-anchor to the end block. Same for
/// rows. The single-cell panel placement (PANEL_ROW × PANEL_COL, 1×1
/// span) stretches across all spanned outer blocks' panel cells.
fn translate_patch_placement(
    local: &Placement,
    block_row: usize,
    block_col: usize,
    block_row_span: usize,
    block_col_span: usize,
) -> Placement {
    let pr = local.row;
    let pc = local.col;
    let pcs_r = local.row_span.max(1);
    let pcs_c = local.col_span.max(1);
    let end_pr = pr + pcs_r - 1;
    let end_pc = pc + pcs_c - 1;
    let start_block_row_u16 = (block_row as u16) * TABLE_ROWS_U16;
    let end_block_row_u16 = (block_row + block_row_span - 1) as u16 * TABLE_ROWS_U16;
    let start_block_col_u16 = (block_col as u16) * TABLE_COLS_U16;
    let end_block_col_u16 = (block_col + block_col_span - 1) as u16 * TABLE_COLS_U16;

    let stretch_panel =
        pc == PANEL_COL && end_pc == PANEL_COL && pr == PANEL_ROW && end_pr == PANEL_ROW;

    let map_col = |c: u16| -> u16 {
        if c <= PANEL_COL {
            start_block_col_u16 + c
        } else {
            end_block_col_u16 + c
        }
    };
    let map_row = |r: u16| -> u16 {
        if r <= PANEL_ROW {
            start_block_row_u16 + r
        } else {
            end_block_row_u16 + r
        }
    };

    let super_col = map_col(pc);
    let super_col_end = if stretch_panel {
        end_block_col_u16 + PANEL_COL
    } else {
        map_col(end_pc)
    };
    let super_row = map_row(pr);
    let super_row_end = if stretch_panel {
        end_block_row_u16 + PANEL_ROW
    } else {
        map_row(end_pr)
    };

    Placement::at(super_row, super_col)
        .span(super_row_end - super_row + 1, super_col_end - super_col + 1)
        .inset(local.inset.clone())
}

/// Recursively walk an [`Element`] tree, returning `true` if any
/// non-anonymous patch carries `id`. Used by
/// [`Composition::contains_patch_id`].
fn element_contains_patch_id(e: &Element, id: &str) -> bool {
    match e {
        Element::Patch(p) => p.id() == Some(id),
        Element::Composition(c) => c.contains_patch_id(id),
    }
}

/// Errors produced by [`Composition::try_solve`].
#[derive(Debug, Clone)]
pub enum CompositionError {
    /// Two patches reachable from the root carry the same id.
    DuplicateId(String),
}

impl std::fmt::Display for CompositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompositionError::DuplicateId(id) => {
                write!(f, "duplicate patch id: {id:?}")
            }
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
    assert_eq!(
        cells.len(),
        rows * cols,
        "grid: cells length must equal rows * cols"
    );
    let mut c = Composition::empty(rows, cols);
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

// ─── CompositionLayout + Region ──────────────────────────────────────────────

/// Resolved layout for a [`Patch`] or [`Composition`]. Query rects by patch id
/// and anatomical region.
pub struct CompositionLayout {
    layout: Layout,
    regions: HashMap<(String, String), CellId>,
}

impl CompositionLayout {
    /// Look up the resolved rect for a `(patch_id, region)` pair. The region
    /// can be a typed [`Slot`] or a raw `&str` (e.g. for `place_at` regions).
    pub fn get(&self, patch_id: &str, region: impl Region) -> Option<Rect> {
        let key = (patch_id.to_string(), region.name().to_string());
        let id = self.regions.get(&key)?;
        self.layout.rect(*id)
    }

    /// Iterate every `(patch_id, region, rect)` triple.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, Rect)> + '_ {
        self.regions.iter().filter_map(|((id, region), cell_id)| {
            self.layout
                .rect(*cell_id)
                .map(|r| (id.as_str(), region.as_str(), r))
        })
    }

    /// Access the underlying [`Layout`] (rare — most callers want
    /// [`get`](Self::get)).
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Shift every resolved rect by `(dx, dy)` pixels. Used by
    /// the orchestrator to centre a natural-aspect composition
    /// inside an over-sized canvas.
    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.layout.translate(dx, dy);
    }
}

/// Anything that names a region for [`CompositionLayout::get`] lookups.
pub trait Region {
    /// The region's name as a `&str`. Used as the lookup key.
    fn name(&self) -> &str;
}

impl Region for Slot {
    fn name(&self) -> &str {
        Slot::name(*self)
    }
}

impl Region for &str {
    fn name(&self) -> &str {
        self
    }
}

impl Region for String {
    fn name(&self) -> &str {
        self.as_str()
    }
}

impl Region for &String {
    fn name(&self) -> &str {
        self.as_str()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Length, Measure, WidthHint};

    fn approx_eq(a: f64, b: f64, tol: f64, msg: &str) {
        assert!((a - b).abs() <= tol, "{msg}: {a} ≠ {b} (tol {tol})");
    }

    /// A fake leaf with a fixed intrinsic width and height. `width_hint`
    /// drives any containing Auto column; `height_at` drives any containing
    /// Auto row.
    struct FixedSize {
        w: f64,
        h: f64,
    }
    impl Measure for FixedSize {
        fn width_hint(&self, _dpi: f64) -> WidthHint {
            WidthHint::Min(self.w)
        }
        fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
            self.h
        }
    }

    fn sized(w: f64, h: f64) -> Cell {
        Cell::measured(FixedSize { w, h })
    }

    // ─── Single-patch tests (step 2) ────────────────────────────────────

    #[test]
    fn patch_single_panel_fills_viewport() {
        let p = Patch::new("p").slot(Slot::Panel, Cell::empty());
        let layout = p.solve(Size::new(400.0, 200.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        approx_eq(panel.x0, 0.0, 0.5, "panel x0");
        approx_eq(panel.y0, 0.0, 0.5, "panel y0");
        approx_eq(panel.x1, 400.0, 0.5, "panel x1");
        approx_eq(panel.y1, 200.0, 0.5, "panel y1");
    }

    #[test]
    fn patch_axes_consume_intrinsic_width() {
        let p = Patch::new("p")
            .slot(Slot::AxisLeft, sized(50.0, 0.0))
            .slot(Slot::Panel, Cell::empty());
        let layout = p.solve(Size::new(400.0, 200.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        let axis = layout.get("p", Slot::AxisLeft).unwrap();
        approx_eq(panel.x0, 50.0, 0.5, "panel x0 == axis width");
        approx_eq(axis.x0, 0.0, 0.5, "axis x0 at left edge");
        approx_eq(axis.x1, 50.0, 0.5, "axis x1 = 50");
    }

    #[test]
    fn patch_axes_consume_intrinsic_height() {
        let p = Patch::new("p")
            .slot(Slot::AxisBottom, sized(0.0, 30.0))
            .slot(Slot::Panel, Cell::empty());
        let layout = p.solve(Size::new(400.0, 200.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        let axis = layout.get("p", Slot::AxisBottom).unwrap();
        approx_eq(panel.y1, 170.0, 0.5, "panel ends 30 above bottom");
        approx_eq(axis.y0, 170.0, 0.5, "axis row starts at 170");
        approx_eq(axis.y1, 200.0, 0.5, "axis row ends at 200");
    }

    #[test]
    fn aspect_locks_panel_per_patch() {
        let p = Patch::new("p")
            .aspect(16.0, 9.0)
            .slot(Slot::Panel, Cell::empty());
        let layout = p.solve(Size::new(400.0, 400.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        let w = panel.x1 - panel.x0;
        let h = panel.y1 - panel.y0;
        approx_eq(w / h, 16.0 / 9.0, 0.01, "aspect ratio 16:9");
    }

    #[test]
    fn patch_aspect_lets_flex_sibling_absorb_slack() {
        // The central regression that drove the layout-level rewrite:
        // `beside(fixed.aspect(1, 1), flex)` should let flex absorb the
        // horizontal slack instead of leaving an empty square next to a
        // centred fixed plot. In 800×400 viewport, fixed's row is 400
        // (binding) → fixed panel = 400×400; flex panel absorbs the
        // remaining 400 width → 400×400.
        let fixed = Patch::new("fixed")
            .aspect(1.0, 1.0)
            .slot(Slot::Panel, Cell::empty());
        let flex = Patch::new("flex").slot(Slot::Panel, Cell::empty());
        let layout = beside(fixed, flex).solve(Size::new(800.0, 400.0), 96.0);
        let fp = layout.get("fixed", Slot::Panel).unwrap();
        let xp = layout.get("flex", Slot::Panel).unwrap();
        approx_eq(fp.x1 - fp.x0, 400.0, 0.5, "fixed panel width");
        approx_eq(fp.y1 - fp.y0, 400.0, 0.5, "fixed panel height");
        approx_eq(xp.x1 - xp.x0, 400.0, 0.5, "flex panel absorbs slack");
        approx_eq(xp.y1 - xp.y0, 400.0, 0.5, "flex panel shares row height");
    }

    #[test]
    fn composition_aspect_propagates_to_each_facet() {
        // 2×2 facet composition with title chrome and .aspect(1, 1).
        // Each facet panel ends up square. With viewport 800×600 and a
        // 40px title row, the per-facet panel area is min((800/2),
        // (600-40)/2) = min(400, 280) = 280 → each panel 280×280.
        let facet = |id: &str| Patch::new(id).slot(Slot::Panel, Cell::empty());
        let comp = beside(
            stack(facet("q1"), facet("q3")),
            stack(facet("q2"), facet("q4")),
        )
        .id("outer")
        .aspect(1.0, 1.0);
        let layout = comp.solve(Size::new(800.0, 600.0), 96.0);
        for id in &["q1", "q2", "q3", "q4"] {
            let r = layout.get(id, Slot::Panel).unwrap();
            let w = r.x1 - r.x0;
            let h = r.y1 - r.y0;
            assert!(w > 0.0, "{id} non-zero width");
            assert!(h > 0.0, "{id} non-zero height");
            approx_eq(w / h, 1.0, 0.02, &format!("{id} panel is square"));
        }
    }

    #[test]
    fn composition_aspect_does_not_override_explicit_patch_aspect() {
        // Outer .aspect(16, 9); child has its own .aspect(4, 3). The
        // explicit child aspect blocks propagation past it. Single-facet
        // composition so siblings don't compete for the shared row fr
        // (the multi-aspect-conflict case is a documented limitation
        // matching patchwork's "if one fixed aspect plot conflicts with
        // another one, one of them will end up not using the full space"
        // behaviour).
        let a = Patch::new("a")
            .aspect(4.0, 3.0)
            .slot(Slot::Panel, Cell::empty());
        let comp = Composition::empty(1, 1)
            .place(1, 1, Span::cell(), a)
            .aspect(16.0, 9.0);
        let layout = comp.solve(Size::new(800.0, 800.0), 96.0);
        let ap = layout.get("a", Slot::Panel).unwrap();
        approx_eq(
            (ap.x1 - ap.x0) / (ap.y1 - ap.y0),
            4.0 / 3.0,
            0.02,
            "a keeps its own 4:3 despite outer 16:9",
        );
    }

    #[test]
    fn composition_aspect_blocked_by_inner_aspect() {
        // Outer .aspect(16, 9) propagates to an immediate-child
        // composition WITHOUT its own aspect; that child propagates
        // further. But an inner composition with its own .aspect(1, 1)
        // wins and blocks propagation past it.
        let leaf_outer = Patch::new("outer_leaf").slot(Slot::Panel, Cell::empty());
        let leaf_inner_a = Patch::new("inner_a").slot(Slot::Panel, Cell::empty());
        let leaf_inner_b = Patch::new("inner_b").slot(Slot::Panel, Cell::empty());
        let inner = beside(leaf_inner_a, leaf_inner_b).aspect(1.0, 1.0);
        let outer = beside(leaf_outer, inner).id("outer").aspect(16.0, 9.0);
        let layout = outer.solve(Size::new(1200.0, 400.0), 96.0);
        let outer_leaf = layout.get("outer_leaf", Slot::Panel).unwrap();
        approx_eq(
            (outer_leaf.x1 - outer_leaf.x0) / (outer_leaf.y1 - outer_leaf.y0),
            16.0 / 9.0,
            0.02,
            "outer leaf receives propagated 16:9",
        );
        let ia = layout.get("inner_a", Slot::Panel).unwrap();
        let ib = layout.get("inner_b", Slot::Panel).unwrap();
        approx_eq(
            (ia.x1 - ia.x0) / (ia.y1 - ia.y0),
            1.0,
            0.02,
            "inner_a from inner .aspect(1,1)",
        );
        approx_eq(
            (ib.x1 - ib.x0) / (ib.y1 - ib.y0),
            1.0,
            0.02,
            "inner_b from inner .aspect(1,1)",
        );
    }

    #[test]
    fn composition_aspect_plus_tall_axis_grows_chrome() {
        // A composition with .aspect(1, 1) on facets that carry a tall
        // axis_bottom. The chrome row grows (forward sizer fires) AND
        // each facet panel remains square in any viewport — the
        // solver's second iteration picks up the resolved Auto-row
        // heights from iter 0's pass 2 and reshapes the respected fr
        // distribution to the actual ratio. Any slack appears as empty
        // space around the grid; chrome doesn't fight the lock.
        let facet = |id: &str| {
            Patch::new(id)
                .slot(Slot::Panel, Cell::empty())
                .slot(Slot::AxisBottom, sized(0.0, 40.0))
        };
        let comp = beside(facet("a"), facet("b")).aspect(1.0, 1.0);
        // 800w × 400h: height binds. Panel row = 400 - 40 axis = 360 per side.
        let layout = comp.solve(Size::new(800.0, 400.0), 96.0);
        for id in &["a", "b"] {
            let panel = layout.get(id, Slot::Panel).unwrap();
            let axis = layout.get(id, Slot::AxisBottom).unwrap();
            approx_eq(
                (panel.x1 - panel.x0) / (panel.y1 - panel.y0),
                1.0,
                0.02,
                &format!("{id} panel is square under chrome"),
            );
            approx_eq(axis.y1 - axis.y0, 40.0, 0.5, &format!("{id} axis 40px"));
        }
    }

    #[test]
    fn place_at_escape_hatch() {
        let p = Patch::new("p").slot(Slot::Panel, Cell::empty()).place_at(
            "overlay",
            2,
            PANEL_COL,
            Span::cols(3),
            sized(0.0, 30.0),
        );
        let layout = p.solve(Size::new(400.0, 400.0), 96.0);
        let overlay = layout.get("p", "overlay").unwrap();
        approx_eq(overlay.y1 - overlay.y0, 30.0, 0.5, "title row 30px");
    }

    #[test]
    fn slot_lookup_by_string_and_typed_slot_agree() {
        let p = Patch::new("p")
            .slot(Slot::Panel, Cell::empty())
            .slot(Slot::Title, sized(0.0, 25.0));
        let layout = p.solve(Size::new(400.0, 200.0), 96.0);
        let typed = layout.get("p", Slot::Title).unwrap();
        let stringy = layout.get("p", "title").unwrap();
        assert_eq!(typed.x0, stringy.x0);
        assert_eq!(typed.y0, stringy.y0);
        assert_eq!(typed.x1, stringy.x1);
        assert_eq!(typed.y1, stringy.y1);
    }

    #[test]
    fn missing_lookup_returns_none() {
        let p = Patch::new("p").slot(Slot::Panel, Cell::empty());
        let layout = p.solve(Size::new(400.0, 200.0), 96.0);
        assert!(layout.get("p", Slot::Title).is_none());
        assert!(layout.get("nope", Slot::Panel).is_none());
        assert!(layout.get("p", "unregistered").is_none());
    }

    // ─── Composition tests (step 3) ─────────────────────────────────────

    /// Build a patch with `panel` and the given left-axis width / bottom-axis
    /// height.
    fn axis_patch(id: &str, axis_left_w: f64, axis_bottom_h: f64) -> Patch {
        Patch::new(id)
            .slot(Slot::AxisLeft, sized(axis_left_w, 0.0))
            .slot(Slot::AxisBottom, sized(0.0, axis_bottom_h))
            .slot(Slot::Panel, Cell::empty())
    }

    #[test]
    fn beside_aligns_panels_with_different_axis_widths() {
        // p1 has a 20px y-axis, p2 has 80px. Their panels should align — both
        // start at x = max(20, 80) = 80 (since stack-wise, both block 0 and
        // block 1's AxisLeft cols merge under the same... wait — beside
        // doesn't merge cols across blocks. The headline alignment in beside
        // is the ROW (y-axis: panels share y range).
        //
        // For "panels share x0 from the left edge of each block", we need
        // each block's AxisLeft Auto col to take its own max. Block 0's
        // AxisLeft col → 20. Block 1's AxisLeft col → 80. Panels start
        // at distinct positions within their blocks.
        //
        // What does align in `beside`: the rows. Both panels share y0/y1.
        let p1 = axis_patch("p1", 20.0, 30.0);
        let p2 = axis_patch("p2", 80.0, 30.0);
        let comp = beside(p1, p2);
        let layout = comp.solve(Size::new(1000.0, 400.0), 96.0);

        let panel1 = layout.get("p1", Slot::Panel).unwrap();
        let panel2 = layout.get("p2", Slot::Panel).unwrap();
        approx_eq(panel1.y0, panel2.y0, 0.5, "panels share y0");
        approx_eq(panel1.y1, panel2.y1, 0.5, "panels share y1");
        // Block 0 panel starts after a 20px axis. Block 1 panel starts after a
        // 80px axis. Both panels have equal Fr(1) widths.
        approx_eq(panel1.x0, 20.0, 0.5, "p1.panel.x0 after 20px y-axis");
    }

    #[test]
    fn stack_aligns_panels_with_different_x_axis_heights() {
        let p1 = axis_patch("p1", 30.0, 20.0);
        let p2 = axis_patch("p2", 30.0, 80.0);
        let comp = stack(p1, p2);
        let layout = comp.solve(Size::new(400.0, 1000.0), 96.0);

        let panel1 = layout.get("p1", Slot::Panel).unwrap();
        let panel2 = layout.get("p2", Slot::Panel).unwrap();
        // In stack, the y-axes (column) merge: max(30, 30) = 30. Both panels
        // share x0/x1.
        approx_eq(panel1.x0, panel2.x0, 0.5, "panels share x0");
        approx_eq(panel1.x1, panel2.x1, 0.5, "panels share x1");
        approx_eq(panel1.x0, 30.0, 0.5, "both panels start at 30 (max axis)");
    }

    #[test]
    fn stack_y_axes_merge_to_max() {
        // y-axes in different rows but same column: AxisLeft Auto col width
        // = max(50, 100) = 100.
        let p1 = axis_patch("p1", 50.0, 0.0);
        let p2 = axis_patch("p2", 100.0, 0.0);
        let comp = stack(p1, p2);
        let layout = comp.solve(Size::new(400.0, 600.0), 96.0);
        let a1 = layout.get("p1", Slot::AxisLeft).unwrap();
        let a2 = layout.get("p2", Slot::AxisLeft).unwrap();
        approx_eq(a1.x1 - a1.x0, 100.0, 0.5, "AxisLeft col = max width");
        approx_eq(a2.x1 - a2.x0, 100.0, 0.5, "both axes occupy the merged col");
    }

    #[test]
    fn grid_2x2_aligns_per_row_and_per_column() {
        // 2x2:
        //   p1 (axis 20 wide, axis 10 tall)   p2 (axis 80 wide, axis 10 tall)
        //   p3 (axis 20 wide, axis 40 tall)   p4 (axis 80 wide, axis 40 tall)
        // p1.AxisLeft and p3.AxisLeft merge in composition col 1 → 20.
        // p2.AxisLeft and p4.AxisLeft merge in composition col 2 → 80.
        // p1.AxisBottom and p2.AxisBottom merge in composition row 1 → 10.
        // p3.AxisBottom and p4.AxisBottom merge in composition row 2 → 40.
        let p1 = axis_patch("p1", 20.0, 10.0);
        let p2 = axis_patch("p2", 80.0, 10.0);
        let p3 = axis_patch("p3", 20.0, 40.0);
        let p4 = axis_patch("p4", 80.0, 40.0);
        let comp = grid(2, 2, vec![p1.into(), p2.into(), p3.into(), p4.into()]);
        let layout = comp.solve(Size::new(800.0, 800.0), 96.0);

        let pan1 = layout.get("p1", Slot::Panel).unwrap();
        let pan2 = layout.get("p2", Slot::Panel).unwrap();
        let pan3 = layout.get("p3", Slot::Panel).unwrap();
        let pan4 = layout.get("p4", Slot::Panel).unwrap();

        // Per composition row, the panels share y range.
        approx_eq(pan1.y0, pan2.y0, 0.5, "p1/p2 panels share y0");
        approx_eq(pan3.y0, pan4.y0, 0.5, "p3/p4 panels share y0");

        // Per composition column, the panels share x range.
        approx_eq(pan1.x0, pan3.x0, 0.5, "p1/p3 panels share x0");
        approx_eq(pan2.x0, pan4.x0, 0.5, "p2/p4 panels share x0");

        // Within composition col 1: panel.x0 = 20 (the AxisLeft width).
        approx_eq(pan1.x0, 20.0, 0.5, "col 1 panels start at 20");
    }

    #[test]
    fn spacer_takes_no_chrome() {
        // A spacer next to a real plot. Spacer has no chrome → its block's
        // axis cols are all 0; both panels split the Fr space equally.
        let p1 = axis_patch("p1", 30.0, 0.0);
        let comp = beside(p1, spacer());
        let layout = comp.solve(Size::new(1000.0, 200.0), 96.0);
        let panel = layout.get("p1", Slot::Panel).unwrap();
        // Width allotted to p1's panel: (1000 - 30) / 2 = 485. (1 Fr out of 2,
        // minus the 30 left axis applied only to block 0.)
        approx_eq(panel.x1 - panel.x0, 485.0, 0.5, "panel takes 1 of 2 Fr");
    }

    #[test]
    fn wrap_aligns_at_panel_row() {
        let p1 = axis_patch("p1", 30.0, 0.0);
        let comp = beside(p1, wrap("w", sized(0.0, 0.0)));
        let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
        let p1_panel = layout.get("p1", Slot::Panel).unwrap();
        let w_panel = layout.get("w", Slot::Panel).unwrap();
        approx_eq(
            p1_panel.y0,
            w_panel.y0,
            0.5,
            "wrap panel.y0 == plot panel.y0",
        );
        approx_eq(
            p1_panel.y1,
            w_panel.y1,
            0.5,
            "wrap panel.y1 == plot panel.y1",
        );
    }

    #[test]
    fn duplicate_ids_caught() {
        let p1 = Patch::new("dup")
            .slot(Slot::Panel, Cell::empty())
            .slot(Slot::Title, sized(0.0, 20.0));
        let p2 = Patch::new("dup")
            .slot(Slot::Panel, Cell::empty())
            .slot(Slot::Title, sized(0.0, 20.0));
        let comp = beside(p1, p2);
        let result = comp.try_solve(Size::new(400.0, 200.0), 96.0);
        assert!(
            matches!(result, Err(CompositionError::DuplicateId(_))),
            "duplicate id not caught (got {})",
            if result.is_ok() { "Ok" } else { "wrong-error" }
        );
    }

    #[test]
    fn widths_relative_ratio() {
        // 2:1 panel ratio. Subtract 30+30 chrome → 740 split 2:1 → ~493 / 247.
        let p1 = axis_patch("p1", 30.0, 0.0);
        let p2 = axis_patch("p2", 30.0, 0.0);
        let comp = beside(p1, p2).widths(vec![Track::Fr(2.0), Track::Fr(1.0)]);
        let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
        let panel1 = layout.get("p1", Slot::Panel).unwrap();
        let panel2 = layout.get("p2", Slot::Panel).unwrap();
        let w1 = panel1.x1 - panel1.x0;
        let w2 = panel2.x1 - panel2.x0;
        approx_eq(w1 / w2, 2.0, 0.01, "panel width ratio 2:1");
    }

    #[test]
    fn widths_absolute() {
        let p1 = Patch::new("p1").slot(Slot::Panel, Cell::empty());
        let p2 = Patch::new("p2").slot(Slot::Panel, Cell::empty());
        let comp = beside(p1, p2).widths(vec![
            Track::Fixed(Length::px(120.0)),
            Track::Fixed(Length::px(60.0)),
        ]);
        let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
        let panel1 = layout.get("p1", Slot::Panel).unwrap();
        let panel2 = layout.get("p2", Slot::Panel).unwrap();
        approx_eq(panel1.x1 - panel1.x0, 120.0, 0.5, "p1 = 120px");
        approx_eq(panel2.x1 - panel2.x0, 60.0, 0.5, "p2 = 60px");
    }

    #[test]
    fn widths_mixed_fixed_and_fr() {
        let p1 = Patch::new("p1").slot(Slot::Panel, Cell::empty());
        let p2 = Patch::new("p2").slot(Slot::Panel, Cell::empty());
        let comp = beside(p1, p2).widths(vec![Track::Fixed(Length::px(120.0)), Track::Fr(1.0)]);
        let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
        let panel1 = layout.get("p1", Slot::Panel).unwrap();
        let panel2 = layout.get("p2", Slot::Panel).unwrap();
        approx_eq(panel1.x1 - panel1.x0, 120.0, 0.5, "p1 fixed at 120px");
        approx_eq(panel2.x1 - panel2.x0, 680.0, 0.5, "p2 absorbs the rest");
    }

    #[test]
    fn composition_place_with_col_span() {
        // p1 spans (row 1, cols 1-2), p2 in (row 2, col 1), p3 in (row 2, col 2).
        let p1 = axis_patch("p1", 0.0, 0.0);
        let p2 = axis_patch("p2", 0.0, 0.0);
        let p3 = axis_patch("p3", 0.0, 0.0);
        let comp = Composition::empty(2, 2)
            .place(1, 1, Span::cols(2), p1)
            .place(2, 1, Span::cell(), p2)
            .place(2, 2, Span::cell(), p3);
        let layout = comp.solve(Size::new(800.0, 400.0), 96.0);

        let pan1 = layout.get("p1", Slot::Panel).unwrap();
        let pan2 = layout.get("p2", Slot::Panel).unwrap();
        let pan3 = layout.get("p3", Slot::Panel).unwrap();

        // p1's panel spans from p2's panel left to p3's panel right
        // (including interior chrome between them).
        assert!(
            pan1.x0 <= pan2.x0 + 0.5 && pan1.x1 >= pan3.x1 - 0.5,
            "p1 panel spans across p2/p3 panels (pan1: {pan1:?}, pan2: {pan2:?}, pan3: {pan3:?})"
        );
        // p2 and p3 share the same y range (both in composition row 2).
        approx_eq(pan2.y0, pan3.y0, 0.5, "p2/p3 share y0");
        approx_eq(pan2.y1, pan3.y1, 0.5, "p2/p3 share y1");
    }

    // ─── Margin + padding tests ─────────────────────────────────────────

    #[test]
    fn margin_pushes_panel_inward_uniformly() {
        // 200×200 viewport with margin = 10pt (= ~13.33 px at 96 dpi).
        // No padding, no chrome → panel fills viewport minus 2*margin
        // on each axis.
        let p = Patch::new("p")
            .slot(Slot::Panel, Cell::empty())
            .margin_all(Length::pt(10.0));
        let layout = p.solve(Size::new(200.0, 200.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        let margin_px = 10.0 * 96.0 / 72.0;
        approx_eq(panel.x0, margin_px, 0.5, "panel starts after margin_left");
        approx_eq(panel.y0, margin_px, 0.5, "panel starts after margin_top");
        approx_eq(
            panel.x1,
            200.0 - margin_px,
            0.5,
            "panel ends before margin_right",
        );
        approx_eq(
            panel.y1,
            200.0 - margin_px,
            0.5,
            "panel ends before margin_bottom",
        );
    }

    #[test]
    fn padding_pushes_panel_inward_too() {
        // Padding has the same effect on chrome+panel position as margin —
        // both ring tracks contribute to pushing the panel inward.
        // Difference: bg covers padding area; bg does not cover margin
        // (verified in a separate test).
        let p = Patch::new("p")
            .slot(Slot::Panel, Cell::empty())
            .padding_all(Length::pt(6.0));
        let layout = p.solve(Size::new(200.0, 200.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        let padding_px = 6.0 * 96.0 / 72.0;
        approx_eq(panel.x0, padding_px, 0.5, "panel starts after padding_left");
        approx_eq(
            panel.x1,
            200.0 - padding_px,
            0.5,
            "panel ends before padding_right",
        );
    }

    #[test]
    fn margin_and_padding_combine() {
        // margin = 5pt, padding = 3pt → chrome offset = 8pt on each side.
        let p = Patch::new("p")
            .slot(Slot::Panel, Cell::empty())
            .margin_all(Length::pt(5.0))
            .padding_all(Length::pt(3.0));
        let layout = p.solve(Size::new(200.0, 200.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        let combined_px = (5.0 + 3.0) * 96.0 / 72.0;
        approx_eq(
            panel.x0,
            combined_px,
            0.5,
            "panel starts after margin + padding",
        );
        approx_eq(
            panel.x1,
            200.0 - combined_px,
            0.5,
            "panel ends before margin + padding",
        );
    }

    #[test]
    fn background_covers_padding_but_not_margin() {
        // With margin = 5pt, padding = 3pt, the bg should be drawn from
        // offset 5pt (margin only) to (size - 5pt), so its area covers the
        // padding ring + chrome+panel area.
        let p = Patch::new("p")
            .slot(Slot::Background, sized(0.0, 0.0)) // bg present, intrinsic 0
            .slot(Slot::Panel, Cell::empty())
            .margin_all(Length::pt(5.0))
            .padding_all(Length::pt(3.0));
        let layout = p.solve(Size::new(200.0, 200.0), 96.0);
        let bg = layout.get("p", Slot::Background).unwrap();
        let margin_px = 5.0 * 96.0 / 72.0;
        approx_eq(
            bg.x0,
            margin_px,
            0.5,
            "bg starts after margin (not padding)",
        );
        approx_eq(bg.x1, 200.0 - margin_px, 0.5, "bg ends before margin");
        approx_eq(bg.y0, margin_px, 0.5, "bg top after margin_top");
        approx_eq(
            bg.y1,
            200.0 - margin_px,
            0.5,
            "bg bottom before margin_bottom",
        );
        // Sanity: padding-sized space between bg edge and panel.
        let panel = layout.get("p", Slot::Panel).unwrap();
        let padding_px = 3.0 * 96.0 / 72.0;
        approx_eq(
            panel.x0 - bg.x0,
            padding_px,
            0.5,
            "padding-sized gap between bg.left and panel.left",
        );
    }

    #[test]
    fn adjacent_patches_have_margin_gap_between_backgrounds() {
        // Two patches side-by-side, each with margin = 4pt. The bgs should
        // be separated by 8pt (margin_a.right + margin_b.left).
        let p1 = Patch::new("p1")
            .slot(Slot::Background, sized(0.0, 0.0))
            .slot(Slot::Panel, Cell::empty())
            .margin_all(Length::pt(4.0));
        let p2 = Patch::new("p2")
            .slot(Slot::Background, sized(0.0, 0.0))
            .slot(Slot::Panel, Cell::empty())
            .margin_all(Length::pt(4.0));
        let comp = beside(p1, p2);
        let layout = comp.solve(Size::new(400.0, 200.0), 96.0);
        let bg1 = layout.get("p1", Slot::Background).unwrap();
        let bg2 = layout.get("p2", Slot::Background).unwrap();
        let margin_px = 4.0 * 96.0 / 72.0;
        approx_eq(
            bg2.x0 - bg1.x1,
            2.0 * margin_px,
            0.5,
            "gap between bgs = margin_a.right + margin_b.left",
        );
    }

    #[test]
    fn asymmetric_margin_per_side() {
        // Different margin on each side — verify each is applied independently.
        let p = Patch::new("p").slot(Slot::Panel, Cell::empty()).margin(
            Inset::default()
                .left(Length::pt(2.0))
                .right(Length::pt(8.0))
                .top(Length::pt(3.0))
                .bottom(Length::pt(6.0)),
        );
        let layout = p.solve(Size::new(200.0, 200.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        approx_eq(panel.x0, 2.0 * 96.0 / 72.0, 0.5, "left margin");
        approx_eq(panel.x1, 200.0 - 8.0 * 96.0 / 72.0, 0.5, "right margin");
        approx_eq(panel.y0, 3.0 * 96.0 / 72.0, 0.5, "top margin");
        approx_eq(panel.y1, 200.0 - 6.0 * 96.0 / 72.0, 0.5, "bottom margin");
    }

    // ─── Nesting tests ──────────────────────────────────────────────────

    #[test]
    fn composition_in_composition_cell_solves() {
        // Nesting a 1×2 inner composition directly inside a 1×1 outer's
        // single cell. With the recursive flatten this is well-defined —
        // the outer's cell footprint expands to accommodate the inner.
        let inner = beside(
            Patch::new("a").slot(Slot::Panel, Cell::empty()),
            Patch::new("b").slot(Slot::Panel, Cell::empty()),
        );
        let outer = Composition::empty(1, 1).place(1, 1, Span::cell(), inner);
        let layout = outer.solve(Size::new(400.0, 200.0), 96.0);
        let a = layout.get("a", Slot::Panel).unwrap();
        let b = layout.get("b", Slot::Panel).unwrap();
        // Two inner panels split the 400px-wide viewport evenly.
        approx_eq(a.x0, 0.0, 0.5, "a starts at left");
        approx_eq(a.x1, 200.0, 0.5, "a ends at midpoint");
        approx_eq(b.x0, 200.0, 0.5, "b starts at midpoint");
        approx_eq(b.x1, 400.0, 0.5, "b ends at right");
        // Both panels share y bounds.
        approx_eq(a.y0, b.y0, 0.5, "panels share y0");
        approx_eq(a.y1, b.y1, 0.5, "panels share y1");
    }

    #[test]
    fn nested_composition_in_composition_cell_with_axis_chrome() {
        // Outer 1×2 composition: cell (1,1) is a plain patch with a 20px
        // axis_left; cell (1,2) is a nested 1×2 composition with two inner
        // patches. The plain block's axis_left contributes 20px to its
        // outer block's axis_left col. The nested block's axis_left col
        // has no content (inner_a has no axis_left), so it stays 0.
        let plain = Patch::new("plain")
            .slot(Slot::AxisLeft, sized(20.0, 0.0))
            .slot(Slot::Panel, Cell::empty());
        let inner = beside(
            Patch::new("inner_a").slot(Slot::Panel, Cell::empty()),
            Patch::new("inner_b").slot(Slot::Panel, Cell::empty()),
        );
        let comp = beside(plain, inner);
        let layout = comp.solve(Size::new(800.0, 300.0), 96.0);

        let plain_axis = layout.get("plain", Slot::AxisLeft).unwrap();
        approx_eq(plain_axis.x1 - plain_axis.x0, 20.0, 0.5, "plain axis width");

        // Nested cell contains both inner panels side-by-side.
        let inner_a_panel = layout.get("inner_a", Slot::Panel).unwrap();
        let inner_b_panel = layout.get("inner_b", Slot::Panel).unwrap();
        approx_eq(
            inner_a_panel.y0,
            inner_b_panel.y0,
            0.5,
            "inner panels share y0",
        );
        approx_eq(
            inner_a_panel.x1,
            inner_b_panel.x0,
            0.5,
            "inner_a's right edge meets inner_b's left edge",
        );
        // Plain panel y range matches inner panels.
        let plain_panel = layout.get("plain", Slot::Panel).unwrap();
        approx_eq(
            plain_panel.y0,
            inner_a_panel.y0,
            0.5,
            "plain and inner share y0",
        );
    }

    #[test]
    fn stack_of_1x3_and_1x2_compositions() {
        // User's stated "would cause havoc" case: a 1×3 stacked over a 1×2.
        // Each row should fill its half of the viewport: row_a's 3 panels
        // tile its 200px height, row_b's 2 panels tile its 200px height.
        // Both rows should consume the full viewport width.
        let row_a = grid(
            1,
            3,
            vec![
                Patch::new("a1").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("a2").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("a3").slot(Slot::Panel, Cell::empty()).into(),
            ],
        );
        let row_b = grid(
            1,
            2,
            vec![
                Patch::new("b1").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("b2").slot(Slot::Panel, Cell::empty()).into(),
            ],
        );
        let stacked = stack(row_a, row_b);
        let layout = stacked.solve(Size::new(600.0, 400.0), 96.0);
        let a1 = layout.get("a1", Slot::Panel).unwrap();
        let a2 = layout.get("a2", Slot::Panel).unwrap();
        let a3 = layout.get("a3", Slot::Panel).unwrap();
        let b1 = layout.get("b1", Slot::Panel).unwrap();
        let b2 = layout.get("b2", Slot::Panel).unwrap();

        // row_a: 3 panels tile the 600 px row width.
        approx_eq(a1.x0, 0.0, 0.5, "a1 starts at left edge");
        approx_eq(a3.x1, 600.0, 0.5, "a3 ends at right edge");
        approx_eq(a1.x1, a2.x0, 0.5, "a1.x1 meets a2.x0");
        approx_eq(a2.x1, a3.x0, 0.5, "a2.x1 meets a3.x0");

        // row_b: 2 panels tile the 600 px row width.
        approx_eq(b1.x0, 0.0, 0.5, "b1 starts at left edge");
        approx_eq(b2.x1, 600.0, 0.5, "b2 ends at right edge");
        approx_eq(b1.x1, b2.x0, 0.5, "b1.x1 meets b2.x0");
        approx_eq(
            b2.x1 - b2.x0,
            300.0,
            0.5,
            "b2 fills its half of the row (300 px)",
        );

        // Rows are vertically separated.
        approx_eq(a1.y1, b1.y0, 0.5, "row_a's bottom meets row_b's top");
    }

    #[test]
    fn nested_composition_panels_align_with_sibling_panel() {
        // A 1×2 outer where cell (1,1) is plain and cell (1,2) is nested
        // 1×2. Both blocks share rows → all panels share y bounds.
        let plain = Patch::new("plain").slot(Slot::Panel, Cell::empty());
        let inner = beside(
            Patch::new("a").slot(Slot::Panel, Cell::empty()),
            Patch::new("b").slot(Slot::Panel, Cell::empty()),
        );
        let comp = beside(plain, inner);
        let layout = comp.solve(Size::new(600.0, 200.0), 96.0);
        let plain_panel = layout.get("plain", Slot::Panel).unwrap();
        let a_panel = layout.get("a", Slot::Panel).unwrap();
        let b_panel = layout.get("b", Slot::Panel).unwrap();
        approx_eq(plain_panel.y0, a_panel.y0, 0.5, "plain & a share y0");
        approx_eq(plain_panel.y1, a_panel.y1, 0.5, "plain & a share y1");
        approx_eq(a_panel.y0, b_panel.y0, 0.5, "a & b share y0");
    }

    // ─── Cross-grid sizer coupling tests ────────────────────────────────

    #[test]
    fn nested_sibling_grows_inner_chrome() {
        // Outer 1×2: plain patch with AxisTop=80 in cell (1,1); nested
        // 1×2 composition in cell (1,2) whose inner patches have no
        // AxisTop. The outer-grid row band 1..16 is shared across both
        // blocks; outer row 8 (AxisTop) resolves from the plain patch's
        // 80px content cell. The nested block contributes 0 (inner has
        // no axis_top); back-sizer in the sub-Grid reads outer row 8 and
        // forces sub's inner row 8 to also be 80. Both panels start at
        // y = 80 (the resolved row band height above panel).
        let plain = Patch::new("plain")
            .slot(Slot::AxisTop, sized(0.0, 80.0))
            .slot(Slot::Panel, Cell::empty());
        let inner = beside(
            Patch::new("c1").slot(Slot::Panel, Cell::empty()),
            Patch::new("c2").slot(Slot::Panel, Cell::empty()),
        );
        let comp = beside(plain, inner);
        let layout = comp.solve(Size::new(800.0, 400.0), 96.0);
        let plain_panel = layout.get("plain", Slot::Panel).unwrap();
        let c1_panel = layout.get("c1", Slot::Panel).unwrap();
        let c2_panel = layout.get("c2", Slot::Panel).unwrap();
        approx_eq(plain_panel.y0, 80.0, 0.5, "plain panel below 80px axis_top");
        approx_eq(
            c1_panel.y0,
            80.0,
            0.5,
            "c1 panel also below 80 via coupling",
        );
        approx_eq(
            c2_panel.y0,
            80.0,
            0.5,
            "c2 panel also below 80 via coupling",
        );
    }

    #[test]
    fn nested_inner_grows_sibling_chrome() {
        // Symmetric: plain patch has no AxisTop; nested inner patches do
        // (60px). The sub-Grid's inner row 8 resolves to 60 from its
        // content. The forward sizer in the outer reads 60 and grows
        // outer row 8 to 60. Plain side now starts its panel at y=60.
        let plain = Patch::new("plain").slot(Slot::Panel, Cell::empty());
        let inner = beside(
            Patch::new("c1")
                .slot(Slot::AxisTop, sized(0.0, 60.0))
                .slot(Slot::Panel, Cell::empty()),
            Patch::new("c2")
                .slot(Slot::AxisTop, sized(0.0, 60.0))
                .slot(Slot::Panel, Cell::empty()),
        );
        let comp = beside(plain, inner);
        let layout = comp.solve(Size::new(800.0, 400.0), 96.0);
        let plain_panel = layout.get("plain", Slot::Panel).unwrap();
        let c1_panel = layout.get("c1", Slot::Panel).unwrap();
        approx_eq(
            plain_panel.y0,
            60.0,
            0.5,
            "plain panel grown by inner chrome",
        );
        approx_eq(c1_panel.y0, 60.0, 0.5, "c1 panel below own axis_top");
    }

    #[test]
    fn nested_axis_left_width_propagates() {
        // Sibling plain patch has no axis_left; nested has c1 with
        // axis_left=70. Outer block col 6 (axis_left col) of the nested
        // block resolves via forward sizer to 70. The plain panel starts
        // at x=0; nested c1 panel starts at x = plain_block_total + 70
        // (start of nested block + axis_left).
        let plain = Patch::new("plain").slot(Slot::Panel, Cell::empty());
        let inner = beside(
            Patch::new("c1")
                .slot(Slot::AxisLeft, sized(70.0, 0.0))
                .slot(Slot::Panel, Cell::empty()),
            Patch::new("c2").slot(Slot::Panel, Cell::empty()),
        );
        let comp = beside(plain, inner);
        let layout = comp.solve(Size::new(800.0, 200.0), 96.0);
        let c1_axis = layout.get("c1", Slot::AxisLeft).unwrap();
        approx_eq(c1_axis.x1 - c1_axis.x0, 70.0, 0.5, "c1 axis_left = 70");
        let c1_panel = layout.get("c1", Slot::Panel).unwrap();
        approx_eq(
            c1_panel.x0 - c1_axis.x0,
            70.0,
            0.5,
            "panel sits right of axis",
        );
    }

    #[test]
    fn three_level_nesting_converges() {
        // Composition-of-composition-of-composition. Deepest inner
        // patches have non-trivial chrome (axis_top, axis_left). The
        // bidirectional sizer pair at each boundary needs ~3 iterations
        // to propagate sizes through the 3-level chain. Just verify
        // finite rects and panel alignment.
        let leaf_row = beside(
            Patch::new("l1")
                .slot(Slot::AxisTop, sized(0.0, 25.0))
                .slot(Slot::Panel, Cell::empty()),
            Patch::new("l2").slot(Slot::Panel, Cell::empty()),
        );
        let mid_row = beside(Patch::new("m1").slot(Slot::Panel, Cell::empty()), leaf_row);
        let outer = beside(Patch::new("o1").slot(Slot::Panel, Cell::empty()), mid_row);
        let layout = outer.solve(Size::new(1200.0, 400.0), 96.0);
        let l1 = layout.get("l1", Slot::Panel).unwrap();
        let l2 = layout.get("l2", Slot::Panel).unwrap();
        let m1 = layout.get("m1", Slot::Panel).unwrap();
        let o1 = layout.get("o1", Slot::Panel).unwrap();
        approx_eq(l1.y0, l2.y0, 0.5, "leaf siblings share y0");
        approx_eq(l1.y0, m1.y0, 0.5, "leaf and mid sibling share y0");
        approx_eq(l1.y0, o1.y0, 0.5, "leaf and outer sibling share y0");
        approx_eq(
            l1.y0,
            25.0,
            0.5,
            "all panels below 25px axis_top from deepest leaf",
        );
        assert!(l1.x1 - l1.x0 > 0.0, "l1 panel has positive width");
        assert!(l2.x1 - l2.x0 > 0.0, "l2 panel has positive width");
    }

    // ─── Composition-level chrome tests ─────────────────────────────────

    #[test]
    fn composition_with_title_spans_facets() {
        // A 2×3 facet composition with a composition-level Title slot.
        // The Title rect should span across all facet panels (since the
        // facets fill the panel cell of the simplified canonical block,
        // and Title at anatomical row 3 cols 3..11 stretches across the
        // composition's full plot-area width).
        let facets = grid(
            2,
            3,
            vec![
                Patch::new("f1").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("f2").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("f3").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("f4").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("f5").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("f6").slot(Slot::Panel, Cell::empty()).into(),
            ],
        )
        .id("plot")
        .slot(Slot::Title, sized(0.0, 30.0))
        .slot(Slot::Caption, sized(0.0, 15.0));
        let layout = facets.solve(Size::new(900.0, 400.0), 96.0);

        let title = layout.get("plot", Slot::Title).expect("title rect");
        let caption = layout.get("plot", Slot::Caption).expect("caption rect");
        let f1 = layout.get("f1", Slot::Panel).unwrap();
        let f3 = layout.get("f3", Slot::Panel).unwrap();
        let f4 = layout.get("f4", Slot::Panel).unwrap();
        let f6 = layout.get("f6", Slot::Panel).unwrap();

        // Title sits above all facet panels.
        assert!(
            title.y1 <= f1.y0 + 0.5,
            "title.y1 ({}) above facet panels",
            title.y1
        );
        approx_eq(title.y1 - title.y0, 30.0, 0.5, "title height = 30");
        // Title spans the full width of the panel band.
        assert!(title.x0 <= f1.x0 + 0.5, "title reaches first facet left");
        assert!(title.x1 >= f3.x1 - 0.5, "title reaches last facet right");

        // Caption sits below all facet panels.
        assert!(caption.y0 >= f4.y1 - 0.5, "caption below all facets");
        approx_eq(caption.y1 - caption.y0, 15.0, 0.5, "caption height = 15");

        // Facet rows align: f1/f2/f3 share y; f4/f5/f6 share y.
        approx_eq(f1.y0, f3.y0, 0.5, "row 1 facets share y0");
        approx_eq(f4.y0, f6.y0, 0.5, "row 2 facets share y0");
    }

    #[test]
    fn composition_chrome_axis_left_title_spans_facet_rows() {
        // A 1×2 facet composition with a left-axis-title at the
        // canonical (panel_row, axis_left_title_col) position. The
        // title sits to the left of BOTH facet panels (since they fill
        // the canonical panel cell).
        let facets = grid(
            1,
            2,
            vec![
                Patch::new("f1").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("f2").slot(Slot::Panel, Cell::empty()).into(),
            ],
        )
        .id("plot")
        .slot(Slot::AxisLeftTitle, sized(40.0, 0.0));
        let layout = facets.solve(Size::new(800.0, 200.0), 96.0);

        let axis_title = layout.get("plot", Slot::AxisLeftTitle).unwrap();
        let f1 = layout.get("f1", Slot::Panel).unwrap();
        let f2 = layout.get("f2", Slot::Panel).unwrap();
        approx_eq(
            axis_title.x1 - axis_title.x0,
            40.0,
            0.5,
            "axis title width 40",
        );
        assert!(
            axis_title.x1 <= f1.x0 + 0.5,
            "axis title sits left of facet panels"
        );
        approx_eq(f1.y0, f2.y0, 0.5, "facet panels share y0");
        // Facets together occupy the panel cell.
        assert!(f2.x1 - f1.x0 > 0.0, "facets span the panel area");
    }

    #[test]
    fn composition_chrome_nested_inside_another_composition() {
        // A wrapped composition (with chrome) placed in another
        // composition's cell behaves like a single Patch with chrome:
        // its Title aligns to outer block row 3 and propagates to the
        // sibling row via the existing Auto + sizer mechanism.
        let plain = Patch::new("plain")
            .slot(Slot::Title, sized(0.0, 60.0))
            .slot(Slot::Panel, Cell::empty());
        let facets = grid(
            1,
            2,
            vec![
                Patch::new("c1").slot(Slot::Panel, Cell::empty()).into(),
                Patch::new("c2").slot(Slot::Panel, Cell::empty()).into(),
            ],
        )
        .id("nested")
        .slot(Slot::Title, sized(0.0, 60.0));
        let comp = beside(plain, facets);
        let layout = comp.solve(Size::new(800.0, 400.0), 96.0);

        let plain_title = layout.get("plain", Slot::Title).unwrap();
        let nested_title = layout.get("nested", Slot::Title).unwrap();
        let plain_panel = layout.get("plain", Slot::Panel).unwrap();
        let c1_panel = layout.get("c1", Slot::Panel).unwrap();

        // Both titles at the same y range (shared outer-grid title row).
        approx_eq(plain_title.y0, nested_title.y0, 0.5, "titles share y0");
        approx_eq(plain_title.y1, nested_title.y1, 0.5, "titles share y1");
        approx_eq(
            plain_title.y1 - plain_title.y0,
            60.0,
            0.5,
            "title row = 60px",
        );

        // Panels share y0.
        approx_eq(
            plain_panel.y0,
            c1_panel.y0,
            0.5,
            "plain and inner panel share y0",
        );
    }
}
