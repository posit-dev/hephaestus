//! Per-primitive hit geometry: what a recorded primitive stores, and the
//! exact test that decides whether a point is inside it.
//!
//! Geometry is kept in the primitive's **own** coordinate frame and shared
//! between primitives that draw the same shape; the query point is pushed
//! through the primitive's inverse transform instead. That is what makes a
//! hundred thousand scatter markers cost one stored path — the plot layer
//! draws them all from one `ShapeRegistry` entry, varying only the
//! transform.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::geometry::{flatten, Affine, PathEl, Point, Rect, Shape};

use crate::mesh::Mesh;
use crate::path::{FillRule, Path};

/// Points per stroke chunk, and triangles per mesh chunk.
///
/// Chunking is what keeps a long primitive from degenerating into a linear
/// scan: a ten-thousand-point line is one primitive with a panel-sized
/// bounding box, so without it every hover inside the panel would walk the
/// whole polyline. Split, it becomes many entries with tight boxes that the
/// tree prunes to one or two.
const CHUNK: usize = 64;

/// Paths longer than this are not interned. Hashing them would cost more
/// than the copy saves, and they do not repeat the way markers do.
const INTERN_MAX_ELEMENTS: usize = 64;

/// Half-width floor, in device pixels, for stroke hit testing.
///
/// A hairline is one pixel of ink and impossible to hit exactly, so it gets
/// a pick target a pixel wide either side. The intent the old pick pass met
/// by widening the stroke it rasterised, without distorting anything drawn.
const MIN_HIT_HALF_WIDTH_PX: f64 = 1.0;

/// Miter joins can push a stroke past `half_width` from the centreline; this
/// bounds how far a chunk's box is grown to allow for it.
const MITER_BBOX_LIMIT: f64 = 4.0;

/// A stored path: where its elements live, and its bounds.
///
/// The bounds are cached because computing them is not cheap — a tight box
/// around a cubic means solving for the curve's extrema — and every mark
/// sharing a marker shape would otherwise recompute the same answer.
#[derive(Debug, Clone, Copy)]
struct PathSlot {
    start: u32,
    len: u32,
    bbox: Rect,
}

/// Handle into the shared path arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShapeId(u32);

/// Handle into the shared flattened-polyline arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PolyId(u32);

/// What a recorded primitive is, for the purpose of testing a point against
/// it. Every variant is tested in the primitive's local frame.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Geom {
    /// The entry's local bounds are the exact answer: an axis-aligned
    /// rectangular fill, an image quad, a glyph run's layout box.
    Box,
    Fill {
        shape: ShapeId,
        even_odd: bool,
    },
    Stroke {
        poly: PolyId,
        first: u32,
        count: u32,
        half_width: f64,
    },
    Tris {
        first: u32,
        count: u32,
    },
}

/// Shared storage for everything the exact tests read.
///
/// Paths live in one flat arena of elements rather than as a `Vec<BezPath>`:
/// a scene that draws a hundred thousand *distinct* paths — a low-level
/// caller placing each mark in absolute coordinates rather than reusing one
/// marker under a transform — would otherwise pay a hundred thousand
/// allocations. Slices of the arena are hit-tested directly, since kurbo
/// implements `Shape` for `&[PathEl]`.
#[derive(Debug, Default)]
pub(crate) struct GeomStore {
    path_els: Vec<PathEl>,
    /// One slot per [`ShapeId`], indexing `path_els`.
    path_ranges: Vec<PathSlot>,
    /// Content hash → shapes sharing it. Collisions are resolved by
    /// comparing the elements outright, and the key is already a hash, so
    /// hashing it again would be pure cost — hence [`PassThrough`].
    intern: HashMap<u64, Vec<u32>, BuildHasherDefault<PassThrough>>,
    /// Flattened polylines, one entry per `(shape, tolerance)` pair.
    polys: Vec<Vec<Point>>,
    poly_cache: HashMap<(u32, i32), u32>,
    /// Triangles in their mesh's own frame.
    tris: Vec<[Point; 3]>,
}

