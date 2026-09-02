//! The pick index: what a scene recorded, and the queries that read it.
//!
//! Entries are held in draw order, so an entry's position **is** its z
//! order and nothing has to be sorted at insert. The R-tree over them is
//! built lazily on the first query after a frame, which is what lets a
//! window redrawing faster than it is queried never build one at all.

use std::cell::RefCell;

use crate::geometry::{Affine, Point, Rect, Shape};

use crate::brush::Image;
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::clip::{as_axis_rect, ClipId, ClipStack};
use crate::pick::geom::{self, Geom, GeomStore};
use crate::pick::rtree::{to_bbox, Bbox, HilbertRtree};
use crate::pick::scope::{PickPath, PickScope, ScopeNode, ScopeTree};
use crate::pick::PickId;
use crate::scene::GlyphRun;
use crate::stroke::Stroke;

/// Em multiples used to synthesize a glyph run's box.
///
/// A run arrives as positioned glyph ids with no metrics attached, and the
/// font that could supply them is not reachable from a module that compiles
/// with no features at all. These cover a CJK or emoji face rather than
/// hugging a Latin one, so the box is a generous **layout** box: leading and
/// side bearings are hittable, which is what a text hit target should be.
const GLYPH_ASCENT_EM: f64 = 1.0;
const GLYPH_DESCENT_EM: f64 = 0.3;
/// Advance allowance for the last glyph when the run carries no source text.
const GLYPH_TRAILING_EM: f64 = 0.6;

/// One recorded primitive.
#[derive(Debug)]
struct Entry {
    /// Device → local. The exact test runs in the primitive's own frame, so
    /// shared geometry needs no per-primitive copy.
    inv: Affine,
    /// Bounds in that local frame.
    local: Rect,
    geom: Geom,
    pick_id: PickId,
    clip: ClipId,
    scope: ScopeNode,
}

/// A primitive found under a query point or region.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Hit<'a> {
    /// The authoring id. [`PickId::Skip`] for chrome, which is a target by
    /// virtue of its scope rather than by carrying an id.
    pub pick_id: PickId,
    /// The scope chain this primitive was drawn inside.
    pub path: PickPath<'a>,
    /// Draw order within the frame; higher is nearer the front.
    pub order: u32,
    /// Device-space bounds of the primitive — for anchoring a tooltip.
    pub bounds: Rect,
}

impl Hit<'_> {
    /// The authoring id, if this hit carries one.
    pub fn id(&self) -> Option<u32> {
        match self.pick_id {
            PickId::Id(n) if n != 0 => Some(n),
            _ => None,
        }
    }
}

/// Everything a scene recorded for hit testing.
///
/// Built by [`PickIndexScene`](crate::pick::PickIndexScene) as a scene is
/// drawn, then queried by point, rectangle or lasso.
#[derive(Debug, Default)]
pub struct PickIndex {
    entries: Vec<Entry>,
    /// Device-space bounds, parallel to `entries`, in tree storage form.
    leaves: Vec<Bbox>,
    store: GeomStore,
    clips: ClipStack,
    scopes: ScopeTree,
    tree: RefCell<Option<HilbertRtree>>,
    scratch: RefCell<Vec<u32>>,
    clip_memo: RefCell<Vec<(ClipId, bool)>>,
}

