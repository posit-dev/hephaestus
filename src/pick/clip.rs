//! The clip stack, and the memoized test that asks whether a clip lets a
//! point through.
//!
//! Clips only ever *subtract*, which is what makes them cheap here. A clip's
//! bounding box is intersected into every entry's bounds as the entry is
//! recorded, so most of the work happens once at insert rather than per
//! query, and a primitive clipped away entirely is never indexed at all. The
//! exact test runs only for a candidate that has already passed its own
//! geometry test, and only once per distinct clip per query.

use crate::geometry::{Affine, Point, Rect, Shape};

use crate::path::Path;

/// Handle into a [`ClipStack`]'s arena. [`ClipId::NONE`] means unclipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ClipId(u32);

impl ClipId {
    /// No clip applies.
    pub(crate) const NONE: ClipId = ClipId(u32::MAX);

    pub(crate) fn is_none(self) -> bool {
        self == ClipId::NONE
    }
}

#[derive(Debug)]
struct ClipNode {
    /// `transform * clip`, baked the way the vector backends bake theirs, so
    /// the test needs no transform of its own.
    path: Path,
    /// This clip's bounds intersected with every ancestor's — the
    /// conservative region anything under it can occupy.
    bounds: Rect,
    parent: ClipId,
    /// The baked path is an axis-aligned rectangle, so `bounds` is already
    /// the exact answer and the winding test can be skipped.
    is_rect: bool,
}

/// The stack of clips in effect, plus the arena the entries point into.
///
/// Nodes are never removed: an entry recorded under a clip holds its
/// [`ClipId`] for the life of the frame, and [`Self::clear`] drops the whole
/// arena at once.
#[derive(Debug, Default)]
pub(crate) struct ClipStack {
    nodes: Vec<ClipNode>,
    stack: Vec<ClipId>,
}

impl ClipStack {
    /// The clip in effect for a primitive recorded right now.
    pub(crate) fn current(&self) -> ClipId {
        self.stack.last().copied().unwrap_or(ClipId::NONE)
    }

    /// Conservative bounds of the clip in effect — the region a primitive
    /// recorded now can occupy. Unbounded when nothing is clipping.
    pub(crate) fn current_bounds(&self) -> Option<Rect> {
        let id = self.current();
        (!id.is_none()).then(|| self.nodes[id.0 as usize].bounds)
    }

    /// Enter a clip. An empty path pushes the enclosing clip again, matching
    /// the vector backends, which emit no `clipPath` for one.
    pub(crate) fn push(&mut self, transform: Affine, clip: &Path) {
        let parent = self.current();
        if clip.elements().is_empty() {
            self.stack.push(parent);
            return;
        }
        let baked = if transform == Affine::IDENTITY {
            clip.clone()
        } else {
            transform * clip.clone()
        };
        let own = baked.bounding_box();
        let bounds = match self.parent_bounds(parent) {
            Some(p) => p.intersect(own),
            None => own,
        };
        let is_rect = as_axis_rect(&baked).is_some();
        self.nodes.push(ClipNode {
            path: baked,
            bounds,
            parent,
            is_rect,
        });
        let id = ClipId(self.nodes.len() as u32 - 1);
        self.stack.push(id);
    }

    /// Leave the innermost clip. Unbalanced pops are ignored — the index has
    /// no warning channel, and a malformed scene should not panic a hover.
    pub(crate) fn pop(&mut self) {
        self.stack.pop();
    }

    /// Forget every clip. Called at the frame boundary.
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
        self.stack.clear();
    }

    fn parent_bounds(&self, parent: ClipId) -> Option<Rect> {
        (!parent.is_none()).then(|| self.nodes[parent.0 as usize].bounds)
    }

    /// Whether `p` survives `clip` and every clip enclosing it.
    ///
    /// `memo` carries verdicts within one query: a clip shared by thousands
    /// of candidates — the panel clip over a dense scatter — is evaluated
    /// once. The caller clears it per query.
    pub(crate) fn allows(&self, clip: ClipId, p: Point, memo: &mut Vec<(ClipId, bool)>) -> bool {
        let mut id = clip;
        // Ancestors visited on the way up, so each gets its own memo entry
        // rather than only the node we were asked about.
        let mark = memo.len();
        while !id.is_none() {
            if let Some(&(_, verdict)) = memo.iter().find(|&&(k, _)| k == id) {
                if !verdict {
                    Self::memo_all(memo, mark, false);
                }
                return verdict;
            }
            let node = &self.nodes[id.0 as usize];
            let inside = node.bounds.contains(p) && (node.is_rect || node.path.contains(p));
            if !inside {
                memo.push((id, false));
                Self::memo_all(memo, mark, false);
                return false;
            }
            memo.push((id, true));
            id = node.parent;
        }
        true
    }

    /// Overwrite verdicts recorded during this walk. A rejection anywhere up
    /// the chain rejects every descendant we passed through.
    fn memo_all(memo: &mut [(ClipId, bool)], from: usize, verdict: bool) {
        for slot in &mut memo[from..] {
            slot.1 = verdict;
        }
    }
}

