//! Layout: compose n×m grids recursively, solve to a flat map of cell-id →
//! pixel rectangle.
//!
//! The public surface is intentionally narrow: grids, 1×1 cells, recursive
//! nesting, row/column placement with spans, optional per-edge insets within
//! a cell using physical or relative units, and the `respect` flag (from R
//! grid's `grid.layout`) for shared cross-axis fr scaling — which is also how
//! aspect ratios are expressed (e.g. a 16:9 cell is
//! `Grid::new([Fr(16.0)], [Fr(9.0)]).respect()`).
//!
//! The solver is a top-down pass: each grid receives its cell area from its
//! parent, resolves its tracks to absolute pixels (applying `respect` if set),
//! recursively solves each placed child against its computed cell area, and
//! emits a rect for every tagged node. No external layout engine is involved.
//!
//! Coordinates are pixels (top-left origin, f64). Physical units (`Mm`, `Cm`,
//! `Inch`, `Pt`) are resolved against the `dpi` passed to [`Grid::solve`].
//!
//! This file holds the grid vocabulary — [`Grid`], [`Track`], [`Placement`],
//! [`Inset`], [`Layout`]. The measurement type lives in `length`, the leaf
//! content protocol in `measure`, and the two-pass solver in `solver`.

use crate::geometry::{Rect, Size};
use std::collections::HashMap;

mod length;
mod measure;
mod solver;
#[cfg(test)]
mod tests;

pub use length::Extent;
pub use measure::{Cell, MaxMergeMeasure, Measure, WidthHint};

/// Identifies an axis (column or row) of a [`Grid`] for [`Extent::TrackOf`]
/// references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Column axis (track width).
    Width,
    /// Row axis (track height).
    Height,
}

/// Sizing rule for a grid column or row.
#[derive(Clone, Debug, PartialEq)]
pub enum Track {
    /// Fixed extent.
    Fixed(Extent),
    /// Fractional share of remaining space (CSS `fr` / R grid's "null" unit).
    Fr(f64),
    /// Size to fit content via the [`Track::Auto`] min-broadcast protocol;
    /// see the `Layout` section of `CLAUDE.md`.
    Auto,
}

/// User-supplied tag for retrieving a node's resolved rect from the [`Layout`]
/// output. Ids you do not tag a node with are simply absent from the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellId(pub u64);

/// A node in the layout tree — either a [`Grid`] or a [`Cell`]. Callers
/// don't construct `Node` directly; `Grid` and `Cell` each impl `Into<Node>`
/// so [`Grid::place`] takes either kind transparently.
pub enum Node {
    #[doc(hidden)]
    Grid(GridNode),
    #[doc(hidden)]
    Cell(Cell),
}

impl From<Grid> for Node {
    fn from(g: Grid) -> Self {
        Node::Grid(g.node)
    }
}

impl From<Cell> for Node {
    fn from(c: Cell) -> Self {
        Node::Cell(c)
    }
}

/// A grid (composite) node in the layout tree. Build top-down with
/// [`Grid::new`], attach children with [`Grid::place`], then call
/// [`Grid::solve`] on the root.
pub struct Grid {
    pub(crate) node: GridNode,
}

#[doc(hidden)]
pub struct GridNode {
    pub(crate) cols: Vec<Track>,
    pub(crate) rows: Vec<Track>,
    pub(crate) gap: (Extent, Extent),
    pub(crate) respect: Respect,
    pub(crate) id: Option<CellId>,
    pub(crate) children: Vec<(Placement, Node)>,
}

/// Per-grid respect policy. Mirrors R `grid`'s `respect` argument:
/// `None` lets each axis size independently; `All` couples every fr
/// track across both axes (today's `Grid::respect()` behaviour); `Matrix`
/// selectively couples only the (row, col) cells marked `true` so the
/// unrespected fr tracks absorb whatever slack remains.
#[derive(Clone, Debug, Default)]
pub enum Respect {
    /// Each axis sizes independently. Default.
    #[default]
    None,
    /// Every (row, col) pair is respected — couples per-fr-w and per-fr-h
    /// across the grid.
    All,
    /// Per-cell respect. `Matrix[row][col] = true` couples that cell's row
    /// and column to the global respected scale; `false` cells let their
    /// row/column stretch with the unrespected remainder. Empty matrix is
    /// treated as `None`.
    Matrix(Vec<Vec<bool>>),
}

impl Respect {
    /// True if any cell in column `col` is respected. For `All`, always
    /// true. For `Matrix`, true if any row at `col` is marked.
    pub(crate) fn col_respected(&self, col: usize) -> bool {
        match self {
            Respect::None => false,
            Respect::All => true,
            Respect::Matrix(m) => m.iter().any(|row| row.get(col).copied().unwrap_or(false)),
        }
    }

