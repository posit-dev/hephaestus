//! The Hilbert d-index, used to order leaves before an R-tree is packed.
//!
//! A Hilbert curve visits every cell of a 2^order × 2^order grid once, and
//! cells close together on the curve are close together in the plane. Sorting
//! leaves by the curve position of their centre is what makes consecutive
//! runs of leaves — which is what a packed node is — occupy a compact region,
//! and so what makes a query prune well.

/// Bits per axis. Inputs are quantised to `0..=65535` before hashing.
pub(crate) const ORDER_BITS: u32 = 16;

/// Position along the Hilbert curve of grid cell `(x, y)`.
///
/// Both coordinates are treated as 16-bit; anything above `0xFFFF` is
/// clamped. The result fits in 32 bits, which is why the tree can sort on a
/// plain `u32` key.
pub(crate) fn hilbert_d(x: u32, y: u32) -> u32 {
    let mut x = x.min(0xFFFF);
    let mut y = y.min(0xFFFF);
    let mut d: u32 = 0;
    let mut s: u32 = 1 << (ORDER_BITS - 1);
    while s > 0 {
        let rx = u32::from(x & s > 0);
        let ry = u32::from(y & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        // Rotate the quadrant so the curve stays continuous across it.
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_sub(1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

/// Quantise `v` from `[min, min + span]` onto the `0..=65535` grid the curve
/// is defined over. A non-positive or non-finite span collapses to `0`, which
/// is what a degenerate axis should do: every leaf lands in the same column
/// and the other axis does the ordering.
pub(crate) fn quantise(v: f64, min: f64, span: f64) -> u32 {
    if !span.is_finite() || span <= 0.0 {
        return 0;
    }
    let t = ((v - min) / span * 65535.0).round();
    if t.is_nan() {
        0
    } else {
        t.clamp(0.0, 65535.0) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The defining property: over a full grid the curve is a bijection.
    #[test]
    fn the_curve_visits_every_cell_of_a_small_grid_exactly_once() {
        // 5 bits' worth of the 16-bit curve, sampled on its own stride.
        let step = 1u32 << (ORDER_BITS - 5);
        let mut seen = HashSet::new();
        for gx in 0..32u32 {
            for gy in 0..32u32 {
                assert!(
                    seen.insert(hilbert_d(gx * step, gy * step)),
                    "duplicate index at ({gx}, {gy})"
                );
            }
        }
        assert_eq!(seen.len(), 32 * 32);
    }

    /// Locality is the whole reason to use the curve: consecutive positions
    /// on it are adjacent cells, never a jump across the grid.
    #[test]
    fn consecutive_curve_positions_are_adjacent_cells() {
        let step = 1u32 << (ORDER_BITS - 4);
        let mut cells: Vec<((u32, u32), u32)> = Vec::new();
        for gx in 0..16u32 {
            for gy in 0..16u32 {
                cells.push(((gx, gy), hilbert_d(gx * step, gy * step)));
            }
        }
        cells.sort_by_key(|&(_, d)| d);
        for w in cells.windows(2) {
            let ((ax, ay), _) = w[0];
            let ((bx, by), _) = w[1];
            let dist = ax.abs_diff(bx) + ay.abs_diff(by);
            assert_eq!(dist, 1, "({ax},{ay}) -> ({bx},{by}) is not a step");
        }
    }

    #[test]
    fn quantise_spans_the_grid_and_survives_degenerate_input() {
        assert_eq!(quantise(0.0, 0.0, 10.0), 0);
        assert_eq!(quantise(10.0, 0.0, 10.0), 65535);
        assert_eq!(quantise(5.0, 0.0, 10.0), 32768);
        // Out of range clamps rather than wrapping.
        assert_eq!(quantise(-1.0, 0.0, 10.0), 0);
        assert_eq!(quantise(11.0, 0.0, 10.0), 65535);
        // Degenerate spans collapse instead of dividing by zero.
        assert_eq!(quantise(5.0, 5.0, 0.0), 0);
        assert_eq!(quantise(5.0, 0.0, f64::NAN), 0);
        assert_eq!(quantise(f64::NAN, 0.0, 10.0), 0);
    }

    #[test]
    fn coordinates_past_the_grid_are_clamped_not_wrapped() {
        assert_eq!(hilbert_d(0x1_0000, 0), hilbert_d(0xFFFF, 0));
        assert_eq!(hilbert_d(0, u32::MAX), hilbert_d(0, 0xFFFF));
    }
}
