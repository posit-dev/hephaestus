//! Arc-length walker — yields position + tangent samples at fixed
//! arc-length intervals along a path.
//!
//! Generic utility used by:
//! - LineGeom's linetype marker rendering, to stamp shapes along a
//!   polyline at fixed pt spacing (`crate::plot::geom::line`).
//! - Phase D's planned `TextPathGeom` (text following a curve).
//!
//! The walker treats each subpath of a path independently — every
//! `MoveTo` starts a new subpath whose cumulative arc length resets to
//! zero. Zero-length segments are skipped during accumulation but their
//! tangent contribution falls back to the most recent valid tangent.

use crate::geometry::{Point, Vec2};
use crate::path::Path;

use super::path_to_rings;

const DEFAULT_TOLERANCE: f64 = 0.5;
const EPSILON: f64 = 1e-9;

/// A sample yielded by [`ArcLengthWalker`].
#[derive(Clone, Copy, Debug)]
pub struct ArcSample {
    /// Position in path coordinates.
    pub point: Point,
    /// Unit tangent at `point`. Length 1 unless every segment is
    /// degenerate, in which case the fallback `Vec2::new(1.0, 0.0)` is
    /// used.
    pub tangent: Vec2,
    /// Cumulative arc length from the start of the current subpath to
    /// `point`. Resets to `0.0` at every subpath boundary.
    pub distance: f64,
    /// 0-based subpath index. Increments on each new subpath.
    pub subpath: usize,
}

/// How the walker handles the leftover after the last full-step sample
/// in each subpath.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrailingPolicy {
    /// Drop the leftover.
    Drop,
    /// Emit one final sample at the subpath endpoint
    /// (`distance == total subpath length`). Skipped when the last
    /// emitted regular sample already coincides with the endpoint.
    #[default]
    PlaceAtEnd,
    /// Scale the step so an integer number of intervals fits the
    /// subpath length exactly. The effective step becomes
    /// `total_length / N` where `N = max(1, round(total_length /
    /// step))`. Samples land at `[0, eff, 2·eff, ..., total_length]`
    /// (or starting from `eff` when `include_start` is `false`).
    ///
    /// Use when you want markers / dashes to align at both endpoints
    /// of a subpath without leaving a partial trailing run. The
    /// configured `step` becomes a target spacing rather than an
    /// exact one.
    Distribute,
}

/// Configuration + entry point for arc-length sampling.
#[derive(Clone, Copy, Debug)]
pub struct ArcLengthWalker {
    /// Flattening tolerance used by [`walk`](Self::walk). Smaller
    /// values produce more vertices when flattening curves. Ignored by
    /// [`walk_polyline`](Self::walk_polyline).
    pub tolerance: f64,
    /// Sampling step in path coordinates. Must be strictly positive.
    pub step: f64,
    /// Trailing policy. Default: [`TrailingPolicy::PlaceAtEnd`].
    pub trailing: TrailingPolicy,
    /// When `true`, emit a sample at `distance == 0` for every subpath.
    /// Default: `true`.
    pub include_start: bool,
}

impl ArcLengthWalker {
    /// Construct a walker with the given step (in path coordinates) and
    /// the default tolerance / trailing / include-start settings.
    ///
    /// Panics if `step` is not strictly positive.
    pub fn new(step: f64) -> Self {
        assert!(
            step > 0.0 && step.is_finite(),
            "ArcLengthWalker::new: step must be strictly positive and finite, got {step}",
        );
        Self {
            tolerance: DEFAULT_TOLERANCE,
            step,
            trailing: TrailingPolicy::default(),
            include_start: true,
        }
    }

    pub fn with_tolerance(mut self, tolerance: f64) -> Self {
        self.tolerance = tolerance;
        self
    }

    pub fn with_trailing(mut self, trailing: TrailingPolicy) -> Self {
        self.trailing = trailing;
        self
    }

    pub fn with_include_start(mut self, include_start: bool) -> Self {
        self.include_start = include_start;
        self
    }

    /// Walk an arbitrary path. Curves are flattened with `tolerance`
    /// via [`super::path_to_rings`]; each resulting subpath is walked
    /// independently.
    pub fn walk(&self, path: &Path) -> Vec<ArcSample> {
        let rings = path_to_rings(path, self.tolerance);
        let mut out = Vec::new();
        for (subpath_idx, ring) in rings.iter().enumerate() {
            self.walk_one(ring, subpath_idx, &mut out);
        }
        out
    }

