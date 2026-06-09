//! Nice-number tick generation for continuous numeric domains.
//!
//! Two algorithms live here:
//!
//! - [`extended_breaks`] — Wilkinson Extended labeling (Talbot, Lin,
//!   Hanrahan, 2010). Scores candidates on simplicity / coverage / density
//!   and picks the best. This is the canonical "pretty axis ticks"
//!   algorithm used by R `pretty`, ggplot2, and many others. Default for
//!   continuous axes.
//! - [`linear_breaks`] — exactly `n` evenly-spaced positions from `min` to
//!   `max`. No nice-rounding. Use when the caller wants exact data
//!   coverage (e.g. a fixed-grid rendering).
//!
//! Algorithm is adapted from posit-dev/ggsql with one deviation from the
//! original paper: `Q` is reordered to `[1, 2, 5, 2.5, 4, 3]` (preferring 2
//! over 5) which produces ggplot/R-compatible tick patterns. The paper's
//! ordering `[1, 5, 2, …]` favours over-coverage on inputs like `(0, 10)`
//! whereas users typically expect ticks to fit within the data range.
//!
//! Reference: Talbot, J., Lin, S., & Hanrahan, P. (2010). *An Extension of
//! Wilkinson's Algorithm for Positioning Tick Labels on Axes*.

/// Default target tick count. Most callers pass `n` explicitly; this is the
/// fallback used by chrome cells when the caller doesn't override.
pub const DEFAULT_BREAK_COUNT: usize = 5;

// ─── Wilkinson Extended ──────────────────────────────────────────────────────

/// "Nice" step multipliers in order of preference (most preferred first).
///
/// Adapted from Talbot et al. The original paper's order is
/// `[1, 5, 2, 2.5, 4, 3]`; we swap 5 and 2 so that ticks tend to fit within
/// the data range (matching R's `pretty()` and ggplot2 conventions) on
/// inputs like `(0, 10)`.
const Q: &[f64] = &[1.0, 2.0, 5.0, 2.5, 4.0, 3.0];

// Scoring weights. The paper's defaults are (0.2, 0.25, 0.5, 0.05); we use
// a more coverage-dominated mix that better matches what users expect from
// "pretty" axis ticks. Density still matters (so a `(-10, 10, 5)`-style
// input picks 5 ticks at step 5 rather than 3 ticks at step 10), but
// coverage is weighted heavily enough to keep ticks within the data
// interval on inputs like `(0, 10, 5)`. Weights sum to 1.0.
const W_SIMPLICITY: f64 = 0.25;
const W_COVERAGE: f64 = 0.4;
const W_DENSITY: f64 = 0.3;
const W_LEGIBILITY: f64 = 0.05;

/// Generate "nice" tick positions covering at least the interval
/// `[min, max]`. `n` is a *target* count — the actual length of the
/// returned slice is chosen to optimise simplicity, coverage, and density,
/// and typically lands within ±2 of `n`.
///
/// Edge cases:
/// - Non-finite inputs → `vec![]`.
/// - `min == max` → `vec![min]`.
/// - `min > max` → silently swapped.
/// - `n == 0` or `n == 1` → treated as `n = 2`.
pub fn extended_breaks(min: f64, max: f64, n: usize) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() {
        return Vec::new();
    }
    if min == max {
        return vec![min];
    }
    let (lo, hi) = if min < max { (min, max) } else { (max, min) };
    let n = n.max(2);
    wilkinson_extended(lo, hi, n)
}

