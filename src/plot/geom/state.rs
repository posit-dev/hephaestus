//! [`GeomState`] — the common state every concrete geom holds, plus the
//! `build_from` helpers that lift the universal validation + key
//! synthesis + first-frame snapshot pattern out of per-geom code.
//!
//! Every geom struct in `crate::plot::geom` is just `GeomState` plus
//! whatever caches the geom needs for its own draw loop (LineGeom adds
//! `Vec<MarkSlot>`; future area / ribbon / bar geoms add their own).
//! The `Geom` trait's `state()` / `state_mut()` accessors give the
//! shared default impls (`len`, `is_empty`, `mark_count`,
//! `rebuild_diff_against_previous`, `declared_channels`) something to
//! dispatch through, so the per-geom `impl Geom` is four lines plus the
//! geom-specific `draw`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::value::{DataColumn, Value};

use super::{empty_datacolumn_like, Channel, ChannelDecl, ExpectedOutput, Keys};

// ─── GeomState ───────────────────────────────────────────────────────────────

/// The shared state every concrete geom carries.
///
/// Owns the keys + channels + diff snapshot + dirty flag + declared
/// channel list. Per-geom impls compose this with whatever extra caches
/// they need (e.g. `LineGeom` adds a precomputed `Vec<MarkSlot>`).
pub struct GeomState {
    pub(crate) keys: Keys,
    pub(crate) channels: HashMap<String, Channel>,

    pub(crate) prev_keys: Keys,
    pub(crate) prev_channels: HashMap<String, Channel>,

    /// Diff results from the most recent [`Self::rebuild_diff_against_previous`].
    /// Stored for the animation pass to interpolate along the update
    /// edges; the draw loop itself snaps to the current state.
    pub(crate) enter: Vec<usize>,
    pub(crate) update: Vec<(usize, usize)>,
    pub(crate) exit: Vec<Value>,

    pub(crate) dirty: bool,
    pub(crate) declared: Vec<ChannelDecl>,
}

impl GeomState {
    /// Build the shared state from a geom's `build_from` call site.
    /// Handles key synthesis, first-frame snapshot creation, and
    /// declared-channel filtering. The caller is responsible for
    /// per-geom validation + default injection before this is invoked.
    pub fn from_builder(
        keys_opt: Option<DataColumn>,
        channels: HashMap<String, Channel>,
        n: usize,
        keys_strategy: KeysStrategy,
        declared: Vec<ChannelDecl>,
    ) -> Self {
        let keys = build_keys(keys_opt, n, keys_strategy);
        let prev_keys = keys.empty_like();
        let prev_channels = empty_channels_like(&channels);
        Self {
            keys,
            channels,
            prev_keys,
            prev_channels,
            enter: Vec::new(),
            update: Vec::new(),
            exit: Vec::new(),
            dirty: true,
            declared,
        }
    }

    /// Row count = `Keys::len()`. All data columns have this length by
    /// build-time validation.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True when the geom holds no rows.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Update a single channel in place. Length-validates data columns
    /// (scaled or raw) against the current row count and marks the
    /// state dirty.
    pub fn set(&mut self, channel: impl Into<String>, value: impl Into<Channel>) {
        let name: String = channel.into();
        let value: Channel = value.into();
        if let Some(len) = value.data_len() {
            if len != self.keys.len() {
                panic!(
                    "GeomState::set: \"{name}\" length {len} does not match row count {}",
                    self.keys.len()
                );
            }
        }
        self.channels.insert(name, value);
        self.dirty = true;
    }

    /// Replace the current keys / channels / declared with a freshly-built
    /// state, rotating the previous current into `prev_*` so diff produces
    /// meaningful enter / update / exit on the next rebuild.
    ///
    /// Used by `Geom::update` after the user's closure rebuilds the geom.
    pub fn replace_current_with(&mut self, new: GeomState) {
        self.prev_keys = std::mem::replace(&mut self.keys, new.keys);
        self.prev_channels = std::mem::replace(&mut self.channels, new.channels);
        self.declared = new.declared;
        self.dirty = true;
    }

