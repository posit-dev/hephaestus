//! Width-major two-pass grid solver.
//!
//! Pass 1 walks the tree resolving every column track. Auto-column sizing
//! consults child `min_width` (recursive for grid children, `width_hint` for
//! cell children). Per-grid (col sizes + x range) are recorded in a side
//! table keyed by tree path.
//!
//! Pass 2 walks the tree again with each grid's allocated y-range; auto rows
//! now consult the **width-aware** `height_at(width)` queries on children,
//! using the widths from pass 1.
//!
//! If any cell signalled `WidthHint::NeedsHeight`, the two passes are
//! wrapped in a damped fixed-point iteration capped at `MAX_ITER` rounds.
//! Convergence is not guaranteed (rotated wrapped text genuinely oscillates);
//! the cap is a safety valve.

use std::collections::HashMap;

use super::{CellId, GridNode, Inset, Layout, Length, Node, Placement, Track, WidthHint};
use crate::geometry::{Rect, Size};

/// Maximum iterations for cells with `WidthHint::NeedsHeight`.
const MAX_ITER: usize = 5;
/// Pixel tolerance for considering a seed converged.
const EPSILON: f64 = 0.5;
/// Damping factor: new = α·proposed + (1-α)·prev. 0.5 kills the rotated-wrap
/// 2-cycle at the cost of slower geometric convergence on nice cases.
const DAMPING: f64 = 0.5;

// ─── Entry point ─────────────────────────────────────────────────────────────

pub(super) fn solve(root: &GridNode, viewport: Size, dpi: f64) -> Layout {
    let root_cell = Rect::new(0.0, 0.0, viewport.width, viewport.height);

    // Collect iterative-cell paths once. Seeds carry the current iteration's
    // estimate for each path's width; non-iterative cells aren't in the map.
    let mut seeds: HashMap<Vec<usize>, f64> = HashMap::new();
    collect_iterative_paths(root, &mut Vec::new(), dpi, &mut seeds);

    let mut widths = WidthResults::default();
    let mut heights = HeightResults::default();

    for iter in 0..MAX_ITER.max(1) {
        widths = WidthResults::default();
        width_pass_grid(
            root,
            &mut Vec::new(),
            root_cell.x0,
            root_cell.x1,
            root_cell.y0,
            root_cell.y1,
            &seeds,
            dpi,
            &mut widths,
        );

        heights = HeightResults::default();
        height_pass_grid(
            root,
            &mut Vec::new(),
            root_cell.y0,
            root_cell.y1,
            &widths,
            dpi,
            &mut heights,
        );

        if seeds.is_empty() {
            break;
        }

        let new_seeds = compute_new_seeds(root, &mut Vec::new(), &widths, &heights, &seeds, dpi);
        if converged(&seeds, &new_seeds) || iter == MAX_ITER - 1 {
            break;
        }
        for (path, new) in new_seeds {
            let prev = seeds.get(&path).copied().unwrap_or(0.0);
            seeds.insert(path, DAMPING * new + (1.0 - DAMPING) * prev);
        }
    }

    // Build the final rect map from the resolved widths and heights.
    let mut rects = HashMap::new();
    emit_rects(root, &mut Vec::new(), &widths, &heights, &mut rects);

    Layout {
        root: root_cell,
        rects,
    }
}

// ─── Side tables ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct WidthResults {
    /// Per-grid resolved column sizes + col gap + the x range the grid occupies.
    /// Path identifies which grid in the tree.
    grids: HashMap<Vec<usize>, GridWidths>,
    /// For each Cell path, the (x0, x1) range its parent's track + inset gave it.
    cell_xs: HashMap<Vec<usize>, (f64, f64)>,
}

struct GridWidths {
    cols: Vec<f64>,
    col_gap: f64,
    x0: f64,
    x1: f64,
    /// Per-fr pixel size on the width axis. Used by the height pass to
    /// re-clamp `respect()` against the height-axis per-fr.
    per_fr_w: f64,
}

