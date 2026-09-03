//! A packed Hilbert R-tree over axis-aligned boxes.
//!
//! Static and bulk-built: leaves are sorted onto a Hilbert curve, then parent
//! levels are packed [`NODE_SIZE`] at a time until one node is left. There is
//! no insert or remove — a scene is indexed wholesale and thrown away, which
//! is what lets the layout be three flat vectors with no per-node allocation.
//!
//! Boxes are `f32` and rounded **outward** from the `f64` originals, so the
//! tree over-reports rather than under-reports: a false positive is caught by
//! the caller's exact test, while a false negative would be a missed hit with
//! nothing to catch it.

use crate::geometry::{Point, Rect};

/// Children per internal node.
const NODE_SIZE: usize = 16;

/// Below this many leaves, no parent levels are built and a query scans them
/// linearly. Packing a handful of boxes costs more than testing them, and
/// chrome-only scenes live here.
const LINEAR_SCAN_MAX: usize = 256;

/// An axis-aligned box in the tree's own storage form.
pub(crate) type Bbox = [f32; 4];

/// Widen a `f64` rect to the `f32` box the tree stores, rounding outward.
pub(crate) fn to_bbox(r: Rect) -> Bbox {
    [
        round_down(r.x0),
        round_down(r.y0),
        round_up(r.x1),
        round_up(r.y1),
    ]
}

fn round_down(v: f64) -> f32 {
    let f = v as f32;
    if (f as f64) <= v {
        f
    } else {
        f32::from_bits(if f.is_sign_negative() {
            f.to_bits() + 1
        } else {
            f.to_bits().wrapping_sub(1)
        })
    }
}

fn round_up(v: f64) -> f32 {
    let f = v as f32;
    if (f as f64) >= v {
        f
    } else {
        f32::from_bits(if f.is_sign_negative() {
            f.to_bits().wrapping_sub(1)
        } else {
            f.to_bits() + 1
        })
    }
}

#[inline]
fn contains_point(b: &Bbox, x: f32, y: f32) -> bool {
    x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]
}

#[inline]
fn intersects(b: &Bbox, q: &Bbox) -> bool {
    b[0] <= q[2] && b[2] >= q[0] && b[1] <= q[3] && b[3] >= q[1]
}

#[inline]
fn union_into(acc: &mut Bbox, b: &Bbox) {
    acc[0] = acc[0].min(b[0]);
    acc[1] = acc[1].min(b[1]);
    acc[2] = acc[2].max(b[2]);
    acc[3] = acc[3].max(b[3]);
}

const EMPTY: Bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];

/// A bulk-built R-tree returning leaf payload indices.
#[derive(Debug, Default)]
pub(crate) struct HilbertRtree {
    /// Leaf boxes in Hilbert order, then each parent level, root last.
    boxes: Vec<Bbox>,
    /// For a leaf, the caller's index. For an internal node, the offset of
    /// its first child in the level below.
    refs: Vec<u32>,
    /// Exclusive end offset of each level in `boxes`, leaves first.
    level_bounds: Vec<u32>,
    /// Leaf count, i.e. where the leaf level ends.
    num_items: usize,
}

