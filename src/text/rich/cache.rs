//! Memoization of shaped rich text across frames.
//!
//! Parsing, reducing and shaping a markdown source is the expensive
//! part of the rich-text pipeline, and an interactive plot redraws the
//! same labels every frame. [`RichShapeCache`] keeps the shaped
//! [`RichTextRun`]s alive between draws, keyed on everything that
//! could change what they look like.
//!
//! **Width is part of the key**, but re-breaking at a width the run
//! already holds is free (`set_max_width` short-circuits), so a hit
//! that re-wraps costs a line-break pass at worst.
//!
//! **Style sheets are keyed by `Arc` identity.** A sheet is documented
//! as immutable once installed; building a new one is what invalidates
//! the entries that shaped against the old one.
//!
//! Entries are `Rc`, not `Arc`: a `RichTextRun` holds `RefCell`s and
//! is single-threaded by construction, and rendering happens on one
//! thread. A cache therefore isn't `Send`, which is why it lives
//! behind a `RefCell` on the geom / plot that owns it rather than in
//! any shared state.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use super::run::{RichTextRun, RichTextWidth};
use super::style::RichTextStyleSheet;
use crate::color::Color;
use crate::style_vocab::{HAlign, Palette};
use crate::text::{FontFamilyEntry, TextStyle};

/// How many shaped runs one cache holds before the least recently used
/// entries are dropped. A dense text geom draws a few hundred labels;
/// beyond that the working set isn't reused frame to frame anyway.
const CAPACITY: usize = 256;

/// Everything that decides what a shaped run looks like.
///
/// Compared in full on a hash hit, so a collision can't hand back the
/// wrong run.
#[derive(Debug, Clone, PartialEq)]
pub struct RichKey {
    source: String,
    style: TextStyle,
    brush: Color,
    /// Pointer identity of the sheet the run resolved its selectors
    /// through.
    sheet: usize,
    palette: Palette,
    dpi: u64,
    /// Quantized wrap width in pixels; `None` = natural.
    width: Option<i32>,
    alignment: HAlign,
    /// Address of the register the run resolved its image tags
    /// against. Two plots on one thread hold different registers, and
    /// the same markdown resolved through each can name different
    /// pixels, so the register is part of what the run *is*.
    images: usize,
}

impl RichKey {
    /// Build a key for the run `source` would shape into.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: &str,
        style: &TextStyle,
        brush: Color,
        sheet: &Arc<RichTextStyleSheet>,
        palette: &Palette,
        dpi: f64,
        width: RichTextWidth,
        alignment: HAlign,
        images: &crate::image_registry::ImageRegistry,
    ) -> Self {
        Self {
            source: source.to_string(),
            style: style.clone(),
            brush,
            sheet: Arc::as_ptr(sheet) as usize,
            palette: *palette,
            dpi: dpi.to_bits(),
            // Quantized to whole pixels: a solver that converges on
            // 180.0001 and one that converges on 180.0 want the same
            // shaped run.
            width: match width {
                RichTextWidth::Natural => None,
                RichTextWidth::Fixed(px) => Some(px.round() as i32),
            },
            alignment,
            images: std::ptr::from_ref(images) as usize,
        }
    }

    fn hash_value(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.source.hash(&mut h);
        hash_style(&self.style, &mut h);
        for c in self.brush.components {
            c.to_bits().hash(&mut h);
        }
        self.sheet.hash(&mut h);
        for anchor in [self.palette.paper, self.palette.ink, self.palette.accent] {
            for c in anchor.components {
                c.to_bits().hash(&mut h);
            }
        }
        self.dpi.hash(&mut h);
        self.width.hash(&mut h);
        self.alignment.hash(&mut h);
        self.images.hash(&mut h);
        h.finish()
    }
}