#[derive(Default)]
struct HeightResults {
    grids: HashMap<Vec<usize>, GridHeights>,
    cell_heights: HashMap<Vec<usize>, (f64, f64)>, // (y0, y1)
}

struct GridHeights {
    y0: f64,
    y1: f64,
}

// ─── Pass 1: widths ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn width_pass_grid(
    node: &GridNode,
    path: &mut Vec<usize>,
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    seeds: &HashMap<Vec<usize>, f64>,
    dpi: f64,
    out: &mut WidthResults,
) {
    let avail = (x1 - x0).max(0.0);
    let avail_h = (y1 - y0).max(0.0);
    let col_gap = length_to_px(&node.gap.0, dpi, avail);
    let col_gap_total = saturating_gap_total(node.cols.len(), col_gap);

    let col_fixed = sum_fixed_track_size(&node.cols, dpi, avail);
    let col_fr_sum = sum_fr(&node.cols);
    let row_fr_sum = sum_fr(&node.rows);

    let col_auto = auto_col_sizes(node, path, seeds, dpi);
    let col_auto_total: f64 = col_auto.iter().sum();

    let free_w = (avail - col_fixed - col_auto_total - col_gap_total).max(0.0);
    let per_fr_w_default = if col_fr_sum > 0.0 {
        free_w / col_fr_sum
    } else {
        0.0
    };

    // Respect's clamp needs the height-axis per-fr too. We don't know auto
    // rows' content-driven heights yet, so we estimate per_fr_h treating auto
    // rows as 0 (the lower bound — they only consume more, never less, so
    // per_fr_h_provisional is an upper bound for per_fr_h_actual). For
    // respect grids with no auto rows this matches pass 2 exactly. With auto
    // rows the grid may end up wider than respect strictly prescribes —
    // documented as the "best effort" combination.
    let row_gap_pass1 = length_to_px(&node.gap.1, dpi, avail_h);
    let row_gap_total_pass1 = saturating_gap_total(node.rows.len(), row_gap_pass1);
    let row_fixed_pass1 = sum_fixed_track_size(&node.rows, dpi, avail_h);
    let free_h_provisional = (avail_h - row_fixed_pass1 - row_gap_total_pass1).max(0.0);
    let per_fr_h_provisional = if row_fr_sum > 0.0 {
        free_h_provisional / row_fr_sum
    } else {
        0.0
    };

    let per_fr_w = if node.respect && col_fr_sum > 0.0 && row_fr_sum > 0.0 {
        per_fr_w_default.min(per_fr_h_provisional)
    } else {
        per_fr_w_default
    };

    let col_sizes: Vec<f64> = node
        .cols
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            Track::Fixed(l) => length_to_px(l, dpi, avail),
            Track::Fr(f) => *f as f64 * per_fr_w,
            Track::Auto => col_auto[i],
        })
        .collect();

    let total_w = col_sizes.iter().sum::<f64>() + col_gap_total;
    let off_x = ((avail - total_w) * 0.5).max(0.0);
    let resolved_x0 = x0 + off_x;
    let resolved_x1 = resolved_x0 + total_w;

    out.grids.insert(
        path.clone(),
        GridWidths {
            cols: col_sizes.clone(),
            col_gap,
            x0: resolved_x0,
            x1: resolved_x1,
            per_fr_w,
        },
    );

    let _ = (per_fr_w_default, row_fr_sum, node.respect);

    // Provisional row sizes used only to derive children's y-ranges for the
    // respect clamp in nested width passes. Auto rows are treated as 0
    // (their content-driven contribution is unknown until pass 2).
    let row_sizes_provisional: Vec<f64> = node
        .rows
        .iter()
        .map(|t| match t {
            Track::Fixed(l) => length_to_px(l, dpi, avail_h),
            Track::Fr(f) => *f as f64 * per_fr_h_provisional,
            Track::Auto => 0.0,
        })
        .collect();

    for (i, (placement, child)) in node.children.iter().enumerate() {
        let (child_x0, child_x1) = child_x_range(
            &col_sizes,
            col_gap,
            resolved_x0,
            placement,
            &placement.inset,
            dpi,
        );
        let (child_y0, child_y1) = child_y_range(
            &row_sizes_provisional,
            row_gap_pass1,
            y0,
            placement,
            &placement.inset,
            dpi,
            &[],
        );
        path.push(i);
        match child {
            Node::Grid(g) => width_pass_grid(
                g, path, child_x0, child_x1, child_y0, child_y1, seeds, dpi, out,
            ),
            Node::Cell(_) => {
                out.cell_xs.insert(path.clone(), (child_x0, child_x1));
            }
        }
        path.pop();
    }
}