impl PickIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of indexed primitives. Not a count of distinct ids: one mark
    /// can be a fill plus a stroke, and a long line is many chunks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop everything recorded. The frame boundary.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.leaves.clear();
        self.store.clear();
        self.clips.clear();
        self.scopes.clear();
        *self.tree.borrow_mut() = None;
        self.clip_memo.borrow_mut().clear();
    }

    // ── Recording ───────────────────────────────────────────────────────

    /// Whether a primitive with this id should be indexed at all.
    ///
    /// An id makes it a target; so does sitting directly inside a
    /// [`ScopeMode::Target`](crate::pick::ScopeMode::Target) scope, which is
    /// how chrome participates without every chrome call site growing a
    /// `PickId` argument.
    fn wants(&self, pick_id: PickId) -> bool {
        pick_id != PickId::Skip || self.scopes.current_is_target()
    }

    pub(crate) fn push_scope(&mut self, scope: &PickScope) {
        self.scopes.push(scope);
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn push_clip(&mut self, transform: Affine, clip: &Path) {
        self.clips.push(transform, clip);
    }

    pub(crate) fn pop_clip(&mut self) {
        self.clips.pop();
    }

    pub(crate) fn record_fill(
        &mut self,
        rule: FillRule,
        transform: Affine,
        path: &Path,
        pick_id: PickId,
    ) {
        if !self.wants(pick_id) || path.elements().is_empty() {
            return;
        }
        // An axis-aligned rectangle is its own bounds, so it needs no stored
        // geometry at all — the bar-chart case, where a fresh path per row
        // would otherwise defeat interning.
        if let Some(r) = as_axis_rect(path) {
            self.push_entry(transform, r, Geom::Box, pick_id);
            return;
        }
        let shape = self.store.intern_path(path);
        let local = self.store.path(shape).bounding_box();
        let even_odd = geom::is_even_odd(rule);
        self.push_entry(transform, local, Geom::Fill { shape, even_odd }, pick_id);
    }

    pub(crate) fn record_stroke(
        &mut self,
        stroke: &Stroke,
        transform: Affine,
        path: &Path,
        pick_id: PickId,
    ) {
        if !self.wants(pick_id) || path.elements().is_empty() {
            return;
        }
        let scale = geom::mean_scale(transform);
        let tolerance = if scale > 0.0 { 0.25 / scale } else { 0.25 };
        let shape = self.store.intern_path(path);
        let (poly, runs) = self.store.flatten_path(shape, tolerance);
        let half_width = geom::hit_half_width(stroke.width, transform);
        let outset = geom::stroke_outset(half_width, stroke.miter_limit);
        for (start, end) in runs {
            for (first, count) in geom::chunk_run(start, end) {
                let pts = &self.store.poly(poly)[first as usize..(first + count) as usize];
                let Some(bounds) = geom::points_bounds(pts) else {
                    continue;
                };
                self.push_entry(
                    transform,
                    bounds.inflate(outset, outset),
                    Geom::Stroke {
                        poly,
                        first,
                        count,
                        half_width,
                    },
                    pick_id,
                );
            }
        }
    }

    pub(crate) fn record_image(&mut self, image: &Image, transform: Affine, pick_id: PickId) {
        if !self.wants(pick_id) || image.width == 0 || image.height == 0 {
            return;
        }
        let local = Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height));
        self.push_entry(transform, local, Geom::Box, pick_id);
    }

    pub(crate) fn record_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        if !self.wants(pick_id) || run.glyphs.is_empty() {
            return;
        }
        let Some(local) = glyph_run_box(run) else {
            return;
        };
        self.push_entry(run.transform, local, Geom::Box, pick_id);
    }

    pub(crate) fn record_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId) {
        if !self.wants(pick_id) || mesh.indices.is_empty() {
            return;
        }
        let (first, count) = self.store.push_triangles(mesh);
        for (chunk_first, chunk_count) in geom::chunk_triangles(first, count) {
            let tris = self.store.triangles(chunk_first, chunk_count);
            let Some(bounds) = geom::triangles_bounds(tris) else {
                continue;
            };
            self.push_entry(
                transform,
                bounds,
                Geom::Tris {
                    first: chunk_first,
                    count: chunk_count,
                },
                pick_id,
            );
        }
    }

    /// Record one entry, dropping it if it is degenerate or clipped away.
    fn push_entry(&mut self, transform: Affine, local: Rect, geom: Geom, pick_id: PickId) {
        // A singular transform paints nothing and has no inverse to test with.
        let Some(inv) = invert(transform) else {
            return;
        };
        let mut device = transform.transform_rect_bbox(local);
        if let Some(clip) = self.clips.current_bounds() {
            // Entirely outside its clip: not drawn, so not hittable. Tested
            // before intersecting, because `Rect::intersect` clamps a
            // disjoint result to zero area rather than leaving it inverted,
            // and a legitimately zero-area primitive must survive.
            if !device.overlaps(clip) {
                return;
            }
            device = device.intersect(clip);
        }
        if !device.x0.is_finite()
            || !device.y0.is_finite()
            || !device.x1.is_finite()
            || !device.y1.is_finite()
        {
            return;
        }
        self.leaves.push(to_bbox(device));
        self.entries.push(Entry {
            inv,
            local,
            geom,
            pick_id,
            clip: self.clips.current(),
            scope: self.scopes.current(),
        });
        *self.tree.borrow_mut() = None;
    }

    // ── Queries ─────────────────────────────────────────────────────────

    /// Every hit at `p`, topmost first, stopping at the first
    /// [`PickId::Block`].
    ///
    /// `p` is in the scene's own coordinate space — device pixels, the space
    /// a `PlotComposition` was rendered at.
    pub fn hits_at(&self, p: Point) -> Vec<Hit<'_>> {
        let mut out = Vec::new();
        self.hits_at_into(p, &mut out);
        out
    }

    /// [`Self::hits_at`] into a caller-owned buffer, for a hover loop that
    /// runs on every pointer event.
    pub fn hits_at_into<'a>(&'a self, p: Point, out: &mut Vec<Hit<'a>>) {
        self.collect(
            out,
            true,
            |tree, scratch| tree.query_point(p, scratch),
            |e, memo| self.hit_by_point(e, p, memo),
        );
    }

    /// The topmost authoring id at `p`, or `None` over empty space or an
    /// occluder. `hits_at(p)` filtered to the first hit carrying an id.
    pub fn pick_at(&self, p: Point) -> Option<u32> {
        self.hits_at(p).iter().find_map(Hit::id)
    }

    /// Every hit whose bounds intersect `rect`, topmost first.
    ///
    /// Bounds-level: a mark whose box clips the rect but whose geometry
    /// misses it is included. See [`Self::hits_within`] for the exact one.
    pub fn hits_in(&self, rect: Rect) -> Vec<Hit<'_>> {
        let mut out = Vec::new();
        self.hits_in_into(rect, &mut out);
        out
    }

    /// [`Self::hits_in`] into a caller-owned buffer.
    pub fn hits_in_into<'a>(&'a self, rect: Rect, out: &mut Vec<Hit<'a>>) {
        self.collect(
            out,
            false,
            |tree, scratch| tree.query_rect(rect, scratch),
            |_, _| true,
        );
    }

    /// Every hit whose bounds lie entirely inside `rect`, topmost first.
    ///
    /// Exact: bounds inside the rect implies the geometry is, so this is the
    /// query a selection marquee wants.
    pub fn hits_within(&self, rect: Rect) -> Vec<Hit<'_>> {
        let mut out = Vec::new();
        self.hits_within_into(rect, &mut out);
        out
    }

    /// [`Self::hits_within`] into a caller-owned buffer.
    pub fn hits_within_into<'a>(&'a self, rect: Rect, out: &mut Vec<Hit<'a>>) {
        self.collect(
            out,
            false,
            |tree, scratch| tree.query_rect(rect, scratch),
            |_, _| true,
        );
        out.retain(|h| {
            h.bounds.x0 >= rect.x0
                && h.bounds.y0 >= rect.y0
                && h.bounds.x1 <= rect.x1
                && h.bounds.y1 <= rect.y1
        });
    }

    /// Lasso selection: every hit whose bounds-centre lies inside `path`.
    ///
    /// Centre-based rather than enclosure-based, and deliberately so. For a
    /// rectangle, bounds-inside-rect implies geometry-inside-rect, which is
    /// what lets [`Self::hits_within`] promise exactness; for an arbitrary
    /// polygon it does not, because a concave lasso can exclude part of a
    /// box whose corners all fall inside it. Centre-in-polygon is the
    /// conventional lasso semantic and is predictable for the small marks a
    /// lasso is used on.
    pub fn hits_in_path(&self, path: &Path, rule: FillRule) -> Vec<Hit<'_>> {
        let mut out = Vec::new();
        self.hits_in_path_into(path, rule, &mut out);
        out
    }

    /// [`Self::hits_in_path`] into a caller-owned buffer.
    pub fn hits_in_path_into<'a>(&'a self, path: &Path, rule: FillRule, out: &mut Vec<Hit<'a>>) {
        let bbox = path.bounding_box();
        self.collect(
            out,
            false,
            |tree, scratch| tree.query_rect(bbox, scratch),
            |_, _| true,
        );
        let even_odd = geom::is_even_odd(rule);
        out.retain(|h| {
            let c = h.bounds.center();
            let w = path.winding(c);
            if even_odd {
                w % 2 != 0
            } else {
                w != 0
            }
        });
    }

    /// Shared query body.
    ///
    /// `descend` picks candidates off the tree; `keep` decides whether a
    /// candidate really is hit, and owns the clip test because only the
    /// point query has a point to test a clip with. `occlude` stops the walk
    /// at the first [`PickId::Block`] — right for a point, which is a ray,
    /// and wrong for a region, which is not.
    fn collect<'a>(
        &'a self,
        out: &mut Vec<Hit<'a>>,
        occlude: bool,
        descend: impl Fn(&HilbertRtree, &mut Vec<u32>),
        keep: impl Fn(&Entry, &mut Vec<(ClipId, bool)>) -> bool,
    ) {
        out.clear();
        if self.entries.is_empty() {
            return;
        }
        self.ensure_tree();
        let tree = self.tree.borrow();
        let tree = tree.as_ref().expect("built above");

        let mut scratch = self.scratch.borrow_mut();
        descend(tree, &mut scratch);
        // Entry order is draw order, so descending is topmost first.
        scratch.sort_unstable_by(|a, b| b.cmp(a));

        let mut memo = self.clip_memo.borrow_mut();
        memo.clear();

        let mut last: Option<(PickId, ScopeNode)> = None;
        for &i in scratch.iter() {
            let e = &self.entries[i as usize];
            if !keep(e, &mut memo) {
                continue;
            }
            if occlude && e.pick_id == PickId::Block {
                return;
            }
            // A fill-plus-stroke mark, a chunked stroke and a chunked mesh
            // all produce several entries a caller should see as one hit.
            // The first one kept is topmost, so it fixes the order; the rest
            // only widen the reported bounds.
            let key = (e.pick_id, e.scope);
            let bounds = rect_of(&self.leaves[i as usize]);
            if last == Some(key) {
                if let Some(prev) = out.last_mut() {
                    prev.bounds = prev.bounds.union(bounds);
                }
                continue;
            }
            last = Some(key);
            out.push(Hit {
                pick_id: e.pick_id,
                path: PickPath::new(&self.scopes, e.scope),
                order: i,
                bounds,
            });
        }
    }

    fn ensure_tree(&self) {
        let mut slot = self.tree.borrow_mut();
        if slot.is_none() {
            *slot = Some(HilbertRtree::pack(&self.leaves));
        }
    }

    /// The exact test for a point: the primitive's own geometry, then every
    /// clip enclosing it.
    fn hit_by_point(&self, e: &Entry, p: Point, memo: &mut Vec<(ClipId, bool)>) -> bool {
        let local = e.inv * p;
        if !e.local.contains(local) || !self.store.contains(&e.geom, e.local, local) {
            return false;
        }
        e.clip.is_none() || self.clips.allows(e.clip, p, memo)
    }
}

