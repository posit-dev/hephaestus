//! Build pipeline that lowers a [`Composition`] / [`Patch`] tree into a
//! [`Grid`] for the layout solver.
//!
//! Each composition produces a uniform `rows · TABLE_ROWS × cols ·
//! TABLE_COLS` outer grid — one canonical 13×16 anatomical block per
//! outer cell, no expansion for nested compositions. A nested
//! composition is placed as a sub-`Grid` spanning the entire outer
//! block (rows 1..16, cols 1..13). The inner composition's outer-
//! facing chrome aligns with the outer block's chrome rows/cols via
//! [`Extent::TrackOf`] sizer cells on both sides of the boundary:
//! forward sizers in the outer point at sub-Grid chrome tracks; back
//! sizers in the sub point at outer chrome tracks. The fixed-point
//! iteration over `TrackOf` references in the solver converges this
//! bidirectional coupling in two or three iterations per nesting level.

use std::collections::HashMap;

use crate::layout::{Axis, Cell, CellId, Extent, Grid, Inset, Placement, Track};

use super::anatomy::{
    MARGIN_BOTTOM_ROW, MARGIN_LEFT_COL, MARGIN_RIGHT_COL, MARGIN_TOP_ROW, PADDING_BOTTOM_ROW,
    PADDING_LEFT_COL, PADDING_RIGHT_COL, PADDING_TOP_ROW, PANEL_COL, PANEL_ROW, TABLE_COLS,
    TABLE_ROWS,
};
use super::{
    Composition, CompositionError, CompositionPlacement, Element, Patch, TABLE_COLS_U16,
    TABLE_ROWS_U16,
};

pub(super) struct BuildState {
    next_id: u64,
    /// Nested rather than keyed on a `(String, String)` tuple so a
    /// lookup borrows both halves of the key instead of allocating
    /// them — `CompositionLayout::get` runs per chrome rect per render.
    pub(super) regions: HashMap<String, HashMap<Box<str>, CellId>>,
}

impl BuildState {
    /// Fresh state with `next_id = 1` and no registered regions.
    pub(super) fn new() -> Self {
        Self {
            next_id: 1,
            regions: HashMap::new(),
        }
    }

    /// Allocate the next monotonic [`CellId`] and bump the counter.
    pub(super) fn alloc_id(&mut self) -> CellId {
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
            let by_region = self.regions.entry(pid.clone()).or_default();
            if by_region.contains_key(region) {
                return Err(CompositionError::DuplicateId(format!("{pid}:{region}")));
            }
            by_region.insert(region.into(), cell_id);
        }
        Ok(cell_id)
    }
}

