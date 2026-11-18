//! Tables for values a document stores once and references by index.
//!
//! Two motives, and it matters which applies to a given table:
//!
//! **Identity.** `Value::Geometry` and `DataColumn::Geometry` hold
//! `Arc<Geometry>`, and `Value::key_eq` / `key_hash` compare it with
//! `Arc::ptr_eq` / `Arc::as_ptr` — so two geometries with identical
//! coordinates are different keys unless they share one allocation.
//! Rich-text style sheets are the same story through
//! [`RichShapeCache`](crate::text::rich::RichShapeCache), which keys on
//! `Arc::as_ptr`: a sheet rebuilt per label instead of once per document
//! misses the cache on every frame. For these, handing out one `Arc` per
//! table entry is a correctness requirement, not a size optimization.
//!
//! **Size.** A geometry column with fifty thousand rows drawn from two
//! hundred country outlines is two hundred outlines, not fifty thousand.
//! Interning is what keeps the document proportional to the distinct
//! values rather than the row count.

#[cfg(feature = "document-write")]
use std::collections::HashMap;
use std::sync::Arc;

use crate::brush::Brush;
use crate::scales::geometry::Geometry;
use crate::text::rich::RichTextStyleSheet;

/// Write-side tables: value to index, assigned in first-seen order.
#[cfg(feature = "document-write")]
#[derive(Debug, Default)]
pub(crate) struct WriteTables {
    /// Distinct geometries, keyed by the `Arc` they arrived in.
    ///
    /// Keyed on pointer rather than content: matching what the live
    /// identity comparison does means the document reproduces exactly
    /// the sharing the plot had, and it avoids hashing large coordinate
    /// lists.
    geometry_index: HashMap<*const Geometry, u32>,
    geometries: Vec<Arc<Geometry>>,
    /// Distinct brushes, keyed by **content** rather than pointer.
    ///
    /// `Value::Brush`'s own `key_eq` is content equality (with a
    /// pointer fast path), so content keying reproduces exactly the
    /// dedup live comparisons report — the same argument as strings,
    /// through zygos's `brush_key_eq` / `brush_key_hash`.
    brush_index: HashMap<BrushKey, u32>,
    brushes: Vec<Arc<Brush>>,
    /// Distinct style sheets, keyed by `Arc` pointer for the same
    /// reason, paired with the name the document refers to them by.
    sheet_index: HashMap<*const RichTextStyleSheet, u32>,
    sheets: Vec<Arc<RichTextStyleSheet>>,
    /// Distinct strings, keyed by **content** rather than pointer.
    ///
    /// `Arc<str>` carries no identity semantics — nothing compares one
    /// by pointer — so content keying is both correct and strictly
    /// better here: it folds together equal strings that arrived in
    /// separate allocations, which is the common case for a grouping
    /// column built from a `Vec<String>`.
    string_index: HashMap<Arc<str>, u32>,
    strings: Vec<Arc<str>>,
}

#[cfg(feature = "document-write")]
impl WriteTables {
    /// Index for `g`, adding it to the table if it's new.
    pub(crate) fn geometry(&mut self, g: &Arc<Geometry>) -> u32 {
        let key = Arc::as_ptr(g);
        if let Some(&i) = self.geometry_index.get(&key) {
            return i;
        }
        let i = self.geometries.len() as u32;
        self.geometries.push(g.clone());
        self.geometry_index.insert(key, i);
        i
    }

    /// Index for `s`, adding it to the table if it's new.
    pub(crate) fn sheet(&mut self, s: &Arc<RichTextStyleSheet>) -> u32 {
        let key = Arc::as_ptr(s);
        if let Some(&i) = self.sheet_index.get(&key) {
            return i;
        }
        let i = self.sheets.len() as u32;
        self.sheets.push(s.clone());
        self.sheet_index.insert(key, i);
        i
    }

    /// The interned geometries, in index order.
    pub(crate) fn geometries(&self) -> &[Arc<Geometry>] {
        &self.geometries
    }

