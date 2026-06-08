//! Key-based diff for typed columnar key columns.
//!
//! Geoms keep a snapshot of their previous-frame key column and rebuild
//! a `(enter, update, exit)` triple before each draw. The draw loop
//! snaps to the current state; the animation pass (when enabled)
//! interpolates between previous and current values along the `update`
//! edges.
//!
//! Semantics:
//!
//! - **Variant-strict.** A `Date(1)` and a `Number(1.0)` are distinct
//!   keys even though both project to f64 `1.0`. The columnar
//!   [`DataColumn::key_eq_at`] / [`DataColumn::key_hash_at`] helpers
//!   handle this.
//! - **Deterministic.** `enter` and `update` are returned in
//!   next-iteration order; `exit` is returned in prev-iteration order.
//!   NaN canonicalises to a single hash + equality class.
//! - **Each prev row matches at most one next row.** If `next` contains
//!   duplicate keys, only the first occurrence pairs with the matching
//!   prev row; later duplicates fall to `enter`. (D3-style: keys should
//!   be unique; we degrade gracefully rather than crash.)

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::Hasher;

use crate::plot::value::{DataColumn, Value};

/// Index from hashed key to bucket of prev-column row indices.
///
/// Built once per diff; reused across the `next` column's iteration to
/// look up matches without re-hashing prev rows.
pub struct KeyIndex {
    /// Bucket per hash. `Vec<usize>` rather than `SmallVec` to keep the
    /// dep surface tight; collisions are rare for the variant-strict
    /// hash strategy used by `DataColumn::key_hash_at`.
    buckets: HashMap<u64, Vec<usize>>,
    /// Total row count. Kept explicitly to avoid summing bucket lengths.
    len: usize,
}

impl KeyIndex {
    /// Build the index from `column`. Time: O(N) hashes.
    pub fn build(column: &DataColumn) -> Self {
        let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
        for i in 0..column.len() {
            let h = hash_at(column, i);
            buckets.entry(h).or_default().push(i);
        }
        Self {
            buckets,
            len: column.len(),
        }
    }

    /// Look up which row of `prev` matches the i-th row of `next`. Returns
    /// the **first** matching prev index, or `None` if no match.
    ///
    /// Does **not** consider whether the prev row has already been matched
    /// against a different next row — callers wanting "each prev row
    /// matches at most one next row" semantics should track consumption
    /// themselves (see [`diff_columns`], which does).
    pub fn lookup(&self, prev: &DataColumn, next: &DataColumn, i: usize) -> Option<usize> {
        let h = hash_at(next, i);
        let bucket = self.buckets.get(&h)?;
        bucket
            .iter()
            .find(|&&p| prev.key_eq_at(p, next, i))
            .copied()
    }