    /// Walk a pre-flattened polyline. Cheaper than [`walk`](Self::walk)
    /// when the caller already has piecewise-linear vertices.
    pub fn walk_polyline(&self, points: &[Point]) -> Vec<ArcSample> {
        let mut out = Vec::new();
        self.walk_one(points, 0, &mut out);
        out
    }

    fn walk_one(&self, points: &[Point], subpath: usize, out: &mut Vec<ArcSample>) {
        if points.len() < 2 {
            return;
        }
        let sampler = PolylineSampler::from_polyline_with_subpath(points, subpath);
        if sampler.segments.is_empty() {
            // All segments degenerate. Emit a single start sample if
            // requested; trailing has no meaning (distance == 0).
            if self.include_start {
                out.push(ArcSample {
                    point: points[0],
                    tangent: sampler.fallback_tangent,
                    distance: 0.0,
                    subpath,
                });
            }
            return;
        }

        let total = sampler.total;

        // Effective step depends on trailing policy. For Distribute,
        // scale step so an integer number of intervals exactly fits.
        let effective_step = match self.trailing {
            TrailingPolicy::Distribute => {
                let n = (total / self.step).round().max(1.0);
                total / n
            }
            _ => self.step,
        };

        let mut cursor = if self.include_start {
            0.0
        } else {
            effective_step
        };
        let mut last_emitted_distance = f64::NEG_INFINITY;
        let mut seg_idx = 0usize;
        while cursor <= total + EPSILON {
            // Advance segment cursor until the current segment contains
            // `cursor`.
            while seg_idx + 1 < sampler.segments.len()
                && cursor
                    > sampler.segments[seg_idx].start_distance + sampler.segments[seg_idx].length
            {
                seg_idx += 1;
            }
            let sample = sample_at(&sampler.segments, seg_idx, cursor, subpath);
            out.push(sample);
            last_emitted_distance = cursor;
            cursor += effective_step;
        }

        if self.trailing == TrailingPolicy::PlaceAtEnd && last_emitted_distance < total - EPSILON {
            // Emit a final sample at the subpath end using the last
            // segment's tangent.
            let last_seg = sampler.segments.len() - 1;
            let sample = sample_at(&sampler.segments, last_seg, total, subpath);
            out.push(sample);
        }
    }
}

/// Pre-flattened polyline with a cumulative segment-length table.
/// Build once, then sample at arbitrary arc-length distances in O(N)
/// per query.
///
/// Used when the caller needs irregular sampling — e.g. LineGeom
/// walking a linetype pattern where Dash / Gap / Marker steps consume
/// different amounts of arc length per step, or Phase D's TextPathGeom
/// placing each glyph at the next-glyph-width along the path. For
/// regular fixed-step sampling use [`ArcLengthWalker`] instead.
#[derive(Clone, Debug)]
pub struct PolylineSampler {
    segments: Vec<SegmentInfo>,
    /// Total arc length (sum of segment lengths, with degenerate
    /// segments dropped).
    total: f64,
    /// Subpath index — propagated into every `ArcSample` this sampler
    /// produces. `0` for the standalone `from_polyline` constructor;
    /// set per-subpath when constructed via [`Self::from_path`].
    subpath: usize,
    /// Tangent to use when the entire polyline is degenerate (every
    /// segment zero-length). Defaults to `Vec2::new(1.0, 0.0)`.
    fallback_tangent: Vec2,
}

impl PolylineSampler {
    /// Build a sampler from a piecewise-linear point list. Zero-length
    /// segments are dropped from the segment table but their start
    /// point still anchors the sampler's `total_length == 0` case
    /// behaviour.
    pub fn from_polyline(points: &[Point]) -> Self {
        Self::from_polyline_with_subpath(points, 0)
    }

    /// Build one sampler per subpath of `path`, flattening curves with
    /// `tolerance`.
    pub fn from_path(path: &Path, tolerance: f64) -> Vec<Self> {
        let rings = path_to_rings(path, tolerance);
        rings
            .iter()
            .enumerate()
            .map(|(i, ring)| Self::from_polyline_with_subpath(ring, i))
            .collect()
    }