/// Resolve Auto column sizes for `node` by looking at its placed children.
fn auto_col_sizes(
    node: &GridNode,
    path: &mut Vec<usize>,
    seeds: &HashMap<Vec<usize>, f64>,
    dpi: f64,
) -> Vec<f64> {
    let mut out = vec![0.0; node.cols.len()];
    for (i, (placement, child)) in node.children.iter().enumerate() {
        let span = placement.col_span.max(1);
        if span != 1 {
            continue; // multi-span skipped in width pass (v1 limitation)
        }
        let col_idx = placement.col.saturating_sub(1) as usize;
        if col_idx >= node.cols.len() {
            continue;
        }
        if !matches!(node.cols[col_idx], Track::Auto) {
            continue;
        }
        path.push(i);
        let contrib = child_min_width(child, path, &placement.inset, seeds, dpi);
        path.pop();
        if contrib > out[col_idx] {
            out[col_idx] = contrib;
        }
    }
    out
}

/// Width contribution of a child to its parent's Auto col, in absolute px.
fn child_min_width(
    child: &Node,
    path: &mut Vec<usize>,
    inset: &Inset,
    seeds: &HashMap<Vec<usize>, f64>,
    dpi: f64,
) -> f64 {
    if let Some(w) = inset.width.as_ref() {
        return length_to_px_abs(w, dpi);
    }
    let l = inset
        .left
        .as_ref()
        .map_or(0.0, |v| length_to_px_abs(v, dpi));
    let r = inset
        .right
        .as_ref()
        .map_or(0.0, |v| length_to_px_abs(v, dpi));
    let inner = match child {
        Node::Grid(g) => grid_min_width(g, path, seeds, dpi),
        Node::Cell(c) => match c.measure.width_hint(dpi) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => seeds.get(path).copied().unwrap_or(seed),
        },
    };
    l + inner + r
}

/// Recursive intrinsic min width of a grid: same shape as the v1 protocol,
/// but now bottoms out at `Cell::width_hint` instead of empty leaves.
fn grid_min_width(
    g: &GridNode,
    path: &mut Vec<usize>,
    seeds: &HashMap<Vec<usize>, f64>,
    dpi: f64,
) -> f64 {
    let col_gap = length_to_px_abs(&g.gap.0, dpi);
    let col_gap_total = saturating_gap_total(g.cols.len(), col_gap);

    // Pre-fill fixed contributions; auto rows are resolved below.
    let mut col_mins = vec![0.0; g.cols.len()];
    for (i, t) in g.cols.iter().enumerate() {
        if let Track::Fixed(l) = t {
            col_mins[i] = length_to_px_abs(l, dpi);
        }
    }
    for (i, (placement, child)) in g.children.iter().enumerate() {
        if placement.col_span.max(1) != 1 {
            continue;
        }
        let col_idx = placement.col.saturating_sub(1) as usize;
        if col_idx >= g.cols.len() {
            continue;
        }
        if !matches!(g.cols[col_idx], Track::Auto) {
            continue;
        }
        path.push(i);
        let contrib = child_min_width(child, path, &placement.inset, seeds, dpi);
        path.pop();
        if contrib > col_mins[col_idx] {
            col_mins[col_idx] = contrib;
        }
    }
    col_mins.iter().sum::<f64>() + col_gap_total
}

