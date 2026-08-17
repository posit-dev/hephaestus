//! Width-major two-pass grid solver.
//!
//! Pass 1 walks the tree resolving every column track. Auto-column sizing
//! consults child `min_width` (recursive for grid children, `width_hint` for
//! cell children). Per-grid column sizes and x ranges are recorded in a side
//! table indexed by the node's dense tree index.
//!
//! Pass 2 walks the tree again with each grid's allocated y-range; auto rows
//! now consult the **width-aware** `height_at(width)` queries on children,
//! using the widths from pass 1.
//!
//! Both passes run the same code: track resolution, placement arithmetic and
//! inset arithmetic are written once against [`AxisView`] and instantiated for
//! [`Horizontal`] and [`Vertical`]. The two passes differ only in what feeds
//! `Track::Auto` (intrinsic widths versus width-aware heights) and in where
//! the cross-axis numbers for `respect()` come from — pass 1 estimates them,
//! pass 2 reads pass 1's result.
//!
//! Both passes resolve [`Extent::TrackOf`] references against the previous
//! iteration's results — on iteration 0 the reference evaluates to 0, on
//! later iterations it returns the cumulative size of the named tracks
//! from the previous pass. Combined with the existing fixed-point loop
//! (which already exists for [`WidthHint::NeedsHeight`] cells), this
//! converges in 1–2 iterations for forward references and tolerates
//! mild cycles up to `MAX_ITER`.
//!
//! If any cell signalled `WidthHint::NeedsHeight` or any Extent::TrackOf
//! reference is in the tree, the two passes are wrapped in a damped
//! fixed-point iteration capped at `MAX_ITER` rounds. Convergence is not
//! guaranteed (rotated wrapped text genuinely oscillates); the cap is a
//! safety valve.

use std::collections::HashMap;

use super::{
    Axis, Cell, CellId, Extent, GridNode, Inset, Layout, Node, Placement, Track, WidthHint,
};
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
/// [`Extent::TrackOf`] reference exists in the tree. See the module
/// docs for the convergence properties.
pub(super) fn solve(root: &GridNode, viewport: Size, dpi: f64) -> Layout {
    let root_cell = Rect::new(0.0, 0.0, viewport.width, viewport.height);

    // One walk of the tree yields the dense node indices every side table is
    // keyed on, the tagged-grid lookup used by `Extent::TrackOf`, and the
    // starting width estimate of every cell that opted into iteration.
    let prepared = prepare(root, dpi);
    let node_count = prepared.nodes.len();
    let mut seeds = prepared.seeds;

    let has_refs = tree_has_track_refs(root);
    let has_respect_auto = tree_has_respect_with_auto_rows(root);

    let mut widths = PassResults::default();
    let mut heights = PassResults::default();

    let needs_iteration = !seeds.is_empty() || has_refs || has_respect_auto;
    let iter_cap = if needs_iteration { MAX_ITER } else { 1 };

    for iter in 0..iter_cap.max(1) {
        let resolved = Resolved {
            grid_index: &prepared.grid_index,
            widths: if iter == 0 { None } else { Some(&widths) },
            heights: if iter == 0 { None } else { Some(&heights) },
        };

        let mut new_widths = PassResults::new(node_count);
        {
            let mut walk = MinWalk::new(
                MinWidth { seeds: &seeds },
                &prepared.nodes,
                &resolved,
                dpi,
                node_count,
            );
            width_pass_grid(
                root,
                0,
                Horizontal::band(&root_cell),
                Vertical::band(&root_cell),
                &mut walk,
                &mut new_widths,
            );
        }

        let mut new_heights = PassResults::new(node_count);
        {
            let mut walk = MinWalk::new(
                MinHeight {
                    widths: &new_widths,
                },
                &prepared.nodes,
                &resolved,
                dpi,
                node_count,
            );
            height_pass_grid(
                root,
                0,
                Vertical::band(&root_cell),
                &new_widths,
                &mut walk,
                &mut new_heights,
            );
        }

        if seeds.is_empty() && !has_refs && !has_respect_auto {
            widths = new_widths;
            heights = new_heights;
            break;
        }

        let needs_stability_check = has_refs || has_respect_auto;
        let stable = !needs_stability_check
            || (results_match(&widths, &new_widths) && results_match(&heights, &new_heights));

        let mut new_seeds = Vec::new();
        compute_new_seeds(root, 0, &prepared.nodes, &new_heights, dpi, &mut new_seeds);
        let seeds_converged = converged(&seeds, &new_seeds);

        widths = new_widths;
        heights = new_heights;

        if (seeds.is_empty() || seeds_converged) && stable {
            break;
        }
        if iter == iter_cap - 1 {
            break;
        }
        for (idx, new) in new_seeds {
            let prev = seeds.get(idx).unwrap_or(0.0);
            seeds.set(idx, DAMPING * new + (1.0 - DAMPING) * prev);
        }
    }

    // Build the final rect map from the resolved widths and heights.
    let mut rects = HashMap::new();
    emit_rects(root, 0, &prepared.nodes, &widths, &heights, &mut rects);

    Layout {
        root: root_cell,
        rects,
    }
}

