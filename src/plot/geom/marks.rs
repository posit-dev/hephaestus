//! Shared mark-grouping for multi-row-per-mark geoms.
//!
//! A `MarkSlot` groups a contiguous (in source order) sequence of row
//! indices that share the same key value. Both [`LineGeom`] and
//! [`TextPathGeom`] consume this — the polyline vertices for the former,
//! the curve for the latter. Adding `PolygonGeom`-style multi-row geoms
//! reuses the same machinery.

use super::Keys;
use crate::plot::value::DataColumn;

/// One mark — a logical group of rows sharing a key value.
#[derive(Clone, Debug)]
pub(crate) struct MarkSlot {
    /// Source-order row index of the first appearance of this mark's key.
    /// Used to resolve per-mark channels.
    pub(crate) first_row: usize,
    /// Row indices that make up this mark, in source order.
    pub(crate) rows: Vec<usize>,
}

/// Walk `col` and produce one [`MarkSlot`] per unique key value, in
/// first-appearance order. Each slot's `rows` are in source order.
pub(crate) fn build_marks_from_column(col: &DataColumn) -> Vec<MarkSlot> {
    let n = col.len();
    let mut order: Vec<MarkSlot> = Vec::new();
    // For small mark counts (typical: K << N) a linear scan over `order`
    // is cheaper than maintaining a HashMap.
    for i in 0..n {
        let key_i = col.get(i);
        let mut found = false;
        for slot in order.iter_mut() {
            if col.get(slot.first_row).key_eq(&key_i) {
                slot.rows.push(i);
                found = true;
                break;
            }
        }
        if !found {
            order.push(MarkSlot {
                first_row: i,
                rows: vec![i],
            });
        }
    }
    order
}

/// Walk a [`Keys`] value, falling back to "every row is its own mark"
/// for the `Positional` variant. The `OneMark` rewriter always produces
/// an `Explicit` placeholder column for grouped geoms, so the
/// `Positional` arm should only fire for misconfigured callers — it
/// matches PointGeom-style semantics for the diff path.
pub(crate) fn build_marks(keys: &Keys) -> Vec<MarkSlot> {
    match keys {
        Keys::Positional(n) => (0..*n)
            .map(|i| MarkSlot {
                first_row: i,
                rows: vec![i],
            })
            .collect(),
        Keys::Explicit(col) => build_marks_from_column(col),
    }
}