// ─── Pass 2: heights ─────────────────────────────────────────────────────────

fn height_pass_grid(
    node: &GridNode,
    path: &mut Vec<usize>,
    y0: f64,
    y1: f64,
    widths: &WidthResults,
    dpi: f64,
    out: &mut HeightResults,
) {
    let gw = widths.grids.get(path).expect("grid widths recorded");
    let avail = (y1 - y0).max(0.0);
    let row_gap = length_to_px(&node.gap.1, dpi, avail);
    let row_gap_total = saturating_gap_total(node.rows.len(), row_gap);

    let row_fixed = sum_fixed_track_size(&node.rows, dpi, avail);
    let row_fr_sum = sum_fr(&node.rows);
    let col_fr_sum = sum_fr(&node.cols);

    let row_auto = auto_row_sizes(node, path, gw, widths, dpi);
    let row_auto_total: f64 = row_auto.iter().sum();

    let free_h = (avail - row_fixed - row_auto_total - row_gap_total).max(0.0);
    let per_fr_h_default = if row_fr_sum > 0.0 {
        free_h / row_fr_sum
    } else {
        0.0
    };

    // respect: pick the smaller of the two axes' per-fr so a single per-fr
    // applies in both directions. Auto rows have already consumed their
    // content height from `free_h`, so if content demand was larger than
    // respect's prediction the grid grows past respect (documented).
    let per_fr_h = if node.respect && col_fr_sum > 0.0 && row_fr_sum > 0.0 {
        per_fr_h_default.min(gw.per_fr_w)
    } else {
        per_fr_h_default
    };
    let _ = col_fr_sum;

    let row_sizes: Vec<f64> = node
        .rows
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            Track::Fixed(l) => length_to_px(l, dpi, avail),
            Track::Fr(f) => *f as f64 * per_fr_h,
            Track::Auto => row_auto[i],
        })
        .collect();

    let total_h = row_sizes.iter().sum::<f64>() + row_gap_total;
    let off_y = ((avail - total_h) * 0.5).max(0.0);
    let resolved_y0 = y0 + off_y;
    let resolved_y1 = resolved_y0 + total_h;

    out.grids.insert(
        path.clone(),
        GridHeights {
            y0: resolved_y0,
            y1: resolved_y1,
        },
    );

    for (i, (placement, child)) in node.children.iter().enumerate() {
        let (child_y0, child_y1) = child_y_range(
            &row_sizes,
            row_gap,
            resolved_y0,
            placement,
            &placement.inset,
            dpi,
            &row_auto,
        );
        path.push(i);
        match child {
            Node::Grid(_g) => {
                if let Node::Grid(g) = child {
                    height_pass_grid(g, path, child_y0, child_y1, widths, dpi, out);
                }
            }
            Node::Cell(_) => {
                out.cell_heights.insert(path.clone(), (child_y0, child_y1));
            }
        }
        path.pop();
    }
}

/// Resolve Auto row sizes via width-aware `height_at` queries on children.
fn auto_row_sizes(
    node: &GridNode,
    path: &mut Vec<usize>,
    gw: &GridWidths,
    widths: &WidthResults,
    dpi: f64,
) -> Vec<f64> {
    let mut out = vec![0.0; node.rows.len()];
    for (i, (placement, child)) in node.children.iter().enumerate() {
        let row_span = placement.row_span.max(1);
        let row_idx = placement.row.saturating_sub(1) as usize;
        // Pass 1's skip held for cols; for rows we DO consider multi-span
        // since widths are known. But we still only attribute to a single
        // (Auto) row — multi-row spans skip Auto attribution for simplicity.
        if row_span != 1 {
            continue;
        }
        if row_idx >= node.rows.len() {
            continue;
        }
        if !matches!(node.rows[row_idx], Track::Auto) {
            continue;
        }

        // Compute the width this child receives.
        let child_w = child_allocated_width(placement, gw, dpi);
        path.push(i);
        let contrib = child_min_height(child, path, &placement.inset, child_w, widths, dpi);
        path.pop();
        if contrib > out[row_idx] {
            out[row_idx] = contrib;
        }
    }
    out
}