    /// Rebuild the enter / update / exit sets against the previous
    /// snapshot, then rotate the snapshot to the current state. Idempotent
    /// — bails early when `dirty` is `false`.
    pub fn rebuild_diff_against_previous(&mut self) {
        if !self.dirty {
            return;
        }
        let (enter, update, exit) = match (&self.prev_keys, &self.keys) {
            (Keys::Explicit(prev_col), Keys::Explicit(next_col)) => {
                let idx = KeyIndex::build(prev_col);
                diff_columns(prev_col, &idx, next_col)
            }
            _ => diff_positional(self.prev_keys.len(), self.keys.len()),
        };
        self.enter = enter;
        self.update = update;
        self.exit = exit;
        self.prev_keys = self.keys.clone();
        self.prev_channels = self.channels.clone();
        self.dirty = false;
    }

    /// [`Self::rebuild_diff_against_previous`] at **mark** granularity.
    ///
    /// A grouped geom's rows aren't its marks — several rows share one
    /// key — so the key column is first collapsed to one entry per
    /// mark, using each mark's first row. Callers pass the first-row
    /// index of every previous and current mark; how those marks are
    /// grouped is the geom's own business (`PolygonGeom` splits on
    /// rings as well as keys).
    ///
    /// Leaves `dirty` set for the caller to clear once it has also
    /// rebuilt whatever mark cache it keeps alongside the state.
    pub fn rebuild_grouped_diff(
        &mut self,
        prev_first_rows: &[usize],
        next_first_rows: &[usize],
        geom_name: &str,
    ) {
        let (enter, update, exit) = match (&self.prev_keys, &self.keys) {
            (Keys::Explicit(prev_col), Keys::Explicit(next_col)) => {
                let prev_unique = super::marks::unique_values_at_first_rows(
                    prev_col,
                    prev_first_rows.iter().copied(),
                    geom_name,
                );
                let next_unique = super::marks::unique_values_at_first_rows(
                    next_col,
                    next_first_rows.iter().copied(),
                    geom_name,
                );
                let idx = KeyIndex::build(&prev_unique);
                diff_columns(&prev_unique, &idx, &next_unique)
            }
            _ => diff_positional(prev_first_rows.len(), next_first_rows.len()),
        };
        self.enter = enter;
        self.update = update;
        self.exit = exit;
        self.prev_keys = self.keys.clone();
        self.prev_channels = self.channels.clone();
        self.dirty = false;
    }
}

// ─── KeysStrategy ────────────────────────────────────────────────────────────

/// How the geom interprets a synthesised (no-`.keys()` supplied) key column.
#[derive(Clone, Copy, Debug)]
pub enum KeysStrategy {
    /// Every row gets its own positional key (PointGeom-style). Unset
    /// keys → `Keys::Positional(n)`, zero allocation.
    PerRow,
    /// All rows form one mark (LineGeom / future multi-row-per-mark
    /// geoms). Unset keys → a length-`n` `Keys::Explicit` column of a
    /// single placeholder value, so the diff machinery sees one mark.
    OneMark,
}

fn build_keys(keys_opt: Option<DataColumn>, n: usize, strategy: KeysStrategy) -> Keys {
    match (keys_opt, strategy) {
        (Some(k), _) => {
            if k.len() != n {
                panic!(
                    "build_keys: keys length {} does not match row count {n}",
                    k.len()
                );
            }
            Keys::Explicit(k)
        }
        (None, KeysStrategy::PerRow) => Keys::Positional(n),
        (None, KeysStrategy::OneMark) => {
            let placeholder: Arc<str> = Arc::from("_");
            Keys::Explicit(DataColumn::String(vec![placeholder; n]))
        }
    }
}

// ─── build_from helpers ──────────────────────────────────────────────────────

