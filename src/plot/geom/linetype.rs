//! Linetype constructors — patterns are sequences of `LinetypeStep`
//! entries (`Dash` / `Marker` / `Gap`) carried by
//! [`Arc<[LinetypeStep]>`](LinetypeStep).
//!
//! Even-length, alternating: even-indexed entries draw something
//! (`Dash` for a stroked segment, `Marker` for a stamped shape),
//! odd-indexed entries are `Gap` (unconditional advance without
//! drawing). Empty array = solid (no dashing, no markers).
//!
//! The pt values resolve to px at draw time using the active dpi (`px
//! = pt * dpi / 72.0`), matching the same convention used for
//! [`linewidth`] and point `size`, so dash and marker proportions stay
//! stable across resolutions. Markers are sized to the resolved
//! `linewidth` pt of arc length, so they don't eat into the
//! surrounding gaps.
//!
//! ```ignore
//! use hephaestus::plot::geom::linetype::{self, dash, gap, marker, pattern};
//!
//! linetype::solid();    // []
//! linetype::dashed();   // [Dash(8), Gap(4)]
//! linetype::dotted();   // [Dash(2), Gap(3)]
//! linetype::dashdot();  // [Dash(8), Gap(3), Dash(2), Gap(3)]
//!
//! // Mixed marker + dash pattern: 5pt dash, 3pt gap, circle marker,
//! // 5pt gap, repeat.
//! pattern([dash(5.0), gap(3.0), marker("circle"), gap(5.0)]);
//! ```

use std::sync::Arc;

pub use crate::plot::value::LinetypeStep;

/// Stroke a segment of `length_pt` pt along the line.
pub fn dash(length_pt: f64) -> LinetypeStep {
    LinetypeStep::Dash(length_pt)
}

/// Advance the cursor by `length_pt` pt without drawing.
pub fn gap(length_pt: f64) -> LinetypeStep {
    LinetypeStep::Gap(length_pt)
}

/// Stamp the named shape at the current cursor (rotated to the local
/// tangent). The marker is sized to the resolved `linewidth` pt of arc
/// length so subsequent gaps measure clear space.
pub fn marker(name: impl Into<Arc<str>>) -> LinetypeStep {
    LinetypeStep::Marker(name.into())
}

/// Build a linetype pattern from a sequence of steps. Validates the
/// "even-index = Dash|Marker, odd-index = Gap" alternation; panics with
/// a clear message on violation. Empty input → solid.
pub fn pattern(steps: impl IntoIterator<Item = LinetypeStep>) -> Arc<[LinetypeStep]> {
    let v: Vec<LinetypeStep> = steps.into_iter().collect();
    validate_alternation(&v);
    Arc::from(v)
}

/// `true` if `pattern` contains no `LinetypeStep::Marker` entries.
/// Marker-free patterns can be rendered via the kurbo dash fast path.
pub fn is_marker_free(pattern: &[LinetypeStep]) -> bool {
    pattern.iter().all(|s| !s.is_marker())
}

/// Project a marker-free pattern to the flat `[dash, gap, dash, gap,
/// ...]` f64 slice that `kurbo::Stroke::with_dashes` expects. Panics if
/// the pattern contains markers (call [`is_marker_free`] first) or if
/// the alternation is malformed (use [`pattern`] / [`validate_pattern`]
/// to construct).
pub fn to_kurbo_dashes(pattern: &[LinetypeStep]) -> Vec<f64> {
    pattern
        .iter()
        .map(|step| match step {
            LinetypeStep::Dash(l) | LinetypeStep::Gap(l) => *l,
            LinetypeStep::Marker(_) => {
                panic!("to_kurbo_dashes: pattern contains a Marker step; not representable as a kurbo dash pattern")
            }
        })
        .collect()
}

/// Replace every `Marker(_)` in `pattern` with `Gap(linewidth_pt)`,
/// preserving the marker's arc-length contribution while skipping the
/// stamp. Used by non-LineGeom geoms to render the dashing portion of
/// a marker-bearing linetype while ignoring the markers themselves.
pub fn strip_markers(pattern: &[LinetypeStep], linewidth_pt: f64) -> Arc<[LinetypeStep]> {
    let mapped: Vec<LinetypeStep> = pattern
        .iter()
        .map(|step| match step {
            LinetypeStep::Marker(_) => LinetypeStep::Gap(linewidth_pt),
            other => other.clone(),
        })
        .collect();
    Arc::from(mapped)
}

/// Validate the alternation invariant: even-indexed entries are `Dash`
/// or `Marker`; odd-indexed entries are `Gap`; length is even. Panics
/// with a clear message on violation. Empty input is valid (solid).
pub fn validate_pattern(pattern: &[LinetypeStep]) {
    validate_alternation(pattern);
}

