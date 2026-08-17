//! The intrinsic-size protocol for content leaves: the [`Measure`] trait,
//! its [`WidthHint`] reply, and the [`Cell`] leaf that carries one.

use super::CellId;

/// Intrinsic-size protocol for content leaves (text, images, charts, etc.).
///
/// The solver runs in two passes: first widths, then heights. `width_hint`
/// is queried during the width pass — the implementation should return the
/// content's minimum width independent of its height (or signal that the
/// width depends on height via [`WidthHint::NeedsHeight`]). After the height
/// pass produces an allocated width, `height_at` is queried.
///
/// `width_at` is consulted only during iteration for cells that returned
/// [`WidthHint::NeedsHeight`]. The default returns 0, which is correct for
/// content that uses [`WidthHint::Min`].
pub trait Measure {
    /// Report this leaf's intrinsic width — either a stable minimum
    /// ([`WidthHint::Min`]) or a height-dependent value that opts the
    /// leaf into iteration ([`WidthHint::NeedsHeight`]).
    fn width_hint(&self, dpi: f64) -> WidthHint;

    /// Report this leaf's intrinsic height when allocated `width`
    /// pixels.
    fn height_at(&self, width: f64, dpi: f64) -> f64;

    /// Report a width given a resolved height. Consulted only during
    /// iteration for cells that returned [`WidthHint::NeedsHeight`].
    /// Default `0.0` is correct for content that uses
    /// [`WidthHint::Min`].
    fn width_at(&self, _height: f64, _dpi: f64) -> f64 {
        0.0
    }
}

/// What pass 1 (the width pass) can know about a [`Cell`]'s width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WidthHint {
    /// Stable minimum width independent of height. The common case.
    Min(f64),
    /// Width depends on height. `seed` is the lower-bound width used in the
    /// first iteration (e.g. the longest unbreakable word for wrapped text).
    /// The solver then queries [`Measure::width_at`] with the resolved height
    /// and re-runs up to `MAX_ITER` times.
    NeedsHeight { seed: f64 },
}

/// A leaf cell in the layout tree. Carries an optional [`Measure`] and an
/// optional [`CellId`]. Build with [`Cell::empty`] or [`Cell::measured`];
/// shorthand: [`Grid::cell`](super::Grid::cell) returns `Cell::empty()`.
pub struct Cell {
    pub(crate) measure: Box<dyn Measure>,
    pub(crate) id: Option<CellId>,
}

impl Cell {
    /// An empty leaf with zero intrinsic size. Useful as a tagged placeholder
    /// inside a parent grid track.
    pub fn empty() -> Self {
        Self {
            measure: Box::new(EmptyMeasure),
            id: None,
        }
    }

    /// A leaf whose intrinsic size comes from `m`.
    pub fn measured(m: impl Measure + 'static) -> Self {
        Self {
            measure: Box::new(m),
            id: None,
        }
    }

    /// Like [`Self::measured`] but takes an already-boxed measure.
    /// Used when a caller has extracted a `Box<dyn Measure>` from
    /// another cell (via [`Self::into_measure`]) and wants to
    /// re-wrap it without unboxing.
    pub fn measured_boxed(m: Box<dyn Measure>) -> Self {
        Self {
            measure: m,
            id: None,
        }
    }

    /// Tag this cell so its resolved rect is retrievable from
    /// [`Layout::rect`](super::Layout::rect).
    pub fn id(mut self, id: CellId) -> Self {
        self.id = Some(id);
        self
    }

    /// Consume this cell and return its [`Measure`]. Callers that
    /// need to merge multiple cells into one (e.g. the orchestrator
    /// when several plots contribute to the same patch slot) extract
    /// the inner measures here and wrap them in
    /// [`MaxMergeMeasure`].
    pub fn into_measure(self) -> Box<dyn Measure> {
        self.measure
    }

    /// Borrow this cell's identifier tag, if any.
    pub fn cell_id(&self) -> Option<CellId> {
        self.id
    }
}

struct EmptyMeasure;

/// A [`Measure`] that delegates to a stack of child measures and
/// reports the **max** across them on every query. Used when the
/// orchestrator merges multiple contributions to the same slot from
/// different plots — the resulting cell sizes itself to fit
/// whichever child needs the most space.
///
/// Width-hint merging picks `Min(max_min)` when every child reports
/// `Min`; if any child opts into iteration via
/// [`WidthHint::NeedsHeight`], the wrapper does too with the max
/// seed. The iteration loop then queries `width_at` on the wrapper,
/// which max-merges across the same children.
pub struct MaxMergeMeasure {
    children: Vec<Box<dyn Measure>>,
}

impl MaxMergeMeasure {
    /// Build a wrapper around a non-empty stack of measures. Passing
    /// an empty `Vec` is allowed; the wrapper then behaves like
    /// an empty measure (zero on every query).
    pub fn new(children: Vec<Box<dyn Measure>>) -> Self {
        Self { children }
    }
}

impl Measure for MaxMergeMeasure {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        let mut max_min: f64 = 0.0;
        let mut any_needs_height = false;
        for c in &self.children {
            match c.width_hint(dpi) {
                WidthHint::Min(w) => max_min = max_min.max(w),
                WidthHint::NeedsHeight { seed } => {
                    any_needs_height = true;
                    max_min = max_min.max(seed);
                }
            }
        }
        if any_needs_height {
            WidthHint::NeedsHeight { seed: max_min }
        } else {
            WidthHint::Min(max_min)
        }
    }

    fn height_at(&self, width: f64, dpi: f64) -> f64 {
        self.children
            .iter()
            .map(|c| c.height_at(width, dpi))
            .fold(0.0_f64, f64::max)
    }

    fn width_at(&self, height: f64, dpi: f64) -> f64 {
        self.children
            .iter()
            .map(|c| c.width_at(height, dpi))
            .fold(0.0_f64, f64::max)
    }
}

impl Measure for EmptyMeasure {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(0.0)
    }
    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        0.0
    }
}
