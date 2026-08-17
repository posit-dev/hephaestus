//! Shared mark-grouping for multi-row-per-mark geoms.
//!
//! A `MarkSlot` groups a contiguous (in source order) sequence of row
//! indices that share the same key value. Both [`LineGeom`] and
//! [`TextPathGeom`] consume this — the polyline vertices for the former,
//! the curve for the latter. Adding `PolygonGeom`-style multi-row geoms
//! reuses the same machinery.

use super::{empty_datacolumn_like, Keys};
use crate::plot::value::{DataColumn, Value};

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

/// Build a column of one entry per mark — the key value at each mark's
/// first row. Used by grouped geoms to feed `diff_columns` at mark
/// granularity. The result carries the same variant as `col`, so it can
/// be diffed against a previous snapshot of the same key column.
/// `geom_name` names the geom in the internal-invariant message.
pub(crate) fn unique_values_at_first_rows(
    col: &DataColumn,
    first_rows: impl IntoIterator<Item = usize>,
    geom_name: &str,
) -> DataColumn {
    let mut template = empty_datacolumn_like(col);
    for i in first_rows {
        match (&mut template, col.get(i)) {
            (DataColumn::F64(vec), Value::Number(n)) => vec.push(n),
            (DataColumn::F32(vec), Value::Number(n)) => vec.push(n as f32),
            (DataColumn::I32(vec), Value::Number(n)) => vec.push(n as i32),
            (DataColumn::I64(vec), Value::Number(n)) => vec.push(n as i64),
            (DataColumn::Bool(vec), Value::Bool(b)) => vec.push(b),
            (DataColumn::String(vec), Value::String(s)) => vec.push(s),
            (DataColumn::Color(vec), Value::Color(c)) => vec.push(c),
            (DataColumn::Date(vec), Value::Date(d)) => vec.push(d),
            (DataColumn::DateTime(vec), Value::DateTime(us)) => vec.push(us),
            (DataColumn::Time(vec), Value::Time(us)) => vec.push(us),
            (DataColumn::Duration(vec), Value::Duration(us)) => vec.push(us),
            (DataColumn::Linetype(vec), Value::Linetype(p)) => vec.push(p),
            (DataColumn::Geometry(vec), Value::Geometry(g)) => vec.push(g),
            _ => unreachable!(
                "{geom_name}: unique-keys template variant does not match its source column"
            ),
        }
    }
    template
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scales::geometry::Geometry;
    use std::sync::Arc;

    fn strings(items: &[&str]) -> DataColumn {
        DataColumn::String(items.iter().map(|s| Arc::from(*s)).collect())
    }

    // ── build_marks ────────────────────────────────────────────────

    #[test]
    fn positional_keys_make_every_row_its_own_mark() {
        let marks = build_marks(&Keys::Positional(3));
        assert_eq!(marks.len(), 3);
        for (i, slot) in marks.iter().enumerate() {
            assert_eq!(slot.first_row, i);
            assert_eq!(slot.rows, vec![i]);
        }
        assert!(build_marks(&Keys::Positional(0)).is_empty());
    }

    #[test]
    fn explicit_keys_group_rows_sharing_a_key() {
        let marks = build_marks(&Keys::Explicit(strings(&["a", "a", "b"])));
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].rows, vec![0, 1]);
        assert_eq!(marks[1].rows, vec![2]);
    }

    // ── build_marks_from_column ────────────────────────────────────

    #[test]
    fn marks_come_back_in_first_appearance_order() {
        // Interleaved keys: mark order follows where each key first
        // appears, and each mark keeps its rows in source order.
        let marks = build_marks_from_column(&strings(&["b", "a", "b", "a", "c"]));
        assert_eq!(marks.len(), 3);
        assert_eq!(marks[0].first_row, 0);
        assert_eq!(marks[0].rows, vec![0, 2]);
        assert_eq!(marks[1].first_row, 1);
        assert_eq!(marks[1].rows, vec![1, 3]);
        assert_eq!(marks[2].first_row, 4);
        assert_eq!(marks[2].rows, vec![4]);
    }

    #[test]
    fn grouping_works_on_a_non_string_key_column() {
        let marks = build_marks_from_column(&DataColumn::Date(vec![1, 2, 1]));
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].rows, vec![0, 2]);
    }

    #[test]
    fn an_empty_key_column_yields_no_marks() {
        assert!(build_marks_from_column(&strings(&[])).is_empty());
    }

    // ── unique_values_at_first_rows ────────────────────────────────

    #[test]
    fn unique_values_keeps_the_source_column_variant() {
        let col = strings(&["a", "a", "b"]);
        let out = unique_values_at_first_rows(&col, [0, 2], "TestGeom");
        assert!(matches!(out, DataColumn::String(_)));
        assert_eq!(out.len(), 2);
        assert!(out.get(0).key_eq(&Value::String(Arc::from("a"))));
        assert!(out.get(1).key_eq(&Value::String(Arc::from("b"))));
    }

    #[test]
    fn unique_values_round_trips_every_scalar_column_variant() {
        let cases: Vec<DataColumn> = vec![
            DataColumn::F64(vec![1.5, 2.5]),
            DataColumn::F32(vec![1.5, 2.5]),
            DataColumn::I32(vec![1, 2]),
            DataColumn::I64(vec![1, 2]),
            DataColumn::Bool(vec![true, false]),
            strings(&["a", "b"]),
            DataColumn::Color(vec![
                crate::color::rgb(1.0, 0.0, 0.0),
                crate::color::rgb(0.0, 1.0, 0.0),
            ]),
            DataColumn::Date(vec![10, 20]),
            DataColumn::DateTime(vec![10, 20]),
            DataColumn::Time(vec![10, 20]),
            DataColumn::Duration(vec![10, 20]),
            DataColumn::Linetype(vec![
                Arc::from(vec![crate::plot::value::LinetypeStep::Dash(1.0)]),
                Arc::from(vec![crate::plot::value::LinetypeStep::Dash(2.0)]),
            ]),
        ];
        for col in cases {
            let out = unique_values_at_first_rows(&col, [1, 0], "TestGeom");
            assert_eq!(out.len(), 2, "{col:?}");
            // Rows come back in the order the first-row indices are given.
            assert!(out.get(0).key_eq(&col.get(1)), "{col:?}");
            assert!(out.get(1).key_eq(&col.get(0)), "{col:?}");
        }
    }

    #[test]
    fn unique_values_round_trips_a_geometry_column() {
        let a = Arc::new(Geometry::Point((1.0, 2.0)));
        let b = Arc::new(Geometry::LineString(vec![(0.0, 0.0), (1.0, 1.0)]));
        let col = DataColumn::Geometry(vec![a.clone(), b.clone(), a.clone()]);
        let out = unique_values_at_first_rows(&col, [0, 1], "TestGeom");
        match &out {
            DataColumn::Geometry(vs) => {
                assert_eq!(vs.len(), 2);
                assert_eq!(*vs[0], *a);
                assert_eq!(*vs[1], *b);
            }
            other => panic!("expected a geometry column, got {other:?}"),
        }
    }

    #[test]
    fn unique_values_of_no_rows_is_an_empty_column_of_the_same_variant() {
        let out = unique_values_at_first_rows(&DataColumn::I32(vec![1, 2]), [], "TestGeom");
        assert!(matches!(out, DataColumn::I32(_)));
        assert!(out.is_empty());
    }
}