/// Core search loop. Assumes `lo < hi`, both finite, `n >= 2`.
fn wilkinson_extended(lo: f64, hi: f64, target_count: usize) -> Vec<f64> {
    let range = hi - lo;

    let mut best_score = f64::NEG_INFINITY;
    let mut best_breaks: Vec<f64> = Vec::new();

    // j = skip factor (1 = every Q value, 2 = every other, …).
    let j_max = target_count.max(10);
    let k_max = (target_count * 2).max(10);
    for j in 1..=j_max {
        for (q_index, &q) in Q.iter().enumerate() {
            let q_score = simplicity_score(q_index, Q.len(), j);

            // Early termination: even with perfect coverage/density/
            // legibility, this q/j pair can't beat the current best.
            if W_SIMPLICITY * q_score + W_COVERAGE + W_DENSITY + W_LEGIBILITY < best_score {
                continue;
            }

            for k in 2..=k_max {
                let density = density_score(k, target_count);
                if W_SIMPLICITY * q_score + W_COVERAGE + W_DENSITY * density + W_LEGIBILITY
                    < best_score
                {
                    continue;
                }

                let delta = (range / (k as f64 - 1.0)) * (j as f64);
                let step = q * nice_step_size(delta / q);
                if step <= 0.0 || !step.is_finite() {
                    continue;
                }

                let nice_min = (lo / step).floor() * step;
                let nice_max = nice_min + step * (k as f64 - 1.0);

                // Must cover the data interval (small tolerance for f64
                // round-off).
                if nice_max + step * 1e-9 < hi {
                    continue;
                }

                let coverage = coverage_score(lo, hi, nice_min, nice_max);
                let legibility = 1.0;

                let score = W_SIMPLICITY * q_score
                    + W_COVERAGE * coverage
                    + W_DENSITY * density
                    + W_LEGIBILITY * legibility;

                if score > best_score {
                    best_score = score;
                    best_breaks = generate_breaks(nice_min, step, k);
                }
            }
        }
    }

    if best_breaks.is_empty() {
        // Fallback to the simple algorithm if the search produced nothing
        // (degenerate ranges, etc.).
        pretty_breaks_simple(lo, hi, target_count)
    } else {
        best_breaks
    }
}

/// Simplicity: prefer earlier `Q` values and smaller skip factors.
fn simplicity_score(q_index: usize, q_len: usize, j: usize) -> f64 {
    1.0 - (q_index as f64) / (q_len as f64) - (j as f64 - 1.0) / 10.0
}

/// Coverage: penalise extending past the data range.
fn coverage_score(data_min: f64, data_max: f64, label_min: f64, label_max: f64) -> f64 {
    let data_range = data_max - data_min;
    let label_range = label_max - label_min;
    if label_range == 0.0 || data_range == 0.0 {
        return 0.0;
    }
    let extension = (label_range - data_range) / data_range;
    (1.0 - 0.5 * extension).max(0.0)
}

/// Density: prefer counts close to the target. Slight under-density is
/// preferred to over-density (which crowds the axis).
fn density_score(actual: usize, target: usize) -> f64 {
    let ratio = actual as f64 / target as f64;
    if ratio >= 1.0 {
        2.0 - ratio
    } else {
        ratio
    }
}

/// Round to the nearest power of 10.
fn nice_step_size(x: f64) -> f64 {
    10_f64.powf(x.log10().round())
}

/// `[start, start + step, …, start + step * (count - 1)]`.
fn generate_breaks(start: f64, step: f64, count: usize) -> Vec<f64> {
    (0..count).map(|i| start + step * i as f64).collect()
}

// ─── Linear (exact-N) breaks ─────────────────────────────────────────────────

/// Generate exactly `n` evenly-spaced positions from `min` to `max`. No
/// nice-rounding — use this when the caller wants pixel-exact gridlines
/// rather than human-readable tick labels.
///
/// Edge cases:
/// - `n == 0` → `vec![]`.
/// - `n == 1` → `vec![(min + max) / 2.0]` (single tick at midpoint).
/// - `min > max` → silently swapped.
/// - non-finite inputs → `vec![]`.
pub fn linear_breaks(min: f64, max: f64, n: usize) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() {
        return Vec::new();
    }
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![(min + max) / 2.0];
    }
    let (lo, hi) = if min < max { (min, max) } else { (max, min) };
    let step = (hi - lo) / (n - 1) as f64;
    (0..n).map(|i| lo + step * i as f64).collect()
}

// ─── Simple fallback ─────────────────────────────────────────────────────────

