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
//! Both passes resolve [`Length::TrackOf`] references against the previous
//! iteration's results — on iteration 0 the reference evaluates to 0, on
//! later iterations it returns the cumulative size of the named tracks
//! from the previous pass. Combined with the existing fixed-point loop
//! (which already exists for [`WidthHint::NeedsHeight`] cells), this
//! converges in 1–2 iterations for forward references and tolerates
//! mild cycles up to `MAX_ITER`.
//!
//! If any cell signalled `WidthHint::NeedsHeight` or any Length::TrackOf
//! reference is in the tree, the two passes are wrapped in a damped
//! fixed-point iteration capped at `MAX_ITER` rounds. Convergence is not
//! guaranteed (rotated wrapped text genuinely oscillates); the cap is a
//! safety valve.

use std::collections::HashMap;

use super::{Axis, CellId, GridNode, Inset, Layout, Length, Node, Placement, Track, WidthHint};
use crate::geometry::{Rect, Size};

/// Maximum iterations for cells with `WidthHint::NeedsHeight`.
const MAX_ITER: usize = 5;
/// Pixel tolerance for considering a seed converged.
const EPSILON: f64 = 0.5;
/// Damping factor: new = α·proposed + (1-α)·prev. 0.5 kills the rotated-wrap
/// 2-cycle at the cost of slower geometric convergence on nice cases.
const DAMPING: f64 = 0.5;

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Solve `root` against a `viewport`-sized cell at `dpi`. Runs the
/// width-major two-pass solver, wrapped in a damped fixed-point
/// iteration when any cell signals [`WidthHint::NeedsHeight`] or any
/// [`Length::TrackOf`] reference exists in the tree. See the module
/// docs for the convergence properties.
pub(super) fn solve(root: &GridNode, viewport: Size, dpi: f64) -> Layout {
    let root_cell = Rect::new(0.0, 0.0, viewport.width, viewport.height);

    // Collect iterative-cell paths once. Seeds carry the current iteration's
    // estimate for each path's width; non-iterative cells aren't in the map.
    let mut seeds: HashMap<Vec<usize>, f64> = HashMap::new();
    collect_iterative_paths(root, &mut Vec::new(), dpi, &mut seeds);

    // Pre-compute CellId → tree path for every tagged grid. Used by
    // `Length::TrackOf` reference resolution.
    let mut grid_paths: HashMap<CellId, Vec<usize>> = HashMap::new();
    collect_grid_paths(root, &mut Vec::new(), &mut grid_paths);

    let has_refs = tree_has_track_refs(root);
    let has_respect_auto = tree_has_respect_with_auto_rows(root);

    let mut widths = WidthResults::default();
    let mut heights = HeightResults::default();

    let needs_iteration = !seeds.is_empty() || has_refs || has_respect_auto;
    let iter_cap = if needs_iteration { MAX_ITER } else { 1 };

    for iter in 0..iter_cap.max(1) {
        let resolved = Resolved {
            grid_paths: &grid_paths,
            widths: if iter == 0 { None } else { Some(&widths) },
            heights: if iter == 0 { None } else { Some(&heights) },
        };

        let mut new_widths = WidthResults::default();
        width_pass_grid(
            root,
            &mut Vec::new(),
            root_cell.x0,
            root_cell.x1,
            root_cell.y0,
            root_cell.y1,
            &seeds,
            dpi,
            &resolved,
            &mut new_widths,
        );

        let mut new_heights = HeightResults::default();
        height_pass_grid(
            root,
            &mut Vec::new(),
            root_cell.y0,
            root_cell.y1,
            &new_widths,
            dpi,
            &resolved,
            &mut new_heights,
        );

        if seeds.is_empty() && !has_refs && !has_respect_auto {
            widths = new_widths;
            heights = new_heights;
            break;
        }

        let needs_stability_check = has_refs || has_respect_auto;
        let stable = !needs_stability_check
            || (widths_match(&widths, &new_widths) && heights_match(&heights, &new_heights));

        let new_seeds = compute_new_seeds(
            root,
            &mut Vec::new(),
            &new_widths,
            &new_heights,
            &seeds,
            dpi,
        );
        let seeds_converged = converged(&seeds, &new_seeds);

        widths = new_widths;
        heights = new_heights;

        if (seeds.is_empty() || seeds_converged) && stable {
            break;
        }
        if iter == iter_cap - 1 {
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

// ─── Reference resolution ────────────────────────────────────────────────────

/// Carries the data needed to resolve [`Length::TrackOf`] references during
/// a width or height pass: the tagged-grid path map, plus optionally the
/// previous iteration's width and height results.
struct Resolved<'a> {
    grid_paths: &'a HashMap<CellId, Vec<usize>>,
    widths: Option<&'a WidthResults>,
    heights: Option<&'a HeightResults>,
}

impl<'a> Resolved<'a> {
    /// Look up the summed track size for a `TrackOf` reference. Returns
    /// `None` if the referenced grid hasn't been resolved yet (e.g.,
    /// iteration 0) — the caller treats this as 0.
    fn track_size(&self, grid: CellId, axis: Axis, track: u16, span: u16) -> Option<f64> {
        let path = self.grid_paths.get(&grid)?;
        let span = span.max(1) as usize;
        let start = (track.saturating_sub(1)) as usize;
        let end = start + span;
        match axis {
            Axis::Width => {
                let gw = self.widths?.grids.get(path)?;
                let end = end.min(gw.cols.len());
                let start = start.min(end);
                if start >= end {
                    return Some(0.0);
                }
                let sum: f64 = gw.cols[start..end].iter().sum();
                let gap_count = (end - start).saturating_sub(1) as f64;
                Some(sum + gap_count * gw.col_gap)
            }
            Axis::Height => {
                let gh = self.heights?.grids.get(path)?;
                let end = end.min(gh.rows.len());
                let start = start.min(end);
                if start >= end {
                    return Some(0.0);
                }
                let sum: f64 = gh.rows[start..end].iter().sum();
                let gap_count = (end - start).saturating_sub(1) as f64;
                Some(sum + gap_count * gh.row_gap)
            }
        }
    }
}

/// Walk the tree once, recording the path for every grid tagged via
/// [`super::Grid::id`]. The map is consulted by `Length::TrackOf`
/// resolution during the width/height passes.
fn collect_grid_paths(
    node: &GridNode,
    path: &mut Vec<usize>,
    out: &mut HashMap<CellId, Vec<usize>>,
) {
    if let Some(id) = node.id {
        out.insert(id, path.clone());
    }
    for (i, (_placement, child)) in node.children.iter().enumerate() {
        if let Node::Grid(g) = child {
            path.push(i);
            collect_grid_paths(g, path, out);
            path.pop();
        }
    }
}

/// Returns `true` if any grid in the tree carries an active respect
/// (selective or all) **and** has at least one Auto row. Aspect locks
/// with Auto chrome rows need a second iteration: pass 1 of iter 0
/// treats Auto rows as 0 in `per_fr_h_provisional`; iter 1 picks up the
/// resolved Auto heights from iter 0's pass 2 and recomputes resp_scale
/// to the correct ratio. Without this trigger, the lock would land
/// slightly off-ratio (cols committed too generously on iter 0).
fn tree_has_respect_with_auto_rows(node: &GridNode) -> bool {
    use crate::layout::Respect;
    let respect_active = !matches!(node.respect, Respect::None);
    if respect_active && node.rows.iter().any(|t| matches!(t, Track::Auto)) {
        return true;
    }
    for (_placement, child) in &node.children {
        if let Node::Grid(g) = child {
            if tree_has_respect_with_auto_rows(g) {
                return true;
            }
        }
    }
    false
}

/// Returns `true` if any `Length` in the tree contains a `TrackOf`
/// variant — triggers the fixed-point iteration loop.
fn tree_has_track_refs(node: &GridNode) -> bool {
    if any_track_ref_in_track(&node.gap.0) || any_track_ref_in_track(&node.gap.1) {
        return true;
    }
    for t in node.cols.iter().chain(node.rows.iter()) {
        if let Track::Fixed(l) = t {
            if length_has_track_ref(l) {
                return true;
            }
        }
    }
    for (placement, child) in &node.children {
        if inset_has_track_ref(&placement.inset) {
            return true;
        }
        if let Node::Grid(g) = child {
            if tree_has_track_refs(g) {
                return true;
            }
        }
    }
    false
}

fn any_track_ref_in_track(l: &Length) -> bool {
    length_has_track_ref(l)
}

fn length_has_track_ref(l: &Length) -> bool {
    match l {
        Length::Sum { .. } => false,
        Length::Min(a, b) | Length::Max(a, b) => length_has_track_ref(a) || length_has_track_ref(b),
        Length::TrackOf { .. } => true,
    }
}

fn inset_has_track_ref(inset: &Inset) -> bool {
    [
        &inset.left,
        &inset.right,
        &inset.top,
        &inset.bottom,
        &inset.width,
        &inset.height,
    ]
    .iter()
    .any(|opt| opt.as_ref().is_some_and(length_has_track_ref))
}

fn widths_match(a: &WidthResults, b: &WidthResults) -> bool {
    if a.grids.len() != b.grids.len() {
        return false;
    }
    for (k, av) in &a.grids {
        let Some(bv) = b.grids.get(k) else {
            return false;
        };
        if av.cols.len() != bv.cols.len() {
            return false;
        }
        for (ac, bc) in av.cols.iter().zip(bv.cols.iter()) {
            if (ac - bc).abs() > EPSILON {
                return false;
            }
        }
    }
    true
}

fn heights_match(a: &HeightResults, b: &HeightResults) -> bool {
    if a.grids.len() != b.grids.len() {
        return false;
    }
    for (k, av) in &a.grids {
        let Some(bv) = b.grids.get(k) else {
            return false;
        };
        if av.rows.len() != bv.rows.len() {
            return false;
        }
        for (ar, br) in av.rows.iter().zip(bv.rows.iter()) {
            if (ar - br).abs() > EPSILON {
                return false;
            }
        }
    }
    true
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
    /// Resolved per-row sizes. Used by `Length::TrackOf { axis: Height, .. }`
    /// reference resolution on the next iteration.
    rows: Vec<f64>,
    row_gap: f64,
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
    resolved: &Resolved,
    out: &mut WidthResults,
) {
    let avail = (x1 - x0).max(0.0);
    let avail_h = (y1 - y0).max(0.0);
    let col_gap = length_to_px(&node.gap.0, dpi, avail, resolved);
    let col_gap_total = saturating_gap_total(node.cols.len(), col_gap);

    let col_fixed = sum_fixed_track_size(&node.cols, dpi, avail, resolved);
    let col_fr_sum = sum_fr(&node.cols);
    let row_fr_sum = sum_fr(&node.rows);

    let col_auto = auto_col_sizes(node, path, seeds, dpi, resolved);
    let col_auto_total: f64 = col_auto.iter().sum();

    let free_w = (avail - col_fixed - col_auto_total - col_gap_total).max(0.0);
    let per_fr_w_default = if col_fr_sum > 0.0 {
        free_w / col_fr_sum
    } else {
        0.0
    };

    // Respect's clamp needs the height-axis per-fr too. We don't know
    // Auto rows' content-driven heights on iter 0 — estimate them as 0
    // (lower bound; they only grow). On iter > 0 we have the previous
    // iteration's resolved row heights via `resolved.heights` and use
    // them as the Auto-row contribution, which lets per_fr_h_provisional
    // converge to the actual per_fr_h. Aspect locks with Auto chrome
    // rows reach the requested ratio in two iterations this way.
    let row_gap_pass1 = length_to_px(&node.gap.1, dpi, avail_h, resolved);
    let row_gap_total_pass1 = saturating_gap_total(node.rows.len(), row_gap_pass1);
    let row_fixed_pass1 = sum_fixed_track_size(&node.rows, dpi, avail_h, resolved);
    let row_auto_pass1: f64 = if let Some(heights) = resolved.heights {
        if let Some(prev) = heights.grids.get(path) {
            node.rows
                .iter()
                .enumerate()
                .filter_map(|(i, t)| match t {
                    Track::Auto => prev.rows.get(i).copied(),
                    _ => None,
                })
                .sum()
        } else {
            0.0
        }
    } else {
        0.0
    };
    let free_h_provisional =
        (avail_h - row_fixed_pass1 - row_auto_pass1 - row_gap_total_pass1).max(0.0);
    let per_fr_h_provisional = if row_fr_sum > 0.0 {
        free_h_provisional / row_fr_sum
    } else {
        0.0
    };

    // Selective respect (R `grid`'s algorithm): split Fr tracks into
    // respected and unrespected; respected tracks share a single scale
    // bound by the smaller of the two axes' demand; unrespected tracks
    // absorb the remainder.
    let (col_fr_respected, col_fr_unrespected) =
        split_fr(&node.cols, |i| node.respect.col_respected(i));
    let (row_fr_respected, _row_fr_unrespected) =
        split_fr(&node.rows, |i| node.respect.row_respected(i));

    let respect_active = col_fr_respected > 0.0 && row_fr_respected > 0.0;

    // resp_scale: the per-fr scale used by every respected track. The
    // smaller of (width-side demand, provisional-height-side demand) wins
    // (the binding axis). When respect isn't active, this is unused.
    let resp_scale_w = if respect_active {
        free_w / col_fr_respected
    } else {
        0.0
    };
    let resp_scale_h_prov = if respect_active {
        free_h_provisional / row_fr_respected
    } else {
        0.0
    };
    let resp_scale = if respect_active {
        resp_scale_w.min(resp_scale_h_prov)
    } else {
        0.0
    };

    // unresp_scale_w: scale for unrespected Fr cols. The respected cols
    // consume `col_fr_respected * resp_scale`; the rest distributes to
    // unrespected fr cols. If there are no unrespected fr cols, this is
    // unused. If respect isn't active, this is the default per-fr.
    let respected_w_total = col_fr_respected * resp_scale;
    let unresp_scale_w = if respect_active && col_fr_unrespected > 0.0 {
        ((free_w - respected_w_total).max(0.0)) / col_fr_unrespected
    } else if !respect_active {
        per_fr_w_default
    } else {
        0.0
    };

    let col_sizes: Vec<f64> = node
        .cols
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            Track::Fixed(l) => length_to_px(l, dpi, avail, resolved),
            Track::Fr(f) => {
                let scale = if respect_active && node.respect.col_respected(i) {
                    resp_scale
                } else {
                    unresp_scale_w
                };
                *f as f64 * scale
            }
            Track::Auto => col_auto[i],
        })
        .collect();

    let total_w = col_sizes.iter().sum::<f64>() + col_gap_total;
    let off_x = ((avail - total_w) * 0.5).max(0.0);
    let resolved_x0 = x0 + off_x;
    let resolved_x1 = resolved_x0 + total_w;

    // Pass 2 (height) needs the respected scale from pass 1 to enforce
    // cross-axis consistency on respected row tracks. Store resp_scale
    // when active; otherwise the default per-fr-w (kept for diagnostics).
    let per_fr_w = if respect_active {
        resp_scale
    } else {
        per_fr_w_default
    };

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

    let _ = (per_fr_w_default, row_fr_sum, col_fr_sum);

    // Provisional row sizes used only to derive children's y-ranges for the
    // respect clamp in nested width passes. Auto rows are treated as 0
    // (their content-driven contribution is unknown until pass 2).
    let row_sizes_provisional: Vec<f64> = node
        .rows
        .iter()
        .map(|t| match t {
            Track::Fixed(l) => length_to_px(l, dpi, avail_h, resolved),
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
            resolved,
        );
        let (child_y0, child_y1) = child_y_range(
            &row_sizes_provisional,
            row_gap_pass1,
            y0,
            placement,
            &placement.inset,
            dpi,
            resolved,
            &[],
        );
        path.push(i);
        match child {
            Node::Grid(g) => width_pass_grid(
                g, path, child_x0, child_x1, child_y0, child_y1, seeds, dpi, resolved, out,
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
    resolved: &Resolved,
) -> Vec<f64> {
    let mut out = vec![0.0; node.cols.len()];
    for (i, (placement, child)) in node.children.iter().enumerate() {
        let span = placement.col_span.max(1);
        if span != 1 {
            continue; // multi-span children are skipped in the width pass
        }
        let col_idx = placement.col.saturating_sub(1) as usize;
        if col_idx >= node.cols.len() {
            continue;
        }
        if !matches!(node.cols[col_idx], Track::Auto) {
            continue;
        }
        path.push(i);
        let contrib = child_min_width(child, path, &placement.inset, seeds, dpi, resolved);
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
    resolved: &Resolved,
) -> f64 {
    if let Some(w) = inset.width.as_ref() {
        return length_to_px_abs(w, dpi, resolved);
    }
    let l = inset
        .left
        .as_ref()
        .map_or(0.0, |v| length_to_px_abs(v, dpi, resolved));
    let r = inset
        .right
        .as_ref()
        .map_or(0.0, |v| length_to_px_abs(v, dpi, resolved));
    let inner = match child {
        Node::Grid(g) => grid_min_width(g, path, seeds, dpi, resolved),
        Node::Cell(c) => match c.measure.width_hint(dpi) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => seeds.get(path).copied().unwrap_or(seed),
        },
    };
    l + inner + r
}

/// Recursive intrinsic min width of a grid. Bottoms out at
/// `Cell::width_hint` for leaves.
fn grid_min_width(
    g: &GridNode,
    path: &mut Vec<usize>,
    seeds: &HashMap<Vec<usize>, f64>,
    dpi: f64,
    resolved: &Resolved,
) -> f64 {
    let col_gap = length_to_px_abs(&g.gap.0, dpi, resolved);
    let col_gap_total = saturating_gap_total(g.cols.len(), col_gap);

    // Pre-fill fixed contributions; auto rows are resolved below.
    let mut col_mins = vec![0.0; g.cols.len()];
    for (i, t) in g.cols.iter().enumerate() {
        if let Track::Fixed(l) = t {
            col_mins[i] = length_to_px_abs(l, dpi, resolved);
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
        let contrib = child_min_width(child, path, &placement.inset, seeds, dpi, resolved);
        path.pop();
        if contrib > col_mins[col_idx] {
            col_mins[col_idx] = contrib;
        }
    }
    col_mins.iter().sum::<f64>() + col_gap_total
}

// ─── Pass 2: heights ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn height_pass_grid(
    node: &GridNode,
    path: &mut Vec<usize>,
    y0: f64,
    y1: f64,
    widths: &WidthResults,
    dpi: f64,
    resolved: &Resolved,
    out: &mut HeightResults,
) {
    let avail = (y1 - y0).max(0.0);
    let gw = widths.grids.get(path).expect("grid widths recorded");

    let row_gap = length_to_px(&node.gap.1, dpi, avail, resolved);
    let row_gap_total = saturating_gap_total(node.rows.len(), row_gap);

    let row_fixed = sum_fixed_track_size(&node.rows, dpi, avail, resolved);
    let row_fr_sum = sum_fr(&node.rows);
    let col_fr_sum = sum_fr(&node.cols);

    let row_auto = auto_row_sizes(node, path, gw, widths, dpi, resolved);
    let row_auto_total: f64 = row_auto.iter().sum();

    let free_h = (avail - row_fixed - row_auto_total - row_gap_total).max(0.0);
    let per_fr_h_default = if row_fr_sum > 0.0 {
        free_h / row_fr_sum
    } else {
        0.0
    };

    // Selective respect (height side). Mirror of the width pass:
    // respected rows share a single scale clamped against pass 1's
    // resp_scale (`gw.per_fr_w`); unrespected rows absorb remainder.
    // Auto rows have already consumed their content height from
    // `free_h`, so if content demand was larger than respect's prediction
    // the grid grows past respect (documented).
    let (row_fr_respected, row_fr_unrespected) =
        split_fr(&node.rows, |i| node.respect.row_respected(i));
    let (col_fr_respected, _col_fr_unrespected) =
        split_fr(&node.cols, |i| node.respect.col_respected(i));
    let respect_active = col_fr_respected > 0.0 && row_fr_respected > 0.0;

    let resp_scale_h = if respect_active {
        free_h / row_fr_respected
    } else {
        0.0
    };
    let resp_scale = if respect_active {
        resp_scale_h.min(gw.per_fr_w)
    } else {
        0.0
    };
    let respected_h_total = row_fr_respected * resp_scale;
    let unresp_scale_h = if respect_active && row_fr_unrespected > 0.0 {
        ((free_h - respected_h_total).max(0.0)) / row_fr_unrespected
    } else if !respect_active {
        per_fr_h_default
    } else {
        0.0
    };
    let _ = (col_fr_sum, row_fr_sum);

    let row_sizes: Vec<f64> = node
        .rows
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            Track::Fixed(l) => length_to_px(l, dpi, avail, resolved),
            Track::Fr(f) => {
                let scale = if respect_active && node.respect.row_respected(i) {
                    resp_scale
                } else {
                    unresp_scale_h
                };
                *f as f64 * scale
            }
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
            rows: row_sizes.clone(),
            row_gap,
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
            resolved,
            &row_auto,
        );
        path.push(i);
        match child {
            Node::Grid(_g) => {
                if let Node::Grid(g) = child {
                    height_pass_grid(g, path, child_y0, child_y1, widths, dpi, resolved, out);
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
    resolved: &Resolved,
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
        let child_w = child_allocated_width(placement, gw, dpi, resolved);
        path.push(i);
        let contrib = child_min_height(
            child,
            path,
            &placement.inset,
            child_w,
            widths,
            dpi,
            resolved,
        );
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
    resolved: &Resolved,
) -> f64 {
    if let Some(h) = inset.height.as_ref() {
        return length_to_px_abs(h, dpi, resolved);
    }
    let t = inset
        .top
        .as_ref()
        .map_or(0.0, |v| length_to_px_abs(v, dpi, resolved));
    let b = inset
        .bottom
        .as_ref()
        .map_or(0.0, |v| length_to_px_abs(v, dpi, resolved));
    // The inner width available to the child's content (after applying any
    // leading/trailing insets that have already shrunk child_width).
    let inner_w = child_width;
    let inner = match child {
        Node::Grid(g) => grid_height_at(g, path, inner_w, widths, dpi, resolved),
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
    resolved: &Resolved,
) -> f64 {
    // We know this grid's resolved widths from pass 1; reuse them.
    let gw = widths.grids.get(path).expect("grid widths recorded");
    let row_gap = length_to_px_abs(&g.gap.1, dpi, resolved);
    let row_gap_total = saturating_gap_total(g.rows.len(), row_gap);

    let mut row_mins = vec![0.0; g.rows.len()];
    for (i, t) in g.rows.iter().enumerate() {
        if let Track::Fixed(l) = t {
            row_mins[i] = length_to_px_abs(l, dpi, resolved);
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
        let child_w = child_allocated_width(placement, gw, dpi, resolved);
        let contrib = child_min_height(
            child,
            path,
            &placement.inset,
            child_w,
            widths,
            dpi,
            resolved,
        );
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

fn sum_fixed_track_size(tracks: &[Track], dpi: f64, axis: f64, resolved: &Resolved) -> f64 {
    tracks
        .iter()
        .filter_map(|t| match t {
            Track::Fixed(l) => Some(length_to_px(l, dpi, axis, resolved)),
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

/// Sum Fr weights split by the `respected` predicate. Returns
/// `(respected_sum, unrespected_sum)`. Fixed/Auto tracks contribute to
/// neither.
fn split_fr<F: Fn(usize) -> bool>(tracks: &[Track], respected: F) -> (f64, f64) {
    let mut resp = 0.0;
    let mut unresp = 0.0;
    for (i, t) in tracks.iter().enumerate() {
        if let Track::Fr(f) = t {
            if respected(i) {
                resp += *f as f64;
            } else {
                unresp += *f as f64;
            }
        }
    }
    (resp, unresp)
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
    resolved: &Resolved,
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
        resolved,
    )
}

#[allow(clippy::too_many_arguments)]
fn child_y_range(
    row_sizes: &[f64],
    row_gap: f64,
    grid_y0: f64,
    placement: &Placement,
    inset: &Inset,
    dpi: f64,
    resolved: &Resolved,
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
        resolved,
    )
}

fn child_allocated_width(
    placement: &Placement,
    gw: &GridWidths,
    dpi: f64,
    resolved: &Resolved,
) -> f64 {
    let (x0, x1) = child_x_range(
        &gw.cols,
        gw.col_gap,
        gw.x0,
        placement,
        &placement.inset,
        dpi,
        resolved,
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
    resolved: &Resolved,
) -> (f64, f64) {
    let l = leading.map_or(0.0, |v| length_to_px(v, dpi, avail, resolved));
    let t = trailing.map_or(0.0, |v| length_to_px(v, dpi, avail, resolved));

    match size {
        None => {
            let start = origin + l;
            let end = (origin + avail - t).max(start);
            (start, end)
        }
        Some(w) => {
            let w_px = length_to_px(w, dpi, avail, resolved);
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

fn length_to_px(l: &Length, dpi: f64, axis_size: f64, resolved: &Resolved) -> f64 {
    match l {
        Length::Sum {
            px,
            inches,
            percent,
        } => px + inches * dpi + percent * axis_size,
        Length::Min(a, b) => {
            length_to_px(a, dpi, axis_size, resolved).min(length_to_px(b, dpi, axis_size, resolved))
        }
        Length::Max(a, b) => {
            length_to_px(a, dpi, axis_size, resolved).max(length_to_px(b, dpi, axis_size, resolved))
        }
        Length::TrackOf {
            grid,
            axis,
            track,
            span,
        } => resolved
            .track_size(*grid, *axis, *track, *span)
            .unwrap_or(0.0),
    }
}

fn length_to_px_abs(l: &Length, dpi: f64, resolved: &Resolved) -> f64 {
    match l {
        Length::Sum { px, inches, .. } => px + inches * dpi,
        Length::Min(a, b) => {
            length_to_px_abs(a, dpi, resolved).min(length_to_px_abs(b, dpi, resolved))
        }
        Length::Max(a, b) => {
            length_to_px_abs(a, dpi, resolved).max(length_to_px_abs(b, dpi, resolved))
        }
        Length::TrackOf {
            grid,
            axis,
            track,
            span,
        } => resolved
            .track_size(*grid, *axis, *track, *span)
            .unwrap_or(0.0),
    }
}