impl GeomStore {
    /// Drop everything a frame accumulated, keeping the allocations.
    pub(crate) fn clear(&mut self) {
        self.path_els.clear();
        self.path_ranges.clear();
        self.intern.clear();
        self.polys.clear();
        self.poly_cache.clear();
        self.tris.clear();
    }

    /// Store `path`, reusing an identical one already held this frame.
    pub(crate) fn intern_path(&mut self, path: &Path) -> ShapeId {
        let els = path.elements();
        // Long paths are stored without hashing: the hash would cost more
        // than the lookup saves, and they do not repeat the way markers do.
        if els.len() <= INTERN_MAX_ELEMENTS {
            let key = hash_path(els);
            if let Some(bucket) = self.intern.get(&key) {
                for &id in bucket {
                    let slot = self.path_ranges[id as usize];
                    if &self.path_els[slot.start as usize..(slot.start + slot.len) as usize] == els
                    {
                        return ShapeId(id);
                    }
                }
            }
            let id = self.push_els(els);
            self.intern.entry(key).or_default().push(id.0);
            return id;
        }
        self.push_els(els)
    }

    fn push_els(&mut self, els: &[PathEl]) -> ShapeId {
        let start = self.path_els.len() as u32;
        self.path_els.extend_from_slice(els);
        self.path_ranges.push(PathSlot {
            start,
            len: els.len() as u32,
            bbox: els.bounding_box(),
        });
        ShapeId(self.path_ranges.len() as u32 - 1)
    }

    /// Borrow a stored path's elements.
    pub(crate) fn path(&self, id: ShapeId) -> &[PathEl] {
        let slot = self.path_ranges[id.0 as usize];
        &self.path_els[slot.start as usize..(slot.start + slot.len) as usize]
    }

    /// A stored path's bounds, computed once when it was stored.
    pub(crate) fn path_bounds(&self, id: ShapeId) -> Rect {
        self.path_ranges[id.0 as usize].bbox
    }

    /// Flatten a stored path at `tolerance`, reusing an earlier flattening
    /// at the same tolerance bucket. Returns the polyline plus the ranges
    /// that must not be joined across — one per subpath.
    pub(crate) fn flatten_path(
        &mut self,
        id: ShapeId,
        tolerance: f64,
    ) -> (PolyId, Vec<(u32, u32)>) {
        let bucket = tolerance_bucket(tolerance);
        let key = (id.0, bucket);
        if let Some(&existing) = self.poly_cache.get(&key) {
            let runs = subpath_runs(&self.polys[existing as usize]);
            return (PolyId(existing), runs);
        }
        let mut pts: Vec<Point> = Vec::new();
        let slot = self.path_ranges[id.0 as usize];
        let els: Vec<PathEl> =
            self.path_els[slot.start as usize..(slot.start + slot.len) as usize].to_vec();
        // `f64::NAN` marks a subpath break, so one flat buffer can hold a
        // path with holes without a parallel index.
        flatten(els.iter().copied(), tolerance.max(1e-6), |el| {
            match el {
                PathEl::MoveTo(p) => {
                    if !pts.is_empty() {
                        pts.push(BREAK);
                    }
                    pts.push(p);
                }
                PathEl::LineTo(p) => pts.push(p),
                PathEl::ClosePath => {
                    // Close the ring so the last edge is testable.
                    if let Some(ring_start) = last_subpath_start(&pts) {
                        pts.push(ring_start);
                    }
                }
                _ => {}
            }
        });
        self.polys.push(pts);
        let poly = self.polys.len() as u32 - 1;
        self.poly_cache.insert(key, poly);
        let runs = subpath_runs(&self.polys[poly as usize]);
        (PolyId(poly), runs)
    }

    /// Borrow a stored polyline.
    pub(crate) fn poly(&self, id: PolyId) -> &[Point] {
        &self.polys[id.0 as usize]
    }

    /// Append a mesh's triangles, returning the range they occupy.
    pub(crate) fn push_triangles(&mut self, mesh: &Mesh) -> (u32, u32) {
        let first = self.tris.len() as u32;
        for tri in mesh.indices.chunks_exact(3) {
            let (a, b, c) = (
                mesh.vertices[tri[0] as usize],
                mesh.vertices[tri[1] as usize],
                mesh.vertices[tri[2] as usize],
            );
            self.tris.push([a, b, c]);
        }
        (first, self.tris.len() as u32 - first)
    }