/// Hash the style fields that reach the shaper. `TextStyle` carries
/// floats, so it can't derive `Hash`.
fn hash_style(style: &TextStyle, h: &mut DefaultHasher) {
    style.size_pt.to_bits().hash(h);
    style.weight.hash(h);
    style.width.to_bits().hash(h);
    format!("{:?}", style.style).hash(h);
    format!("{:?}", style.line_height).hash(h);
    style.tracking.to_bits().hash(h);
    style.underline.hash(h);
    style.strikethrough.hash(h);
    for f in &style.families {
        match f {
            FontFamilyEntry::Named(n) => n.hash(h),
            FontFamilyEntry::Generic(k) => format!("{k:?}").hash(h),
        }
    }
    for f in &style.features {
        f.tag.hash(h);
        f.value.hash(h);
    }
    for v in &style.variations {
        v.tag.hash(h);
        v.value.to_bits().hash(h);
    }
}

struct Entry {
    key: RichKey,
    run: Rc<RichTextRun>,
    /// Value of the cache's access clock when the entry was last
    /// handed out. Strictly increasing, so it orders entries even
    /// within one frame.
    last_used: u64,
}

/// Shaped-run cache owned by whoever draws rich text repeatedly.
#[derive(Default)]
pub struct RichShapeCache {
    entries: RefCell<HashMap<u64, Vec<Entry>>>,
    /// Monotonic access counter driving the eviction order.
    clock: RefCell<u64>,
    len: RefCell<usize>,
}

impl RichShapeCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every entry. Call when something the key doesn't cover
    /// changes — in practice, when the owner's data is replaced.
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
        *self.len.borrow_mut() = 0;
    }

    /// Number of shaped runs currently held.
    pub fn len(&self) -> usize {
        *self.len.borrow()
    }

    /// True when nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The shaped run for `key`, shaping it through `build` on a miss.
    pub fn get_or_shape(
        &self,
        key: RichKey,
        build: impl FnOnce() -> RichTextRun,
    ) -> Rc<RichTextRun> {
        let hash = key.hash_value();
        let stamp = {
            let mut clock = self.clock.borrow_mut();
            *clock += 1;
            *clock
        };
        {
            let mut entries = self.entries.borrow_mut();
            if let Some(bucket) = entries.get_mut(&hash) {
                if let Some(e) = bucket.iter_mut().find(|e| e.key == key) {
                    e.last_used = stamp;
                    return Rc::clone(&e.run);
                }
            }
        }
        let run = Rc::new(build());
        {
            let mut entries = self.entries.borrow_mut();
            entries.entry(hash).or_default().push(Entry {
                key,
                run: Rc::clone(&run),
                last_used: stamp,
            });
            *self.len.borrow_mut() += 1;
        }
        self.evict_if_full();
        run
    }

    /// Drop the least recently used entries once the cache is over
    /// capacity, down to three quarters so eviction isn't per-insert.
    fn evict_if_full(&self) {
        if *self.len.borrow() <= CAPACITY {
            return;
        }
        let target = CAPACITY * 3 / 4;
        let mut entries = self.entries.borrow_mut();
        let mut ages: Vec<u64> = entries
            .values()
            .flat_map(|b| b.iter().map(|e| e.last_used))
            .collect();
        ages.sort_unstable();
        let cutoff = ages[ages.len().saturating_sub(target)];
        let mut kept = 0usize;
        entries.retain(|_, bucket| {
            bucket.retain(|e| e.last_used >= cutoff);
            kept += bucket.len();
            !bucket.is_empty()
        });
        *self.len.borrow_mut() = kept;
    }
}