    /// True if any cell in row `row` is respected. For `All`, always true.
    /// For `Matrix`, true if any col at `row` is marked.
    pub(crate) fn row_respected(&self, row: usize) -> bool {
        match self {
            Respect::None => false,
            Respect::All => true,
            Respect::Matrix(m) => m
                .get(row)
                .map(|cols| cols.iter().any(|b| *b))
                .unwrap_or(false),
        }
    }
}

impl Grid {
    /// n columns × m rows.
    pub fn new(
        cols: impl IntoIterator<Item = Track>,
        rows: impl IntoIterator<Item = Track>,
    ) -> Self {
        Self {
            node: GridNode {
                cols: cols.into_iter().collect(),
                rows: rows.into_iter().collect(),
                gap: (Extent::ZERO, Extent::ZERO),
                respect: Respect::None,
                id: None,
                children: Vec::new(),
            },
        }
    }

    /// An empty leaf cell — shorthand for [`Cell::empty`]. Use as a tagged
    /// placeholder inside a parent grid track.
    pub fn cell() -> Cell {
        Cell::empty()
    }

    /// Tag this node with an id so its resolved rect is retrievable from
    /// [`Layout::rect`].
    pub fn id(mut self, id: CellId) -> Self {
        self.node.id = Some(id);
        self
    }

    /// Force every `Fr` track across both axes to share a single per-fr
    /// pixel size (R grid's `respect = TRUE`). The grid's natural aspect
    /// ratio `sum_fr_cols : sum_fr_rows` is preserved; the grid shrinks
    /// to fit the available cell area and is centered within it.
    ///
    /// Specific aspect ratios are expressed by choosing fr weights:
    /// a 16:9 single cell is `Grid::new([Fr(16.0)], [Fr(9.0)]).respect()`.
    pub fn respect(mut self) -> Self {
        self.node.respect = Respect::All;
        self
    }

    /// Selectively respect a single `(row, col)` cell. Couples that cell's
    /// row-fr and column-fr to the global respected scale (R grid's
    /// `respect = matrix(...)` with one `1` cell). Unrespected fr tracks
    /// absorb any remaining slack — use this to compose a fixed-aspect
    /// plot beside a flex plot and have the flex plot expand to fill.
    ///
    /// Indices are 0-based and clamped to the current `rows.len()` /
    /// `cols.len()`. Subsequent calls accumulate. If the matrix didn't
    /// exist yet, it is allocated sized to the current grid; if
    /// `respect()` (all) was called previously, this call replaces it
    /// with a single-cell matrix.
    pub fn respect_at(mut self, row: usize, col: usize) -> Self {
        let nrows = self.node.rows.len();
        let ncols = self.node.cols.len();
        if row >= nrows || col >= ncols {
            return self;
        }
        let m = match std::mem::replace(&mut self.node.respect, Respect::None) {
            Respect::Matrix(mut m) => {
                // Resize to current grid shape if it had grown.
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
            _ => vec![vec![false; ncols]; nrows],
        };
        let mut m = m;
        m[row][col] = true;
        self.node.respect = Respect::Matrix(m);
        self
    }

    /// Set the full respect matrix directly. Rows beyond `rows.len()` and
    /// cols beyond `cols.len()` are clipped at solve time.
    pub fn respect_matrix(mut self, m: Vec<Vec<bool>>) -> Self {
        self.node.respect = Respect::Matrix(m);
        self
    }

    /// Gap between columns / rows.
    pub fn gap(mut self, col: Extent, row: Extent) -> Self {
        self.node.gap = (col, row);
        self
    }

    /// Place a child (either a [`Grid`] or a [`Cell`]) at the given position
    /// within this grid. Multiple children may occupy overlapping cells; they
    /// will overlap visually in the order they were placed.
    /// Chainable form, matching every other builder on this type and
    /// [`Composition::place`](crate::composition::Composition::place).
    /// Use [`Self::place_mut`] when building in a loop.
    #[must_use]
    pub fn place(mut self, placement: Placement, child: impl Into<Node>) -> Self {
        self.place_mut(placement, child);
        self
    }

    /// [`Self::place`] against a `&mut` binding, for loops that would
    /// otherwise have to thread the grid through each iteration.
    pub fn place_mut(&mut self, placement: Placement, child: impl Into<Node>) {
        self.node.children.push((placement, child.into()));
    }

    /// Solve for a viewport of `size` pixels. `dpi` converts physical units
    /// (`Mm`/`Cm`/`Inch`/`Pt`) to pixels — a common screen value is 96.
    pub fn solve(&self, size: Size, dpi: f64) -> Layout {
        solver::solve(&self.node, size, dpi)
    }

    /// Solve, reporting a failure instead of producing a layout.
    ///
    /// The grid solver itself is total — every track configuration
    /// resolves to *some* geometry — so this exists for parity with
    /// [`Composition::try_solve`](crate::composition::Composition::try_solve)
    /// and for callers that want one fallible entry point across both
    /// layers. It currently never returns `Err`.
    pub fn try_solve(&self, size: Size, dpi: f64) -> Result<Layout, LayoutError> {
        Ok(self.solve(size, dpi))
    }
}

/// Failure modes of [`Grid::try_solve`]. Empty today — the solver is
/// total — but present so the fallible signature is stable.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayoutError {}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}

impl std::error::Error for LayoutError {}

/// Position of a child within a parent grid.
#[derive(Clone, Debug)]
pub struct Placement {
    /// 1-indexed row position of the child's top-left corner.
    pub row: u16,
    /// 1-indexed column position of the child's top-left corner.
    pub col: u16,
    /// Number of rows the child spans. Treated as 1 if 0.
    pub row_span: u16,
    /// Number of columns the child spans. Treated as 1 if 0.
    pub col_span: u16,
    /// Optional insets relative to the parent's grid cell area edges.
    pub inset: Inset,
}

impl Placement {
    /// Place at the given 1-indexed (row, col), span 1×1, no insets.
    pub fn at(row: u16, col: u16) -> Self {
        Self {
            row,
            col,
            row_span: 1,
            col_span: 1,
            inset: Inset::default(),
        }
    }