    /// Borrow a range of triangles.
    pub(crate) fn triangles(&self, first: u32, count: u32) -> &[[Point; 3]] {
        &self.tris[first as usize..(first + count) as usize]
    }

    /// Whether `p`, already in the primitive's local frame, is inside it.
    ///
    /// `local` is the entry's own bounds and has already been tested by the
    /// caller, so this is only the part the bounds cannot answer.
    pub(crate) fn contains(&self, geom: &Geom, local: Rect, p: Point) -> bool {
        match *geom {
            Geom::Box => true,
            Geom::Fill { shape, even_odd } => {
                let w = self.path(shape).winding(p);
                if even_odd {
                    w % 2 != 0
                } else {
                    w != 0
                }
            }
            Geom::Stroke {
                poly,
                first,
                count,
                half_width,
            } => {
                let pts = &self.poly(poly)[first as usize..(first + count) as usize];
                let limit = half_width * half_width;
                pts.windows(2).any(|w| {
                    !is_break(w[0]) && !is_break(w[1]) && dist_sq_to_segment(p, w[0], w[1]) <= limit
                })
            }
            Geom::Tris { first, count } => {
                let _ = local;
                self.triangles(first, count)
                    .iter()
                    .any(|t| point_in_triangle(p, t[0], t[1], t[2]))
            }
        }
    }
}

/// A hasher for keys that are already hashes.
///
/// The intern table is keyed by a content hash of a path's elements, so the
/// default SipHash would be a second hash over a good one. Only ever fed a
/// single `u64`; anything else would defeat it, so `write` says so.
#[derive(Default)]
pub(crate) struct PassThrough(u64);

impl Hasher for PassThrough {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _bytes: &[u8]) {
        unreachable!("PassThrough only accepts a single u64 key");
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

/// Sentinel separating subpaths inside a flattened polyline.
const BREAK: Point = Point {
    x: f64::NAN,
    y: f64::NAN,
};

fn is_break(p: Point) -> bool {
    p.x.is_nan()
}

fn last_subpath_start(pts: &[Point]) -> Option<Point> {
    pts.iter()
        .rposition(|p| is_break(*p))
        .map_or_else(|| pts.first().copied(), |i| pts.get(i + 1).copied())
}

/// Contiguous `[start, end)` ranges of a polyline that contain no break.
fn subpath_runs(pts: &[Point]) -> Vec<(u32, u32)> {
    let mut runs = Vec::new();
    let mut start = 0usize;
    for (i, p) in pts.iter().enumerate() {
        if is_break(*p) {
            if i > start {
                runs.push((start as u32, i as u32));
            }
            start = i + 1;
        }
    }
    if pts.len() > start {
        runs.push((start as u32, pts.len() as u32));
    }
    runs
}

/// Split `[start, end)` into overlapping chunks of at most [`CHUNK`] points.
///
/// Chunks share an endpoint so the segment spanning a boundary is tested by
/// exactly one of them rather than falling between the two.
pub(crate) fn chunk_run(start: u32, end: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    if end <= start + 1 {
        return out;
    }
    let mut a = start;
    while a + 1 < end {
        let b = (a + CHUNK as u32).min(end);
        out.push((a, b - a));
        if b >= end {
            break;
        }
        a = b - 1;
    }
    out
}

/// Split a mesh's triangle range into chunks of at most [`CHUNK`].
pub(crate) fn chunk_triangles(first: u32, count: u32) -> Vec<(u32, u32)> {
    (0..count)
        .step_by(CHUNK)
        .map(|off| (first + off, CHUNK.min((count - off) as usize) as u32))
        .collect()
}

/// The pick half-width for a stroke of `width` under `transform`.
///
/// Widths are local, the floor is in device pixels, so the floor is divided
/// back through the transform's mean scale to land in the same frame.
pub(crate) fn hit_half_width(width: f64, transform: Affine) -> f64 {
    let scale = mean_scale(transform);
    let floor = if scale > 0.0 {
        MIN_HIT_HALF_WIDTH_PX / scale
    } else {
        MIN_HIT_HALF_WIDTH_PX
    };
    (width * 0.5).max(floor)
}

/// How much a chunk's bounds must grow to contain the stroke around it.
pub(crate) fn stroke_outset(half_width: f64, miter_limit: f64) -> f64 {
    half_width * miter_limit.clamp(1.0, MITER_BBOX_LIMIT)
}

/// Geometric mean of the transform's axis scales — the factor a length in
/// local units is multiplied by on its way to device space.
pub(crate) fn mean_scale(t: Affine) -> f64 {
    t.determinant().abs().sqrt()
}

/// Bounds of a run of points, ignoring subpath breaks.
pub(crate) fn points_bounds(pts: &[Point]) -> Option<Rect> {
    let mut acc: Option<Rect> = None;
    for p in pts.iter().filter(|p| !is_break(**p)) {
        let r = Rect::new(p.x, p.y, p.x, p.y);
        acc = Some(match acc {
            Some(a) => a.union(r),
            None => r,
        });
    }
    acc
}

/// Bounds of a run of triangles.
pub(crate) fn triangles_bounds(tris: &[[Point; 3]]) -> Option<Rect> {
    let mut acc: Option<Rect> = None;
    for t in tris {
        for p in t {
            let r = Rect::new(p.x, p.y, p.x, p.y);
            acc = Some(match acc {
                Some(a) => a.union(r),
                None => r,
            });
        }
    }
    acc
}

/// Whether a fill rule wants even-odd winding.
pub(crate) fn is_even_odd(rule: FillRule) -> bool {
    matches!(rule, FillRule::EvenOdd)
}

/// Bucket a tolerance so nearby values share one flattening.
fn tolerance_bucket(tolerance: f64) -> i32 {
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return i32::MIN;
    }
    (tolerance.log2() * 4.0).round() as i32
}