/// The rectangle a path describes, if it describes one exactly.
///
/// Recognises the `MoveTo` + three or four `LineTo` (+ optional `ClosePath`)
/// shape that rect constructors emit, with all edges axis-aligned. Bar charts
/// build a fresh path per row, so catching this keeps them from interning
/// hundreds of thousands of near-identical paths.
pub(crate) fn as_axis_rect(path: &Path) -> Option<Rect> {
    use crate::geometry::PathEl;
    // A fixed buffer, not a `Vec`: this runs on every fill, and one heap
    // allocation per mark is a real cost at scatter densities.
    let mut pts = [Point::ZERO; 5];
    let mut n = 0usize;
    for el in path.elements() {
        match el {
            PathEl::MoveTo(p) => {
                if n != 0 {
                    return None; // more than one subpath
                }
                pts[0] = *p;
                n = 1;
            }
            PathEl::LineTo(p) => {
                if n == 0 || n >= 5 {
                    return None;
                }
                pts[n] = *p;
                n += 1;
            }
            PathEl::ClosePath => {}
            _ => return None, // any curve disqualifies it
        }
    }
    // A closed rect is 4 corners, or 5 with the first repeated.
    if n == 5 {
        if !near(pts[4], pts[0]) {
            return None;
        }
        n = 4;
    }
    if n != 4 {
        return None;
    }
    let pts = &pts[..4];
    // An axis-aligned rect uses exactly two x values and two y values, and
    // its four corners are the four distinct pairings of them.
    let (mut xs, mut ys): (Vec<f64>, Vec<f64>) = (
        pts.iter().map(|p| p.x).collect(),
        pts.iter().map(|p| p.y).collect(),
    );
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (x0, x1) = (xs[0], xs[3]);
    let (y0, y1) = (ys[0], ys[3]);
    // Two of each value, so the middle pair must match the outer ones.
    if (xs[1] - x0).abs() > EPS || (xs[2] - x1).abs() > EPS {
        return None;
    }
    if (ys[1] - y0).abs() > EPS || (ys[2] - y1).abs() > EPS {
        return None;
    }
    // Each corner distinct: a degenerate or zigzag quad repeats one.
    for i in 0..4 {
        for j in i + 1..4 {
            if near(pts[i], pts[j]) {
                return None;
            }
        }
    }
    Some(Rect::new(x0, y0, x1, y1))
}

const EPS: f64 = 1e-9;