/// Read a required data channel out of `channels`, panicking with a
/// clear message when the channel is missing or holds a constant.
/// Returns a borrow that callers can `.clone()` if they need a `DataColumn`
/// (or just use for length validation).
pub fn require_data_column<'a>(
    name: &str,
    channels: &'a HashMap<String, Channel>,
    geom_label: &str,
) -> &'a DataColumn {
    match channels.get(name) {
        Some(Channel::Data(c)) | Some(Channel::RawData(c)) => c,
        Some(Channel::Constant(_)) | Some(Channel::RawConstant(_)) => panic!(
            "{geom_label}::build: \"{name}\" must be data, not constant — positions vary per row"
        ),
        None => panic!("{geom_label}::build: missing required channel \"{name}\""),
    }
}

/// Pull the required `x` data column to fix the row count `n`, then
/// require every name in `siblings` to be a data channel of the same
/// length. Panics with a geom-labelled message on missing channel,
/// constant where a column was expected, or length mismatch.
pub fn require_x_and_siblings(
    channels: &HashMap<String, Channel>,
    siblings: &[&str],
    geom_label: &str,
) -> usize {
    let n = require_data_column("x", channels, geom_label).len();
    for &name in siblings {
        let len = require_data_column(name, channels, geom_label).len();
        if len != n {
            panic!("{geom_label}::build: \"{name}\" length {len} does not match \"x\" length {n}");
        }
    }
    n
}

/// Validate that every data channel in `channels` (scaled or raw) has
/// length `n`. Panics on mismatch with a geom-labelled message
/// identifying the offending channel.
pub fn validate_channel_lengths(channels: &HashMap<String, Channel>, n: usize, geom_label: &str) {
    for (name, ch) in channels {
        if let Some(len) = ch.data_len() {
            if len != n {
                panic!("{geom_label}::build: \"{name}\" length {len} does not match row count {n}");
            }
        }
    }
}

/// Validate the `"pick_id"` channel if present. Constants and Data
/// columns whose values are knowable at build time are checked to be
/// finite non-negative integers ≤ `0xFF_FFFF` (the 24-bit pick budget).
/// Scale-routed values are deferred to per-row draw resolution (the
/// output depends on draw-time scale state and can't be checked here).
///
/// `0` is permitted — it maps to [`PickId::Block`](crate::pick::PickId)
/// per the user-controlled pick-id contract.
pub fn validate_pick_id_channel(channels: &HashMap<String, Channel>, geom_label: &str) {
    let ch = match channels.get("pick_id") {
        Some(c) => c,
        None => return,
    };
    let check = |v: &Value, where_: &str| {
        match v.as_number() {
        Some(n) if n.is_finite() && n >= 0.0 && n <= 0xFF_FFFF as f64 && n.trunc() == n => {}
        Some(n) => panic!(
            "{geom_label}::build: \"pick_id\" {where_} must be a non-negative integer ≤ 0xFFFFFF, got {n}"
        ),
        None => panic!(
            "{geom_label}::build: \"pick_id\" {where_} must be numeric (Number/Date/DateTime/Time/Duration), got {v:?}"
        ),
    }
    };
    match ch {
        Channel::Constant(v) | Channel::RawConstant(v) => check(v, "constant"),
        Channel::Data(col) | Channel::RawData(col) => {
            for i in 0..col.len() {
                check(&col.get(i), "data column");
            }
        }
    }
}

