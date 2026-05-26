//! High-level plot composition layout.
//!
//! Stacks on top of [`crate::layout`] to provide a patchwork-style model
//! where every plot is the same 11×14 anatomical grid (see [`anatomy::Slot`])
//! and composed plots automatically align by anatomical position.
//!
//! Construction is id-addressed: every [`Patch`] is created with a string id,
//! and resolved rects are looked up via
//! [`CompositionLayout::get(id, region)`](CompositionLayout::get) — flat
//! across any nesting depth.

pub mod anatomy;

pub use anatomy::{
    Slot, PANEL_COL, PANEL_ROW, PLOT_BOTTOM, PLOT_LEFT, PLOT_RIGHT, PLOT_TOP, TABLE_COLS,
    TABLE_ROWS,
};

use crate::geometry::{Rect, Size};
use crate::layout::{Cell, CellId, Grid, Layout, Node, Placement, Track};
use std::collections::HashMap;

const TABLE_COLS_U16: u16 = TABLE_COLS as u16;
const TABLE_ROWS_U16: u16 = TABLE_ROWS as u16;

// ─── Patch ───────────────────────────────────────────────────────────────────

/// A single plot's content laid out into the 11×14 anatomical grid.
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
    /// Content nested into this patch's panel via [`Patch::place_in_panel`].
    /// The inner element's chrome merges into this patch's chrome at the
    /// same anatomical positions; the inner's panel content fills this
    /// patch's panel area.
    inner: Option<Box<Element>>,
}

struct PatchPlacement {
    placement: Placement,
    region: String,
    cell: Cell,
}

impl Patch {
    /// Create a named patch. The id must be unique across all patches reachable
    /// from the root of a composition.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            placements: Vec::new(),
            aspect: None,
            inner: None,
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
            inner: None,
        }
    }

    /// Place content into a named anatomical [`Slot`]. The slot's region name
    /// (e.g. `"axis_left_text"`) is used in [`CompositionLayout::get`] lookups.
    pub fn slot(mut self, s: Slot, cell: Cell) -> Self {
        let (r, c, rs, cs) = s.placement();
        self.placements.push(PatchPlacement {
            placement: Placement::at(r, c).span(rs, cs),
            region: s.name().to_string(),
            cell,
        });
        self
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

    /// Nest `inner` into this patch's panel. The inner element's chrome
    /// merges with this patch's chrome at the corresponding anatomical
    /// positions (e.g. both titles land in the same super-grid title row,
    /// both left-axes land in the same left-chrome column), so size
    /// contributions take their max.
    ///
    /// For an inner [`Patch`], the result has the same `(1, 1)` block-shape
    /// as a single patch — the two patches share the outer's anatomy.
    /// For an inner [`Composition`] the result widens to the inner's
    /// block-shape; see step 5.
    pub fn place_in_panel(mut self, inner: impl Into<Element>) -> Element {
        self.inner = Some(Box::new(inner.into()));
        Element::Patch(self)
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
/// **v1 restriction**: every placed element must be a [`Patch`] (anonymous
/// spacer or named). Nesting another [`Composition`] inside a composition is
/// not supported in v1; nest via [`Patch::place_in_panel`] instead (step 5).
pub struct Composition {
    placements: Vec<CompositionPlacement>,
    cols: usize,
    rows: usize,
    widths: Vec<Track>,
    heights: Vec<Track>,
}

struct CompositionPlacement {
    /// 1-indexed top-left cell within the composition.
    row: u16,
    col: u16,
    span: Span,
    element: Element,
}

/// Either a [`Patch`] or a (nested) [`Composition`]. In v1, only `Patch`
/// elements are allowed inside a `Composition`.
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
        }
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
        self.try_solve(size, dpi).expect(
            "composition error — use try_solve to inspect (duplicate ids or unsupported nesting)",
        )
    }

    /// Like [`Self::solve`] but returns errors instead of panicking.
    pub fn try_solve(self, size: Size, dpi: f64) -> Result<CompositionLayout, CompositionError> {
        let body = self.into_root_body();
        let mut state = FlattenState::new(
            body.block_cols,
            body.block_rows,
            body.widths.clone(),
            body.heights.clone(),
        );
        body.flatten(&mut state)?;
        let (grid, regions) = state.finish();
        let layout = grid.solve(size, dpi);
        Ok(CompositionLayout { layout, regions })
    }

    /// Determine the super-grid shape and decompose the element into a
    /// flattening recipe. See [`RootBody`].
    fn into_root_body(self) -> RootBody {
        match self {
            Element::Patch(mut p) => {
                let inner = p.inner.take();
                match inner {
                    Some(boxed) => match *boxed {
                        Element::Composition(c) => {
                            // Outer patch wraps a composition — block shape
                            // comes from the inner composition.
                            RootBody {
                                block_cols: c.cols,
                                block_rows: c.rows,
                                widths: c.widths,
                                heights: c.heights,
                                kind: RootKind::OuterWithComposition {
                                    outer: p,
                                    placements: c.placements,
                                },
                            }
                        }
                        Element::Patch(inner_patch) => {
                            // Nested patch — re-attach and treat as a single
                            // (1, 1) block.
                            p.inner = Some(Box::new(Element::Patch(inner_patch)));
                            RootBody {
                                block_cols: 1,
                                block_rows: 1,
                                widths: vec![Track::Fr(1.0)],
                                heights: vec![Track::Fr(1.0)],
                                kind: RootKind::Patch(p),
                            }
                        }
                    },
                    None => RootBody {
                        block_cols: 1,
                        block_rows: 1,
                        widths: vec![Track::Fr(1.0)],
                        heights: vec![Track::Fr(1.0)],
                        kind: RootKind::Patch(p),
                    },
                }
            }
            Element::Composition(c) => RootBody {
                block_cols: c.cols,
                block_rows: c.rows,
                widths: c.widths,
                heights: c.heights,
                kind: RootKind::Composition(c.placements),
            },
        }
    }
}