fn validate_alternation(pattern: &[LinetypeStep]) {
    if pattern.is_empty() {
        return;
    }
    if !pattern.len().is_multiple_of(2) {
        panic!(
            "linetype::pattern: must have even length (alternating Dash|Marker / Gap), got {}",
            pattern.len()
        );
    }
    for (i, step) in pattern.iter().enumerate() {
        let is_gap = matches!(step, LinetypeStep::Gap(_));
        let expected_gap = i % 2 == 1;
        if is_gap != expected_gap {
            let kind = match step {
                LinetypeStep::Dash(_) => "Dash",
                LinetypeStep::Marker(_) => "Marker",
                LinetypeStep::Gap(_) => "Gap",
            };
            let expected = if expected_gap {
                "Gap"
            } else {
                "Dash or Marker"
            };
            panic!(
                "linetype::pattern: entry {i} is {kind} but expected {expected} \
                 (patterns must alternate Dash|Marker, Gap, Dash|Marker, Gap, …)"
            );
        }
    }
}

/// No dashing — a continuous solid line.
pub fn solid() -> Arc<[LinetypeStep]> {
    Arc::from(Vec::<LinetypeStep>::new())
}

/// `[Dash(8), Gap(4)]`.
pub fn dashed() -> Arc<[LinetypeStep]> {
    pattern([dash(8.0), gap(4.0)])
}

/// `[Dash(2), Gap(3)]`.
pub fn dotted() -> Arc<[LinetypeStep]> {
    pattern([dash(2.0), gap(3.0)])
}

/// `[Dash(8), Gap(3), Dash(2), Gap(3)]`.
pub fn dashdot() -> Arc<[LinetypeStep]> {
    pattern([dash(8.0), gap(3.0), dash(2.0), gap(3.0)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_is_empty() {
        assert_eq!(solid().len(), 0);
    }

    #[test]
    fn named_patterns_alternate_and_are_marker_free() {
        for p in [dashed(), dotted(), dashdot()] {
            assert!(p.len().is_multiple_of(2));
            assert!(!p.is_empty());
            assert!(is_marker_free(&p));
            validate_pattern(&p);
        }
    }

    #[test]
    fn dashed_canonical_values() {
        let p = dashed();
        assert!(matches!(p[0], LinetypeStep::Dash(d) if (d - 8.0).abs() < 1e-12));
        assert!(matches!(p[1], LinetypeStep::Gap(g) if (g - 4.0).abs() < 1e-12));
    }

    #[test]
    fn dashdot_canonical_values() {
        let p = dashdot();
        assert_eq!(p.len(), 4);
        assert!(matches!(p[0], LinetypeStep::Dash(d) if (d - 8.0).abs() < 1e-12));
        assert!(matches!(p[1], LinetypeStep::Gap(g) if (g - 3.0).abs() < 1e-12));
        assert!(matches!(p[2], LinetypeStep::Dash(d) if (d - 2.0).abs() < 1e-12));
        assert!(matches!(p[3], LinetypeStep::Gap(g) if (g - 3.0).abs() < 1e-12));
    }

    #[test]
    fn pattern_accepts_markers() {
        let p = pattern([marker("circle"), gap(5.0)]);
        assert_eq!(p.len(), 2);
        assert!(p[0].is_marker());
        assert!(!is_marker_free(&p));
    }

    #[test]
    fn pattern_mixed_dash_and_marker() {
        let p = pattern([dash(6.0), gap(2.0), marker("square"), gap(4.0)]);
        assert_eq!(p.len(), 4);
        assert!(!is_marker_free(&p));
    }

    #[test]
    #[should_panic(expected = "must have even length")]
    fn pattern_panics_on_odd_length() {
        let _ = pattern([dash(1.0), gap(2.0), dash(3.0)]);
    }

    #[test]
    #[should_panic(expected = "expected Gap")]
    fn pattern_panics_on_dash_in_gap_slot() {
        let _ = pattern([dash(1.0), dash(2.0)]);
    }

    #[test]
    #[should_panic(expected = "expected Dash or Marker")]
    fn pattern_panics_on_gap_in_dash_slot() {
        let _ = pattern([gap(2.0), gap(1.0)]);
    }

    #[test]
    fn to_kurbo_dashes_round_trip() {
        let p = pattern([dash(5.0), gap(3.0)]);
        assert_eq!(to_kurbo_dashes(&p), vec![5.0, 3.0]);
    }

    #[test]
    #[should_panic(expected = "contains a Marker step")]
    fn to_kurbo_dashes_panics_on_markers() {
        let p = pattern([marker("circle"), gap(5.0)]);
        let _ = to_kurbo_dashes(&p);
    }

    #[test]
    fn strip_markers_replaces_with_gap_of_linewidth() {
        let p = pattern([dash(6.0), gap(2.0), marker("circle"), gap(5.0)]);
        let stripped = strip_markers(&p, 4.0);
        assert_eq!(stripped.len(), 4);
        assert!(matches!(stripped[0], LinetypeStep::Dash(d) if (d - 6.0).abs() < 1e-12));
        assert!(matches!(stripped[1], LinetypeStep::Gap(g) if (g - 2.0).abs() < 1e-12));
        assert!(matches!(stripped[2], LinetypeStep::Gap(g) if (g - 4.0).abs() < 1e-12));
        assert!(matches!(stripped[3], LinetypeStep::Gap(g) if (g - 5.0).abs() < 1e-12));
        assert!(is_marker_free(&stripped));
    }
}