fn child_min_height(
    child: &Node,
    path: &mut Vec<usize>,
    inset: &Inset,
    child_width: f64,
    widths: &WidthResults,
    dpi: f64,
) -> f64 {
    if let Some(h) = inset.height.as_ref() {
        return length_to_px_abs(h, dpi);
    }
    let t = inset.top.as_ref().map_or(0.0, |v| length_to_px_abs(v, dpi));
    let b = inset
        .bottom
        .as_ref()
        .map_or(0.0, |v| length_to_px_abs(v, dpi));
    // The inner width available to the child's content (after applying any
    // leading/trailing insets that have already shrunk child_width).
    let inner_w = child_width;
    let inner = match child {
        Node::Grid(g) => grid_height_at(g, path, inner_w, widths, dpi),
        Node::Cell(c) => c.measure.height_at(inner_w, dpi),
    };
    t + inner + b
}

/// Recursive height of a Grid given an allocated width — used by `height_at`
/// queries from the parent's auto-row resolution.
fn grid_height_at(
    g: &GridNode,
    path: &mut Vec<usize>,
    width: f64,
    widths: &WidthResults,
    dpi: f64,
) -> f64 {
    // We know this grid's resolved widths from pass 1; reuse them.
    let gw = widths.grids.get(path).expect("grid widths recorded");
    let row_gap = length_to_px_abs(&g.gap.1, dpi);
    let row_gap_total = saturating_gap_total(g.rows.len(), row_gap);

    let mut row_mins = vec![0.0; g.rows.len()];
    for (i, t) in g.rows.iter().enumerate() {
        if let Track::Fixed(l) = t {
            row_mins[i] = length_to_px_abs(l, dpi);
        }
    }
    for (i, (placement, child)) in g.children.iter().enumerate() {
        if placement.row_span.max(1) != 1 {
            continue;
        }
        let row_idx = placement.row.saturating_sub(1) as usize;
        if row_idx >= g.rows.len() {
            continue;
        }
        if !matches!(g.rows[row_idx], Track::Auto) {
            continue;
        }
        path.push(i);
        let child_w = child_allocated_width(placement, gw, dpi);
        let contrib = child_min_height(child, path, &placement.inset, child_w, widths, dpi);
        path.pop();
        if contrib > row_mins[row_idx] {
            row_mins[row_idx] = contrib;
        }
    }
    // Suppress unused param when width-driven height calculations don't need
    // the outer width directly — Fr rows would, but we treat them as 0 for
    // intrinsic-min queries (no axis context).
    let _ = width;
    row_mins.iter().sum::<f64>() + row_gap_total
}

// ─── Rect emission ───────────────────────────────────────────────────────────

fn emit_rects(
    node: &GridNode,
    path: &mut Vec<usize>,
    widths: &WidthResults,
    heights: &HeightResults,
    out: &mut HashMap<CellId, Rect>,
) {
    let gw = widths.grids.get(path).expect("grid widths recorded");
    let gh = heights.grids.get(path).expect("grid heights recorded");
    if let Some(id) = node.id {
        out.insert(id, Rect::new(gw.x0, gh.y0, gw.x1, gh.y1));
    }
    for (i, (_placement, child)) in node.children.iter().enumerate() {
        path.push(i);
        match child {
            Node::Grid(g) => emit_rects(g, path, widths, heights, out),
            Node::Cell(c) => {
                if let Some(id) = c.id {
                    let (cx0, cx1) = widths.cell_xs.get(path).copied().unwrap_or((0.0, 0.0));
                    let (cy0, cy1) = heights
                        .cell_heights
                        .get(path)
                        .copied()
                        .unwrap_or((0.0, 0.0));
                    out.insert(id, Rect::new(cx0, cy0, cx1, cy1));
                }
            }
        }
        path.pop();
    }
}