    /// Number of rows indexed.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no rows are indexed.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Diff the previous key column against the next.
///
/// Returns `(enter, update, exit)` where:
/// - `enter` — indices in `next` whose keys are not in `prev`, in
///   next-iteration order.
/// - `update` — `(prev_idx, new_idx)` pairs whose keys appear in both,
///   in next-iteration order.
/// - `exit` — [`Value`] keys from `prev` that don't appear in `next`, in
///   prev-iteration order. Returned as `Value`s rather than indices
///   because the caller is about to rotate `prev` to the new column.
///
/// # Panics
///
/// Panics if `prev` and `next` have different [`DataColumn`] variants.
/// The caller has just rebuilt keys for the same geom; a variant mismatch
/// signals a structural bug (e.g. swapping the key column for one of a
/// different type without resetting state).
pub fn diff_columns(
    prev: &DataColumn,
    prev_index: &KeyIndex,
    next: &DataColumn,
) -> (Vec<usize>, Vec<(usize, usize)>, Vec<Value>) {
    if std::mem::discriminant(prev) != std::mem::discriminant(next) {
        panic!("diff_columns: prev/next variant mismatch (prev: {prev:?}, next: {next:?})");
    }

    let mut enter = Vec::new();
    let mut update = Vec::new();
    let mut consumed = vec![false; prev.len()];

    for i in 0..next.len() {
        let matched = find_match(&prev_index.buckets, prev, next, i, &consumed);
        match matched {
            Some(p) => {
                consumed[p] = true;
                update.push((p, i));
            }
            None => enter.push(i),
        }
    }

    let exit: Vec<Value> = (0..prev.len())
        .filter(|&i| !consumed[i])
        .map(|i| prev.get(i))
        .collect();

    (enter, update, exit)
}

/// Walk the bucket for `next[i]`, returning the first **unconsumed** prev
/// index whose key equals `next[i]`. Separated from `KeyIndex::lookup`
/// because the public `lookup` deliberately doesn't track consumption.
fn find_match(
    buckets: &HashMap<u64, Vec<usize>>,
    prev: &DataColumn,
    next: &DataColumn,
    i: usize,
    consumed: &[bool],
) -> Option<usize> {
    let h = hash_at(next, i);
    let bucket = buckets.get(&h)?;
    bucket
        .iter()
        .find(|&&p| !consumed[p] && prev.key_eq_at(p, next, i))
        .copied()
}

fn hash_at(col: &DataColumn, i: usize) -> u64 {
    let mut h = DefaultHasher::new();
    col.key_hash_at(i, &mut h);
    h.finish()
}

// ─── Positional fast path ────────────────────────────────────────────────────

/// Positional diff: match rows by index, no key column required.
///
/// Equivalent to building two `DataColumn::I64((0..n).collect())` columns
/// and running [`diff_columns`] on them, but skips the hashing and
/// bucket-building work. Use this whenever the geom has no user-supplied
/// key column — D3-style "by-position" enter/update/exit semantics.
///
/// Returned shape matches [`diff_columns`] exactly:
/// - `update` — pairs `(i, i)` for `i in 0..min(prev_n, next_n)`.
/// - `enter` — tail indices `[common .. next_n)` when `next` is longer.
/// - `exit` — tail prev indices `[common .. prev_n)` returned as
///   `Value::Number(i as f64)`, matching what `DataColumn::I64::get(i)`
///   would produce. Identical to the equivalent [`diff_columns`] output;
///   verified by [`tests::positional_matches_columns_form`].
pub fn diff_positional(
    prev_n: usize,
    next_n: usize,
) -> (Vec<usize>, Vec<(usize, usize)>, Vec<Value>) {
    let common = prev_n.min(next_n);
    let update: Vec<(usize, usize)> = (0..common).map(|i| (i, i)).collect();
    let enter: Vec<usize> = (common..next_n).collect();
    let exit: Vec<Value> = (common..prev_n).map(|i| Value::Number(i as f64)).collect();
    (enter, update, exit)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::value::Date;

    fn diff(prev: &DataColumn, next: &DataColumn) -> (Vec<usize>, Vec<(usize, usize)>, Vec<Value>) {
        let idx = KeyIndex::build(prev);
        diff_columns(prev, &idx, next)
    }

    // ── Shapes ──

    #[test]
    fn pure_enter() {
        let prev: DataColumn = Vec::<&'static str>::new().into();
        let next: DataColumn = vec!["a", "b", "c"].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert_eq!(enter, vec![0, 1, 2]);
        assert!(update.is_empty());
        assert!(exit.is_empty());
    }

    #[test]
    fn pure_exit() {
        let prev: DataColumn = vec!["a", "b", "c"].into();
        let next: DataColumn = Vec::<&'static str>::new().into();
        let (enter, update, exit) = diff(&prev, &next);
        assert!(enter.is_empty());
        assert!(update.is_empty());
        assert_eq!(exit.len(), 3);
        for (i, v) in exit.iter().enumerate() {
            let expected = ["a", "b", "c"][i];
            assert_eq!(v.as_str(), Some(expected));
        }
    }

    #[test]
    fn pure_update_identity() {
        let prev: DataColumn = vec!["a", "b", "c"].into();
        let next: DataColumn = vec!["a", "b", "c"].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert!(enter.is_empty());
        assert_eq!(update, vec![(0, 0), (1, 1), (2, 2)]);
        assert!(exit.is_empty());
    }

    #[test]
    fn reordered_keys_keep_all_in_update() {
        // prev=[a, b, c], next=[c, a, b] → update pairs reflect the
        // permutation; enter/exit empty.
        let prev: DataColumn = vec!["a", "b", "c"].into();
        let next: DataColumn = vec!["c", "a", "b"].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert!(enter.is_empty());
        assert_eq!(update, vec![(2, 0), (0, 1), (1, 2)]);
        assert!(exit.is_empty());
    }

    #[test]
    fn mixed_enter_update_exit() {
        // prev=[a, b, c], next=[b, c, d] → update {(b→0), (c→1)},
        // enter [2] (d), exit [a].
        let prev: DataColumn = vec!["a", "b", "c"].into();
        let next: DataColumn = vec!["b", "c", "d"].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert_eq!(enter, vec![2]);
        assert_eq!(update, vec![(1, 0), (2, 1)]);
        assert_eq!(exit.len(), 1);
        assert_eq!(exit[0].as_str(), Some("a"));
    }

    #[test]
    fn empty_both() {
        let prev: DataColumn = Vec::<&'static str>::new().into();
        let next: DataColumn = Vec::<&'static str>::new().into();
        let (enter, update, exit) = diff(&prev, &next);
        assert!(enter.is_empty());
        assert!(update.is_empty());
        assert!(exit.is_empty());
    }

    // ── Determinism ──

    #[test]
    fn enter_in_next_order() {
        let prev: DataColumn = vec!["x"].into();
        let next: DataColumn = vec!["a", "x", "b", "c"].into();
        let (enter, _update, _exit) = diff(&prev, &next);
        // a, b, c are enters at indices 0, 2, 3 — in that order.
        assert_eq!(enter, vec![0, 2, 3]);
    }

    #[test]
    fn exit_in_prev_order() {
        let prev: DataColumn = vec!["a", "b", "c", "d", "e"].into();
        let next: DataColumn = vec!["c"].into();
        let (_enter, _update, exit) = diff(&prev, &next);
        // a, b, d, e exit, in prev order.
        let names: Vec<&str> = exit.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(names, vec!["a", "b", "d", "e"]);
    }

    // ── Duplicate next keys ──

    #[test]
    fn duplicate_next_keys_fall_to_enter() {
        // prev=[a], next=[a, a, a] → first a updates, later two enter.
        let prev: DataColumn = vec!["a"].into();
        let next: DataColumn = vec!["a", "a", "a"].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert_eq!(update, vec![(0, 0)]);
        assert_eq!(enter, vec![1, 2]);
        assert!(exit.is_empty());
    }

    // ── Variant strictness ──

    #[test]
    fn number_and_date_with_same_projection_are_distinct() {
        // Date(1) projects to 1.0 numerically, but a Number column with
        // value 1.0 is a different DataColumn variant — diff would panic
        // (variant mismatch). This test verifies that mismatch is caught.
        let prev: DataColumn = vec![Date::from_days(1)].into();
        let next: DataColumn = vec![1.0_f64].into();
        let idx = KeyIndex::build(&prev);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            diff_columns(&prev, &idx, &next)
        }));
        assert!(result.is_err(), "expected variant-mismatch panic");
    }

    #[test]
    #[should_panic(expected = "variant mismatch")]
    fn variant_mismatch_panics() {
        let prev: DataColumn = vec![1_i32, 2, 3].into();
        let next: DataColumn = vec!["a", "b", "c"].into();
        let idx = KeyIndex::build(&prev);
        let _ = diff_columns(&prev, &idx, &next);
    }

    // ── NaN handling ──

    #[test]
    fn nan_keys_match() {
        // f64 NaN canonicalises in key_eq/key_hash, so two NaNs are
        // considered equal as keys.
        let prev: DataColumn = vec![f64::NAN].into();
        let next: DataColumn = vec![f64::NAN].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert!(enter.is_empty());
        assert_eq!(update, vec![(0, 0)]);
        assert!(exit.is_empty());
    }

    // ── Variant coverage ──

    #[test]
    fn f64_keys() {
        let prev: DataColumn = vec![1.0_f64, 2.0, 3.0].into();
        let next: DataColumn = vec![2.0_f64, 3.0, 4.0].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert_eq!(enter, vec![2]);
        assert_eq!(update, vec![(1, 0), (2, 1)]);
        assert_eq!(exit.len(), 1);
        assert_eq!(exit[0].as_number(), Some(1.0));
    }

    #[test]
    fn i64_keys() {
        // The default key column for "no .keys() supplied" — synthesised
        // (0..n as i64) on the geom side. Diff'ing across an N change
        // should produce tail-enters or tail-exits.
        let prev: DataColumn = (0_i64..3).into();
        let next: DataColumn = (0_i64..5).into();
        let (enter, update, exit) = diff(&prev, &next);
        assert_eq!(enter, vec![3, 4]); // tail-enter
        assert_eq!(update, vec![(0, 0), (1, 1), (2, 2)]);
        assert!(exit.is_empty());

        // Reverse direction.
        let prev: DataColumn = (0_i64..5).into();
        let next: DataColumn = (0_i64..3).into();
        let (enter, update, exit) = diff(&prev, &next);
        assert!(enter.is_empty());
        assert_eq!(update, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(exit.len(), 2); // tail-exit
        assert_eq!(exit[0].as_number(), Some(3.0));
        assert_eq!(exit[1].as_number(), Some(4.0));
    }

    #[test]
    fn bool_keys() {
        let prev: DataColumn = vec![true, false].into();
        let next: DataColumn = vec![false, true].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert!(enter.is_empty());
        assert!(exit.is_empty());
        // false → idx 1 in prev maps to idx 0 in next; true vice versa.
        assert_eq!(update, vec![(1, 0), (0, 1)]);
    }

    #[test]
    fn date_keys() {
        let a = Date::from_ymd(2024, 1, 1);
        let b = Date::from_ymd(2024, 1, 2);
        let c = Date::from_ymd(2024, 1, 3);
        let prev: DataColumn = vec![a, b, c].into();
        let next: DataColumn = vec![b, c].into();
        let (enter, update, exit) = diff(&prev, &next);
        assert!(enter.is_empty());
        assert_eq!(update, vec![(1, 0), (2, 1)]);
        assert_eq!(exit.len(), 1);
        match &exit[0] {
            Value::Date(d) => assert_eq!(*d, a.to_days()),
            _ => panic!("expected Value::Date"),
        }
    }

    // ── KeyIndex API ──

    #[test]
    fn key_index_lookup_returns_first_match() {
        let prev: DataColumn = vec!["a", "b", "c"].into();
        let next: DataColumn = vec!["b"].into();
        let idx = KeyIndex::build(&prev);
        assert_eq!(idx.lookup(&prev, &next, 0), Some(1));
    }

    #[test]
    fn key_index_lookup_miss() {
        let prev: DataColumn = vec!["a", "b", "c"].into();
        let next: DataColumn = vec!["z"].into();
        let idx = KeyIndex::build(&prev);
        assert!(idx.lookup(&prev, &next, 0).is_none());
    }

    #[test]
    fn key_index_len_and_empty() {
        let empty: DataColumn = Vec::<&'static str>::new().into();
        let idx = KeyIndex::build(&empty);
        assert_eq!(idx.len(), 0);
        assert!(idx.is_empty());

        let col: DataColumn = vec!["a", "b", "c"].into();
        let idx = KeyIndex::build(&col);
        assert_eq!(idx.len(), 3);
        assert!(!idx.is_empty());
    }

    // ── Positional fast path ──

    #[test]
    fn positional_pure_enter() {
        let (enter, update, exit) = diff_positional(0, 3);
        assert_eq!(enter, vec![0, 1, 2]);
        assert!(update.is_empty());
        assert!(exit.is_empty());
    }

    #[test]
    fn positional_pure_exit() {
        let (enter, update, exit) = diff_positional(3, 0);
        assert!(enter.is_empty());
        assert!(update.is_empty());
        assert_eq!(exit.len(), 3);
        for (i, v) in exit.iter().enumerate() {
            assert_eq!(v.as_number(), Some(i as f64));
        }
    }

    #[test]
    fn positional_pure_update_identity() {
        let (enter, update, exit) = diff_positional(3, 3);
        assert!(enter.is_empty());
        assert_eq!(update, vec![(0, 0), (1, 1), (2, 2)]);
        assert!(exit.is_empty());
    }

    #[test]
    fn positional_tail_enter() {
        let (enter, update, exit) = diff_positional(3, 5);
        assert_eq!(enter, vec![3, 4]);
        assert_eq!(update, vec![(0, 0), (1, 1), (2, 2)]);
        assert!(exit.is_empty());
    }

    #[test]
    fn positional_tail_exit() {
        let (enter, update, exit) = diff_positional(5, 3);
        assert!(enter.is_empty());
        assert_eq!(update, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(exit.len(), 2);
        assert_eq!(exit[0].as_number(), Some(3.0));
        assert_eq!(exit[1].as_number(), Some(4.0));
    }

    #[test]
    fn positional_empty_both() {
        let (enter, update, exit) = diff_positional(0, 0);
        assert!(enter.is_empty());
        assert!(update.is_empty());
        assert!(exit.is_empty());
    }

    #[test]
    fn positional_matches_columns_form() {
        // For every shape we care about, diff_positional(prev_n, next_n)
        // produces the same triple as diff_columns on the equivalent
        // synthesised I64 columns. This is the contract that lets the
        // PointGeom fast-path swap freely between the two without
        // observable difference.
        for (p, n) in [
            (0_usize, 0),
            (0, 3),
            (3, 0),
            (3, 3),
            (3, 5),
            (5, 3),
            (1, 10),
            (10, 1),
        ] {
            let (e_fast, u_fast, x_fast) = diff_positional(p, n);

            let prev: DataColumn = (0_i64..p as i64).into();
            let next: DataColumn = (0_i64..n as i64).into();
            let idx = KeyIndex::build(&prev);
            let (e_full, u_full, x_full) = diff_columns(&prev, &idx, &next);

            assert_eq!(e_fast, e_full, "enter mismatch for ({p}, {n})");
            assert_eq!(u_fast, u_full, "update mismatch for ({p}, {n})");
            assert_eq!(
                x_fast.len(),
                x_full.len(),
                "exit len mismatch for ({p}, {n})"
            );
            for (a, b) in x_fast.iter().zip(&x_full) {
                assert!(
                    a.key_eq(b),
                    "exit value mismatch for ({p}, {n}): fast={a:?} full={b:?}"
                );
            }
        }
    }

    #[test]
    fn key_index_lookup_ignores_consumption() {
        // KeyIndex::lookup deliberately doesn't track which prev rows
        // have been used. Repeated lookups for the same key always
        // return the same first match.
        let prev: DataColumn = vec!["a", "a"].into();
        let next: DataColumn = vec!["a"].into();
        let idx = KeyIndex::build(&prev);
        assert_eq!(idx.lookup(&prev, &next, 0), Some(0));
        // Calling twice — still returns 0 because there's no state.
        assert_eq!(idx.lookup(&prev, &next, 0), Some(0));
    }
}