    /// Set the row and column span. Zero is treated as 1.
    pub fn span(mut self, rows: u16, cols: u16) -> Self {
        self.row_span = rows.max(1);
        self.col_span = cols.max(1);
        self
    }

    /// Set the inset within the grid cell area.
    pub fn inset(mut self, inset: Inset) -> Self {
        self.inset = inset;
        self
    }
}

/// Position of a placement's bounding rect within its grid cell area.
///
/// The four edge fields ([`left`](Self::left), [`right`](Self::right),
/// [`top`](Self::top), [`bottom`](Self::bottom)) are offsets from the cell
/// area's edges. The two size fields ([`width`](Self::width),
/// [`height`](Self::height)) are explicit dimensions.
///
/// For each axis the rules are:
/// - If only edges are set, the dimension is derived as
///   `cell_dim - leading - trailing` (unset edges contribute 0).
/// - If an explicit dimension is set, it wins. The unset edge of that axis
///   acts as the anchor:
///   - `width(2cm).right(0)` → right-anchored 2cm-wide child
///   - `width(2cm).left(1cm)` → starts 1cm from the left, 2cm wide
///   - `width(2cm)` with neither edge set → left-anchored (0 from left)
///
/// When `width`/`height` is set and *both* edges are also set, the explicit
/// dimension wins and the trailing edge (right/bottom) is ignored.
#[derive(Clone, Debug, Default)]
pub struct Inset {
    pub left: Option<Extent>,
    pub right: Option<Extent>,
    pub top: Option<Extent>,
    pub bottom: Option<Extent>,
    pub width: Option<Extent>,
    pub height: Option<Extent>,
}

impl Inset {
    /// The same offset on all four edges.
    pub fn all(extent: Extent) -> Self {
        Self::default()
            .left(extent.clone())
            .right(extent.clone())
            .top(extent.clone())
            .bottom(extent)
    }

    /// Set the left edge offset from the cell area.
    pub fn left(mut self, l: Extent) -> Self {
        self.left = Some(l);
        self
    }
    /// Set the right edge offset from the cell area.
    pub fn right(mut self, l: Extent) -> Self {
        self.right = Some(l);
        self
    }
    /// Set the top edge offset from the cell area.
    pub fn top(mut self, l: Extent) -> Self {
        self.top = Some(l);
        self
    }
    /// Set the bottom edge offset from the cell area.
    pub fn bottom(mut self, l: Extent) -> Self {
        self.bottom = Some(l);
        self
    }
    /// Set an explicit width; the unset horizontal edge anchors the child.
    pub fn width(mut self, l: Extent) -> Self {
        self.width = Some(l);
        self
    }
    /// Set an explicit height; the unset vertical edge anchors the child.
    pub fn height(mut self, l: Extent) -> Self {
        self.height = Some(l);
        self
    }
}

/// Flat output of solving a layout.
pub struct Layout {
    /// Bounding rect of the root — equal to the viewport passed to `solve`.
    pub root: Rect,
    pub(crate) rects: HashMap<CellId, Rect>,
}

impl Layout {
    /// Resolved pixel rect for the node tagged with `id`, if any.
    pub fn rect(&self, id: CellId) -> Option<Rect> {
        self.rects.get(&id).copied()
    }

    /// Iterate every tagged node.
    pub fn iter(&self) -> impl Iterator<Item = (CellId, Rect)> + '_ {
        self.rects.iter().map(|(k, v)| (*k, *v))
    }

    /// Shift every resolved rect (and the root) by `(dx, dy)` pixels.
    /// Used to centre a layout inside a larger surface when the
    /// composition's natural aspect leaves slack on one or both axes.
    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.root = Rect::new(
            self.root.x0 + dx,
            self.root.y0 + dy,
            self.root.x1 + dx,
            self.root.y1 + dy,
        );
        for rect in self.rects.values_mut() {
            *rect = Rect::new(rect.x0 + dx, rect.y0 + dy, rect.x1 + dx, rect.y1 + dy);
        }
    }
}