// ─── Axis view ───────────────────────────────────────────────────────────────

/// A pixel interval on one axis.
#[derive(Clone, Copy, Debug, Default)]
struct Band {
    start: f64,
    end: f64,
}

impl Band {
    /// Length of the interval, clamped at zero.
    fn size(self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// One axis of a grid, as the solver sees it.
///
/// Track resolution, Auto sizing, placement arithmetic and inset arithmetic
/// are written once against this view and instantiated for [`Horizontal`]
/// (columns) and [`Vertical`] (rows).
trait AxisView {
    /// The grid's tracks along this axis.
    fn tracks(node: &GridNode) -> &[Track];
    /// The gap between adjacent tracks on this axis.
    fn gap(node: &GridNode) -> &Extent;
    /// True when `track` participates in the grid's respect coupling.
    fn respected(node: &GridNode, track: usize) -> bool;
    /// 1-indexed first track a placement occupies on this axis.
    fn placement_start(placement: &Placement) -> u16;
    /// Number of tracks a placement spans on this axis.
    fn placement_span(placement: &Placement) -> u16;
    /// Inset from the leading edge (left / top).
    fn inset_leading(inset: &Inset) -> Option<&Extent>;
    /// Inset from the trailing edge (right / bottom).
    fn inset_trailing(inset: &Inset) -> Option<&Extent>;
    /// Explicit size along this axis, if the inset pins one.
    fn inset_size(inset: &Inset) -> Option<&Extent>;
    /// The interval a rect occupies on this axis.
    fn band(rect: &Rect) -> Band;
}

/// The column axis.
struct Horizontal;
/// The row axis.
struct Vertical;

impl AxisView for Horizontal {
    fn tracks(node: &GridNode) -> &[Track] {
        &node.cols
    }
    fn gap(node: &GridNode) -> &Extent {
        &node.gap.0
    }
    fn respected(node: &GridNode, track: usize) -> bool {
        node.respect.col_respected(track)
    }
    fn placement_start(placement: &Placement) -> u16 {
        placement.col
    }
    fn placement_span(placement: &Placement) -> u16 {
        placement.col_span
    }
    fn inset_leading(inset: &Inset) -> Option<&Extent> {
        inset.left.as_ref()
    }
    fn inset_trailing(inset: &Inset) -> Option<&Extent> {
        inset.right.as_ref()
    }
    fn inset_size(inset: &Inset) -> Option<&Extent> {
        inset.width.as_ref()
    }
    fn band(rect: &Rect) -> Band {
        Band {
            start: rect.x0,
            end: rect.x1,
        }
    }
}

impl AxisView for Vertical {
    fn tracks(node: &GridNode) -> &[Track] {
        &node.rows
    }
    fn gap(node: &GridNode) -> &Extent {
        &node.gap.1
    }
    fn respected(node: &GridNode, track: usize) -> bool {
        node.respect.row_respected(track)
    }
    fn placement_start(placement: &Placement) -> u16 {
        placement.row
    }
    fn placement_span(placement: &Placement) -> u16 {
        placement.row_span
    }
    fn inset_leading(inset: &Inset) -> Option<&Extent> {
        inset.top.as_ref()
    }
    fn inset_trailing(inset: &Inset) -> Option<&Extent> {
        inset.bottom.as_ref()
    }
    fn inset_size(inset: &Inset) -> Option<&Extent> {
        inset.height.as_ref()
    }
    fn band(rect: &Rect) -> Band {
        Band {
            start: rect.y0,
            end: rect.y1,
        }
    }
}

// ─── Tree pre-pass ───────────────────────────────────────────────────────────

/// Dense index of a node (grid or cell) within the layout tree.
type NodeIdx = usize;

/// The tree's shape flattened to dense indices, so every side table is a
/// `Vec` lookup rather than a hash of a heap-allocated path.
struct NodeMap {
    /// Index of each child, in placement order, for every node.
    children: Vec<Vec<NodeIdx>>,
}

impl NodeMap {
    /// Number of nodes in the tree.
    fn len(&self) -> usize {
        self.children.len()
    }

    /// Index of the child in placement slot `slot` of the node at `parent`.
    fn child(&self, parent: NodeIdx, slot: usize) -> NodeIdx {
        self.children[parent][slot]
    }
}

/// Current width estimate for every cell that opted into iteration.
#[derive(Default)]
struct Seeds {
    /// Estimate per node; `None` for cells that don't iterate.
    values: Vec<Option<f64>>,
    /// Nodes carrying an estimate.
    nodes: Vec<NodeIdx>,
}

impl Seeds {
    /// True when no cell opted into iteration.
    fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Current estimate for `idx`, if that cell iterates.
    fn get(&self, idx: NodeIdx) -> Option<f64> {
        self.values.get(idx).copied().flatten()
    }

    /// Record a new estimate for `idx`.
    fn set(&mut self, idx: NodeIdx, value: f64) {
        if self.values[idx].is_none() {
            self.nodes.push(idx);
        }
        self.values[idx] = Some(value);
    }
}

/// What a single walk of the tree gives the solve loop.
struct Prepared {
    nodes: NodeMap,
    /// Index of every grid tagged via [`super::Grid::id`], for `TrackOf`
    /// reference resolution.
    grid_index: HashMap<CellId, NodeIdx>,
    seeds: Seeds,
}

fn prepare(root: &GridNode, dpi: f64) -> Prepared {
    let mut nodes = NodeMap {
        children: vec![Vec::new()],
    };
    let mut grid_index = HashMap::new();
    let mut initial_seeds: Vec<(NodeIdx, f64)> = Vec::new();
    if let Some(id) = root.id {
        grid_index.insert(id, 0);
    }
    walk_prepare(
        root,
        0,
        dpi,
        &mut nodes,
        &mut grid_index,
        &mut initial_seeds,
    );

    let mut seeds = Seeds {
        values: vec![None; nodes.len()],
        nodes: Vec::with_capacity(initial_seeds.len()),
    };
    for (idx, seed) in initial_seeds {
        seeds.set(idx, seed);
    }
    Prepared {
        nodes,
        grid_index,
        seeds,
    }
}

fn walk_prepare(
    node: &GridNode,
    idx: NodeIdx,
    dpi: f64,
    nodes: &mut NodeMap,
    grid_index: &mut HashMap<CellId, NodeIdx>,
    seeds: &mut Vec<(NodeIdx, f64)>,
) {
    for (_placement, child) in &node.children {
        let child_idx = nodes.children.len();
        nodes.children.push(Vec::new());
        nodes.children[idx].push(child_idx);
        match child {
            Node::Grid(g) => {
                if let Some(id) = g.id {
                    grid_index.insert(id, child_idx);
                }
                walk_prepare(g, child_idx, dpi, nodes, grid_index, seeds);
            }
            Node::Cell(c) => {
                if let WidthHint::NeedsHeight { seed } = c.measure.width_hint(dpi) {
                    seeds.push((child_idx, seed));
                }
            }
        }
    }
}

// ─── Reference resolution ────────────────────────────────────────────────────

/// Carries the data needed to resolve [`Extent::TrackOf`] references during
/// a width or height pass: the tagged-grid index map, plus optionally the
/// previous iteration's width and height results.
struct Resolved<'a> {
    grid_index: &'a HashMap<CellId, NodeIdx>,
    widths: Option<&'a PassResults>,
    heights: Option<&'a PassResults>,
}

impl Resolved<'_> {
    /// Previous iteration's resolved height for row `row` of the grid at
    /// `idx`. `None` on iteration 0, before any height pass has run.
    /// Callers treat that as 0 — Auto rows only grow from content, so
    /// zero is a safe lower bound to start from.
    fn prev_row_height(&self, idx: NodeIdx, row: usize) -> Option<f64> {
        self.heights?.tracks(idx)?.sizes.get(row).copied()
    }