/// "Nice numbers" algorithm — picks step ∈ `{1, 2, 5, 10} × 10^k` closest
/// to the raw step. Used as a fallback when `wilkinson_extended` produces
/// nothing (degenerate inputs).
fn pretty_breaks_simple(min: f64, max: f64, n: usize) -> Vec<f64> {
    if n == 0 || min >= max {
        return Vec::new();
    }
    let n = n.max(2);
    let range = max - min;
    let rough_step = range / (n - 1) as f64;

    let magnitude = 10_f64.powf(rough_step.log10().floor());
    let residual = rough_step / magnitude;
    let nice_step = if residual <= 1.0 {
        magnitude
    } else if residual <= 2.0 {
        2.0 * magnitude
    } else if residual <= 5.0 {
        5.0 * magnitude
    } else {
        10.0 * magnitude
    };

    let nice_min = (min / nice_step).floor() * nice_step;
    let nice_max = (max / nice_step).ceil() * nice_step;

    let mut out = Vec::new();
    let mut v = nice_min;
    while v <= nice_max + nice_step * 0.5 {
        if v.abs() < nice_step * 1e-12 {
            out.push(0.0);
        } else {
            out.push(v);
        }
        v += nice_step;
    }
    out
}

// ─── Log breaks (1-2-5 across decades; powers-only when many decades) ───────

/// "Pretty" log-spaced major breaks: powers of `base` with optional
/// 1 / 2 / 5 multipliers when the span is ≤ a few decades. Used for
/// log-transformed continuous scales.
///
/// - `min` / `max`: the data range in **input** space (must be > 0).
/// - `n_target`: target number of breaks (informational; the algorithm
///   chooses a multiplier set that fits the span).
/// - `base`: log base (10, 2, or `e` typical).
///
/// Returns the breaks in input space, sorted, all within `[min, max]`.
/// Empty for non-positive / non-finite / degenerate inputs.
pub fn log_pretty_breaks(min: f64, max: f64, n_target: usize, base: f64) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || min <= 0.0 || max <= 0.0 || min >= max || base <= 1.0
    {
        return Vec::new();
    }
    let log_min = min.log(base);
    let log_max = max.log(base);
    let n_decades = log_max - log_min;

    // For wide spans, just powers of base. For narrow spans, expand with
    // 1 / 2 / 5 sub-decade stops. Threshold is loose; tweakable.
    let mults: &[f64] = if n_decades > 4.0 {
        &[1.0]
    } else if n_decades > 1.5 || n_target <= 5 {
        &[1.0, 2.0, 5.0]
    } else {
        &[1.0, 2.0, 3.0, 5.0, 7.0]
    };

    let lo_decade = log_min.floor() as i32;
    let hi_decade = log_max.ceil() as i32;

    let mut result = Vec::new();
    for d in lo_decade..=hi_decade {
        let base_d = base.powi(d);
        for m in mults {
            let v = m * base_d;
            if v >= min && v <= max {
                result.push(v);
            }
        }
    }
    result.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    result.dedup_by(|a, b| (*a - *b).abs() < (*b).abs() * 1e-9);
    result
}

/// Geometric minor breaks between consecutive powers of `base`. Emits
/// `k * base^d` for `k ∈ {2, 3, …, ⌊base⌋ - 1}` and each integer decade
/// `d` overlapping `[min, max]`. The canonical log-axis "subticks
/// between decades" look.
///
/// For base = 10 this produces `2, 3, 4, 5, 6, 7, 8, 9` between each
/// pair of decade powers.
pub fn log_minor_breaks(min: f64, max: f64, base: f64) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || min <= 0.0 || max <= 0.0 || min >= max || base <= 1.0
    {
        return Vec::new();
    }
    let base_int = base.round() as i32;
    if base_int < 3 {
        // base = 2 has no integer multipliers in (1, base) — no minors.
        return Vec::new();
    }
    let log_min = min.log(base);
    let log_max = max.log(base);
    let lo_decade = log_min.floor() as i32;
    let hi_decade = log_max.ceil() as i32;

    let mut result = Vec::new();
    for d in lo_decade..=hi_decade {
        let base_d = base.powi(d);
        for k in 2..base_int {
            let v = k as f64 * base_d;
            if v >= min && v <= max {
                result.push(v);
            }
        }
    }
    result
}

// ─── Sqrt breaks ─────────────────────────────────────────────────────────────