/// Decomposed root element ready for flattening.
struct RootBody {
    block_cols: usize,
    block_rows: usize,
    widths: Vec<Track>,
    heights: Vec<Track>,
    kind: RootKind,
}

enum RootKind {
    /// A single patch (possibly with a nested patch via `place_in_panel`).
    /// Block shape `(1, 1)`.
    Patch(Patch),
    /// A bare composition. Each placement goes to its own composition cell.
    Composition(Vec<CompositionPlacement>),
    /// An outer patch wrapping a composition. The outer's chrome spans the
    /// full block range; each inner placement goes to its own composition
    /// cell.
    OuterWithComposition {
        outer: Patch,
        placements: Vec<CompositionPlacement>,
    },
}

impl RootBody {
    fn flatten(self, state: &mut FlattenState) -> Result<(), CompositionError> {
        let m = self.block_cols as u16;
        let n = self.block_rows as u16;
        match self.kind {
            RootKind::Patch(p) => add_patch_at(state, p, 1, 1, 1, 1),
            RootKind::Composition(placements) => flatten_composition_cells(state, placements),
            RootKind::OuterWithComposition { outer, placements } => {
                // Outer's chrome spans all blocks of the composition.
                add_patch_at(state, outer, 1, 1, n, m)?;
                flatten_composition_cells(state, placements)
            }
        }
    }
}

/// Recursively walk an [`Element`] tree, returning `true` if any
/// non-anonymous patch carries `id`. Used by
/// [`Composition::contains_patch_id`].
fn element_contains_patch_id(e: &Element, id: &str) -> bool {
    match e {
        Element::Patch(p) => {
            if p.id() == Some(id) {
                return true;
            }
            // Nested inner (via Patch::place_in_panel).
            if let Some(inner) = &p.inner {
                if element_contains_patch_id(inner, id) {
                    return true;
                }
            }
            false
        }
        Element::Composition(c) => c.contains_patch_id(id),
    }
}