impl HilbertRtree {
    /// Build a tree over `leaves`, whose positions become the payload
    /// indices the queries report.
    pub(crate) fn pack(leaves: &[Bbox]) -> Self {
        let n = leaves.len();
        if n == 0 {
            return Self::default();
        }
        if n <= LINEAR_SCAN_MAX {
            return Self {
                boxes: leaves.to_vec(),
                refs: (0..n as u32).collect(),
                level_bounds: vec![n as u32],
                num_items: n,
            };
        }

        let mut extent = EMPTY;
        for b in leaves {
            union_into(&mut extent, b);
        }
        let (min_x, min_y) = (extent[0] as f64, extent[1] as f64);
        let span_x = extent[2] as f64 - min_x;
        let span_y = extent[3] as f64 - min_y;

        let mut order: Vec<(u32, u32)> = leaves
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let cx = (b[0] as f64 + b[2] as f64) * 0.5;
                let cy = (b[1] as f64 + b[3] as f64) * 0.5;
                let key = super::hilbert::hilbert_d(
                    super::hilbert::quantise(cx, min_x, span_x),
                    super::hilbert::quantise(cy, min_y, span_y),
                );
                (key, i as u32)
            })
            .collect();
        order.sort_unstable();

        // Leaves, then one entry per node of each parent level.
        let mut level_bounds = vec![n as u32];
        let mut count = n;
        while count > 1 {
            count = count.div_ceil(NODE_SIZE);
            level_bounds.push(level_bounds.last().unwrap() + count as u32);
        }
        let total = *level_bounds.last().unwrap() as usize;

        let mut boxes = vec![EMPTY; total];
        let mut refs = vec![0u32; total];
        for (slot, &(_, src)) in order.iter().enumerate() {
            boxes[slot] = leaves[src as usize];
            refs[slot] = src;
        }

        let mut read = 0usize;
        for &bound in &level_bounds[..level_bounds.len() - 1] {
            let end = bound as usize;
            let mut write = end;
            while read < end {
                let first = read;
                let mut acc = EMPTY;
                for _ in 0..NODE_SIZE {
                    if read >= end {
                        break;
                    }
                    union_into(&mut acc, &boxes[read]);
                    read += 1;
                }
                boxes[write] = acc;
                refs[write] = first as u32;
                write += 1;
            }
        }

        Self {
            boxes,
            refs,
            level_bounds,
            num_items: n,
        }
    }

    /// Number of indexed leaves.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.num_items
    }

    /// Payload indices of every leaf whose box contains `p`, pushed onto
    /// `out` (which is cleared first). Order is unspecified.
    pub(crate) fn query_point(&self, p: Point, out: &mut Vec<u32>) {
        let (x, y) = (p.x as f32, p.y as f32);
        self.query(out, |b| contains_point(b, x, y));
    }

    /// Payload indices of every leaf whose box intersects `rect`.
    pub(crate) fn query_rect(&self, rect: Rect, out: &mut Vec<u32>) {
        let q = to_bbox(rect);
        self.query(out, |b| intersects(b, &q));
    }

    /// Shared descent. `hit` is monotone in the box-containment order —
    /// it must be true for a parent whenever it is true for a child, which
    /// both point and rect predicates are.
    fn query(&self, out: &mut Vec<u32>, hit: impl Fn(&Bbox) -> bool) {
        out.clear();
        if self.num_items == 0 {
            return;
        }
        // No parent levels: the leaves are the whole tree.
        if self.level_bounds.len() == 1 {
            for i in 0..self.num_items {
                if hit(&self.boxes[i]) {
                    out.push(self.refs[i]);
                }
            }
            return;
        }

        let root = self.boxes.len() - 1;
        // Depth is log16(n); 64 frames covers far more than u32 can index.
        let mut stack: Vec<(usize, usize)> = Vec::with_capacity(64);
        stack.push((root, self.level_bounds.len() - 1));
        while let Some((node, level)) = stack.pop() {
            if !hit(&self.boxes[node]) {
                continue;
            }
            if level == 0 {
                out.push(self.refs[node]);
                continue;
            }
            let child_start = self.refs[node] as usize;
            let child_end = (child_start + NODE_SIZE).min(self.level_bounds[level - 1] as usize);
            for c in child_start..child_end {
                stack.push((c, level - 1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random boxes, so a failure reproduces.
    fn boxes(n: usize, seed: u64) -> Vec<Bbox> {
        let mut s = seed;
        let mut unit = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        (0..n)
            .map(|_| {
                let x = unit() * 900.0;
                let y = unit() * 560.0;
                let w = unit() * 20.0;
                let h = unit() * 20.0;
                to_bbox(Rect::new(x, y, x + w, y + h))
            })
            .collect()
    }

    fn brute_point(bs: &[Bbox], p: Point) -> Vec<u32> {
        let (x, y) = (p.x as f32, p.y as f32);
        (0..bs.len() as u32)
            .filter(|&i| contains_point(&bs[i as usize], x, y))
            .collect()
    }

    fn brute_rect(bs: &[Bbox], r: Rect) -> Vec<u32> {
        let q = to_bbox(r);
        (0..bs.len() as u32)
            .filter(|&i| intersects(&bs[i as usize], &q))
            .collect()
    }

    fn sorted(mut v: Vec<u32>) -> Vec<u32> {
        v.sort_unstable();
        v
    }

    /// The oracle: whatever the tree returns, a linear scan must agree.
    /// Sizes straddle the linear-scan cutoff and the node-width boundaries,
    /// which is where a packing or level-bound error would hide.
    #[test]
    fn point_queries_agree_with_a_linear_scan_at_every_size() {
        for &n in &[0, 1, 2, 15, 16, 17, 255, 256, 257, 1000, 4096, 4097] {
            let bs = boxes(n, 0x2545_f491_4f6c_dd1d);
            let tree = HilbertRtree::pack(&bs);
            assert_eq!(tree.len(), n);
            let mut out = Vec::new();
            for q in boxes(200, 0x9E37_79B9_7F4A_7C15) {
                let p = Point::new(q[0] as f64, q[1] as f64);
                tree.query_point(p, &mut out);
                assert_eq!(
                    sorted(out.clone()),
                    sorted(brute_point(&bs, p)),
                    "n = {n}, p = {p:?}"
                );
            }
        }
    }

    #[test]
    fn rect_queries_agree_with_a_linear_scan_at_every_size() {
        for &n in &[0, 1, 17, 257, 1000, 4097] {
            let bs = boxes(n, 0x1234_5678_9ABC_DEF0);
            let tree = HilbertRtree::pack(&bs);
            let mut out = Vec::new();
            for q in boxes(60, 0xDEAD_BEEF_CAFE_1234) {
                let r = Rect::new(
                    q[0] as f64,
                    q[1] as f64,
                    q[2] as f64 + 40.0,
                    q[3] as f64 + 40.0,
                );
                tree.query_rect(r, &mut out);
                assert_eq!(
                    sorted(out.clone()),
                    sorted(brute_rect(&bs, r)),
                    "n = {n}, r = {r:?}"
                );
            }
        }
    }

    #[test]
    fn degenerate_geometry_still_answers_correctly() {
        // Every box identical — the Hilbert keys all collide.
        let same = vec![to_bbox(Rect::new(10.0, 10.0, 20.0, 20.0)); 500];
        let tree = HilbertRtree::pack(&same);
        let mut out = Vec::new();
        tree.query_point(Point::new(15.0, 15.0), &mut out);
        assert_eq!(out.len(), 500);
        tree.query_point(Point::new(0.0, 0.0), &mut out);
        assert!(out.is_empty());

        // Zero-area boxes, and an extent with no width at all.
        let column: Vec<Bbox> = (0..400)
            .map(|i| to_bbox(Rect::new(5.0, i as f64, 5.0, i as f64)))
            .collect();
        let tree = HilbertRtree::pack(&column);
        tree.query_point(Point::new(5.0, 42.0), &mut out);
        assert_eq!(
            sorted(out.clone()),
            sorted(brute_point(&column, Point::new(5.0, 42.0)))
        );
    }

    #[test]
    fn an_empty_tree_reports_nothing() {
        let tree = HilbertRtree::pack(&[]);
        let mut out = vec![7, 8, 9];
        tree.query_point(Point::new(0.0, 0.0), &mut out);
        assert!(out.is_empty(), "query must clear the output buffer");
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn stored_boxes_never_shrink_below_the_source_rect() {
        // Outward rounding is what makes a false negative impossible.
        for q in boxes(200, 0x51E7_A11E_D00D) {
            let r = Rect::new(
                q[0] as f64 + 0.123_456_789,
                q[1] as f64 + 0.987_654_321,
                q[2] as f64 + 0.111_111_111,
                q[3] as f64 + 0.222_222_222,
            );
            let b = to_bbox(r);
            assert!(b[0] as f64 <= r.x0, "x0 grew: {} > {}", b[0], r.x0);
            assert!(b[1] as f64 <= r.y0, "y0 grew: {} > {}", b[1], r.y0);
            assert!(b[2] as f64 >= r.x1, "x1 shrank: {} < {}", b[2], r.x1);
            assert!(b[3] as f64 >= r.y1, "y1 shrank: {} < {}", b[3], r.y1);
        }
    }
}
