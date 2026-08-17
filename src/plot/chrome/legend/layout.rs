//! The inner grid behind a discrete-stack legend body.
//!
//! A stack legend is a run of `(swatch, label)` pairs. Each pair
//! reports its intrinsic size through [`SwatchCellMeasure`] /
//! [`LabelMeasure`], and [`build_discrete_stack_layout`] solves them as
//! one [`Grid`] so per-row auto sizing and cross-axis uniformity come
//! from the layout engine rather than hand-summed extents. The solved
//! rects land in a [`DiscreteStackLayout`], which the legend's measure
//! carries and the draw pass offsets to the slot's anchored corner.

use crate::geometry::{Rect, Size};
use crate::layout::{Cell, CellId, Grid, Measure, Placement, Track, WidthHint};

/// Reports a swatch cell's intrinsic dimensions to the layout solver.
/// Combines the natural marker extent of every key drawn into a single
/// row with the [`KeyTheme`](crate::plot::theme::KeyTheme) cell floor —
/// the cell is the per-axis max of intrinsic content and the theme
/// minimum. Two-axis independent: a row with a tall point and a short
/// rect grows in height but stays at floor width.
pub(super) struct SwatchCellMeasure {
    /// Per-row intrinsic width (max across keys), already in pixels.
    pub(super) intrinsic_w_px: f64,
    /// Per-row intrinsic height (max across keys), already in pixels.
    pub(super) intrinsic_h_px: f64,
    /// `KeyTheme.width` resolved to pixels — floor below which the cell
    /// will not shrink even if every key is zero-sized.
    pub(super) floor_w_px: f64,
    /// `KeyTheme.height` resolved to pixels — height floor counterpart.
    pub(super) floor_h_px: f64,
}

impl Measure for SwatchCellMeasure {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(self.intrinsic_w_px.max(self.floor_w_px))
    }
    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        self.intrinsic_h_px.max(self.floor_h_px)
    }
}

/// Reports a legend label's intrinsic dimensions, sized at the text's
/// **natural** (single-line) width. Labels never wrap inside legend
/// cells — `TextRun`'s default `Measure` impl would undershoot for
/// multi-word labels because it reports the longest-unbreakable-cluster
/// bound; this wrapper substitutes `natural_width`.
pub(super) struct LabelMeasure {
    pub(super) natural_w_px: f64,
    pub(super) natural_h_px: f64,
}

impl Measure for LabelMeasure {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(self.natural_w_px)
    }
    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        self.natural_h_px
    }
}

/// Pre-solved layout for a discrete-stack legend body — the swatch +
/// label cells for every row, plus the block's natural extent. The
/// renderer translates each cached rect by the slot-anchor offset and
/// draws.
pub(super) struct DiscreteStackLayout {
    /// One `(swatch, label)` pair per non-null break, in iteration
    /// order. Rect origins are in the layout's local coordinate space
    /// (top-left of the entries block at 0, 0).
    pub(super) entries: Vec<(Rect, Rect)>,
    /// Total content width of the entries block (max x1 across rects).
    pub(super) entries_w_px: f64,
    /// Total content height of the entries block.
    pub(super) entries_h_px: f64,
}

impl DiscreteStackLayout {
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
            entries_w_px: 0.0,
            entries_h_px: 0.0,
        }
    }
}