/// Validate that every supplied channel name appears in the geom's
/// catalog. A name the geom doesn't declare can only be a caller
/// mistake — a typo, or a channel that belongs to a different geom —
/// and nothing downstream would ever read it, so it fails here rather
/// than rendering as the default. Panics with the offending names.
pub fn validate_known_channels(
    channels: &HashMap<String, Channel>,
    catalog: &[(&'static str, ExpectedOutput)],
    geom_label: &str,
) {
    let mut unknown: Vec<&str> = channels
        .keys()
        .map(String::as_str)
        .filter(|name| !catalog.iter().any(|(known, _)| known == name))
        .collect();
    if unknown.is_empty() {
        return;
    }
    unknown.sort_unstable();
    let names: Vec<String> = unknown.iter().map(|n| format!("\"{n}\"")).collect();
    let label = match unknown.len() {
        1 => "channel",
        _ => "channels",
    };
    panic!(
        "{geom_label}::build: unknown {label} {} — not declared by this geom",
        names.join(", ")
    );
}

/// Filter the geom's channel catalog against the channels actually
/// supplied by the user. Each catalog entry is `(name, expected_output)`;
/// supplied channels become [`ChannelDecl`] entries, others are dropped.
/// Output is sorted alphabetically for determinism in tests / validation.
pub fn filter_declared(
    channels: &HashMap<String, Channel>,
    catalog: &[(&'static str, ExpectedOutput)],
) -> Vec<ChannelDecl> {
    let mut out = Vec::with_capacity(catalog.len());
    for (name, expected) in catalog {
        if let Some(ch) = channels.get(*name) {
            out.push(ChannelDecl {
                name,
                data_bound: ch.is_data(),
                expected_output: *expected,
            });
        }
    }
    out.sort_by_key(|d| d.name);
    out
}

/// Run the universal validate-then-build tail every `BuildableGeom`
/// implementation finishes with: [`validate_known_channels`],
/// [`validate_channel_lengths`], [`validate_pick_id_channel`],
/// [`filter_declared`], then [`GeomState::from_builder`].
pub fn finalize_state(
    keys_opt: Option<DataColumn>,
    channels: HashMap<String, Channel>,
    n: usize,
    strategy: KeysStrategy,
    catalog: &[(&'static str, ExpectedOutput)],
    geom_label: &str,
) -> GeomState {
    validate_known_channels(&channels, catalog, geom_label);
    validate_channel_lengths(&channels, n, geom_label);
    validate_pick_id_channel(&channels, geom_label);
    let declared = filter_declared(&channels, catalog);
    GeomState::from_builder(keys_opt, channels, n, strategy, declared)
}

// ─── Snapshot helper ─────────────────────────────────────────────────────────

/// Snapshot of `channels` where every `Data` column is replaced by its
/// length-0 counterpart and every `Constant` is preserved. Seeds the
/// first-frame `prev_channels` so the animation pass has a stable
/// "previous state" to interpolate from on the first draw.
fn empty_channels_like(channels: &HashMap<String, Channel>) -> HashMap<String, Channel> {
    channels
        .iter()
        .map(|(name, ch)| {
            let prev = match ch {
                Channel::Constant(v) => Channel::Constant(v.clone()),
                Channel::Data(col) => Channel::Data(empty_datacolumn_like(col)),
                Channel::RawConstant(v) => Channel::RawConstant(v.clone()),
                Channel::RawData(col) => Channel::RawData(empty_datacolumn_like(col)),
            };
            (name.clone(), prev)
        })
        .collect()
}

// ─── impl_geom_inherents! macros ────────────────────────────────────────────
//
// Every concrete geom shares the same forwarding shape: `builder()` returns
// an empty `GeomBuilder<Self>`; `len` / `is_empty` / `set` / `update`
// forward through `self.state`. The macros generate that shape so each
// geom contributes a single-line invocation instead of an 18-line `impl`
// block.
//
// Two variants:
//
// - [`impl_geom_inherents!($ty)`] — for **per-row geoms** (row == mark):
//   PointGeom, RectGeom, SegmentGeom, EllipseGeom, WedgeGeom, TextGeom.
//   Adds `has_explicit_keys()` as a meaningful predicate (the keys column
//   is only Explicit when the user supplied one via `.keys(col)`).
//
// - [`impl_geom_inherents_grouped!($ty)`] — for **multi-row-per-mark
//   geoms**: LineGeom, PolygonGeom. Omits `has_explicit_keys` (the
//   `KeysStrategy::OneMark` rewriter always produces an Explicit
//   placeholder column, so the predicate would always be `true` and
//   carry no information). The shared [`Geom::invalidate_caches`] hook is
//   invoked at the tail of `update`; grouped geoms override the hook to
//   drop their per-mark layout cache.
//
// Both macros call `<Self as Geom>::invalidate_caches(self)` at the tail
// of `update`. Per-row geoms inherit the empty default; LineGeom and
// PolygonGeom override the trait method.

/// Generate the standard inherent block for a **per-row** geom (row == mark).
///
/// Emits: `builder`, `len`, `is_empty`, `has_explicit_keys`, `set`, `update`.
/// `update` calls [`crate::plot::geom::Geom::invalidate_caches`] at the
/// tail so future per-row geoms with caches can opt in without touching
/// the macro.
#[macro_export]
#[doc(hidden)]
macro_rules! impl_geom_inherents {
    ($ty:ident) => {
        impl $ty {
            /// Entry point for construction. Returns an empty
            /// `GeomBuilder<Self>`.
            pub fn builder() -> $crate::plot::GeomBuilder<Self> {
                $crate::plot::GeomBuilder::new()
            }

            /// Row count. All data columns + keys have this length.
            pub fn len(&self) -> usize {
                self.state.len()
            }

            /// True when the geom holds no rows.
            pub fn is_empty(&self) -> bool {
                self.state.is_empty()
            }

            /// `true` if the user supplied an explicit key column via
            /// `.keys(col)`. Per-row geoms synthesise positional keys
            /// when none are supplied, so this predicate distinguishes
            /// identity-matched vs. positional-matched diff behaviour.
            pub fn has_explicit_keys(&self) -> bool {
                self.state.keys.is_explicit()
            }

            /// Update a single channel in place. Length-validates data
            /// columns against the current row count (mismatch panics)
            /// and marks the geom dirty so diff rebuilds before the next
            /// draw.
            pub fn set(
                &mut self,
                channel: impl Into<String>,
                value: impl Into<$crate::plot::Channel>,
            ) {
                self.state.set(channel, value);
            }

            /// Atomic multi-channel / N-changing update. The closure
            /// receives a `GeomBuilder` pre-populated with the geom's
            /// current state; on return the builder is built and the
            /// result atomically replaces the geom's state, rotating the
            /// previous state into the diff snapshot.
            pub fn update(&mut self, f: impl FnOnce(&mut $crate::plot::GeomBuilder<Self>)) {
                let carry_keys = match &self.state.keys {
                    $crate::plot::Keys::Explicit(col) => Some(col.clone()),
                    $crate::plot::Keys::Positional(_) => None,
                };
                let mut b =
                    $crate::plot::GeomBuilder::from_parts(carry_keys, self.state.channels.clone());
                f(&mut b);
                let new = b.build();
                self.state.replace_current_with(new.state);
                <Self as $crate::plot::Geom>::invalidate_caches(self);
            }
        }
    };
}

/// Generate the standard inherent block for a **grouped** (multi-row-per-mark)
/// geom — LineGeom, PolygonGeom.
///
/// Same shape as [`impl_geom_inherents!`] minus `has_explicit_keys`:
/// grouped geoms' `KeysStrategy::OneMark` always rewrites the keys to an
/// Explicit placeholder, so the predicate would carry no information.
/// `update`'s tail still invokes [`crate::plot::geom::Geom::invalidate_caches`]
/// — grouped geoms override that hook to drop their per-mark layout cache.
#[macro_export]
#[doc(hidden)]
macro_rules! impl_geom_inherents_grouped {
    ($ty:ident) => {
        impl $ty {
            /// Entry point for construction. Returns an empty
            /// `GeomBuilder<Self>`.
            pub fn builder() -> $crate::plot::GeomBuilder<Self> {
                $crate::plot::GeomBuilder::new()
            }

            /// Row count. All data columns + keys have this length.
            pub fn len(&self) -> usize {
                self.state.len()
            }

            /// True when the geom holds no rows.
            pub fn is_empty(&self) -> bool {
                self.state.is_empty()
            }

            /// Update a single channel in place. Length-validates data
            /// columns against the current row count (mismatch panics)
            /// and marks the geom dirty so diff rebuilds before the next
            /// draw.
            pub fn set(
                &mut self,
                channel: impl Into<String>,
                value: impl Into<$crate::plot::Channel>,
            ) {
                self.state.set(channel, value);
            }

            /// Atomic multi-channel / N-changing update. The closure
            /// receives a `GeomBuilder` pre-populated with the geom's
            /// current state; on return the builder is built and the
            /// result atomically replaces the geom's state, rotating the
            /// previous state into the diff snapshot.
            pub fn update(&mut self, f: impl FnOnce(&mut $crate::plot::GeomBuilder<Self>)) {
                let carry_keys = match &self.state.keys {
                    $crate::plot::Keys::Explicit(col) => Some(col.clone()),
                    $crate::plot::Keys::Positional(_) => None,
                };
                let mut b =
                    $crate::plot::GeomBuilder::from_parts(carry_keys, self.state.channels.clone());
                f(&mut b);
                let new = b.build();
                self.state.replace_current_with(new.state);
                <Self as $crate::plot::Geom>::invalidate_caches(self);
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::value::Value;

    const CATALOG: &[(&str, ExpectedOutput)] = &[
        ("x", ExpectedOutput::Numbers),
        ("y", ExpectedOutput::Numbers),
        ("fill", ExpectedOutput::Colors),
        ("pick_id", ExpectedOutput::Numbers),
    ];

    fn channels(entries: Vec<(&str, Channel)>) -> HashMap<String, Channel> {
        entries
            .into_iter()
            .map(|(name, ch)| (name.to_string(), ch))
            .collect()
    }

    fn xy(n: usize) -> HashMap<String, Channel> {
        channels(vec![
            ("x", Channel::Data(DataColumn::F64(vec![0.0; n]))),
            ("y", Channel::Data(DataColumn::F64(vec![0.0; n]))),
        ])
    }

    fn finalize(ch: HashMap<String, Channel>, n: usize, strategy: KeysStrategy) -> GeomState {
        finalize_state(None, ch, n, strategy, CATALOG, "TestGeom")
    }

    // ── Validation ─────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "unknown channel \"colour\" — not declared by this geom")]
    fn finalize_state_rejects_a_channel_the_geom_does_not_declare() {
        let mut ch = xy(2);
        ch.insert("colour".to_string(), Channel::Constant(Value::Number(1.0)));
        finalize(ch, 2, KeysStrategy::PerRow);
    }

    #[test]
    #[should_panic(expected = "unknown channels \"colour\", \"shape\"")]
    fn finalize_state_lists_every_unknown_channel_sorted() {
        let mut ch = xy(2);
        ch.insert("shape".to_string(), Channel::Constant(Value::Number(1.0)));
        ch.insert("colour".to_string(), Channel::Constant(Value::Number(1.0)));
        finalize(ch, 2, KeysStrategy::PerRow);
    }

    #[test]
    #[should_panic(expected = "\"y\" length 2 does not match row count 3")]
    fn finalize_state_rejects_a_short_data_column() {
        let ch = channels(vec![
            ("x", Channel::Data(DataColumn::F64(vec![0.0; 3]))),
            ("y", Channel::Data(DataColumn::F64(vec![0.0; 2]))),
        ]);
        finalize(ch, 3, KeysStrategy::PerRow);
    }

    #[test]
    #[should_panic(expected = "\"pick_id\" data column must be a non-negative integer")]
    fn finalize_state_rejects_a_fractional_pick_id() {
        let mut ch = xy(2);
        ch.insert(
            "pick_id".to_string(),
            Channel::Data(DataColumn::F64(vec![1.0, 2.5])),
        );
        finalize(ch, 2, KeysStrategy::PerRow);
    }

    #[test]
    #[should_panic(expected = "missing required channel \"x\"")]
    fn require_data_column_reports_a_missing_channel() {
        require_data_column("x", &HashMap::new(), "TestGeom");
    }

    #[test]
    #[should_panic(expected = "\"x\" must be data, not constant")]
    fn require_data_column_rejects_a_constant_where_a_column_is_needed() {
        let ch = channels(vec![("x", Channel::Constant(Value::Number(1.0)))]);
        require_data_column("x", &ch, "TestGeom");
    }

    #[test]
    #[should_panic(expected = "\"y\" length 1 does not match \"x\" length 3")]
    fn require_x_and_siblings_rejects_a_sibling_of_a_different_length() {
        let ch = channels(vec![
            ("x", Channel::Data(DataColumn::F64(vec![0.0; 3]))),
            ("y", Channel::Data(DataColumn::F64(vec![0.0; 1]))),
        ]);
        require_x_and_siblings(&ch, &["y"], "TestGeom");
    }

    #[test]
    #[should_panic(expected = "keys length 2 does not match row count 3")]
    fn from_builder_rejects_a_key_column_of_the_wrong_length() {
        GeomState::from_builder(
            Some(DataColumn::String(vec![Arc::from("a"), Arc::from("b")])),
            xy(3),
            3,
            KeysStrategy::PerRow,
            Vec::new(),
        );
    }

    #[test]
    #[should_panic(expected = "\"fill\" length 4 does not match row count 2")]
    fn set_rejects_a_column_that_does_not_match_the_row_count() {
        let mut state = finalize(xy(2), 2, KeysStrategy::PerRow);
        state.set(
            "fill",
            Channel::Data(DataColumn::F64(vec![0.0, 1.0, 2.0, 3.0])),
        );
    }

    // ── Key synthesis ──────────────────────────────────────────────

    #[test]
    fn per_row_strategy_synthesises_positional_keys() {
        let state = finalize(xy(3), 3, KeysStrategy::PerRow);
        match &state.keys {
            Keys::Positional(n) => assert_eq!(*n, 3),
            Keys::Explicit(_) => panic!("expected positional keys"),
        }
        assert_eq!(state.len(), 3);
        assert!(!state.is_empty());
    }

    #[test]
    fn one_mark_strategy_synthesises_a_single_shared_key() {
        // Every row carries the same placeholder, so the diff machinery
        // and mark grouping both see exactly one mark.
        let state = finalize(xy(3), 3, KeysStrategy::OneMark);
        match &state.keys {
            Keys::Explicit(col) => {
                assert_eq!(col.len(), 3);
                assert!(col.get(0).key_eq(&col.get(2)));
            }
            Keys::Positional(_) => panic!("expected an explicit placeholder column"),
        }
        assert_eq!(super::super::marks::build_marks(&state.keys).len(), 1);
    }

    #[test]
    fn a_supplied_key_column_survives_either_strategy() {
        for strategy in [KeysStrategy::PerRow, KeysStrategy::OneMark] {
            let keys = DataColumn::String(vec![Arc::from("a"), Arc::from("b")]);
            let state = GeomState::from_builder(Some(keys), xy(2), 2, strategy, Vec::new());
            match &state.keys {
                Keys::Explicit(col) => assert!(col.get(0).key_eq(&Value::String(Arc::from("a")))),
                Keys::Positional(_) => panic!("supplied keys must not be replaced"),
            }
        }
    }

    #[test]
    fn a_fresh_state_diffs_as_all_enter() {
        // `prev_*` is seeded with the length-zero counterpart, so the
        // first draw treats every row as entering.
        let mut state = finalize(xy(3), 3, KeysStrategy::PerRow);
        assert!(state.dirty);
        state.rebuild_diff_against_previous();
        assert_eq!(state.enter, vec![0, 1, 2]);
        assert!(state.update.is_empty());
        assert!(state.exit.is_empty());
        assert!(!state.dirty);
    }

    // ── Declared channels ──────────────────────────────────────────

    #[test]
    fn filter_declared_keeps_supplied_catalog_entries_in_name_order() {
        let mut ch = xy(2);
        ch.insert(
            "fill".to_string(),
            Channel::Constant(Value::Color(crate::color::rgb(1.0, 0.0, 0.0))),
        );
        let state = finalize(ch, 2, KeysStrategy::PerRow);
        let names: Vec<&str> = state.declared.iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["fill", "x", "y"]);
        let fill = state.declared.iter().find(|d| d.name == "fill").unwrap();
        assert!(!fill.data_bound, "a constant channel is not data-bound");
        assert_eq!(fill.expected_output, ExpectedOutput::Colors);
        let x = state.declared.iter().find(|d| d.name == "x").unwrap();
        assert!(x.data_bound);
    }
}