    /// Build one sampler per subpath of `path`, treating each subpath
    /// as **closed** — the segment from the last vertex back to the
    /// first is included in the total arc length. Use this for
    /// walking the perimeter of a closed shape (Polygon / Rect /
    /// Ellipse / Wedge) where the pattern should wrap continuously
    /// around the boundary.
    pub fn from_closed_path(path: &Path, tolerance: f64) -> Vec<Self> {
        let rings = path_to_rings(path, tolerance);
        rings
            .iter()
            .enumerate()
            .map(|(i, ring)| {
                let mut closed = ring.clone();
                if let (Some(&first), Some(&last)) = (closed.first(), closed.last()) {
                    if (last - first).hypot() > EPSILON {
                        closed.push(first);
                    }
                }
                Self::from_polyline_with_subpath(&closed, i)
            })
            .collect()
    }

    fn from_polyline_with_subpath(points: &[Point], subpath: usize) -> Self {
        let mut segments: Vec<SegmentInfo> = Vec::with_capacity(points.len().saturating_sub(1));
        let mut acc = 0.0;
        let mut last_tangent: Option<Vec2> = None;
        for window in points.windows(2) {
            let start = window[0];
            let end = window[1];
            let delta = end - start;
            let length = delta.hypot();
            if length <= EPSILON {
                continue;
            }
            let tangent = delta / length;
            last_tangent = Some(tangent);
            segments.push(SegmentInfo {
                start,
                end,
                start_distance: acc,
                length,
                tangent,
            });
            acc += length;
        }
        Self {
            segments,
            total: acc,
            subpath,
            fallback_tangent: last_tangent.unwrap_or(Vec2::new(1.0, 0.0)),
        }
    }

    /// Total arc length of the polyline.
    pub fn total_length(&self) -> f64 {
        self.total
    }

    /// 0-based subpath index this sampler was built from.
    pub fn subpath(&self) -> usize {
        self.subpath
    }

    /// Cumulative distances at every interior original-segment
    /// boundary strictly between `start` and `end` (exclusive on both
    /// ends, with a small epsilon to avoid duplicates). Iterates in
    /// arc-length order. Empty when `end <= start` or when no interior
    /// boundary falls in the interval.
    ///
    /// Useful for clients that need to draw a sub-polyline of the
    /// sampler spanning `[start, end]` while preserving the original
    /// polyline's vertex set inside the interval (so collinear runs
    /// remain a single segment).
    pub fn segment_boundaries_between(&self, start: f64, end: f64) -> Vec<f64> {
        let lo = start + EPSILON;
        let hi = end - EPSILON;
        if hi <= lo {
            return Vec::new();
        }
        self.segments
            .iter()
            .skip(1)
            .map(|s| s.start_distance)
            .filter(|d| *d > lo && *d < hi)
            .collect()
    }

    /// Sample at arbitrary `distance`. Clamped to `[0, total_length]`.
    /// Returns `None` only when the polyline has no non-degenerate
    /// segments.
    pub fn sample_at(&self, distance: f64) -> Option<ArcSample> {
        if self.segments.is_empty() {
            return None;
        }
        let d = distance.clamp(0.0, self.total);
        // Linear scan for the segment containing `d`. Acceptable for
        // typical polylines; if a caller needs many samples on a long
        // polyline, it should sort distances and amortise.
        let mut seg_idx = 0usize;
        while seg_idx + 1 < self.segments.len()
            && d > self.segments[seg_idx].start_distance + self.segments[seg_idx].length
        {
            seg_idx += 1;
        }
        Some(sample_at(&self.segments, seg_idx, d, self.subpath))
    }
}

#[derive(Clone, Copy, Debug)]
struct SegmentInfo {
    start: Point,
    end: Point,
    start_distance: f64,
    length: f64,
    tangent: Vec2,
}