    /// Look up the summed track size for a `TrackOf` reference. Returns
    /// `None` if the referenced grid hasn't been resolved yet (e.g.,
    /// iteration 0) — the caller treats this as 0.
    fn track_size(&self, grid: CellId, axis: Axis, track: u16, span: u16) -> Option<f64> {
        let idx = *self.grid_index.get(&grid)?;
        let band = match axis {
            Axis::Width => self.widths?.tracks(idx)?,
            Axis::Height => self.heights?.tracks(idx)?,
        };
        let span = span.max(1) as usize;
        let start = (track.saturating_sub(1)) as usize;
        let end = (start + span).min(band.sizes.len());
        let start = start.min(end);
        if start >= end {
            return Some(0.0);
        }
        let sum: f64 = band.sizes[start..end].iter().sum();
        let gap_count = (end - start).saturating_sub(1) as f64;
        Some(sum + gap_count * band.gap)
    }
}

/// Returns `true` if any grid in the tree carries an active respect
/// (selective or all) **and** has at least one Auto row. Aspect locks
/// with Auto chrome rows need a second iteration: pass 1 of iter 0
/// treats Auto rows as 0 in the provisional per-fr-h; iter 1 picks up the
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

/// Returns `true` if any `Extent` in the tree contains a `TrackOf`
/// variant — triggers the fixed-point iteration loop.
fn tree_has_track_refs(node: &GridNode) -> bool {
    if length_has_track_ref(&node.gap.0) || length_has_track_ref(&node.gap.1) {
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

fn length_has_track_ref(l: &Extent) -> bool {
    match l {
        Extent::Sum { .. } => false,
        Extent::Min(a, b) | Extent::Max(a, b) => length_has_track_ref(a) || length_has_track_ref(b),
        Extent::TrackOf { .. } => true,
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

/// True when two passes over the same tree agree on every track size to
/// within [`EPSILON`].
fn results_match(a: &PassResults, b: &PassResults) -> bool {
    if a.grids.len() != b.grids.len() {
        return false;
    }
    for (av, bv) in a.grids.iter().zip(b.grids.iter()) {
        match (av, bv) {
            (None, None) => {}
            (Some(av), Some(bv)) => {
                if av.sizes.len() != bv.sizes.len() {
                    return false;
                }
                for (a_size, b_size) in av.sizes.iter().zip(bv.sizes.iter()) {
                    if (a_size - b_size).abs() > EPSILON {
                        return false;
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

// ─── Side tables ─────────────────────────────────────────────────────────────

/// One grid's resolved tracks on one axis.
struct TrackBand {
    /// Resolved size of each track.
    sizes: Vec<f64>,
    /// Gap between adjacent tracks.
    gap: f64,
    /// Interval the grid occupies on this axis.
    span: Band,
    /// Pixel size of one fr unit — the respected scale when `respect()` is
    /// active on this grid, otherwise the plain share of the free space.
    /// The height pass re-clamps `respect()` against the width pass's value.
    per_fr: f64,
}

/// Everything one axis pass records about the tree.
#[derive(Default)]
struct PassResults {
    /// Resolved tracks per grid, indexed by node. `None` at cell nodes.
    grids: Vec<Option<TrackBand>>,
    /// Interval each cell's parent track + inset gave it, indexed by node.
    cells: Vec<Band>,
}

impl PassResults {
    fn new(count: usize) -> Self {
        Self {
            grids: (0..count).map(|_| None).collect(),
            cells: vec![Band::default(); count],
        }
    }

    /// Resolved tracks of the grid at `idx`, if this pass reached it.
    fn tracks(&self, idx: NodeIdx) -> Option<&TrackBand> {
        self.grids.get(idx)?.as_ref()
    }
}

// ─── Track resolution ────────────────────────────────────────────────────────

/// The cross axis's contribution to `respect()`'s clamp.
struct CrossRespect {
    /// Summed fr weight of the cross axis's respected tracks. Zero disables
    /// the coupling: respect needs respected fr tracks on *both* axes.
    fr_respected: f64,
    /// Per-fr pixel size the cross axis can afford for respected tracks.
    scale: f64,
}

impl CrossRespect {
    /// The cross axis has nothing to couple to.
    const INACTIVE: CrossRespect = CrossRespect {
        fr_respected: 0.0,
        scale: 0.0,
    };
}

/// One axis's resolved tracks, before they are positioned.
struct AxisSolution {
    sizes: Vec<f64>,
    gap: f64,
    /// Track sizes plus the gaps between them.
    total: f64,
    /// Space left for fr tracks after fixed tracks, Auto tracks and gaps.
    free: f64,
    /// Pixel size of one fr unit; see [`TrackBand::per_fr`].
    per_fr: f64,
}

impl AxisSolution {
    /// The interval the tracks occupy, centered within `avail` from `origin`.
    fn span(&self, origin: f64, avail: f64) -> Band {
        let start = origin + ((avail - self.total) * 0.5).max(0.0);
        Band {
            start,
            end: start + self.total,
        }
    }
}

/// Resolve one axis of `node` into pixel track sizes.
///
/// `auto` supplies the already-measured size of each `Track::Auto` (zero
/// elsewhere); `cross` supplies the other axis's numbers so `respect()` can
/// clamp both axes to a shared per-fr scale.
fn resolve_tracks<A: AxisView>(
    node: &GridNode,
    avail: f64,
    auto: &[f64],
    cross: CrossRespect,
    dpi: f64,
    resolved: &Resolved,
) -> AxisSolution {
    let tracks = A::tracks(node);
    let gap = length_to_px(A::gap(node), dpi, avail, resolved);
    let gap_total = saturating_gap_total(tracks.len(), gap);

    let fixed = sum_fixed_track_size(tracks, dpi, avail, resolved);
    let fr_sum = sum_fr(tracks);
    let auto_total: f64 = auto.iter().sum();

    let free = (avail - fixed - auto_total - gap_total).max(0.0);
    let per_fr_default = if fr_sum > 0.0 { free / fr_sum } else { 0.0 };

    // Selective respect (R `grid`'s algorithm): split fr tracks into
    // respected and unrespected; respected tracks share a single scale
    // bound by the smaller of the two axes' demand; unrespected tracks
    // absorb the remainder.
    //
    // The resp_scale division denominator is `fr_respected +
    // fr_unrespected` (i.e. the total fr) rather than just the
    // respected part. The intent: leave the unrespected sibling
    // tracks their fr share before the respected aspect lock claims
    // the rest. Without this, a single aspect-locked cell on an
    // axis with unrespected siblings would consume the whole axis,
    // collapsing the siblings to zero. With it, a 2×2 grid that
    // has a square-locked cell at (1,1) and three flex cells
    // resolves cleanly — locked cell square at the min axis scale,
    // siblings absorb the rest of each axis.
    let (fr_respected, fr_unrespected) = split_fr(tracks, |i| A::respected(node, i));
    let respect_active = fr_respected > 0.0 && cross.fr_respected > 0.0;
    let fr_total_resp = fr_respected + fr_unrespected;

    // resp_scale: the per-fr scale used by every respected track. The
    // smaller of (this axis's demand, the cross axis's demand) wins — the
    // binding axis. When respect isn't active, this is unused.
    let resp_scale_main = if respect_active && fr_total_resp > 0.0 {
        free / fr_total_resp
    } else {
        0.0
    };
    let resp_scale = if respect_active {
        resp_scale_main.min(cross.scale)
    } else {
        0.0
    };

    // unresp_scale: scale for unrespected fr tracks. The respected tracks
    // consume `fr_respected * resp_scale`; the rest distributes to the
    // unrespected fr tracks. If there are none, this is unused. If respect
    // isn't active, this is the default per-fr.
    let respected_total = fr_respected * resp_scale;
    let unresp_scale = if respect_active && fr_unrespected > 0.0 {
        ((free - respected_total).max(0.0)) / fr_unrespected
    } else if !respect_active {
        per_fr_default
    } else {
        0.0
    };

    let sizes: Vec<f64> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            Track::Fixed(l) => length_to_px(l, dpi, avail, resolved),
            Track::Fr(f) => {
                let scale = if respect_active && A::respected(node, i) {
                    resp_scale
                } else {
                    unresp_scale
                };
                *f * scale
            }
            Track::Auto => auto[i],
        })
        .collect();

    let total = sizes.iter().sum::<f64>() + gap_total;
    let per_fr = if respect_active {
        resp_scale
    } else {
        per_fr_default
    };

    AxisSolution {
        sizes,
        gap,
        total,
        free,
        per_fr,
    }
}

// ─── Auto sizing ─────────────────────────────────────────────────────────────

/// Per-axis hooks for the intrinsic-minimum walk that sizes `Track::Auto`.
///
/// The walk itself — recurse into grids, take the max child contribution per
/// Auto track, add insets and gaps — is the same on both axes. The sizer
/// supplies the two axis-specific steps: how a leaf reports its size, and
/// what cross-axis size a child was allotted.
trait MinSizer {
    /// The axis whose tracks the walk sizes.
    type Axis: AxisView;

    /// Intrinsic size of a leaf along the sized axis, given the cross-axis
    /// size the leaf was allotted.
    fn leaf(&self, cell: &Cell, node: NodeIdx, cross: f64, dpi: f64) -> f64;

    /// Cross-axis size allotted to the child placed at `placement` within
    /// the grid at `parent`.
    fn cross(&self, parent: NodeIdx, placement: &Placement, dpi: f64, resolved: &Resolved) -> f64;
}

/// Sizes Auto columns from `width_hint`, with the iteration seed standing in
/// for cells whose width depends on their height.
struct MinWidth<'a> {
    seeds: &'a Seeds,
}

impl MinSizer for MinWidth<'_> {
    type Axis = Horizontal;

    fn leaf(&self, cell: &Cell, node: NodeIdx, _cross: f64, dpi: f64) -> f64 {
        match cell.measure.width_hint(dpi) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => self.seeds.get(node).unwrap_or(seed),
        }
    }

    fn cross(
        &self,
        _parent: NodeIdx,
        _placement: &Placement,
        _dpi: f64,
        _resolved: &Resolved,
    ) -> f64 {
        0.0
    }
}

/// Sizes Auto rows from `height_at`, using the widths pass 1 resolved.
struct MinHeight<'a> {
    widths: &'a PassResults,
}

impl MinSizer for MinHeight<'_> {
    type Axis = Vertical;

    fn leaf(&self, cell: &Cell, _node: NodeIdx, cross: f64, dpi: f64) -> f64 {
        cell.measure.height_at(cross, dpi)
    }

    fn cross(&self, parent: NodeIdx, placement: &Placement, dpi: f64, resolved: &Resolved) -> f64 {
        let gw = self.widths.tracks(parent).expect("grid widths recorded");
        child_range::<Horizontal>(&gw.sizes, gw.gap, gw.span.start, placement, dpi, resolved).size()
    }
}

/// The intrinsic-minimum walk. Grid results are memoized per node: within one
/// pass the inputs are fixed, and the same grid is queried once per ancestor
/// that has an Auto track.
struct MinWalk<'a, S: MinSizer> {
    sizer: S,
    nodes: &'a NodeMap,
    resolved: &'a Resolved<'a>,
    dpi: f64,
    memo: Vec<Option<f64>>,
}

impl<'a, S: MinSizer> MinWalk<'a, S> {
    fn new(
        sizer: S,
        nodes: &'a NodeMap,
        resolved: &'a Resolved<'a>,
        dpi: f64,
        node_count: usize,
    ) -> Self {
        Self {
            sizer,
            nodes,
            resolved,
            dpi,
            memo: vec![None; node_count],
        }
    }

    /// Size of every `Track::Auto` of the grid at `idx`, zero elsewhere.
    ///
    /// Only children occupying a single track on the sized axis contribute;
    /// a child spanning several tracks has no unambiguous track to grow.
    fn auto_tracks(&mut self, node: &GridNode, idx: NodeIdx) -> Vec<f64> {
        let tracks = S::Axis::tracks(node);
        let mut out = vec![0.0; tracks.len()];
        for (slot, (placement, child)) in node.children.iter().enumerate() {
            if S::Axis::placement_span(placement).max(1) != 1 {
                continue;
            }
            let track = (S::Axis::placement_start(placement).saturating_sub(1)) as usize;
            if track >= tracks.len() {
                continue;
            }
            if !matches!(tracks[track], Track::Auto) {
                continue;
            }
            let cross = self.sizer.cross(idx, placement, self.dpi, self.resolved);
            let child_idx = self.nodes.child(idx, slot);
            let contrib = self.child_min(child, child_idx, &placement.inset, cross);
            if contrib > out[track] {
                out[track] = contrib;
            }
        }
        out
    }

    /// Size a child contributes to its parent's Auto track, insets included.
    fn child_min(&mut self, child: &Node, idx: NodeIdx, inset: &Inset, cross: f64) -> f64 {
        if let Some(size) = S::Axis::inset_size(inset) {
            return length_to_px_abs(size, self.dpi, self.resolved);
        }
        let leading = S::Axis::inset_leading(inset)
            .map_or(0.0, |v| length_to_px_abs(v, self.dpi, self.resolved));
        let trailing = S::Axis::inset_trailing(inset)
            .map_or(0.0, |v| length_to_px_abs(v, self.dpi, self.resolved));
        let inner = match child {
            Node::Grid(g) => self.grid_min(g, idx),
            Node::Cell(c) => self.sizer.leaf(c, idx, cross, self.dpi),
        };
        leading + inner + trailing
    }

    /// Recursive intrinsic size of a grid: fixed tracks at their absolute
    /// size, Auto tracks at their content size, fr tracks at zero.
    fn grid_min(&mut self, g: &GridNode, idx: NodeIdx) -> f64 {
        if let Some(cached) = self.memo[idx] {
            return cached;
        }
        let gap = length_to_px_abs(S::Axis::gap(g), self.dpi, self.resolved);
        let gap_total = saturating_gap_total(S::Axis::tracks(g).len(), gap);
        let auto = self.auto_tracks(g, idx);
        let total = S::Axis::tracks(g)
            .iter()
            .enumerate()
            .map(|(i, t)| match t {
                Track::Fixed(l) => length_to_px_abs(l, self.dpi, self.resolved),
                _ => auto[i],
            })
            .sum::<f64>()
            + gap_total;
        self.memo[idx] = Some(total);
        total
    }
}

// ─── Pass 1: widths ──────────────────────────────────────────────────────────

fn width_pass_grid(
    node: &GridNode,
    idx: NodeIdx,
    x: Band,
    y: Band,
    walk: &mut MinWalk<'_, MinWidth<'_>>,
    out: &mut PassResults,
) {
    let dpi = walk.dpi;
    let resolved = walk.resolved;
    let nodes = walk.nodes;
    let avail_w = x.size();
    let avail_h = y.size();

    // Respect's clamp needs the height-axis per-fr too, and so do the child
    // windows handed down to nested width passes. We don't know Auto rows'
    // content-driven heights on iter 0 — estimate them as 0 (lower bound;
    // they only grow). On iter > 0 the previous iteration's resolved row
    // heights stand in, which lets the provisional per-fr-h converge to the
    // actual per-fr-h. Aspect locks with Auto chrome rows reach the
    // requested ratio in two iterations this way.
    //
    // Both consumers must use the same numbers: a child spanning Auto rows
    // whose window omits their height reads the height axis as tighter than
    // it is, which undersizes a respected child's aspect-locked cells and
    // leaves the surplus as centering slack.
    let prev_rows: Vec<f64> = node
        .rows
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            Track::Auto => resolved.prev_row_height(idx, i).unwrap_or(0.0),
            _ => 0.0,
        })
        .collect();
    let provisional = resolve_tracks::<Vertical>(
        node,
        avail_h,
        &prev_rows,
        CrossRespect::INACTIVE,
        dpi,
        resolved,
    );
    let (row_fr_respected, row_fr_unrespected) =
        split_fr(&node.rows, |i| node.respect.row_respected(i));
    let row_fr_total_resp = row_fr_respected + row_fr_unrespected;
    let cross = CrossRespect {
        fr_respected: row_fr_respected,
        scale: if row_fr_total_resp > 0.0 {
            provisional.free / row_fr_total_resp
        } else {
            0.0
        },
    };

    let auto = walk.auto_tracks(node, idx);
    let solution = resolve_tracks::<Horizontal>(node, avail_w, &auto, cross, dpi, resolved);
    let span = solution.span(x.start, avail_w);

    out.grids[idx] = Some(TrackBand {
        sizes: solution.sizes.clone(),
        gap: solution.gap,
        span,
        per_fr: solution.per_fr,
    });

    for (slot, (placement, child)) in node.children.iter().enumerate() {
        let child_x = child_range::<Horizontal>(
            &solution.sizes,
            solution.gap,
            span.start,
            placement,
            dpi,
            resolved,
        );
        // The provisional rows are never positioned, so children take their
        // y window from the raw origin; only its length feeds respect.
        let child_y = child_range::<Vertical>(
            &provisional.sizes,
            provisional.gap,
            y.start,
            placement,
            dpi,
            resolved,
        );
        let child_idx = nodes.child(idx, slot);
        match child {
            Node::Grid(g) => width_pass_grid(g, child_idx, child_x, child_y, walk, out),
            Node::Cell(_) => out.cells[child_idx] = child_x,
        }
    }
}

