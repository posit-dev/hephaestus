//! Build pipeline that lowers a [`Composition`] / [`Patch`] tree into a
//! [`Grid`] for the layout solver.
//!
//! Each composition produces a uniform `rows · TABLE_ROWS × cols ·
//! TABLE_COLS` outer grid — one canonical 13×16 anatomical block per
//! outer cell, no expansion for nested compositions. A nested
//! composition is placed as a sub-`Grid` spanning the entire outer
//! block (rows 1..16, cols 1..13). The inner composition's outer-
//! facing chrome aligns with the outer block's chrome rows/cols via
//! [`Length::TrackOf`] sizer cells on both sides of the boundary:
//! forward sizers in the outer point at sub-Grid chrome tracks; back
//! sizers in the sub point at outer chrome tracks. The fixed-point
//! iteration over `TrackOf` references in the solver converges this
//! bidirectional coupling in two or three iterations per nesting level.

use std::collections::HashMap;

use crate::layout::{Axis, Cell, CellId, Grid, Inset, Length, Placement, Track};

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
    pub(super) regions: HashMap<(String, String), CellId>,
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
pub(super) fn element_contains_patch_id(e: &Element, id: &str) -> bool {
    match e {
        Element::Patch(p) => p.id() == Some(id),
        Element::Composition(c) => c.contains_patch_id(id),
    }
}