fn near(a: Point, b: Point) -> bool {
    (a.x - b.x).abs() <= EPS && (a.y - b.y).abs() <= EPS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    fn rect_path(r: Rect) -> Path {
        primitives::rect(r)
    }

    #[test]
    fn a_rect_path_is_recognised_and_a_curved_one_is_not() {
        let r = Rect::new(10.0, 20.0, 110.0, 220.0);
        let got = as_axis_rect(&rect_path(r)).expect("rect not recognised");
        assert!((got.x0 - r.x0).abs() < 1e-9 && (got.y0 - r.y0).abs() < 1e-9);
        assert!((got.x1 - r.x1).abs() < 1e-9 && (got.y1 - r.y1).abs() < 1e-9);

        // A rounded rect has curves, so it is not the fast path.
        assert!(as_axis_rect(&primitives::rounded_rect(r, 8.0)).is_none());
        assert!(as_axis_rect(&primitives::circle(Point::new(0.0, 0.0), 5.0)).is_none());
    }

    #[test]
    fn a_rotated_or_degenerate_quad_is_not_an_axis_rect() {
        // Diamond: four corners, no axis-aligned edge.
        let mut d = Path::new();
        d.move_to((10.0, 0.0));
        d.line_to((20.0, 10.0));
        d.line_to((10.0, 20.0));
        d.line_to((0.0, 10.0));
        d.close_path();
        assert!(as_axis_rect(&d).is_none());

        // Zero-width: two corners coincide with two others.
        let flat = rect_path(Rect::new(5.0, 5.0, 5.0, 20.0));
        assert!(as_axis_rect(&flat).is_none());

        // Two subpaths is not one rect.
        let mut two = rect_path(Rect::new(0.0, 0.0, 1.0, 1.0));
        two.move_to((5.0, 5.0));
        two.line_to((6.0, 6.0));
        assert!(as_axis_rect(&two).is_none());
    }

    #[test]
    fn a_clip_shrinks_to_the_intersection_of_its_ancestors() {
        let mut cs = ClipStack::default();
        assert!(cs.current().is_none());
        assert_eq!(cs.current_bounds(), None);

        cs.push(
            Affine::IDENTITY,
            &rect_path(Rect::new(0.0, 0.0, 100.0, 100.0)),
        );
        assert_eq!(cs.current_bounds(), Some(Rect::new(0.0, 0.0, 100.0, 100.0)));

        cs.push(
            Affine::IDENTITY,
            &rect_path(Rect::new(50.0, 50.0, 200.0, 200.0)),
        );
        // Cumulative, not just the innermost.
        assert_eq!(
            cs.current_bounds(),
            Some(Rect::new(50.0, 50.0, 100.0, 100.0))
        );

        cs.pop();
        assert_eq!(cs.current_bounds(), Some(Rect::new(0.0, 0.0, 100.0, 100.0)));
        cs.pop();
        assert!(cs.current().is_none());
    }

    #[test]
    fn the_push_transform_is_baked_into_the_clip() {
        let mut cs = ClipStack::default();
        cs.push(
            Affine::translate((1000.0, 0.0)),
            &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
        );
        let b = cs.current_bounds().expect("clipped");
        assert!((b.x0 - 1000.0).abs() < 1e-9, "got {b:?}");
        assert!((b.x1 - 1010.0).abs() < 1e-9, "got {b:?}");

        let mut memo = Vec::new();
        assert!(cs.allows(cs.current(), Point::new(1005.0, 5.0), &mut memo));
        memo.clear();
        assert!(!cs.allows(cs.current(), Point::new(5.0, 5.0), &mut memo));
    }

    #[test]
    fn an_arbitrary_curved_clip_is_tested_exactly() {
        let mut cs = ClipStack::default();
        // A circle of radius 50 at (50, 50): the rect corner is outside it.
        cs.push(
            Affine::IDENTITY,
            &primitives::circle(Point::new(50.0, 50.0), 50.0),
        );
        let id = cs.current();
        let mut memo = Vec::new();

        assert!(cs.allows(id, Point::new(50.0, 50.0), &mut memo));
        memo.clear();
        // Inside the bounding box, outside the circle — only an exact test
        // rejects this, which is the whole point.
        assert!(!cs.allows(id, Point::new(2.0, 2.0), &mut memo));
    }

    #[test]
    fn an_empty_clip_path_pushes_the_enclosing_clip_unchanged() {
        let mut cs = ClipStack::default();
        cs.push(
            Affine::IDENTITY,
            &rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
        );
        let outer = cs.current();
        cs.push(Affine::IDENTITY, &Path::new());
        assert_eq!(cs.current(), outer);
        assert_eq!(cs.current_bounds(), Some(Rect::new(0.0, 0.0, 10.0, 10.0)));
        cs.pop();
        assert_eq!(cs.current(), outer);
    }

    #[test]
    fn a_rejection_anywhere_up_the_chain_rejects_the_leaf() {
        let mut cs = ClipStack::default();
        cs.push(
            Affine::IDENTITY,
            &rect_path(Rect::new(0.0, 0.0, 20.0, 20.0)),
        );
        cs.push(
            Affine::IDENTITY,
            &rect_path(Rect::new(0.0, 0.0, 100.0, 100.0)),
        );
        let inner = cs.current();
        let mut memo = Vec::new();

        // Inside the inner clip but outside the outer one.
        assert!(!cs.allows(inner, Point::new(50.0, 50.0), &mut memo));
        // Both the leaf and its ancestor are memoized as rejecting, so a
        // second candidate under the same clip costs a lookup, not a test.
        assert!(memo.iter().all(|&(_, v)| !v), "memo = {memo:?}");
        assert!(!cs.allows(inner, Point::new(50.0, 50.0), &mut memo));
    }

    #[test]
    fn the_memo_answers_repeated_candidates_under_one_clip() {
        let mut cs = ClipStack::default();
        cs.push(
            Affine::IDENTITY,
            &primitives::rounded_rect(Rect::new(0.0, 0.0, 100.0, 100.0), 20.0),
        );
        let id = cs.current();
        let mut memo = Vec::new();
        for _ in 0..1000 {
            assert!(cs.allows(id, Point::new(50.0, 50.0), &mut memo));
        }
        // One clip touched, so one entry however many candidates asked.
        assert_eq!(memo.len(), 1);
    }

    #[test]
    fn an_unbalanced_pop_does_not_panic() {
        let mut cs = ClipStack::default();
        cs.pop();
        cs.pop();
        assert!(cs.current().is_none());
        // And the stack still works afterwards.
        cs.push(Affine::IDENTITY, &rect_path(Rect::new(0.0, 0.0, 1.0, 1.0)));
        assert!(!cs.current().is_none());
    }

    #[test]
    fn clear_drops_the_arena_and_the_stack() {
        let mut cs = ClipStack::default();
        cs.push(Affine::IDENTITY, &rect_path(Rect::new(0.0, 0.0, 1.0, 1.0)));
        cs.clear();
        assert!(cs.current().is_none());
        assert_eq!(cs.current_bounds(), None);
    }
}