/// Couples a sub-composition's inner border tracks to the parent
/// composition's outer-block chrome tracks. Carried into
/// [`build_composition_grid`] when recursing into a nested composition.
pub(super) struct ParentCoupling {
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
pub(super) fn build_single_patch(
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

pub(super) fn inset_is_zero(inset: &Inset) -> bool {
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
pub(super) fn build_composition_grid(
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
                g.place_mut(
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
                            set_fr_if_fr(&mut g.node.cols, panel_col_0, ratio);
                            if alone_in_row {
                                set_fr_if_fr(&mut g.node.rows, panel_row_0, 1.0);
                            }
                        } else if alone_in_row {
                            let ratio = if aw.abs() < f64::EPSILON { ah } else { ah / aw };
                            set_fr_if_fr(&mut g.node.rows, panel_row_0, ratio);
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
    // The caller's aspect already cascaded to the children before the
    // dispatch here; the wrapping block itself has no cell to lock,
    // since the facets sub-Grid spans all of it.
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
        g.place_mut(translated, ch.cell.id(cell_id));
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
    g.place_mut(
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
        g.place_mut(translated.clone(), p.cell.id(cell_id));
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
                let ratio = if ah.abs() < f64::EPSILON { aw } else { aw / ah };
                set_fr_if_fr(&mut g.node.cols, panel_col_0, ratio);
                if alone_in_row {
                    set_fr_if_fr(&mut g.node.rows, panel_row_0, 1.0);
                }
            } else if alone_in_row {
                let ratio = if aw.abs() < f64::EPSILON { ah } else { ah / aw };
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

/// Push a composition's `aspect = Some((aw, ah))` down to every
/// descendant that doesn't already carry its own. A child with its own
/// explicit aspect wins and blocks propagation past that node.
///
/// The walk is depth-first rather than one level at a time because the
/// aspect accounting that runs straight after it
/// ([`composition_natural_aspect`]) reports `None` for a nested
/// composition whose leaves aren't locked yet. That would leave the
/// nested block's panel track unrespected, letting it claim the whole
/// axis and stranding the slack *inside* the composition as a gap
/// between siblings.
fn propagate_aspect(placements: &mut [CompositionPlacement], aspect: (f64, f64)) {
    for p in placements.iter_mut() {
        match &mut p.element {
            Element::Patch(patch) if patch.aspect.is_none() => {
                patch.aspect = Some(aspect);
            }
            Element::Composition(child) if child.aspect.is_none() => {
                child.aspect = Some(aspect);
                propagate_aspect(&mut child.placements, aspect);
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
fn set_fr_if_fr(tracks: &mut [Track], idx: usize, f: f64) {
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
            Element::Patch(patch) => patch.aspect?,
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
    let row_sizers: [(u16, usize, &Option<Extent>); 4] = [
        (MARGIN_TOP_ROW, block_row, &margin.top),
        (MARGIN_BOTTOM_ROW, end_block_row, &margin.bottom),
        (PADDING_TOP_ROW, block_row, &padding.top),
        (PADDING_BOTTOM_ROW, end_block_row, &padding.bottom),
    ];
    // Left/right ring cols similarly anchor to start/end block.
    let col_sizers: [(u16, usize, &Option<Extent>); 4] = [
        (MARGIN_LEFT_COL, block_col, &margin.left),
        (MARGIN_RIGHT_COL, end_block_col, &margin.right),
        (PADDING_LEFT_COL, block_col, &padding.left),
        (PADDING_RIGHT_COL, end_block_col, &padding.right),
    ];

    for (anat_row, br, length) in row_sizers {
        if let Some(l) = length {
            let row = (br * TABLE_ROWS) as u16 + anat_row;
            let col = (block_col * TABLE_COLS) as u16 + PANEL_COL;
            g.place_mut(
                Placement::at(row, col).inset(Inset::default().height(l.clone())),
                Cell::empty(),
            );
        }
    }
    for (anat_col, bc, length) in col_sizers {
        if let Some(l) = length {
            let row = (block_row * TABLE_ROWS) as u16 + PANEL_ROW;
            let col = (bc * TABLE_COLS) as u16 + anat_col;
            g.place_mut(
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
/// `Extent::track_of(sub_id, ...)` reference — the solver's fixed-point
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
        g.place_mut(
            Placement::at(outer_row, outer_col).inset(Inset::default().height(Extent::track_of(
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
        g.place_mut(
            Placement::at(outer_row, outer_col).inset(Inset::default().width(Extent::track_of(
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
            g.place_mut(
                Placement::at(sub_row, sub_col).inset(Inset::default().height(Extent::track_of(
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
            g.place_mut(
                Placement::at(sub_row, sub_col).inset(Inset::default().width(Extent::track_of(
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
pub(super) fn element_contains_patch_id(e: &Element, id: &str) -> bool {
    match e {
        Element::Patch(p) => p.patch_id() == Some(id),
        Element::Composition(c) => c.contains_patch_id(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::grid as compose_grid;

    // ─── helpers ────────────────────────────────────────────────────────

    /// A grid big enough to hold `block_rows × block_cols` anatomical
    /// blocks, with nothing placed in it yet.
    fn block_grid(block_rows: usize, block_cols: usize) -> Grid {
        Grid::new(
            vec![Track::Fr(1.0); block_cols * TABLE_COLS],
            vec![Track::Fr(1.0); block_rows * TABLE_ROWS],
        )
    }

    /// The `(row, col)` of every child placed in `g`, in emission order.
    fn positions(g: &Grid) -> Vec<(u16, u16)> {
        g.node
            .children
            .iter()
            .map(|(p, _)| (p.row, p.col))
            .collect()
    }

    /// The inset of the single child placed at `(row, col)`. Panics when
    /// no child — or more than one — sits there.
    fn inset_at(g: &Grid, row: u16, col: u16) -> &Inset {
        let mut found = g
            .node
            .children
            .iter()
            .filter(|(p, _)| p.row == row && p.col == col);
        let (p, _) = found
            .next()
            .unwrap_or_else(|| panic!("no child at ({row}, {col})"));
        assert!(
            found.next().is_none(),
            "more than one child at ({row}, {col})"
        );
        &p.inset
    }

    fn patch_with_aspect(id: &str, w: f64, h: f64) -> Element {
        Patch::new(id).aspect(w, h).into()
    }

    #[track_caller]
    fn assert_aspect(got: Option<(f64, f64)>, want: (f64, f64)) {
        let (gw, gh) = got.expect("expected a determinate natural aspect");
        assert!(
            (gw - want.0).abs() < 1e-9 && (gh - want.1).abs() < 1e-9,
            "got ({gw}, {gh}), want ({}, {})",
            want.0,
            want.1
        );
    }

    // ─── composition_natural_aspect ─────────────────────────────────────

    #[test]
    fn natural_aspect_is_absent_for_a_composition_with_no_placements() {
        assert_eq!(
            composition_natural_aspect(&Composition::empty(2, 2)),
            None,
            "a composition with nothing in it has no determinate shape"
        );
    }

    #[test]
    fn natural_aspect_is_absent_when_any_patch_lacks_a_lock() {
        let c = compose_grid(
            1,
            2,
            vec![
                patch_with_aspect("locked", 2.0, 1.0),
                Patch::new("free").into(),
            ],
        );
        assert_eq!(composition_natural_aspect(&c), None);
    }

    #[test]
    fn natural_aspect_of_a_lone_locked_patch_is_its_own_ratio() {
        let c = compose_grid(1, 1, vec![patch_with_aspect("a", 3.0, 2.0)]);
        assert_aspect(composition_natural_aspect(&c), (1.5, 1.0));
    }

    #[test]
    fn natural_aspect_widens_across_a_row_of_locked_patches() {
        // Two 2:1 patches side by side read as one 4:1 block.
        let c = compose_grid(
            1,
            2,
            vec![
                patch_with_aspect("a", 2.0, 1.0),
                patch_with_aspect("b", 2.0, 1.0),
            ],
        );
        assert_aspect(composition_natural_aspect(&c), (4.0, 1.0));
    }

    #[test]
    fn natural_aspect_deepens_down_a_stack_of_locked_patches() {
        // Four 2:1 patches stacked read as 1 : 2 — each row contributes
        // half a unit of height against the shared unit of width.
        let cells: Vec<Element> = (0..4)
            .map(|i| patch_with_aspect(&format!("p{i}"), 2.0, 1.0))
            .collect();
        let c = compose_grid(4, 1, cells);
        assert_aspect(composition_natural_aspect(&c), (1.0, 2.0));
    }

    #[test]
    fn natural_aspect_leaves_tracks_canonical_when_cells_share_row_and_col() {
        // In a 2×2 grid no cell is alone on either axis, so no track can
        // carry the ratio and the composition reports the canonical
        // cell-count shape.
        let cells: Vec<Element> = (0..4)
            .map(|i| patch_with_aspect(&format!("p{i}"), 2.0, 1.0))
            .collect();
        let c = compose_grid(2, 2, cells);
        assert_aspect(composition_natural_aspect(&c), (2.0, 2.0));
    }

    #[test]
    fn natural_aspect_recurses_through_a_nested_composition() {
        // A 1:1 stack beside a 2:1 patch, each alone in its column.
        let inner = compose_grid(
            2,
            1,
            vec![
                patch_with_aspect("a", 2.0, 1.0),
                patch_with_aspect("b", 2.0, 1.0),
            ],
        );
        let outer = compose_grid(1, 2, vec![inner.into(), patch_with_aspect("c", 2.0, 1.0)]);
        assert_aspect(composition_natural_aspect(&outer), (3.0, 1.0));
    }

    #[test]
    fn natural_aspect_is_absent_when_a_nested_composition_is_unlocked() {
        let inner = compose_grid(
            2,
            1,
            vec![patch_with_aspect("a", 2.0, 1.0), Patch::new("b").into()],
        );
        let outer = compose_grid(1, 2, vec![inner.into(), patch_with_aspect("c", 2.0, 1.0)]);
        assert_eq!(composition_natural_aspect(&outer), None);
    }

    #[test]
    fn natural_aspect_is_absent_when_a_lock_has_a_zero_axis() {
        let c = compose_grid(1, 1, vec![patch_with_aspect("a", 0.0, 1.0)]);
        assert_eq!(composition_natural_aspect(&c), None);
    }

    #[test]
    fn cascading_an_aspect_makes_an_unlocked_composition_determinate() {
        // The cascade has to finish before the aspect accounting in
        // `build_composition_grid` runs: until every leaf is locked the
        // nested composition reads as shapeless and its panel track
        // would go unrespected.
        let mut inner = compose_grid(1, 2, vec![Patch::new("a").into(), Patch::new("b").into()]);
        assert_eq!(composition_natural_aspect(&inner), None);
        propagate_aspect(&mut inner.placements, (2.0, 1.0));
        assert_aspect(composition_natural_aspect(&inner), (4.0, 1.0));
    }

    #[test]
    fn cascading_an_aspect_stops_at_a_patch_that_carries_its_own() {
        let mut c = compose_grid(
            1,
            2,
            vec![
                patch_with_aspect("locked", 4.0, 1.0),
                Patch::new("free").into(),
            ],
        );
        propagate_aspect(&mut c.placements, (1.0, 1.0));
        // The locked patch keeps 4:1; the free one adopts 1:1.
        assert_aspect(composition_natural_aspect(&c), (5.0, 1.0));
    }

    // ─── ring sizers ────────────────────────────────────────────────────

    #[test]
    fn ring_sizers_land_on_their_own_anatomical_track() {
        let margin = Inset::default()
            .top(Extent::px(1.0))
            .right(Extent::px(2.0))
            .bottom(Extent::px(3.0))
            .left(Extent::px(4.0));
        let padding = Inset::default()
            .top(Extent::px(5.0))
            .right(Extent::px(6.0))
            .bottom(Extent::px(7.0))
            .left(Extent::px(8.0));
        let mut g = block_grid(1, 1);
        emit_ring_sizers(&mut g, 0, 0, 1, 1, &margin, &padding);

        assert_eq!(g.node.children.len(), 8);
        // Row tracks are driven by height, anchored on the panel column.
        for (row, px) in [
            (MARGIN_TOP_ROW, 1.0),
            (MARGIN_BOTTOM_ROW, 3.0),
            (PADDING_TOP_ROW, 5.0),
            (PADDING_BOTTOM_ROW, 7.0),
        ] {
            let inset = inset_at(&g, row, PANEL_COL);
            assert_eq!(inset.height, Some(Extent::px(px)));
            assert_eq!(inset.width, None);
        }
        // Col tracks are driven by width, anchored on the panel row.
        for (col, px) in [
            (MARGIN_LEFT_COL, 4.0),
            (MARGIN_RIGHT_COL, 2.0),
            (PADDING_LEFT_COL, 8.0),
            (PADDING_RIGHT_COL, 6.0),
        ] {
            let inset = inset_at(&g, PANEL_ROW, col);
            assert_eq!(inset.width, Some(Extent::px(px)));
            assert_eq!(inset.height, None);
        }
    }

    #[test]
    fn ring_sizers_are_emitted_only_for_the_sides_that_are_set() {
        let margin = Inset::default().left(Extent::px(4.0));
        let mut g = block_grid(1, 1);
        emit_ring_sizers(&mut g, 0, 0, 1, 1, &margin, &Inset::default());
        assert_eq!(positions(&g), vec![(PANEL_ROW, MARGIN_LEFT_COL)]);
    }

    #[test]
    fn ring_sizers_anchor_trailing_sides_to_the_end_block_of_a_span() {
        let ring = Inset::default()
            .top(Extent::px(1.0))
            .right(Extent::px(2.0))
            .bottom(Extent::px(3.0))
            .left(Extent::px(4.0));
        let mut g = block_grid(2, 3);
        emit_ring_sizers(&mut g, 0, 0, 2, 3, &ring, &Inset::default());

        // Leading sides stay in the start block; trailing sides move to
        // the last block the span covers.
        let bottom_row = TABLE_ROWS as u16 + MARGIN_BOTTOM_ROW;
        let right_col = 2 * TABLE_COLS as u16 + MARGIN_RIGHT_COL;
        assert_eq!(
            positions(&g),
            vec![
                (MARGIN_TOP_ROW, PANEL_COL),
                (bottom_row, PANEL_COL),
                (PANEL_ROW, MARGIN_LEFT_COL),
                (PANEL_ROW, right_col),
            ]
        );
        assert_eq!(
            inset_at(&g, bottom_row, PANEL_COL).height,
            Some(Extent::px(3.0))
        );
        assert_eq!(
            inset_at(&g, PANEL_ROW, right_col).width,
            Some(Extent::px(2.0))
        );
    }

    // ─── forward sizers ─────────────────────────────────────────────────

    #[test]
    fn forward_sizers_mirror_every_chrome_track_and_skip_the_panel() {
        let sub_id = CellId(42);
        let mut g = block_grid(1, 1);
        emit_forward_sizers(&mut g, 0, 0, 1, 1, sub_id, 1, 1);

        // 15 chrome rows + 12 chrome cols; the panel row and panel col
        // are the two the block sizes from its content instead.
        assert_eq!(g.node.children.len(), 27);
        for r in 1..=TABLE_ROWS as u16 {
            if r == PANEL_ROW {
                continue;
            }
            assert_eq!(
                inset_at(&g, r, PANEL_COL).height,
                Some(Extent::track_of(sub_id, Axis::Height, r))
            );
        }
        for c in 1..=TABLE_COLS as u16 {
            if c == PANEL_COL {
                continue;
            }
            assert_eq!(
                inset_at(&g, PANEL_ROW, c).width,
                Some(Extent::track_of(sub_id, Axis::Width, c))
            );
        }
        assert!(!positions(&g).contains(&(PANEL_ROW, PANEL_COL)));
    }

    #[test]
    fn forward_sizers_point_trailing_chrome_at_the_sub_grids_last_block() {
        let sub_id = CellId(7);
        let (sub_rows, sub_cols) = (2, 3);
        let mut g = block_grid(2, 2);
        emit_forward_sizers(&mut g, 0, 0, 2, 2, sub_id, sub_rows, sub_cols);

        // Bottom chrome of the end block reads the sub-grid's bottom
        // inner block, not its top one.
        let outer_row = TABLE_ROWS as u16 + MARGIN_BOTTOM_ROW;
        assert_eq!(
            inset_at(&g, outer_row, PANEL_COL).height,
            Some(Extent::track_of(
                sub_id,
                Axis::Height,
                ((sub_rows - 1) * TABLE_ROWS) as u16 + MARGIN_BOTTOM_ROW
            ))
        );
        let outer_col = TABLE_COLS as u16 + MARGIN_RIGHT_COL;
        assert_eq!(
            inset_at(&g, PANEL_ROW, outer_col).width,
            Some(Extent::track_of(
                sub_id,
                Axis::Width,
                ((sub_cols - 1) * TABLE_COLS) as u16 + MARGIN_RIGHT_COL
            ))
        );
        // Leading chrome still reads the sub-grid's first inner block.
        assert_eq!(
            inset_at(&g, MARGIN_TOP_ROW, PANEL_COL).height,
            Some(Extent::track_of(sub_id, Axis::Height, MARGIN_TOP_ROW))
        );
    }

    // ─── back sizers ────────────────────────────────────────────────────

    #[test]
    fn back_sizers_bind_every_inner_border_block_to_the_parent_chrome() {
        let parent = ParentCoupling {
            parent_id: CellId(3),
            parent_block_row: 0,
            parent_block_col: 0,
            parent_block_row_span: 1,
            parent_block_col_span: 1,
        };
        let (sub_rows, sub_cols) = (2, 2);
        let mut g = block_grid(sub_rows, sub_cols);
        emit_back_sizers(&mut g, &parent, sub_rows, sub_cols);

        // Every inner column carries the 15 chrome rows; every inner row
        // carries the 12 chrome cols.
        assert_eq!(g.node.children.len(), sub_cols * 15 + sub_rows * 12);
        // Top chrome of the first inner row block points at the parent's
        // matching chrome row.
        assert_eq!(
            inset_at(&g, MARGIN_TOP_ROW, PANEL_COL).height,
            Some(Extent::track_of(CellId(3), Axis::Height, MARGIN_TOP_ROW))
        );
        // Bottom chrome lives in the last inner row block but still
        // points at the parent's bottom chrome row.
        let sub_row = TABLE_ROWS as u16 + MARGIN_BOTTOM_ROW;
        let second_col_anchor = TABLE_COLS as u16 + PANEL_COL;
        assert_eq!(
            inset_at(&g, sub_row, second_col_anchor).height,
            Some(Extent::track_of(CellId(3), Axis::Height, MARGIN_BOTTOM_ROW))
        );
    }

    #[test]
    fn back_sizers_target_the_parent_blocks_the_span_actually_covers() {
        let parent = ParentCoupling {
            parent_id: CellId(9),
            parent_block_row: 1,
            parent_block_col: 2,
            parent_block_row_span: 2,
            parent_block_col_span: 3,
        };
        let mut g = block_grid(1, 1);
        emit_back_sizers(&mut g, &parent, 1, 1);

        // Leading chrome reads the span's start block …
        assert_eq!(
            inset_at(&g, MARGIN_TOP_ROW, PANEL_COL).height,
            Some(Extent::track_of(
                CellId(9),
                Axis::Height,
                TABLE_ROWS as u16 + MARGIN_TOP_ROW
            ))
        );
        assert_eq!(
            inset_at(&g, PANEL_ROW, MARGIN_LEFT_COL).width,
            Some(Extent::track_of(
                CellId(9),
                Axis::Width,
                2 * TABLE_COLS as u16 + MARGIN_LEFT_COL
            ))
        );
        // … and trailing chrome reads its end block.
        assert_eq!(
            inset_at(&g, MARGIN_BOTTOM_ROW, PANEL_COL).height,
            Some(Extent::track_of(
                CellId(9),
                Axis::Height,
                2 * TABLE_ROWS as u16 + MARGIN_BOTTOM_ROW
            ))
        );
        assert_eq!(
            inset_at(&g, PANEL_ROW, MARGIN_RIGHT_COL).width,
            Some(Extent::track_of(
                CellId(9),
                Axis::Width,
                4 * TABLE_COLS as u16 + MARGIN_RIGHT_COL
            ))
        );
    }

    #[test]
    fn a_single_inner_block_couples_both_borders_to_the_same_track() {
        // With one inner block the top and bottom chrome sizers collapse
        // onto the same sub-grid rows, so a 1×1 sub-grid emits exactly
        // one sizer per chrome track.
        let parent = ParentCoupling {
            parent_id: CellId(1),
            parent_block_row: 0,
            parent_block_col: 0,
            parent_block_row_span: 1,
            parent_block_col_span: 1,
        };
        let mut g = block_grid(1, 1);
        emit_back_sizers(&mut g, &parent, 1, 1);
        assert_eq!(g.node.children.len(), 27);
    }

    #[test]
    fn every_sizer_cell_covers_exactly_one_track() {
        // Every sizer is a 1×1 cell — it drives one track, and widening
        // it would let one chrome track absorb a neighbour's demand.
        let mut g = block_grid(1, 1);
        emit_forward_sizers(&mut g, 0, 0, 1, 1, CellId(1), 1, 1);
        emit_ring_sizers(
            &mut g,
            0,
            0,
            1,
            1,
            &Inset::default().top(Extent::px(1.0)),
            &Inset::default(),
        );
        assert!(g
            .node
            .children
            .iter()
            .all(|(p, _)| p.row_span == 1 && p.col_span == 1));
    }

    #[test]
    fn placing_a_span_does_not_shift_the_perpendicular_anchor() {
        // Row sizers anchor on the start block's panel column and col
        // sizers on the start block's panel row, whatever the span.
        let mut g = block_grid(2, 2);
        emit_forward_sizers(&mut g, 0, 0, 2, 2, CellId(1), 1, 1);
        for (p, _) in &g.node.children {
            if p.inset.height.is_some() {
                assert_eq!(p.col, PANEL_COL);
            } else {
                assert_eq!(p.row, PANEL_ROW);
            }
        }
    }

    #[test]
    fn set_fr_if_fr_only_rewrites_fr_tracks() {
        let mut tracks = vec![Track::Fr(1.0), Track::Auto, Track::Fixed(Extent::px(10.0))];
        set_fr_if_fr(&mut tracks, 0, 2.5);
        set_fr_if_fr(&mut tracks, 1, 2.5);
        set_fr_if_fr(&mut tracks, 2, 2.5);
        set_fr_if_fr(&mut tracks, 9, 2.5);
        assert!(matches!(tracks[0], Track::Fr(w) if (w - 2.5).abs() < 1e-9));
        assert!(matches!(tracks[1], Track::Auto));
        assert!(matches!(tracks[2], Track::Fixed(_)));
    }
}
