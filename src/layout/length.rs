//! [`Extent`], the layout module's measurement type, and its arithmetic.
//!
//! An `Extent` resolves to pixels against a dpi and, for
//! [`Extent::TrackOf`], against another grid's already-resolved tracks. It is
//! distinct from [`crate::style_vocab::Length`], the typographic measurement.

use std::ops::{Add, Div, Mul, Neg, Sub};

use super::{Axis, CellId};

/// A length value. Internally either a linear combination of pixels, inches,
/// and percentage of the containing axis, a deferred `min`/`max` of two
/// sub-lengths (because `min(absolute, percent)` cannot be reduced without
/// knowing the axis size), or a reference to a tagged grid's resolved track
/// size (which is only known after solve).
///
/// Construct via the `px` / `mm` / `cm` / `inch` / `pt` / `percent`
/// associated functions, [`Extent::min`] / [`Extent::max`], or
/// [`Extent::track_of`] / [`Extent::tracks_of`]. Lengths compose with `+`,
/// `-`, unary `-`, `* f64`, and `/ f64`; addition through `Min`/`Max`
/// distributes exactly (`min(a, b) + c = min(a+c, b+c)`), so arithmetic
/// stays closed without losing structure. `TrackOf` is opaque to arithmetic
/// — it composes with `Min`/`Max` transparently but `+ / - / *` on a tree
/// containing `TrackOf` panics. Reach a multi-segment sum via
/// [`Extent::tracks_of`]'s `span` parameter.
///
/// Physical units (`mm`, `cm`, `inch`, `pt`) are resolved to pixels via the
/// `dpi` passed to [`Grid::solve`](super::Grid::solve). `percent` is taken as a fraction of the
/// relevant axis of the parent's grid cell area; the constructor argument is
/// `0.0..=1.0` (so `Extent::percent(0.5)` is "50%").
#[derive(Clone, Debug, PartialEq)]
pub enum Extent {
    /// Linear combination: `px + inches * dpi + percent * axis`.
    Sum {
        /// DPI-independent pixel offset.
        px: f64,
        /// Physical inches; multiplied by `dpi` at resolution.
        inches: f64,
        /// Fraction of the containing axis (1.0 = 100%).
        percent: f64,
    },
    /// Pointwise minimum of two lengths, evaluated at resolution time.
    Min(Box<Extent>, Box<Extent>),
    /// Pointwise maximum of two lengths, evaluated at resolution time.
    Max(Box<Extent>, Box<Extent>),
    /// Resolves at solve time to the summed resolved size of `span`
    /// consecutive tracks starting at `track` (1-indexed) on the given
    /// `axis` of the [`Grid`](super::Grid) tagged with `id == grid`. For `span > 1`
    /// the corresponding gaps between tracks are included.
    ///
    /// The solver runs as a damped fixed-point iteration over its width
    /// and height passes; on the first iteration `TrackOf` evaluates to
    /// `0` (no prior data); on subsequent iterations it picks up the
    /// resolved track size from the previous iteration. Forward
    /// references (a track that references a track later in the solve)
    /// are handled by iteration; cycles will not converge and exhaust
    /// `MAX_ITER`.
    TrackOf {
        /// Tag of the target [`Grid`](super::Grid) (from [`Grid::id`](super::Grid::id)).
        grid: CellId,
        /// Whether to read column widths or row heights.
        axis: Axis,
        /// 1-indexed start track within the target grid.
        track: u16,
        /// Number of consecutive tracks to sum. Treated as 1 if 0.
        span: u16,
    },
}

impl Extent {
    /// The zero length.
    pub const ZERO: Extent = Extent::Sum {
        px: 0.0,
        inches: 0.0,
        percent: 0.0,
    };

    /// Pure pixels (DPI-independent).
    pub const fn px(v: f64) -> Self {
        Extent::Sum {
            px: v,
            inches: 0.0,
            percent: 0.0,
        }
    }
    /// Millimeters — `v / 25.4` inches.
    pub const fn mm(v: f64) -> Self {
        Extent::Sum {
            px: 0.0,
            inches: v / 25.4,
            percent: 0.0,
        }
    }
    /// Centimeters — `v / 2.54` inches.
    pub const fn cm(v: f64) -> Self {
        Extent::Sum {
            px: 0.0,
            inches: v / 2.54,
            percent: 0.0,
        }
    }
    /// Inches.
    pub const fn inch(v: f64) -> Self {
        Extent::Sum {
            px: 0.0,
            inches: v,
            percent: 0.0,
        }
    }
    /// Points (1pt = 1/72 inch).
    pub const fn pt(v: f64) -> Self {
        Extent::Sum {
            px: 0.0,
            inches: v / 72.0,
            percent: 0.0,
        }
    }
    /// A fraction of the containing axis. `0.5` is 50%.
    pub const fn percent(v: f64) -> Self {
        Extent::Sum {
            px: 0.0,
            inches: 0.0,
            percent: v,
        }
    }

    /// Pointwise minimum of two lengths.
    pub fn min(a: Extent, b: Extent) -> Self {
        Extent::Min(Box::new(a), Box::new(b))
    }
    /// Pointwise maximum of two lengths.
    pub fn max(a: Extent, b: Extent) -> Self {
        Extent::Max(Box::new(a), Box::new(b))
    }