/// The layout box of a glyph run, in the run's own frame.
fn glyph_run_box(run: &GlyphRun<'_>) -> Option<Rect> {
    let first = run.glyphs.first()?;
    let (mut x0, mut x1) = (f64::from(first.x), f64::from(first.x));
    let (mut y0, mut y1) = (f64::from(first.y), f64::from(first.y));
    for g in run.glyphs {
        x0 = x0.min(f64::from(g.x));
        x1 = x1.max(f64::from(g.x));
        y0 = y0.min(f64::from(g.y));
        y1 = y1.max(f64::from(g.y));
    }
    let size = f64::from(run.font_size);
    // The last glyph's own advance is not in its origin, so add one.
    match run.source.as_ref() {
        Some(src) => x1 = x1.max(x0 + f64::from(src.advance)),
        None => x1 += size * GLYPH_TRAILING_EM,
    }
    // A skew leans the glyphs out of their origins' box.
    if let Some(gt) = run.glyph_transform {
        let skew = gt.as_coeffs()[2].abs() * size;
        x0 -= skew;
        x1 += skew;
    }
    let r = Rect::new(
        x0,
        y0 - size * GLYPH_ASCENT_EM,
        x1,
        y1 + size * GLYPH_DESCENT_EM,
    );
    r.is_finite().then_some(r)
}