/// Major breaks for a sqrt-transformed scale: Wilkinson-Extended on
/// the sqrt domain, squared back. Produces visually-even spacing in
/// transformed space and clean numbers when squared.
///
/// `min` must be ≥ 0 (sqrt's allowed domain); otherwise returns empty.
pub fn sqrt_breaks(min: f64, max: f64, n: usize) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || min < 0.0 || min >= max {
        return Vec::new();
    }
    let sqrt_min = min.sqrt();
    let sqrt_max = max.sqrt();
    extended_breaks(sqrt_min, sqrt_max, n)
        .into_iter()
        .map(|v| v * v)
        .filter(|v| *v >= min && *v <= max)
        .collect()
}

// ─── Symlog / asinh / pseudo-log breaks ─────────────────────────────────────

/// Major breaks for a symmetric-log-like transform (Asinh / PseudoLog).
/// Handles domains that straddle zero by running [`log_pretty_breaks`] on
/// the positive and negative branches independently and stitching with a
/// zero break in the middle.
///
/// `base` is the log base for the positive / negative branches
/// (typically `e` for Asinh, or 2 / 10 for PseudoLog2 / PseudoLog10).
pub fn symlog_breaks(min: f64, max: f64, n: usize, base: f64) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || min >= max || base <= 1.0 {
        return Vec::new();
    }

    if min >= 0.0 {
        // All non-negative.
        let lo = if min == 0.0 { f64::MIN_POSITIVE } else { min };
        let mut out = log_pretty_breaks(lo, max, n, base);
        if min == 0.0 {
            out.insert(0, 0.0);
        }
        return out;
    }
    if max <= 0.0 {
        // All non-positive: mirror the positive branch.
        let lo = if max == 0.0 { f64::MIN_POSITIVE } else { -max };
        let mut pos = log_pretty_breaks(lo, -min, n, base);
        // pos is ascending in magnitude; reverse so that negating
        // produces an ascending sequence in the negative branch.
        pos.reverse();
        let mut out: Vec<f64> = pos.into_iter().map(|v| -v).collect();
        if max == 0.0 {
            out.push(0.0);
        }
        return out;
    }
    // Straddles zero.
    let n_each = (n / 2).max(2);
    let neg = log_pretty_breaks(f64::MIN_POSITIVE, -min, n_each, base);
    let pos = log_pretty_breaks(f64::MIN_POSITIVE, max, n_each, base);
    let mut out: Vec<f64> = neg.into_iter().rev().map(|v| -v).collect();
    out.push(0.0);
    out.extend(pos);
    out
}

/// Minor breaks for a symmetric-log-like transform. Mirrors
/// [`log_minor_breaks`] on each branch and stitches.
pub fn symlog_minor_breaks(min: f64, max: f64, base: f64) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || min >= max || base <= 1.0 {
        return Vec::new();
    }
    if min >= 0.0 {
        let lo = if min == 0.0 { f64::MIN_POSITIVE } else { min };
        return log_minor_breaks(lo, max, base);
    }
    if max <= 0.0 {
        let lo = if max == 0.0 { f64::MIN_POSITIVE } else { -max };
        let pos = log_minor_breaks(lo, -min, base);
        // pos ascends in magnitude; reverse + negate to ascend in
        // negative-branch values.
        return pos.into_iter().rev().map(|v| -v).collect();
    }
    let neg = log_minor_breaks(f64::MIN_POSITIVE, -min, base);
    let pos = log_minor_breaks(f64::MIN_POSITIVE, max, base);
    let mut out: Vec<f64> = neg.into_iter().rev().map(|v| -v).collect();
    out.extend(pos);
    out
}

// ─── Default linear minor breaks ────────────────────────────────────────────