    /// Reference the resolved size of a single track in a tagged grid.
    /// `track` is 1-indexed. See [`Extent::TrackOf`].
    pub const fn track_of(grid: CellId, axis: Axis, track: u16) -> Self {
        Extent::TrackOf {
            grid,
            axis,
            track,
            span: 1,
        }
    }

    /// Reference the resolved summed size of `span` consecutive tracks in
    /// a tagged grid, starting at `start` (1-indexed). Gaps between
    /// tracks are included. See [`Extent::TrackOf`].
    pub const fn tracks_of(grid: CellId, axis: Axis, start: u16, span: u16) -> Self {
        Extent::TrackOf {
            grid,
            axis,
            track: start,
            span: if span == 0 { 1 } else { span },
        }
    }

    /// True if this length has no `percent` term anywhere in its tree and
    /// no [`Extent::TrackOf`] reference (whose value isn't known without
    /// a prior solve pass). Lengths that are absolute can be resolved to
    /// pixels without an axis size or prior resolved tracks (used for
    /// intrinsic-size computation in `Track::Auto`).
    pub fn is_absolute(&self) -> bool {
        match self {
            Extent::Sum { percent, .. } => *percent == 0.0,
            Extent::Min(a, b) | Extent::Max(a, b) => a.is_absolute() && b.is_absolute(),
            Extent::TrackOf { .. } => false,
        }
    }
}

impl Default for Extent {
    fn default() -> Self {
        Extent::ZERO
    }
}

// ─── Arithmetic ──────────────────────────────────────────────────────────────
//
// `Sum + Sum` reduces field-wise. `Sum + Min/Max` (or vice versa) distributes
// the addition through the `Min`/`Max`, preserving exact semantics
// (`min(a, b) + c = min(a + c, b + c)`). The tree can grow under repeated
// arithmetic, but for any well-formed expression the growth is bounded.

impl Add for Extent {
    type Output = Extent;
    fn add(self, rhs: Extent) -> Extent {
        match (self, rhs) {
            (
                Extent::Sum {
                    px: a,
                    inches: b,
                    percent: c,
                },
                Extent::Sum {
                    px: x,
                    inches: y,
                    percent: z,
                },
            ) => Extent::Sum {
                px: a + x,
                inches: b + y,
                percent: c + z,
            },
            (Extent::Min(a, b), other) => {
                let other_clone = other.clone();
                Extent::Min(Box::new(*a + other), Box::new(*b + other_clone))
            }
            (other, Extent::Min(a, b)) => {
                let other_clone = other.clone();
                Extent::Min(Box::new(other + *a), Box::new(other_clone + *b))
            }
            (Extent::Max(a, b), other) => {
                let other_clone = other.clone();
                Extent::Max(Box::new(*a + other), Box::new(*b + other_clone))
            }
            (other, Extent::Max(a, b)) => {
                let other_clone = other.clone();
                Extent::Max(Box::new(other + *a), Box::new(other_clone + *b))
            }
            (Extent::TrackOf { .. }, _) | (_, Extent::TrackOf { .. }) => panic!(
                "Extent::TrackOf cannot participate in +/-/*; \
                 use Extent::tracks_of(.., span = N) for consecutive tracks, \
                 or compose via Extent::min / Extent::max"
            ),
        }
    }
}

impl Neg for Extent {
    type Output = Extent;
    fn neg(self) -> Extent {
        match self {
            Extent::Sum {
                px,
                inches,
                percent,
            } => Extent::Sum {
                px: -px,
                inches: -inches,
                percent: -percent,
            },
            // Negating swaps Min/Max: -min(a,b) = max(-a, -b).
            Extent::Min(a, b) => Extent::Max(Box::new(-*a), Box::new(-*b)),
            Extent::Max(a, b) => Extent::Min(Box::new(-*a), Box::new(-*b)),
            Extent::TrackOf { .. } => panic!(
                "Extent::TrackOf cannot be negated; use Extent::min / Extent::max for composition"
            ),
        }
    }
}

impl Sub for Extent {
    type Output = Extent;
    fn sub(self, rhs: Extent) -> Extent {
        self + (-rhs)
    }
}

impl Mul<f64> for Extent {
    type Output = Extent;
    fn mul(self, k: f64) -> Extent {
        match self {
            Extent::Sum {
                px,
                inches,
                percent,
            } => Extent::Sum {
                px: px * k,
                inches: inches * k,
                percent: percent * k,
            },
            // Distribute. Note: a negative scalar swaps Min/Max in the
            // resulting tree (same reasoning as Neg).
            Extent::Min(a, b) if k >= 0.0 => Extent::Min(Box::new(*a * k), Box::new(*b * k)),
            Extent::Min(a, b) => Extent::Max(Box::new(*a * k), Box::new(*b * k)),
            Extent::Max(a, b) if k >= 0.0 => Extent::Max(Box::new(*a * k), Box::new(*b * k)),
            Extent::Max(a, b) => Extent::Min(Box::new(*a * k), Box::new(*b * k)),
            Extent::TrackOf { .. } => panic!(
                "Extent::TrackOf cannot be scaled by f64; use Extent::min / Extent::max for composition"
            ),
        }
    }
}

impl Mul<Extent> for f64 {
    type Output = Extent;
    fn mul(self, l: Extent) -> Extent {
        l * self
    }
}

impl Div<f64> for Extent {
    type Output = Extent;
    fn div(self, k: f64) -> Extent {
        self * (1.0 / k)
    }
}