fn invert(t: Affine) -> Option<Affine> {
    let det = t.determinant();
    (det.abs() > 1e-12 && det.is_finite()).then(|| t.inverse())
}

fn rect_of(b: &Bbox) -> Rect {
    Rect::new(b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    impl PickIndex {
        /// What `hits_at` must agree with: every entry, back to front, with
        /// no tree and no coalescing. Deliberately the dumbest correct
        /// implementation.
        fn naive_ids_at(&self, p: Point) -> Vec<u32> {
            let mut memo = Vec::new();
            let mut out = Vec::new();
            for (i, e) in self.entries.iter().enumerate().rev() {
                memo.clear();
                if !self.hit_by_point(e, p, &mut memo) {
                    continue;
                }
                if e.pick_id == PickId::Block {
                    break;
                }
                if let PickId::Id(n) = e.pick_id {
                    if out.last() != Some(&(i as u32)) {
                        out.push(n);
                    }
                }
            }
            out
        }
    }

    /// A scatter of small circles plus a few stroked lines and rects, with
    /// enough marks to force real tree levels.
    fn populated() -> PickIndex {
        let mut ix = PickIndex::new();
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut unit = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let marker = primitives::circle(Point::new(0.0, 0.0), 4.0);
        for i in 0..2000u32 {
            let x = unit() * 400.0;
            let y = unit() * 300.0;
            ix.record_fill(
                FillRule::NonZero,
                Affine::translate((x, y)),
                &marker,
                PickId::Id(i + 1),
            );
        }
        for i in 0..50u32 {
            let y = unit() * 300.0;
            let mut line = Path::new();
            line.move_to((0.0, y));
            line.line_to((400.0, y + 10.0));
            ix.record_stroke(
                &Stroke::new(3.0),
                Affine::IDENTITY,
                &line,
                PickId::Id(10_000 + i),
            );
        }
        ix
    }

    #[test]
    fn hits_agree_with_a_naive_scan_over_every_entry() {
        let ix = populated();
        assert!(
            ix.len() > 2000,
            "the fixture must exercise real tree levels"
        );

        let mut s = 0xDEAD_BEEF_CAFE_1234u64;
        let mut unit = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        for _ in 0..3000 {
            let p = Point::new(unit() * 420.0 - 10.0, unit() * 320.0 - 10.0);
            let got: Vec<u32> = ix.hits_at(p).iter().filter_map(Hit::id).collect();
            let want = ix.naive_ids_at(p);
            assert_eq!(got, want, "disagreement at {p:?}");
        }
    }

    #[test]
    fn the_tree_is_built_lazily_and_invalidated_by_recording() {
        let mut ix = PickIndex::new();
        ix.record_fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &primitives::rect(Rect::new(0.0, 0.0, 10.0, 10.0)),
            PickId::Id(1),
        );
        // Recording alone builds nothing: a frame nobody queries costs no
        // tree, which is what replaces the old pick-interval throttle.
        assert!(ix.tree.borrow().is_none());

        assert_eq!(ix.pick_at(Point::new(5.0, 5.0)), Some(1));
        assert!(ix.tree.borrow().is_some());

        // A further draw invalidates it rather than leaving a stale answer.
        ix.record_fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &primitives::rect(Rect::new(0.0, 0.0, 10.0, 10.0)),
            PickId::Id(2),
        );
        assert!(ix.tree.borrow().is_none());
        assert_eq!(ix.pick_at(Point::new(5.0, 5.0)), Some(2));
    }

    #[test]
    fn a_glyph_run_box_covers_the_line_and_is_finite() {
        use crate::scene::Glyph;
        let glyphs = [
            Glyph {
                id: 1,
                x: 0.0,
                y: 0.0,
            },
            Glyph {
                id: 2,
                x: 20.0,
                y: 0.0,
            },
        ];
        let font = crate::scene::Font::new(crate::brush::Blob::from(vec![0u8; 4]), 0);
        let brush = crate::brush::Brush::Solid(crate::color::rgb8(0, 0, 0));
        let run = GlyphRun {
            font: &font,
            font_size: 10.0,
            transform: Affine::IDENTITY,
            glyph_transform: None,
            brush: &brush,
            brush_alpha: 1.0,
            hint: false,
            glyphs: &glyphs,
            style: None,
            source: None,
        };
        let b = glyph_run_box(&run).expect("a box");
        // Ascent above the baseline, descent below it.
        assert!(b.y0 < 0.0 && b.y1 > 0.0, "{b:?}");
        // Wide enough for the last glyph's own advance.
        assert!(b.x1 > 20.0, "{b:?}");
        assert!(b.x0 <= 0.0, "{b:?}");
    }
}