/// Build the discrete-stack legend's inner grid, solve it, and capture
/// the resulting cell rects. The grid is one row × `(swatch / gap /
/// label) × N` cols for horizontal legends, or N rows × 3 cols for
/// vertical — the layout solver handles per-row auto sizing for free,
/// so the cross-axis dimensions match the largest row while each row's
/// along-axis dimension fits its own content.
pub(super) fn build_discrete_stack_layout(
    horizontal: bool,
    rows: Vec<(SwatchCellMeasure, LabelMeasure)>,
    swatch_label_gap_px: f64,
    row_gap_px: f64,
    dpi: f64,
) -> DiscreteStackLayout {
    if rows.is_empty() {
        return DiscreteStackLayout::empty();
    }
    let n = rows.len();
    let mut next_id: u64 = 1;
    let mut entry_ids: Vec<(CellId, CellId)> = Vec::with_capacity(n);
    let grid: Grid = if horizontal {
        let mut cols: Vec<Track> = Vec::with_capacity(n * 4);
        let mut entry_cols: Vec<(u16, u16)> = Vec::with_capacity(n);
        for i in 0..n {
            if i > 0 {
                cols.push(Track::Fixed(crate::layout::Extent::px(row_gap_px)));
            }
            cols.push(Track::Auto);
            let sw_col = cols.len() as u16;
            cols.push(Track::Fixed(crate::layout::Extent::px(swatch_label_gap_px)));
            cols.push(Track::Auto);
            let lb_col = cols.len() as u16;
            entry_cols.push((sw_col, lb_col));
        }
        let mut grid = Grid::new(cols, [Track::Auto]);
        for (i, (swatch, label)) in rows.into_iter().enumerate() {
            let (sw_col, lb_col) = entry_cols[i];
            let sw_id = CellId(next_id);
            next_id += 1;
            let lb_id = CellId(next_id);
            next_id += 1;
            grid.place_mut(Placement::at(1, sw_col), Cell::measured(swatch).id(sw_id));
            grid.place_mut(Placement::at(1, lb_col), Cell::measured(label).id(lb_id));
            entry_ids.push((sw_id, lb_id));
        }
        grid
    } else {
        let mut row_tracks: Vec<Track> = Vec::with_capacity(n * 2);
        let mut entry_rows: Vec<u16> = Vec::with_capacity(n);
        for i in 0..n {
            if i > 0 {
                row_tracks.push(Track::Fixed(crate::layout::Extent::px(row_gap_px)));
            }
            row_tracks.push(Track::Auto);
            entry_rows.push(row_tracks.len() as u16);
        }
        let cols = vec![
            Track::Auto,
            Track::Fixed(crate::layout::Extent::px(swatch_label_gap_px)),
            Track::Auto,
        ];
        let mut grid = Grid::new(cols, row_tracks);
        for (i, (swatch, label)) in rows.into_iter().enumerate() {
            let row = entry_rows[i];
            let sw_id = CellId(next_id);
            next_id += 1;
            let lb_id = CellId(next_id);
            next_id += 1;
            grid.place_mut(Placement::at(row, 1), Cell::measured(swatch).id(sw_id));
            grid.place_mut(Placement::at(row, 3), Cell::measured(label).id(lb_id));
            entry_ids.push((sw_id, lb_id));
        }
        grid
    };

    // Solve at a generous container so every Auto track sizes to its
    // content. Fr tracks would stretch, but the grid has none — every
    // track is Auto or Fixed. The solver positions auto-sized content
    // inside the container without anchoring to (0, 0), so the cell
    // rects come back offset by a constant — normalise to put the
    // entries block's top-left at the local origin.
    let solved = grid.solve(Size::new(1.0e7, 1.0e7), dpi);
    let mut raw: Vec<(Rect, Rect)> = Vec::with_capacity(n);
    let mut min_x0 = f64::INFINITY;
    let mut min_y0 = f64::INFINITY;
    let mut max_x1 = f64::NEG_INFINITY;
    let mut max_y1 = f64::NEG_INFINITY;
    for (sw_id, lb_id) in &entry_ids {
        let sw = solved.rect(*sw_id).unwrap_or(Rect::ZERO);
        let lb = solved.rect(*lb_id).unwrap_or(Rect::ZERO);
        min_x0 = min_x0.min(sw.x0).min(lb.x0);
        min_y0 = min_y0.min(sw.y0).min(lb.y0);
        max_x1 = max_x1.max(sw.x1).max(lb.x1);
        max_y1 = max_y1.max(sw.y1).max(lb.y1);
        raw.push((sw, lb));
    }
    if !min_x0.is_finite() {
        return DiscreteStackLayout::empty();
    }
    let entries: Vec<(Rect, Rect)> = raw
        .into_iter()
        .map(|(sw, lb)| {
            (
                translate_rect(sw, -min_x0, -min_y0),
                translate_rect(lb, -min_x0, -min_y0),
            )
        })
        .collect();
    DiscreteStackLayout {
        entries,
        entries_w_px: max_x1 - min_x0,
        entries_h_px: max_y1 - min_y0,
    }
}

/// Translate a rect by `(dx, dy)` pixels — the discrete stack layout
/// is solved at the legend's local origin, then offset to the slot's
/// anchored corner at draw time.
pub(super) fn translate_rect(r: Rect, dx: f64, dy: f64) -> Rect {
    Rect::new(r.x0 + dx, r.y0 + dy, r.x1 + dx, r.y1 + dy)
}