// ─── Iteration support ───────────────────────────────────────────────────────

fn collect_iterative_paths(
    node: &GridNode,
    path: &mut Vec<usize>,
    dpi: f64,
    seeds: &mut HashMap<Vec<usize>, f64>,
) {
    for (i, (_, child)) in node.children.iter().enumerate() {
        path.push(i);
        match child {
            Node::Grid(g) => collect_iterative_paths(g, path, dpi, seeds),
            Node::Cell(c) => {
                if let WidthHint::NeedsHeight { seed } = c.measure.width_hint(dpi) {
                    seeds.insert(path.clone(), seed);
                }
            }
        }
        path.pop();
    }
}

fn compute_new_seeds(
    node: &GridNode,
    path: &mut Vec<usize>,
    _widths: &WidthResults,
    heights: &HeightResults,
    _seeds: &HashMap<Vec<usize>, f64>,
    dpi: f64,
) -> HashMap<Vec<usize>, f64> {
    let mut out = HashMap::new();
    for (i, (_, child)) in node.children.iter().enumerate() {
        path.push(i);
        match child {
            Node::Grid(g) => {
                let nested = compute_new_seeds(g, path, _widths, heights, _seeds, dpi);
                for (k, v) in nested {
                    out.insert(k, v);
                }
            }
            Node::Cell(c) => {
                if matches!(c.measure.width_hint(dpi), WidthHint::NeedsHeight { .. }) {
                    let (cy0, cy1) = heights
                        .cell_heights
                        .get(path)
                        .copied()
                        .unwrap_or((0.0, 0.0));
                    let h = (cy1 - cy0).max(0.0);
                    let proposed = c.measure.width_at(h, dpi);
                    out.insert(path.clone(), proposed);
                }
            }
        }
        path.pop();
    }
    out
}

fn converged(old: &HashMap<Vec<usize>, f64>, new: &HashMap<Vec<usize>, f64>) -> bool {
    if old.len() != new.len() {
        return false;
    }
    for (k, v_new) in new {
        let v_old = old.get(k).copied().unwrap_or(f64::INFINITY);
        if (v_new - v_old).abs() > EPSILON {
            return false;
        }
    }
    true
}

// ─── Geometry helpers (lifted from previous solver) ──────────────────────────

fn sum_fixed_track_size(tracks: &[Track], dpi: f64, axis: f64) -> f64 {
    tracks
        .iter()
        .filter_map(|t| match t {
            Track::Fixed(l) => Some(length_to_px(l, dpi, axis)),
            _ => None,
        })
        .sum()
}

fn sum_fr(tracks: &[Track]) -> f64 {
    tracks
        .iter()
        .filter_map(|t| match t {
            Track::Fr(f) => Some(*f as f64),
            _ => None,
        })
        .sum()
}

fn saturating_gap_total(track_count: usize, gap: f64) -> f64 {
    if track_count <= 1 {
        0.0
    } else {
        (track_count - 1) as f64 * gap
    }
}

fn child_x_range(
    col_sizes: &[f64],
    col_gap: f64,
    grid_x0: f64,
    placement: &Placement,
    inset: &Inset,
    dpi: f64,
) -> (f64, f64) {
    let col_span = placement.col_span.max(1);
    let col_start = (placement.col.saturating_sub(1)) as usize;
    let col_end_excl = (col_start + col_span as usize).min(col_sizes.len());
    let col_start = col_start.min(col_sizes.len());

    let cell_x0 = grid_x0 + track_offset(col_sizes, col_gap, col_start);
    let cell_x1 = if col_end_excl == 0 {
        cell_x0
    } else {
        grid_x0 + track_end(col_sizes, col_gap, col_end_excl - 1)
    };
    let avail = (cell_x1 - cell_x0).max(0.0);
    resolve_axis(
        cell_x0,
        avail,
        inset.left.as_ref(),
        inset.right.as_ref(),
        inset.width.as_ref(),
        dpi,
    )
}

