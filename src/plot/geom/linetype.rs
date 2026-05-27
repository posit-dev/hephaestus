//! Linetype constructors — canonical named dash patterns as
//! `Arc<[f64]>` arrays of alternating dash / gap lengths in pt.
//!
//! Empty array = solid (no dashing). The pt values are converted to
//! px at draw time using the active dpi (`px = pt * dpi / 72.0`),
//! matching the same convention used for [`linewidth`] and point
//! `size`, so dash proportions stay stable across resolutions.
//!
//! The constructors here are convenience: any `Arc<[f64]>` produced by
//! user code works too (e.g. `Arc::from(vec![6.0, 2.0])`). The geom
//! validates only that the array has even length (or is empty).
//!
//! ```ignore
//! use hephaestus::plot::geom::linetype;
//!
//! linetype::solid();    // []
//! linetype::dashed();   // [8.0, 4.0]
//! linetype::dotted();   // [2.0, 3.0]
//! linetype::dashdot();  // [8.0, 3.0, 2.0, 3.0]
//! ```

use std::sync::Arc;

/// No dashing — a continuous solid line.
pub fn solid() -> Arc<[f64]> {
    Arc::from(Vec::<f64>::new())
}

/// `[8 pt on, 4 pt off]`.
pub fn dashed() -> Arc<[f64]> {
    Arc::from(vec![8.0, 4.0])
}

/// `[2 pt on, 3 pt off]`.
pub fn dotted() -> Arc<[f64]> {
    Arc::from(vec![2.0, 3.0])
}

/// `[8 pt on, 3 pt off, 2 pt on, 3 pt off]`.
pub fn dashdot() -> Arc<[f64]> {
    Arc::from(vec![8.0, 3.0, 2.0, 3.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_is_empty() {
        assert_eq!(solid().len(), 0);
    }

    #[test]
    fn named_patterns_are_even_length() {
        for pattern in [dashed(), dotted(), dashdot()] {
            assert_eq!(pattern.len() % 2, 0, "even length expected");
            assert!(!pattern.is_empty(), "non-solid is non-empty");
        }
    }

    #[test]
    fn dashed_canonical_values() {
        let p = dashed();
        assert_eq!(p.as_ref(), &[8.0, 4.0][..]);
    }

    #[test]
    fn dashdot_canonical_values() {
        let p = dashdot();
        assert_eq!(p.as_ref(), &[8.0, 3.0, 2.0, 3.0][..]);
    }
}