    /// Index for `b`, adding it to the table if it's new.
    pub(crate) fn brush(&mut self, b: &Arc<Brush>) -> u32 {
        let key = BrushKey(b.clone());
        if let Some(&i) = self.brush_index.get(&key) {
            return i;
        }
        let i = self.brushes.len() as u32;
        self.brushes.push(b.clone());
        self.brush_index.insert(key, i);
        i
    }

    /// The interned brushes, in index order.
    pub(crate) fn brushes(&self) -> &[Arc<Brush>] {
        &self.brushes
    }

    /// Index for `s`, adding it to the table if it's new.
    pub(crate) fn string(&mut self, s: &Arc<str>) -> u32 {
        if let Some(&i) = self.string_index.get(s) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.clone());
        self.string_index.insert(s.clone(), i);
        i
    }

    /// The interned style sheets, in index order.
    pub(crate) fn sheets(&self) -> &[Arc<RichTextStyleSheet>] {
        &self.sheets
    }

    /// The interned strings, in index order.
    pub(crate) fn strings(&self) -> &[Arc<str>] {
        &self.strings
    }
}

/// A brush as a content-keyed table entry: equality and hashing are
/// zygos's structural brush comparison, so two brushes that render
/// identically share one entry no matter how they were allocated.
#[cfg(feature = "document-write")]
#[derive(Clone, Debug)]
struct BrushKey(Arc<Brush>);

#[cfg(feature = "document-write")]
impl PartialEq for BrushKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.key_eq(&other.0)
    }
}

#[cfg(feature = "document-write")]
impl Eq for BrushKey {}

#[cfg(feature = "document-write")]
impl std::hash::Hash for BrushKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.key_hash(state);
    }
}

/// Read-side tables: index to the single `Arc` every reference shares.
///
/// `Clone` is O(1) and shallow — cloning shares the same tables, and so the
/// same `Arc`s, so a nested reader resolves an index to the very allocation
/// its parent would. The tables themselves are shared and not just their
/// entries because a document is read chunk by chunk against one set of them,
/// and copying a string table of one entry per data row per chunk is work for
/// nothing.
#[cfg(feature = "document-read")]
#[derive(Debug, Default, Clone)]
pub(crate) struct ReadTables {
    geometries: Arc<Vec<Arc<Geometry>>>,
    sheets: Arc<Vec<Arc<RichTextStyleSheet>>>,
    strings: Arc<Vec<Arc<str>>>,
    brushes: Arc<Vec<Arc<Brush>>>,
}

#[cfg(feature = "document-read")]
impl ReadTables {
    /// Install the geometry table read from its chunk.
    pub(crate) fn set_geometries(&mut self, gs: Vec<Arc<Geometry>>) {
        self.geometries = Arc::new(gs);
    }

    /// Install the style-sheet table read from its chunk.
    pub(crate) fn set_sheets(&mut self, ss: Vec<Arc<RichTextStyleSheet>>) {
        self.sheets = Arc::new(ss);
    }

    /// The geometry at `index`, or `None` when the document referenced
    /// past the end of its own table.
    pub(crate) fn geometry(&self, index: u32) -> Option<Arc<Geometry>> {
        self.geometries.get(index as usize).cloned()
    }

    /// The style sheet at `index`, or `None` when the document
    /// referenced past the end of its own table.
    pub(crate) fn sheet(&self, index: u32) -> Option<Arc<RichTextStyleSheet>> {
        self.sheets.get(index as usize).cloned()
    }

    /// Install the string table read from its chunk.
    pub(crate) fn set_strings(&mut self, ss: Vec<Arc<str>>) {
        self.strings = Arc::new(ss);
    }

    /// Install the brush table read from its chunk.
    pub(crate) fn set_brushes(&mut self, bs: Vec<Arc<Brush>>) {
        self.brushes = Arc::new(bs);
    }

    /// The brush at `index`, or `None` when the document referenced
    /// past the end of its own table.
    pub(crate) fn brush(&self, index: u32) -> Option<Arc<Brush>> {
        self.brushes.get(index as usize).cloned()
    }

    /// The string at `index`, or `None` when the document referenced
    /// past the end of its own table.
    pub(crate) fn string(&self, index: u32) -> Option<Arc<str>> {
        self.strings.get(index as usize).cloned()
    }
}