impl std::fmt::Debug for RichShapeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RichShapeCache")
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(source: &str, sheet: &Arc<RichTextStyleSheet>, palette: &Palette) -> RichKey {
        RichKey::new(
            source,
            &TextStyle::new(12.0),
            Color::from_rgba8(0, 0, 0, 255),
            sheet,
            palette,
            96.0,
            RichTextWidth::Natural,
            HAlign::Start,
            crate::image_registry::ImageRegistry::shared_empty(),
        )
    }

    fn shape(source: &str, sheet: &Arc<RichTextStyleSheet>, palette: &Palette) -> RichTextRun {
        RichTextRun::new(
            source,
            &TextStyle::new(12.0),
            Color::from_rgba8(0, 0, 0, 255),
            sheet,
            palette,
            96.0,
        )
    }

    #[test]
    fn the_same_key_hands_back_the_same_run() {
        let cache = RichShapeCache::new();
        let sheet = Arc::new(RichTextStyleSheet::new());
        let palette = Palette::default();
        let a = cache.get_or_shape(key("**hi**", &sheet, &palette), || {
            shape("**hi**", &sheet, &palette)
        });
        let b = cache.get_or_shape(key("**hi**", &sheet, &palette), || {
            panic!("second lookup must hit the cache")
        });
        assert!(Rc::ptr_eq(&a, &b));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn a_different_palette_is_a_different_entry() {
        let cache = RichShapeCache::new();
        let sheet = Arc::new(RichTextStyleSheet::new());
        let light = Palette::default();
        let dark = Palette::new(
            Color::from_rgba8(0, 0, 0, 255),
            Color::from_rgba8(255, 255, 255, 255),
            Color::from_rgba8(51, 105, 232, 255),
        );
        cache.get_or_shape(key("hi", &sheet, &light), || shape("hi", &sheet, &light));
        cache.get_or_shape(key("hi", &sheet, &dark), || shape("hi", &sheet, &dark));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_different_sheet_is_a_different_entry() {
        let cache = RichShapeCache::new();
        let palette = Palette::default();
        let a = Arc::new(RichTextStyleSheet::new());
        let b = Arc::new(RichTextStyleSheet::new());
        cache.get_or_shape(key("hi", &a, &palette), || shape("hi", &a, &palette));
        cache.get_or_shape(key("hi", &b, &palette), || shape("hi", &b, &palette));
        assert_eq!(cache.len(), 2, "sheets are keyed by identity");
    }

    #[test]
    fn stale_entries_are_evicted_once_over_capacity() {
        let cache = RichShapeCache::new();
        let sheet = Arc::new(RichTextStyleSheet::new());
        let palette = Palette::default();
        for i in 0..(CAPACITY + 20) {
            let src = format!("label {i}");
            cache.get_or_shape(key(&src, &sheet, &palette), || {
                shape(&src, &sheet, &palette)
            });
        }
        assert!(cache.len() <= CAPACITY, "got {}", cache.len());
        assert!(!cache.is_empty());
    }

    #[test]
    fn clear_empties_the_cache() {
        let cache = RichShapeCache::new();
        let sheet = Arc::new(RichTextStyleSheet::new());
        let palette = Palette::default();
        cache.get_or_shape(key("hi", &sheet, &palette), || {
            shape("hi", &sheet, &palette)
        });
        assert!(!cache.is_empty());
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn recently_used_entries_survive_eviction() {
        let cache = RichShapeCache::new();
        let sheet = Arc::new(RichTextStyleSheet::new());
        let palette = Palette::default();
        let hot = "the label that keeps being drawn";
        let first = cache.get_or_shape(key(hot, &sheet, &palette), || shape(hot, &sheet, &palette));
        for i in 0..(CAPACITY + 20) {
            let src = format!("label {i}");
            cache.get_or_shape(key(&src, &sheet, &palette), || {
                shape(&src, &sheet, &palette)
            });
            // Touch the hot entry every round so it stays the most
            // recently used.
            cache.get_or_shape(key(hot, &sheet, &palette), || shape(hot, &sheet, &palette));
        }
        let again = cache.get_or_shape(key(hot, &sheet, &palette), || {
            panic!("the hot entry must not have been evicted")
        });
        assert!(Rc::ptr_eq(&first, &again));
    }
}