/// Default minor-break algorithm: `n_per_interval` evenly-spaced points
/// between each consecutive pair of majors, exclusive of the endpoints.
/// For `n_per_interval = 1` this places a single minor at each interval
/// midpoint.
///
/// Used by transforms that don't provide a custom minor algorithm
/// (Identity, Square, Exp*, Sqrt — sqrt is already nice-spaced in input
/// units, so linear subdivision works well).
pub fn linear_minor_breaks_between(majors: &[f64], n_per_interval: usize) -> Vec<f64> {
    if majors.len() < 2 || n_per_interval == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((majors.len() - 1) * n_per_interval);
    for w in majors.windows(2) {
        let a = w[0];
        let b = w[1];
        for i in 1..=n_per_interval {
            let t = i as f64 / (n_per_interval + 1) as f64;
            out.push(a + t * (b - a));
        }
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_slice(actual: &[f64], expected: &[f64], tol: f64) {
        assert_eq!(
            actual.len(),
            expected.len(),
            "length mismatch: actual={actual:?}, expected={expected:?}"
        );
        for (a, e) in actual.iter().zip(expected) {
            assert!(
                (a - e).abs() < tol,
                "tick mismatch: actual={actual:?}, expected={expected:?}"
            );
        }
    }

    // ── extended_breaks ──

    #[test]
    fn breaks_0_to_10_n5() {
        approx_slice(
            &extended_breaks(0.0, 10.0, 5),
            &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0],
            1e-9,
        );
    }

    #[test]
    fn breaks_0_to_100_n5() {
        approx_slice(
            &extended_breaks(0.0, 100.0, 5),
            &[0.0, 20.0, 40.0, 60.0, 80.0, 100.0],
            1e-9,
        );
    }

    #[test]
    fn breaks_0_to_1_n5() {
        approx_slice(
            &extended_breaks(0.0, 1.0, 5),
            &[0.0, 0.2, 0.4, 0.6, 0.8, 1.0],
            1e-9,
        );
    }

    #[test]
    fn breaks_negative_to_positive() {
        approx_slice(
            &extended_breaks(-10.0, 10.0, 5),
            &[-10.0, -5.0, 0.0, 5.0, 10.0],
            1e-9,
        );
    }

    #[test]
    fn breaks_brackets_at_or_above_input() {
        // The chosen step may extend the upper bound slightly past `hi`;
        // every break is finite and the last one must be ≥ hi.
        let bs = extended_breaks(3.0, 7.0, 4);
        assert!(!bs.is_empty());
        assert!(bs.first().copied().unwrap() <= 3.0);
        assert!(bs.last().copied().unwrap() >= 7.0);
    }

    #[test]
    fn breaks_small_decimal_range() {
        approx_slice(
            &extended_breaks(0.01, 0.05, 5),
            &[0.01, 0.02, 0.03, 0.04, 0.05],
            1e-12,
        );
    }

    #[test]
    fn breaks_swapped_inputs_are_handled() {
        let a = extended_breaks(0.0, 10.0, 5);
        let b = extended_breaks(10.0, 0.0, 5);
        approx_slice(&a, &b, 1e-9);
    }

    #[test]
    fn breaks_min_equals_max_returns_single() {
        let bs = extended_breaks(5.0, 5.0, 5);
        assert_eq!(bs, vec![5.0]);
    }

    #[test]
    fn breaks_non_finite_returns_empty() {
        assert!(extended_breaks(f64::NAN, 1.0, 5).is_empty());
        assert!(extended_breaks(0.0, f64::INFINITY, 5).is_empty());
        assert!(extended_breaks(f64::NEG_INFINITY, 0.0, 5).is_empty());
    }

    #[test]
    fn breaks_n_zero_treated_as_two() {
        let bs = extended_breaks(0.0, 10.0, 0);
        assert!(!bs.is_empty());
    }

    #[test]
    fn breaks_n_one_treated_as_two() {
        let bs = extended_breaks(0.0, 10.0, 1);
        assert!(!bs.is_empty());
    }

    #[test]
    fn breaks_count_within_target_band() {
        // For target n=5, the actual count should be in [4, 7] across a
        // range of inputs.
        for hi in [3.0, 7.5, 12.0, 47.0, 99.0, 100.0, 500.0] {
            let bs = extended_breaks(0.0, hi, 5);
            assert!(
                (4..=7).contains(&bs.len()),
                "hi={hi}: got {} breaks ({:?})",
                bs.len(),
                bs
            );
        }
    }

    #[test]
    fn breaks_always_cover_data_interval() {
        // For any reasonable input, the breaks should span [min, max]
        // (first ≤ min, last ≥ max).
        for (lo, hi) in [
            (0.0, 10.0),
            (-5.0, 5.0),
            (1.0, 1000.0),
            (0.0, 0.001),
            (-1e6, 1e6),
            (100.5, 200.7),
        ] {
            let bs = extended_breaks(lo, hi, 5);
            assert!(!bs.is_empty(), "({lo}, {hi}) produced no breaks");
            assert!(
                *bs.first().unwrap() <= lo + 1e-9,
                "({lo}, {hi}): first {} > lo {lo}",
                bs.first().unwrap()
            );
            assert!(
                *bs.last().unwrap() >= hi - 1e-9,
                "({lo}, {hi}): last {} < hi {hi}",
                bs.last().unwrap()
            );
        }
    }

    // ── linear_breaks ──

    #[test]
    fn linear_breaks_exact_n() {
        let bs = linear_breaks(0.0, 1.0, 5);
        approx_slice(&bs, &[0.0, 0.25, 0.5, 0.75, 1.0], 1e-12);
    }

    #[test]
    fn linear_breaks_n0_empty() {
        assert!(linear_breaks(0.0, 1.0, 0).is_empty());
    }

    #[test]
    fn linear_breaks_n1_midpoint() {
        let bs = linear_breaks(0.0, 10.0, 1);
        assert_eq!(bs, vec![5.0]);
    }

    #[test]
    fn linear_breaks_swapped_inputs() {
        let a = linear_breaks(0.0, 10.0, 3);
        let b = linear_breaks(10.0, 0.0, 3);
        approx_slice(&a, &b, 1e-12);
    }

    #[test]
    fn linear_breaks_non_finite_empty() {
        assert!(linear_breaks(f64::NAN, 0.0, 3).is_empty());
        assert!(linear_breaks(0.0, f64::INFINITY, 3).is_empty());
    }

    // ── helper fns ──

    #[test]
    fn simplicity_score_monotone() {
        // Lower q_index → higher score; lower j → higher score.
        assert!(simplicity_score(0, 6, 1) > simplicity_score(1, 6, 1));
        assert!(simplicity_score(0, 6, 1) > simplicity_score(0, 6, 2));
    }

    #[test]
    fn density_score_at_target_is_one() {
        assert_eq!(density_score(5, 5), 1.0);
    }

    #[test]
    fn density_score_under_is_ratio() {
        assert_eq!(density_score(3, 5), 0.6);
    }

    #[test]
    fn density_score_over_is_penalised() {
        // 2 - ratio for over-density.
        assert!((density_score(7, 5) - (2.0 - 7.0 / 5.0)).abs() < 1e-12);
    }

    #[test]
    fn coverage_score_no_extension_is_one() {
        assert_eq!(coverage_score(0.0, 10.0, 0.0, 10.0), 1.0);
    }

    #[test]
    fn coverage_score_extension_reduces() {
        // Extending past data lowers the score.
        let no_ext = coverage_score(0.0, 10.0, 0.0, 10.0);
        let with_ext = coverage_score(0.0, 10.0, 0.0, 20.0);
        assert!(with_ext < no_ext);
        assert!(with_ext >= 0.0);
    }

    // ── log_pretty_breaks ──

    #[test]
    fn log10_pretty_one_decade_includes_powers_of_ten() {
        let b = log_pretty_breaks(1.0, 10.0, 5, 10.0);
        assert!(b.contains(&1.0), "{b:?} missing 1");
        assert!(b.contains(&10.0), "{b:?} missing 10");
    }

    #[test]
    fn log10_pretty_two_decades_has_1_2_5_pattern() {
        let b = log_pretty_breaks(1.0, 100.0, 6, 10.0);
        // Expect the 1, 2, 5, 10, 20, 50, 100 set (a subset suffices —
        // the algorithm may drop the very-narrow stops).
        for v in [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0] {
            assert!(b.contains(&v), "{b:?} missing {v}");
        }
    }

    #[test]
    fn log10_pretty_wide_span_collapses_to_powers() {
        // 6 decades → just powers of 10.
        let b = log_pretty_breaks(1.0, 1_000_000.0, 6, 10.0);
        for v in [1.0, 10.0, 100.0, 1_000.0, 10_000.0, 100_000.0, 1_000_000.0] {
            assert!(b.contains(&v), "{b:?} missing {v}");
        }
        // Should not include 2 or 5 multiples for very wide spans.
        assert!(!b.contains(&2.0));
        assert!(!b.contains(&50_000.0));
    }

    #[test]
    fn log_pretty_invalid_inputs_return_empty() {
        assert!(log_pretty_breaks(0.0, 10.0, 5, 10.0).is_empty());
        assert!(log_pretty_breaks(-1.0, 10.0, 5, 10.0).is_empty());
        assert!(log_pretty_breaks(10.0, 1.0, 5, 10.0).is_empty());
        assert!(log_pretty_breaks(1.0, 10.0, 5, 1.0).is_empty());
        assert!(log_pretty_breaks(f64::NAN, 10.0, 5, 10.0).is_empty());
    }

    // ── log_minor_breaks ──

    #[test]
    fn log10_minor_2_to_9_between_decades() {
        let m = log_minor_breaks(1.0, 100.0, 10.0);
        // 2..9 in the [1, 10] decade, 20..90 in the [10, 100] decade.
        for v in [2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0] {
            assert!(m.contains(&v), "{m:?} missing {v}");
        }
        for v in [20.0, 30.0, 90.0] {
            assert!(m.contains(&v), "{m:?} missing {v}");
        }
    }

    #[test]
    fn log2_minor_empty_no_integer_multipliers_in_band() {
        // base 2 has no integers in (1, 2) so log_minor_breaks is empty.
        assert!(log_minor_breaks(1.0, 8.0, 2.0).is_empty());
    }

    // ── sqrt_breaks ──

    #[test]
    fn sqrt_breaks_squares_back_to_data_space() {
        let b = sqrt_breaks(0.0, 100.0, 5);
        // Expect breaks at squares of evenly-spaced sqrt values.
        // sqrt(0)=0, sqrt(100)=10; Wilkinson over [0, 10] → [0, 2.5, 5, 7.5, 10] ish.
        assert!(!b.is_empty());
        assert!(b.iter().all(|v| (0.0..=100.0).contains(v)));
        // The smallest non-zero should be < 25 (sqrt-evenly-spaced).
        let smallest_nonzero = b.iter().copied().find(|v| *v > 0.0).unwrap();
        assert!(
            smallest_nonzero < 30.0,
            "sqrt breaks should pack tighter at the bottom: {b:?}"
        );
    }

    #[test]
    fn sqrt_breaks_rejects_negative_min() {
        assert!(sqrt_breaks(-1.0, 100.0, 5).is_empty());
    }

    // ── symlog_breaks ──

    #[test]
    fn symlog_positive_branch_only() {
        let b = symlog_breaks(1.0, 100.0, 5, 10.0);
        assert!(!b.is_empty());
        assert!(b.iter().all(|v| *v > 0.0));
    }

    #[test]
    fn symlog_straddles_zero_includes_zero() {
        let b = symlog_breaks(-100.0, 100.0, 6, 10.0);
        assert!(b.contains(&0.0), "{b:?} missing 0");
        assert!(b.iter().any(|v| *v < 0.0), "{b:?} missing negative");
        assert!(b.iter().any(|v| *v > 0.0), "{b:?} missing positive");
    }

    #[test]
    fn symlog_all_negative_branch_mirrors() {
        let b = symlog_breaks(-100.0, -1.0, 5, 10.0);
        assert!(!b.is_empty());
        assert!(b.iter().all(|v| *v < 0.0));
    }

    // ── linear_minor_breaks_between ──

    #[test]
    fn linear_minor_one_per_interval_is_midpoint() {
        let m = linear_minor_breaks_between(&[0.0, 1.0, 2.0], 1);
        approx_slice(&m, &[0.5, 1.5], 1e-12);
    }

    #[test]
    fn linear_minor_three_per_interval_evenly_spaced() {
        let m = linear_minor_breaks_between(&[0.0, 4.0], 3);
        approx_slice(&m, &[1.0, 2.0, 3.0], 1e-12);
    }

    #[test]
    fn linear_minor_empty_for_short_input() {
        assert!(linear_minor_breaks_between(&[], 1).is_empty());
        assert!(linear_minor_breaks_between(&[1.0], 1).is_empty());
        assert!(linear_minor_breaks_between(&[1.0, 2.0], 0).is_empty());
    }
}
