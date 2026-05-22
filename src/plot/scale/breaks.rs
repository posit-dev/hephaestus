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

/// Legacy "nice numbers" algorithm — picks step ∈ `{1, 2, 5, 10} × 10^k`
/// closest to the raw step. Kept as a fallback for `wilkinson_extended`
/// when the search produces nothing (degenerate inputs).
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
}