// ─── Pass 2: heights ─────────────────────────────────────────────────────────

fn height_pass_grid(
    node: &GridNode,
    idx: NodeIdx,
    y: Band,
    widths: &PassResults,
    walk: &mut MinWalk<'_, MinHeight<'_>>,
    out: &mut PassResults,
) {
    let dpi = walk.dpi;
    let resolved = walk.resolved;
    let nodes = walk.nodes;
    let avail = y.size();

    // Respect on the height side clamps against pass 1's resolved per-fr-w.
    // Auto rows have already consumed their content height from the free
    // space, so if content demand was larger than respect's prediction the
    // grid grows past respect (documented).
    let gw = widths.tracks(idx).expect("grid widths recorded");
    let (col_fr_respected, _col_fr_unrespected) =
        split_fr(&node.cols, |i| node.respect.col_respected(i));
    let cross = CrossRespect {
        fr_respected: col_fr_respected,
        scale: gw.per_fr,
    };

    let auto = walk.auto_tracks(node, idx);
    let solution = resolve_tracks::<Vertical>(node, avail, &auto, cross, dpi, resolved);
    let span = solution.span(y.start, avail);

    out.grids[idx] = Some(TrackBand {
        sizes: solution.sizes.clone(),
        gap: solution.gap,
        span,
        per_fr: solution.per_fr,
    });

    for (slot, (placement, child)) in node.children.iter().enumerate() {
        let child_y = child_range::<Vertical>(
            &solution.sizes,
            solution.gap,
            span.start,
            placement,
            dpi,
            resolved,
        );
        let child_idx = nodes.child(idx, slot);
        match child {
            Node::Grid(g) => height_pass_grid(g, child_idx, child_y, widths, walk, out),
            Node::Cell(_) => out.cells[child_idx] = child_y,
        }
    }
}