fn sample_at(segments: &[SegmentInfo], seg_idx: usize, distance: f64, subpath: usize) -> ArcSample {
    let seg = &segments[seg_idx];
    let local = (distance - seg.start_distance).clamp(0.0, seg.length);
    let t = if seg.length > 0.0 {
        local / seg.length
    } else {
        0.0
    };
    let point = Point::new(
        seg.start.x + (seg.end.x - seg.start.x) * t,
        seg.start.y + (seg.end.y - seg.start.y) * t,
    );
    ArcSample {
        point,
        tangent: seg.tangent,
        distance,
        subpath,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::circle;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn walk_polyline_straight_line_with_step() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let walker = ArcLengthWalker::new(2.0);
        let samples = walker.walk_polyline(&pts);
        let distances: Vec<f64> = samples.iter().map(|s| s.distance).collect();
        assert_eq!(distances, vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0]);
        for s in &samples {
            assert!((s.tangent.x - 1.0).abs() < 1e-12);
            assert!(s.tangent.y.abs() < 1e-12);
            assert_eq!(s.subpath, 0);
        }
    }

    #[test]
    fn walk_polyline_corner_tangent_changes() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0)];
        let walker = ArcLengthWalker::new(2.0);
        let samples = walker.walk_polyline(&pts);
        // Total length = 20; samples at 0,2,4,6,8,10,12,14,16,18,20.
        assert_eq!(samples.len(), 11);
        for s in &samples[..=5] {
            // distances 0..=10 land on first segment (boundary wins
            // earlier segment per implementation).
            assert!((s.tangent.x - 1.0).abs() < 1e-12);
            assert!(s.tangent.y.abs() < 1e-12);
        }
        for s in &samples[6..] {
            assert!(s.tangent.x.abs() < 1e-12);
            assert!((s.tangent.y - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn walk_polyline_trailing_drop() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let walker = ArcLengthWalker::new(3.0).with_trailing(TrailingPolicy::Drop);
        let samples = walker.walk_polyline(&pts);
        let distances: Vec<f64> = samples.iter().map(|s| s.distance).collect();
        assert_eq!(distances, vec![0.0, 3.0, 6.0, 9.0]);
    }

    #[test]
    fn walk_polyline_trailing_place_at_end() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let walker = ArcLengthWalker::new(3.0).with_trailing(TrailingPolicy::PlaceAtEnd);
        let samples = walker.walk_polyline(&pts);
        let distances: Vec<f64> = samples.iter().map(|s| s.distance).collect();
        assert_eq!(distances, vec![0.0, 3.0, 6.0, 9.0, 10.0]);
    }

    #[test]
    fn walk_polyline_include_start_false() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let walker = ArcLengthWalker::new(3.0).with_include_start(false);
        let samples = walker.walk_polyline(&pts);
        let distances: Vec<f64> = samples.iter().map(|s| s.distance).collect();
        assert_eq!(distances, vec![3.0, 6.0, 9.0, 10.0]);
    }

    #[test]
    fn walk_polyline_distribute_scales_step_to_fit() {
        // length 10, target step 3 → N = round(10/3) = 3, eff_step ≈ 3.333.
        // Samples at [0, 3.333, 6.667, 10].
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let walker = ArcLengthWalker::new(3.0).with_trailing(TrailingPolicy::Distribute);
        let samples = walker.walk_polyline(&pts);
        assert_eq!(samples.len(), 4);
        let expected = [0.0, 10.0 / 3.0, 20.0 / 3.0, 10.0];
        for (s, e) in samples.iter().zip(expected.iter()) {
            assert!(
                (s.distance - e).abs() < 1e-9,
                "expected distance {e}, got {}",
                s.distance,
            );
        }
        // Last sample lands exactly on the endpoint.
        assert!((samples.last().unwrap().distance - 10.0).abs() < 1e-9);
    }

    #[test]
    fn walk_polyline_distribute_step_larger_than_length() {
        // step > length → N clamped to 1, eff_step = total.
        let pts = [pt(0.0, 0.0), pt(3.0, 0.0)];
        let walker = ArcLengthWalker::new(10.0).with_trailing(TrailingPolicy::Distribute);
        let samples = walker.walk_polyline(&pts);
        let distances: Vec<f64> = samples.iter().map(|s| s.distance).collect();
        assert_eq!(distances, vec![0.0, 3.0]);
    }

    #[test]
    fn walk_polyline_distribute_exact_divisor_matches_place_at_end() {
        let pts = [pt(0.0, 0.0), pt(6.0, 0.0)];
        let dist = ArcLengthWalker::new(2.0).with_trailing(TrailingPolicy::Distribute);
        let place = ArcLengthWalker::new(2.0); // PlaceAtEnd default
        let a: Vec<f64> = dist
            .walk_polyline(&pts)
            .iter()
            .map(|s| s.distance)
            .collect();
        let b: Vec<f64> = place
            .walk_polyline(&pts)
            .iter()
            .map(|s| s.distance)
            .collect();
        assert_eq!(a, b);
        assert_eq!(a, vec![0.0, 2.0, 4.0, 6.0]);
    }

    #[test]
    fn walk_polyline_distribute_include_start_false() {
        // length 10, target step 3 → N=3, eff_step ≈ 3.333; without start
        // samples are at [3.333, 6.667, 10].
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let walker = ArcLengthWalker::new(3.0)
            .with_trailing(TrailingPolicy::Distribute)
            .with_include_start(false);
        let samples = walker.walk_polyline(&pts);
        assert_eq!(samples.len(), 3);
        let expected = [10.0 / 3.0, 20.0 / 3.0, 10.0];
        for (s, e) in samples.iter().zip(expected.iter()) {
            assert!((s.distance - e).abs() < 1e-9);
        }
    }

    #[test]
    fn walk_polyline_exact_divisor_does_not_duplicate_endpoint() {
        // Step divides length exactly; PlaceAtEnd should NOT add a
        // duplicate sample at distance == total_length.
        let pts = [pt(0.0, 0.0), pt(6.0, 0.0)];
        let walker = ArcLengthWalker::new(2.0); // PlaceAtEnd default
        let samples = walker.walk_polyline(&pts);
        let distances: Vec<f64> = samples.iter().map(|s| s.distance).collect();
        assert_eq!(distances, vec![0.0, 2.0, 4.0, 6.0]);
    }

    #[test]
    fn walk_path_flattens_circle() {
        // Circle of radius 5 — circumference 10π ≈ 31.4. With step π,
        // expect ≈ 10 samples + the PlaceAtEnd sample.
        let path = circle(Point::ORIGIN, 5.0);
        let walker = ArcLengthWalker::new(std::f64::consts::PI).with_tolerance(0.05);
        let samples = walker.walk(&path);
        assert!(
            (9..=12).contains(&samples.len()),
            "expected ~10 samples on a 5-radius circle stepped by π, got {}",
            samples.len()
        );
        // Tangent perpendicular to radius vector at every interior
        // sample (skip the very last PlaceAtEnd one which sits on the
        // closing edge of the flattened polyline).
        for s in &samples[1..samples.len().saturating_sub(1)] {
            let radius_dir = Vec2::new(s.point.x, s.point.y).normalize();
            let dot = radius_dir.x * s.tangent.x + radius_dir.y * s.tangent.y;
            assert!(
                dot.abs() < 0.3,
                "tangent should be ~perpendicular to radius at sample {s:?}; dot={dot}",
            );
        }
    }

    #[test]
    fn walk_path_multiple_subpaths() {
        let mut path = Path::new();
        path.move_to(pt(0.0, 0.0));
        path.line_to(pt(10.0, 0.0));
        path.move_to(pt(0.0, 20.0));
        path.line_to(pt(5.0, 20.0));
        let walker = ArcLengthWalker::new(2.0);
        let samples = walker.walk(&path);
        // Subpath 0: 6 samples at 0,2,4,6,8,10.
        // Subpath 1: 4 samples at 0,2,4,5 (PlaceAtEnd at 5).
        let s0: Vec<_> = samples.iter().filter(|s| s.subpath == 0).collect();
        let s1: Vec<_> = samples.iter().filter(|s| s.subpath == 1).collect();
        assert_eq!(s0.len(), 6);
        assert_eq!(s1.len(), 4);
        for s in &s0 {
            assert_eq!(s.subpath, 0);
        }
        for s in &s1 {
            // Each subpath's distance resets to 0.
            assert!(s.distance >= 0.0 && s.distance <= 5.0 + EPSILON);
        }
    }

    #[test]
    fn walk_polyline_degenerate_segment_tangent_fallback() {
        // Duplicate start vertex — first segment is zero-length and
        // skipped during accumulation.
        let pts = [pt(0.0, 0.0), pt(0.0, 0.0), pt(5.0, 0.0)];
        let walker = ArcLengthWalker::new(1.0);
        let samples = walker.walk_polyline(&pts);
        assert!(!samples.is_empty());
        // First valid tangent should be +x.
        assert!((samples[0].tangent.x - 1.0).abs() < 1e-12);
        assert!(samples[0].tangent.y.abs() < 1e-12);
    }

    #[test]
    fn walk_polyline_all_degenerate_uses_fallback_tangent() {
        let pts = [pt(2.0, 3.0), pt(2.0, 3.0), pt(2.0, 3.0)];
        let walker = ArcLengthWalker::new(1.0);
        let samples = walker.walk_polyline(&pts);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].distance, 0.0);
        assert_eq!(samples[0].point, pt(2.0, 3.0));
        assert_eq!(samples[0].tangent, Vec2::new(1.0, 0.0));
    }

    #[test]
    fn walk_polyline_empty_or_single_point() {
        let walker = ArcLengthWalker::new(1.0);
        assert!(walker.walk_polyline(&[]).is_empty());
        assert!(walker.walk_polyline(&[pt(0.0, 0.0)]).is_empty());
    }

    #[test]
    #[should_panic(expected = "step must be strictly positive")]
    fn walker_new_rejects_zero_step() {
        let _ = ArcLengthWalker::new(0.0);
    }

    #[test]
    #[should_panic(expected = "step must be strictly positive")]
    fn walker_new_rejects_negative_step() {
        let _ = ArcLengthWalker::new(-1.0);
    }

    // ── PolylineSampler ──

    #[test]
    fn polyline_sampler_total_length_sums_segments() {
        let s = PolylineSampler::from_polyline(&[pt(0.0, 0.0), pt(3.0, 0.0), pt(3.0, 4.0)]);
        assert!((s.total_length() - 7.0).abs() < 1e-12);
    }

    #[test]
    fn polyline_sampler_sample_at_interpolates_within_segment() {
        let s = PolylineSampler::from_polyline(&[pt(0.0, 0.0), pt(10.0, 0.0)]);
        let mid = s.sample_at(5.0).unwrap();
        assert!((mid.point.x - 5.0).abs() < 1e-12);
        assert!(mid.point.y.abs() < 1e-12);
        assert!((mid.tangent.x - 1.0).abs() < 1e-12);
    }

    #[test]
    fn polyline_sampler_sample_at_clamps_out_of_range() {
        let s = PolylineSampler::from_polyline(&[pt(0.0, 0.0), pt(10.0, 0.0)]);
        let pre = s.sample_at(-5.0).unwrap();
        assert!(pre.point.x.abs() < 1e-12);
        let post = s.sample_at(20.0).unwrap();
        assert!((post.point.x - 10.0).abs() < 1e-12);
    }

    #[test]
    fn polyline_sampler_segment_boundaries_between_excludes_endpoints() {
        let s = PolylineSampler::from_polyline(&[pt(0.0, 0.0), pt(3.0, 0.0), pt(6.0, 0.0)]);
        // Boundaries strictly between start=0 and end=6: just [3.0].
        let bs = s.segment_boundaries_between(0.0, 6.0);
        assert_eq!(bs.len(), 1);
        assert!((bs[0] - 3.0).abs() < 1e-12);
        // Boundary outside the interval is excluded.
        let bs = s.segment_boundaries_between(4.0, 6.0);
        assert!(bs.is_empty());
    }

    #[test]
    fn polyline_sampler_from_path_per_subpath() {
        let mut path = Path::new();
        path.move_to(pt(0.0, 0.0));
        path.line_to(pt(5.0, 0.0));
        path.move_to(pt(0.0, 10.0));
        path.line_to(pt(8.0, 10.0));
        let samplers = PolylineSampler::from_path(&path, 0.5);
        assert_eq!(samplers.len(), 2);
        assert_eq!(samplers[0].subpath(), 0);
        assert!((samplers[0].total_length() - 5.0).abs() < 1e-12);
        assert_eq!(samplers[1].subpath(), 1);
        assert!((samplers[1].total_length() - 8.0).abs() < 1e-12);
    }

    #[test]
    fn polyline_sampler_empty_returns_none() {
        let s = PolylineSampler::from_polyline(&[]);
        assert!(s.sample_at(0.0).is_none());
        assert_eq!(s.total_length(), 0.0);
    }
}