fn hash_path(els: &[PathEl]) -> u64 {
    // FNV-1a over the element bit patterns. Only a bucket key — equality is
    // still checked outright — so speed matters more than distribution.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bits: u64| {
        h ^= bits;
        h = h.wrapping_mul(0x1000_0000_01b3);
    };
    for el in els {
        let (tag, pts): (u64, &[Point]) = match el {
            PathEl::MoveTo(p) => (1, std::slice::from_ref(p)),
            PathEl::LineTo(p) => (2, std::slice::from_ref(p)),
            PathEl::QuadTo(a, _) => (3, std::slice::from_ref(a)),
            PathEl::CurveTo(a, _, _) => (4, std::slice::from_ref(a)),
            PathEl::ClosePath => (5, &[]),
        };
        feed(tag);
        for p in pts {
            feed(p.x.to_bits());
            feed(p.y.to_bits());
        }
    }
    feed(els.len() as u64);
    h
}

fn dist_sq_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f64::EPSILON {
        let (ex, ey) = (p.x - a.x, p.y - a.y);
        return ex * ex + ey * ey;
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len_sq).clamp(0.0, 1.0);
    let (cx, cy) = (a.x + t * dx, a.y + t * dy);
    let (ex, ey) = (p.x - cx, p.y - cy);
    ex * ex + ey * ey
}