// ─── Rect emission ───────────────────────────────────────────────────────────

fn emit_rects(
    node: &GridNode,
    idx: NodeIdx,
    nodes: &NodeMap,
    widths: &PassResults,
    heights: &PassResults,
    out: &mut HashMap<CellId, Rect>,
) {
    let gw = widths.tracks(idx).expect("grid widths recorded");
    let gh = heights.tracks(idx).expect("grid heights recorded");
    if let Some(id) = node.id {
        out.insert(
            id,
            Rect::new(gw.span.start, gh.span.start, gw.span.end, gh.span.end),
        );
    }
    for (slot, (_placement, child)) in node.children.iter().enumerate() {
        let child_idx = nodes.child(idx, slot);
        match child {
            Node::Grid(g) => emit_rects(g, child_idx, nodes, widths, heights, out),
            Node::Cell(c) => {
                if let Some(id) = c.id {
                    let x = widths.cells[child_idx];
                    let y = heights.cells[child_idx];
                    out.insert(id, Rect::new(x.start, y.start, x.end, y.end));
                }
            }
        }
    }
}

// ─── Iteration support ───────────────────────────────────────────────────────

fn compute_new_seeds(
    node: &GridNode,
    idx: NodeIdx,
    nodes: &NodeMap,
    heights: &PassResults,
    dpi: f64,
    out: &mut Vec<(NodeIdx, f64)>,
) {
    for (slot, (_placement, child)) in node.children.iter().enumerate() {
        let child_idx = nodes.child(idx, slot);
        match child {
            Node::Grid(g) => compute_new_seeds(g, child_idx, nodes, heights, dpi, out),
            Node::Cell(c) => {
                if matches!(c.measure.width_hint(dpi), WidthHint::NeedsHeight { .. }) {
                    let h = heights.cells[child_idx].size();
                    out.push((child_idx, c.measure.width_at(h, dpi)));
                }
            }
        }
    }
}