fn child_y_range(
    row_sizes: &[f64],
    row_gap: f64,
    grid_y0: f64,
    placement: &Placement,
    inset: &Inset,
    dpi: f64,
    _row_auto: &[f64],
) -> (f64, f64) {
    let row_span = placement.row_span.max(1);
    let row_start = (placement.row.saturating_sub(1)) as usize;
    let row_end_excl = (row_start + row_span as usize).min(row_sizes.len());
    let row_start = row_start.min(row_sizes.len());

    let cell_y0 = grid_y0 + track_offset(row_sizes, row_gap, row_start);
    let cell_y1 = if row_end_excl == 0 {
        cell_y0
    } else {
        grid_y0 + track_end(row_sizes, row_gap, row_end_excl - 1)
    };
    let avail = (cell_y1 - cell_y0).max(0.0);
    resolve_axis(
        cell_y0,
        avail,
        inset.top.as_ref(),
        inset.bottom.as_ref(),
        inset.height.as_ref(),
        dpi,
    )
}

fn child_allocated_width(placement: &Placement, gw: &GridWidths, dpi: f64) -> f64 {
    let (x0, x1) = child_x_range(
        &gw.cols,
        gw.col_gap,
        gw.x0,
        placement,
        &placement.inset,
        dpi,
    );
    (x1 - x0).max(0.0)
}

fn track_offset(sizes: &[f64], gap: f64, idx: usize) -> f64 {
    let mut acc = 0.0;
    for (i, s) in sizes.iter().enumerate() {
        if i >= idx {
            break;
        }
        acc += s + gap;
    }
    acc
}

fn track_end(sizes: &[f64], gap: f64, idx: usize) -> f64 {
    let mut acc = 0.0;
    for (i, s) in sizes.iter().enumerate() {
        if i > idx {
            break;
        }
        acc += s;
        if i < idx {
            acc += gap;
        }
    }
    acc
}

fn resolve_axis(
    origin: f64,
    avail: f64,
    leading: Option<&Length>,
    trailing: Option<&Length>,
    size: Option<&Length>,
    dpi: f64,
) -> (f64, f64) {
    let l = leading.map_or(0.0, |v| length_to_px(v, dpi, avail));
    let t = trailing.map_or(0.0, |v| length_to_px(v, dpi, avail));

    match size {
        None => {
            let start = origin + l;
            let end = (origin + avail - t).max(start);
            (start, end)
        }
        Some(w) => {
            let w_px = length_to_px(w, dpi, avail);
            match (leading.is_some(), trailing.is_some()) {
                (true, _) => (origin + l, origin + l + w_px),
                (false, true) => {
                    let end = origin + avail - t;
                    (end - w_px, end)
                }
                (false, false) => (origin, origin + w_px),
            }
        }
    }
}

fn length_to_px(l: &Length, dpi: f64, axis_size: f64) -> f64 {
    match l {
        Length::Sum {
            px,
            inches,
            percent,
        } => px + inches * dpi + percent * axis_size,
        Length::Min(a, b) => length_to_px(a, dpi, axis_size).min(length_to_px(b, dpi, axis_size)),
        Length::Max(a, b) => length_to_px(a, dpi, axis_size).max(length_to_px(b, dpi, axis_size)),
    }
}

fn length_to_px_abs(l: &Length, dpi: f64) -> f64 {
    match l {
        Length::Sum { px, inches, .. } => px + inches * dpi,
        Length::Min(a, b) => length_to_px_abs(a, dpi).min(length_to_px_abs(b, dpi)),
        Length::Max(a, b) => length_to_px_abs(a, dpi).max(length_to_px_abs(b, dpi)),
    }
}