fn point_in_triangle(p: Point, a: Point, b: Point, c: Point) -> bool {
    let d1 = cross(p, a, b);
    let d2 = cross(p, b, c);
    let d3 = cross(p, c, a);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn cross(p: Point, a: Point, b: Point) -> f64 {
    (p.x - b.x) * (a.y - b.y) - (a.x - b.x) * (p.y - b.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    fn store() -> GeomStore {
        GeomStore::default()
    }

    #[test]
    fn identical_paths_share_one_stored_shape() {
        let mut s = store();
        let a = primitives::circle(Point::new(0.0, 0.0), 3.0);
        let b = primitives::circle(Point::new(0.0, 0.0), 3.0);
        let c = primitives::circle(Point::new(0.0, 0.0), 4.0);

        let ia = s.intern_path(&a);
        let ib = s.intern_path(&b);
        let ic = s.intern_path(&c);
        assert_eq!(ia, ib, "identical paths must intern to one shape");
        assert_ne!(ia, ic);
        assert_eq!(s.path_ranges.len(), 2);

        // Repeating the same shape ten thousand times stores it once — the
        // scatter-marker case the whole scheme exists for.
        for _ in 0..10_000 {
            assert_eq!(s.intern_path(&a), ia);
        }
        assert_eq!(s.path_ranges.len(), 2);
    }

    #[test]
    fn a_long_path_is_stored_without_interning() {
        let mut s = store();
        let mut long = Path::new();
        long.move_to((0.0, 0.0));
        for i in 1..(INTERN_MAX_ELEMENTS + 10) {
            long.line_to((i as f64, 0.0));
        }
        let first = s.intern_path(&long);
        let second = s.intern_path(&long);
        // Two copies: hashing a path this size costs more than it saves.
        assert_ne!(first, second);
    }

    #[test]
    fn clear_drops_everything_a_frame_accumulated() {
        let mut s = store();
        let p = primitives::circle(Point::new(0.0, 0.0), 3.0);
        let id = s.intern_path(&p);
        s.flatten_path(id, 0.25);
        s.clear();
        assert!(s.polys.is_empty());
        assert!(s.path_els.is_empty());
        assert!(s.path_ranges.is_empty());
        // Interning starts over, which costs a handful of hashes: the
        // distinct *shapes* in a frame are few even when the marks are many.
        assert_eq!(s.intern_path(&p), ShapeId(0));
    }

    #[test]
    fn flattening_closes_rings_so_the_last_edge_is_testable() {
        let mut s = store();
        let sq = primitives::rect(Rect::new(0.0, 0.0, 10.0, 10.0));
        let id = s.intern_path(&sq);
        let (poly, runs) = s.flatten_path(id, 0.1);
        assert_eq!(runs.len(), 1, "one subpath");
        let pts = s.poly(poly);
        assert!(
            pts.len() >= 5,
            "closed square needs its first point repeated"
        );
        let (first, last) = (pts[0], *pts.last().unwrap());
        assert!(
            (first.x - last.x).abs() < 1e-9 && (first.y - last.y).abs() < 1e-9,
            "ring not closed: {first:?} vs {last:?}"
        );
    }

    #[test]
    fn a_path_with_a_hole_flattens_into_two_runs() {
        let mut s = store();
        let mut annulus = primitives::circle(Point::new(0.0, 0.0), 10.0);
        annulus.extend(primitives::circle(Point::new(0.0, 0.0), 5.0).iter());
        let id = s.intern_path(&annulus);
        let (poly, runs) = s.flatten_path(id, 0.1);
        assert_eq!(runs.len(), 2, "outer ring and hole are separate runs");
        // Runs never span the break sentinel.
        for &(a, b) in &runs {
            for p in &s.poly(poly)[a as usize..b as usize] {
                assert!(!is_break(*p));
            }
        }
    }

    #[test]
    fn chunks_share_an_endpoint_so_no_segment_falls_between_them() {
        // One short run is a single chunk.
        assert_eq!(chunk_run(0, 5), vec![(0, 5)]);
        // A single point has no segment at all.
        assert!(chunk_run(0, 1).is_empty());
        assert!(chunk_run(7, 7).is_empty());

        // Every segment of a long run is covered exactly once.
        let n = 300u32;
        let chunks = chunk_run(0, n);
        assert!(chunks.len() > 1);
        let mut covered = vec![0u32; (n - 1) as usize];
        for &(first, count) in &chunks {
            assert!(count as usize <= CHUNK);
            for seg in first..first + count - 1 {
                covered[seg as usize] += 1;
            }
        }
        assert!(
            covered.iter().all(|&c| c == 1),
            "segments covered {:?} times",
            covered.iter().collect::<std::collections::BTreeSet<_>>()
        );
    }

    #[test]
    fn triangle_chunks_partition_the_range() {
        let chunks = chunk_triangles(10, 150);
        assert_eq!(chunks.iter().map(|&(_, c)| c).sum::<u32>(), 150);
        assert_eq!(chunks[0].0, 10);
        for &(_, c) in &chunks {
            assert!(c as usize <= CHUNK);
        }
        assert!(chunk_triangles(0, 0).is_empty());
    }

    #[test]
    fn a_hairline_still_gets_a_pixel_of_pick_target() {
        // Under an identity transform the floor is in device units already.
        assert_eq!(hit_half_width(0.1, Affine::IDENTITY), MIN_HIT_HALF_WIDTH_PX);
        // A wide stroke keeps its own half-width.
        assert_eq!(hit_half_width(10.0, Affine::IDENTITY), 5.0);
        // Under a 10x scale, one device pixel is a tenth of a local unit.
        let scaled = hit_half_width(0.01, Affine::scale(10.0));
        assert!((scaled - 0.1).abs() < 1e-12, "got {scaled}");
        // A singular transform must not divide by zero.
        assert!(hit_half_width(0.0, Affine::scale(0.0)).is_finite());
    }

    #[test]
    fn fill_winding_honours_the_rule_over_a_hole() {
        let mut s = store();
        let mut annulus = primitives::circle(Point::new(0.0, 0.0), 10.0);
        annulus.extend(primitives::circle(Point::new(0.0, 0.0), 5.0).iter());
        let shape = s.intern_path(&annulus);
        let local = s.path_bounds(shape);

        let in_ring = Point::new(7.5, 0.0);
        let in_hole = Point::new(0.0, 0.0);

        let even_odd = Geom::Fill {
            shape,
            even_odd: true,
        };
        assert!(s.contains(&even_odd, local, in_ring));
        assert!(
            !s.contains(&even_odd, local, in_hole),
            "even-odd must see the hole"
        );

        // Both rings wind the same way here, so nonzero fills the hole in.
        let nonzero = Geom::Fill {
            shape,
            even_odd: false,
        };
        assert!(s.contains(&nonzero, local, in_ring));
        assert!(s.contains(&nonzero, local, in_hole));
    }

    #[test]
    fn a_stroke_is_hit_within_its_half_width_and_missed_outside_it() {
        let mut s = store();
        let mut line = Path::new();
        line.move_to((0.0, 0.0));
        line.line_to((100.0, 0.0));
        let shape = s.intern_path(&line);
        let (poly, runs) = s.flatten_path(shape, 0.1);
        let (first, count) = (runs[0].0, runs[0].1 - runs[0].0);
        let geom = Geom::Stroke {
            poly,
            first,
            count,
            half_width: 4.0,
        };
        let local = Rect::new(0.0, -4.0, 100.0, 4.0);

        assert!(s.contains(&geom, local, Point::new(50.0, 0.0)));
        assert!(s.contains(&geom, local, Point::new(50.0, 3.99)));
        assert!(!s.contains(&geom, local, Point::new(50.0, 4.01)));
        // Past the end cap, distance is to the endpoint, not the infinite line.
        assert!(s.contains(&geom, local, Point::new(102.0, 0.0)));
        assert!(!s.contains(&geom, local, Point::new(105.0, 0.0)));
    }

    #[test]
    fn a_mesh_is_hit_inside_a_triangle_and_missed_between_them() {
        use crate::color::rgb8;
        let mut s = store();
        let mesh = Mesh::new(
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(0.0, 10.0),
            ],
            vec![rgb8(0, 0, 0); 3],
            vec![0, 1, 2],
        );
        let (first, count) = s.push_triangles(&mesh);
        assert_eq!(count, 1);
        let geom = Geom::Tris { first, count };
        let local = Rect::new(0.0, 0.0, 10.0, 10.0);

        assert!(s.contains(&geom, local, Point::new(1.0, 1.0)));
        // Inside the bounding box, outside the triangle.
        assert!(!s.contains(&geom, local, Point::new(9.0, 9.0)));
    }

    #[test]
    fn bounds_helpers_skip_breaks_and_report_none_when_empty() {
        assert_eq!(points_bounds(&[]), None);
        assert_eq!(points_bounds(&[BREAK]), None);
        let b = points_bounds(&[Point::new(1.0, 2.0), BREAK, Point::new(5.0, 0.0)]).unwrap();
        assert_eq!(b, Rect::new(1.0, 0.0, 5.0, 2.0));
        assert_eq!(triangles_bounds(&[]), None);
    }
}