fn converged(old: &Seeds, new: &[(NodeIdx, f64)]) -> bool {
    if old.nodes.len() != new.len() {
        return false;
    }
    for (idx, v_new) in new {
        let v_old = old.get(*idx).unwrap_or(f64::INFINITY);
        if (v_new - v_old).abs() > EPSILON {
            return false;
        }
    }
    true
}

// ─── Geometry helpers ────────────────────────────────────────────────────────

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
            Track::Fr(f) => Some(*f),
            _ => None,
        })
        .sum()
}

/// Sum fr weights split by the `respected` predicate. Returns
/// `(respected_sum, unrespected_sum)`. Fixed/Auto tracks contribute to
/// neither.
fn split_fr<F: Fn(usize) -> bool>(tracks: &[Track], respected: F) -> (f64, f64) {
    let mut resp = 0.0;
    let mut unresp = 0.0;
    for (i, t) in tracks.iter().enumerate() {
        if let Track::Fr(f) = t {
            if respected(i) {
                resp += *f;
            } else {
                unresp += *f;
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

/// The interval a placement's child occupies on one axis, after applying the
/// tracks it spans and its inset.
fn child_range<A: AxisView>(
    sizes: &[f64],
    gap: f64,
    grid_start: f64,
    placement: &Placement,
    dpi: f64,
    resolved: &Resolved,
) -> Band {
    let span = A::placement_span(placement).max(1);
    let start = (A::placement_start(placement).saturating_sub(1)) as usize;
    let end_excl = (start + span as usize).min(sizes.len());
    let start = start.min(sizes.len());

    let cell_start = grid_start + track_offset(sizes, gap, start);
    let cell_end = if end_excl == 0 {
        cell_start
    } else {
        grid_start + track_end(sizes, gap, end_excl - 1)
    };
    let avail = (cell_end - cell_start).max(0.0);
    let inset = &placement.inset;
    resolve_axis(
        cell_start,
        avail,
        A::inset_leading(inset),
        A::inset_trailing(inset),
        A::inset_size(inset),
        dpi,
        resolved,
    )
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
    leading: Option<&Extent>,
    trailing: Option<&Extent>,
    size: Option<&Extent>,
    dpi: f64,
    resolved: &Resolved,
) -> Band {
    let l = leading.map_or(0.0, |v| length_to_px(v, dpi, avail, resolved));
    let t = trailing.map_or(0.0, |v| length_to_px(v, dpi, avail, resolved));

    let (start, end) = match size {
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
    };
    Band { start, end }
}

fn length_to_px(l: &Extent, dpi: f64, axis_size: f64, resolved: &Resolved) -> f64 {
    match l {
        Extent::Sum {
            px,
            inches,
            percent,
        } => px + inches * dpi + percent * axis_size,
        Extent::Min(a, b) => {
            length_to_px(a, dpi, axis_size, resolved).min(length_to_px(b, dpi, axis_size, resolved))
        }
        Extent::Max(a, b) => {
            length_to_px(a, dpi, axis_size, resolved).max(length_to_px(b, dpi, axis_size, resolved))
        }
        Extent::TrackOf {
            grid,
            axis,
            track,
            span,
        } => resolved
            .track_size(*grid, *axis, *track, *span)
            .unwrap_or(0.0),
    }
}

fn length_to_px_abs(l: &Extent, dpi: f64, resolved: &Resolved) -> f64 {
    match l {
        Extent::Sum { px, inches, .. } => px + inches * dpi,
        Extent::Min(a, b) => {
            length_to_px_abs(a, dpi, resolved).min(length_to_px_abs(b, dpi, resolved))
        }
        Extent::Max(a, b) => {
            length_to_px_abs(a, dpi, resolved).max(length_to_px_abs(b, dpi, resolved))
        }
        Extent::TrackOf {
            grid,
            axis,
            track,
            span,
        } => resolved
            .track_size(*grid, *axis, *track, *span)
            .unwrap_or(0.0),
    }
}