fn flatten_composition_cells(
    state: &mut FlattenState,
    placements: Vec<CompositionPlacement>,
) -> Result<(), CompositionError> {
    for cp in placements {
        match cp.element {
            Element::Patch(p) => {
                // A patch with a nested Composition can only appear at the
                // root, not inside a Composition's cell.
                if let Some(boxed) = &p.inner {
                    if matches!(**boxed, Element::Composition(_)) {
                        return Err(CompositionError::UnsupportedNesting(
                            "Patch with inner Composition cannot be nested in a composition cell",
                        ));
                    }
                }
                add_patch_at(state, p, cp.row, cp.col, cp.span.rows, cp.span.cols)?;
            }
            Element::Composition(_) => {
                return Err(CompositionError::UnsupportedNesting(
                    "Composition placed directly in another Composition's cell — use Patch::place_in_panel",
                ));
            }
        }
    }
    Ok(())
}

/// Errors produced by [`Composition::try_solve`].
#[derive(Debug, Clone)]
pub enum CompositionError {
    /// Two patches reachable from the root carry the same id.
    DuplicateId(String),
    /// A nested element type that v1 does not support (e.g. a Composition
    /// directly inside another Composition's cell).
    UnsupportedNesting(&'static str),
}

impl std::fmt::Display for CompositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompositionError::DuplicateId(id) => {
                write!(f, "duplicate patch id: {id:?}")
            }
            CompositionError::UnsupportedNesting(what) => {
                write!(f, "unsupported nesting: {what}")
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

// ─── Flatten state (private) ─────────────────────────────────────────────────

/// Mutable scratchpad collecting placements and id assignments as the
/// flattener walks the element tree.
struct FlattenState {
    next_id: u64,
    regions: HashMap<(String, String), CellId>,
    emissions: Vec<(Placement, Node)>,
    block_cols: usize,
    block_rows: usize,
    widths: Vec<Track>,
    heights: Vec<Track>,
}

impl FlattenState {
    fn new(block_cols: usize, block_rows: usize, widths: Vec<Track>, heights: Vec<Track>) -> Self {
        Self {
            next_id: 1,
            regions: HashMap::new(),
            emissions: Vec::new(),
            block_cols,
            block_rows,
            widths,
            heights,
        }
    }

    fn alloc_id(&mut self) -> CellId {
        let id = CellId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Register a patch's region under its id. Returns the allocated cell id,
    /// or `Err(DuplicateId)` if the same `(id, region)` is registered twice.
    fn register_region(
        &mut self,
        patch_id: &Option<String>,
        region: &str,
    ) -> Result<CellId, CompositionError> {
        let cell_id = self.alloc_id();
        if let Some(pid) = patch_id {
            let key = (pid.clone(), region.to_string());
            if self.regions.contains_key(&key) {
                return Err(CompositionError::DuplicateId(format!("{}:{}", pid, region)));
            }
            self.regions.insert(key, cell_id);
        }
        Ok(cell_id)
    }

    /// Consume the state and produce the super-grid plus the regions map.
    fn finish(self) -> (Grid, HashMap<(String, String), CellId>) {
        let mut grid = Grid::new(
            super_cols_tracks(self.block_cols, &self.widths),
            super_rows_tracks(self.block_rows, &self.heights),
        );
        for (placement, node) in self.emissions {
            grid.place(placement, node);
        }
        (grid, self.regions)
    }
}

/// Add a patch to the super-grid at composition cell `(cr, cc)` with span
/// `(sr, sc)`. Both `cr/cc` and `sr/sc` are 1-indexed.
///
/// If the patch carries a nested `inner` (set via
/// [`Patch::place_in_panel`]), recurse into it at the same composition cell
/// — the inner's chrome merges with the outer's by sharing super-grid
/// positions.
fn add_patch_at(
    state: &mut FlattenState,
    patch: Patch,
    cr: u16,
    cc: u16,
    sr: u16,
    sc: u16,
) -> Result<(), CompositionError> {
    let Patch {
        id,
        placements,
        aspect,
        inner,
    } = patch;
    for p in placements {
        let cell_id = state.register_region(&id, &p.region)?;
        let translated = translate_placement(&p.placement, cc, cr, sc, sr);
        let panel_with_aspect = aspect.is_some() && is_panel_anatomical(&p.placement);
        if panel_with_aspect {
            let (aw, ah) = aspect.unwrap();
            let cell = p.cell.id(cell_id);
            let mut wrapped = Grid::new([Track::Fr(aw)], [Track::Fr(ah)]).respect();
            wrapped.place(Placement::at(1, 1), cell);
            state.emissions.push((translated, wrapped.into()));
        } else {
            let cell = p.cell.id(cell_id);
            state.emissions.push((translated, cell.into()));
        }
    }
    // Recurse into nested content. For inner = Patch, the inner shares the
    // outer's composition cell+span (same (1, 1) block-shape contribution).
    // For inner = Composition, the routing happens at the root (via
    // `Element::into_root_body`); here we'd see only Composition if the
    // user nested a `place_in_panel(Composition)` patch inside a Composition
    // cell, which we reject before reaching this branch in
    // `flatten_composition_cells`.
    if let Some(boxed) = inner {
        match *boxed {
            Element::Patch(inner_patch) => {
                add_patch_at(state, inner_patch, cr, cc, sr, sc)?;
            }
            Element::Composition(_) => {
                return Err(CompositionError::UnsupportedNesting(
                    "Composition in panel must be at the root (not nested in another structure)",
                ));
            }
        }
    }
    Ok(())
}

/// Translate a patch-local placement to its super-grid coordinates.
///
/// `(cc, cr)` is the patch's 1-indexed top-left composition cell, and
/// `(sc, sr)` is the composition span (how many cells the patch covers).
///
/// The panel-band rule: per-patch left chrome (cols < PANEL_COL) maps to the
/// leftmost spanned block; right chrome maps to the rightmost spanned block;
/// content at or spanning PANEL_COL spans across all spanned blocks at the
/// panel column. Same logic on the row axis.
fn translate_placement(local: &Placement, cc: u16, cr: u16, sc: u16, sr: u16) -> Placement {
    let pr = local.row;
    let pc = local.col;
    let pcs_r = local.row_span.max(1);
    let pcs_c = local.col_span.max(1);
    let end_pr = pr + pcs_r - 1;
    let end_pc = pc + pcs_c - 1;

    // Columns.
    let start_col_block = if pc <= PANEL_COL { cc - 1 } else { cc + sc - 2 };
    let end_col_block = if end_pc >= PANEL_COL {
        cc + sc - 2
    } else {
        cc - 1
    };
    let super_col = start_col_block * TABLE_COLS_U16 + pc;
    let super_col_end = end_col_block * TABLE_COLS_U16 + end_pc;
    let super_col_span = super_col_end - super_col + 1;

    // Rows.
    let start_row_block = if pr <= PANEL_ROW { cr - 1 } else { cr + sr - 2 };
    let end_row_block = if end_pr >= PANEL_ROW {
        cr + sr - 2
    } else {
        cr - 1
    };
    let super_row = start_row_block * TABLE_ROWS_U16 + pr;
    let super_row_end = end_row_block * TABLE_ROWS_U16 + end_pr;
    let super_row_span = super_row_end - super_row + 1;

    Placement::at(super_row, super_col)
        .span(super_row_span, super_col_span)
        .inset(local.inset.clone())
}

/// Build the column-track sequence for an `m`-block-wide super-grid.
fn super_cols_tracks(m: usize, widths: &[Track]) -> Vec<Track> {
    assert_eq!(widths.len(), m, "widths length must equal block-cols (m)");
    assert!(m >= 1, "super-grid needs at least one block column");
    let mut cols = Vec::with_capacity(m * TABLE_COLS);
    for panel_track in widths.iter() {
        for i in 1..=TABLE_COLS_U16 {
            cols.push(if i == PANEL_COL {
                panel_track.clone()
            } else {
                Track::Auto
            });
        }
    }
    cols
}

/// Build the row-track sequence for an `n`-block-tall super-grid.
fn super_rows_tracks(n: usize, heights: &[Track]) -> Vec<Track> {
    assert_eq!(heights.len(), n, "heights length must equal block-rows (n)");
    assert!(n >= 1, "super-grid needs at least one block row");
    let mut rows = Vec::with_capacity(n * TABLE_ROWS);
    for panel_track in heights.iter() {
        for r in 1..=TABLE_ROWS_U16 {
            rows.push(if r == PANEL_ROW {
                panel_track.clone()
            } else {
                Track::Auto
            });
        }
    }
    rows
}

fn is_panel_anatomical(p: &Placement) -> bool {
    p.row == PANEL_ROW && p.col == PANEL_COL && p.row_span <= 1 && p.col_span <= 1
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
}

/// Anything that names a region for [`CompositionLayout::get`] lookups.
pub trait Region {
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

    // ─── Step 4: nested Patch in Panel ──────────────────────────────────

    #[test]
    fn nested_patch_in_panel_inherits_outer_chrome() {
        // outer has a 30px title; inner has a 20px title. Both contribute to
        // the same super-grid title row (Auto), which resolves to max(30, 20)
        // = 30. Both layout.get(_, Title) return rects with that height.
        let inner = Patch::new("inner")
            .slot(Slot::Title, sized(0.0, 20.0))
            .slot(Slot::Panel, Cell::empty());
        let composed = Patch::new("outer")
            .slot(Slot::Title, sized(0.0, 30.0))
            .place_in_panel(inner);
        let layout = composed.solve(Size::new(400.0, 400.0), 96.0);

        let outer_title = layout.get("outer", Slot::Title).unwrap();
        let inner_title = layout.get("inner", Slot::Title).unwrap();
        // Same row → identical rects (placed in the same super-grid cell).
        approx_eq(outer_title.y0, inner_title.y0, 0.5, "title rows align");
        approx_eq(outer_title.y1, inner_title.y1, 0.5, "title rows align");
        approx_eq(
            outer_title.y1 - outer_title.y0,
            30.0,
            0.5,
            "title height = max(30, 20)",
        );
        // Both addressable.
        let outer_panel = layout.get("outer", Slot::Panel);
        let inner_panel = layout.get("inner", Slot::Panel).unwrap();
        assert!(
            outer_panel.is_none(),
            "outer has no Panel slot since it didn't set one"
        );
        // Inner's panel sits below the title row.
        approx_eq(inner_panel.y0, 30.0, 0.5, "inner panel y0 below title");
    }

    // ─── Step 5: nested Composition in Panel ────────────────────────────

    #[test]
    fn nested_composition_in_panel_widens_outer() {
        // outer.place_in_panel(beside(a, b)):
        //  - outer's Title should span across both inner panels (its rect
        //    width >= a.panel.x1 - to b.panel.x0).
        //  - outer's left chrome (AxisLeft) merges with a's left chrome at the
        //    same super-grid col.
        //  - outer's right chrome merges with b's right chrome.
        //  - a and b share y range (same panel row).
        let a = Patch::new("a")
            .slot(Slot::AxisLeft, sized(20.0, 0.0))
            .slot(Slot::Panel, Cell::empty());
        let b = Patch::new("b")
            .slot(Slot::AxisRight, sized(30.0, 0.0))
            .slot(Slot::Panel, Cell::empty());
        let composed = Patch::new("outer")
            .slot(Slot::Title, sized(0.0, 40.0))
            .slot(Slot::AxisLeftTitle, sized(50.0, 0.0))
            .place_in_panel(beside(a, b));
        let layout = composed.solve(Size::new(1000.0, 400.0), 96.0);

        let outer_title = layout.get("outer", Slot::Title).unwrap();
        let outer_axis_title = layout.get("outer", Slot::AxisLeftTitle).unwrap();
        let a_axis = layout.get("a", Slot::AxisLeft).unwrap();
        let a_panel = layout.get("a", Slot::Panel).unwrap();
        let b_panel = layout.get("b", Slot::Panel).unwrap();
        let b_axis = layout.get("b", Slot::AxisRight).unwrap();

        // Title spans across both panels.
        assert!(
            outer_title.x0 <= a_panel.x0 + 0.5,
            "outer title reaches left of a.panel"
        );
        assert!(
            outer_title.x1 >= b_panel.x1 - 0.5,
            "outer title reaches right of b.panel"
        );
        approx_eq(
            outer_title.y1 - outer_title.y0,
            40.0,
            0.5,
            "title height 40",
        );

        // Outer's AxisLeftTitle sits in the same column as where a's left
        // chrome would be — i.e. left of a's AxisLeft.
        assert!(
            outer_axis_title.x1 <= a_axis.x0 + 0.5,
            "outer.AxisLeftTitle.x1 ({}) <= a.AxisLeft.x0 ({})",
            outer_axis_title.x1,
            a_axis.x0
        );
        approx_eq(
            outer_axis_title.x1 - outer_axis_title.x0,
            50.0,
            0.5,
            "outer.AxisLeftTitle col = 50",
        );

        // Panels share y range.
        approx_eq(a_panel.y0, b_panel.y0, 0.5, "a/b share y0");
        approx_eq(a_panel.y1, b_panel.y1, 0.5, "a/b share y1");

        // b.AxisRight is to the right of b.panel — between the two panels
        // (interior chrome) or at the far right? It's on b's right which in
        // a 2-block composition is the rightmost block's right chrome → far
        // right of the whole grid.
        assert!(
            b_axis.x0 >= b_panel.x1 - 0.5,
            "b.AxisRight is to the right of b.panel"
        );
    }

    #[test]
    fn nested_composition_outer_left_chrome_merges_with_leftmost_block() {
        // outer AxisLeft = 100, a (leftmost) AxisLeft = 40 → merged to 100.
        let a = Patch::new("a")
            .slot(Slot::AxisLeft, sized(40.0, 0.0))
            .slot(Slot::Panel, Cell::empty());
        let b = Patch::new("b").slot(Slot::Panel, Cell::empty());
        let composed = Patch::new("outer")
            .slot(Slot::AxisLeft, sized(100.0, 0.0))
            .place_in_panel(beside(a, b));
        let layout = composed.solve(Size::new(1000.0, 400.0), 96.0);
        let outer_axis = layout.get("outer", Slot::AxisLeft).unwrap();
        let a_axis = layout.get("a", Slot::AxisLeft).unwrap();
        approx_eq(outer_axis.x0, a_axis.x0, 0.5, "merged x0");
        approx_eq(outer_axis.x1, a_axis.x1, 0.5, "merged x1");
        approx_eq(outer_axis.x1 - outer_axis.x0, 100.0, 0.5, "max of 100/40");
        // a's panel starts after the 100px axis col.
        let a_panel = layout.get("a", Slot::Panel).unwrap();
        approx_eq(a_panel.x0, 100.0, 0.5, "a.panel after merged 100px axis");
    }

    #[test]
    fn nested_patch_chrome_merges_per_direction() {
        // Outer's left axis 100 wide, inner's left axis 40 wide → merged
        // left chrome col = 100.
        let inner = Patch::new("inner")
            .slot(Slot::AxisLeft, sized(40.0, 0.0))
            .slot(Slot::Panel, Cell::empty());
        let composed = Patch::new("outer")
            .slot(Slot::AxisLeft, sized(100.0, 0.0))
            .place_in_panel(inner);
        let layout = composed.solve(Size::new(800.0, 400.0), 96.0);
        let inner_panel = layout.get("inner", Slot::Panel).unwrap();
        approx_eq(inner_panel.x0, 100.0, 0.5, "panel starts after max axis");
        let outer_axis = layout.get("outer", Slot::AxisLeft).unwrap();
        let inner_axis = layout.get("inner", Slot::AxisLeft).unwrap();
        approx_eq(outer_axis.x0, inner_axis.x0, 0.5, "axis cells share x0");
        approx_eq(outer_axis.x1, inner_axis.x1, 0.5, "axis cells share x1");
    }

    #[test]
    fn composition_in_composition_unsupported() {
        // Nesting a Composition directly inside a Composition is rejected
        // in v1.
        let inner = beside(
            Patch::new("a").slot(Slot::Panel, Cell::empty()),
            Patch::new("b").slot(Slot::Panel, Cell::empty()),
        );
        let outer = Composition::empty(1, 1).place(1, 1, Span::cell(), inner);
        let result = outer.try_solve(Size::new(400.0, 200.0), 96.0);
        assert!(
            matches!(result, Err(CompositionError::UnsupportedNesting(_))),
            "expected UnsupportedNesting (got {})",
            if result.is_ok() { "Ok" } else { "wrong-error" }
        );
    }
}
