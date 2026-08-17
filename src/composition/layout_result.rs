//! The resolved output of a composition solve: [`CompositionLayout`] and the
//! [`Region`] key it is queried with.

use std::collections::HashMap;

use crate::geometry::Rect;
use crate::layout::{CellId, Layout};

use super::Slot;

/// Resolved layout for a [`Patch`] or [`Composition`]. Query rects by patch id
/// and anatomical region.
///
/// [`Patch`]: super::Patch
/// [`Composition`]: super::Composition
pub struct CompositionLayout {
    pub(super) layout: Layout,
    pub(super) regions: HashMap<String, HashMap<Box<str>, CellId>>,
}

impl CompositionLayout {
    /// Look up the resolved rect for a `(patch_id, region)` pair. The region
    /// can be a typed [`Slot`] or a raw `&str` (e.g. for `place_at` regions).
    pub fn get(&self, patch_id: &str, region: impl Region) -> Option<Rect> {
        let id = self.regions.get(patch_id)?.get(region.name())?;
        self.layout.rect(*id)
    }

    /// Iterate every `(patch_id, region, rect)` triple.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, Rect)> + '_ {
        let layout = &self.layout;
        self.regions.iter().flat_map(move |(id, by_region)| {
            by_region.iter().filter_map(move |(region, cell_id)| {
                layout.rect(*cell_id).map(|r| (id.as_str(), &**region, r))
            })
        })
    }

    /// Access the underlying [`Layout`] (rare — most callers want
    /// [`get`](Self::get)).
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Shift every resolved rect by `(dx, dy)` pixels. Used by
    /// the orchestrator to centre a natural-aspect composition
    /// inside an over-sized canvas.
    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.layout.translate(dx, dy);
    }
}

/// Anything that names a region for [`CompositionLayout::get`] lookups.
pub trait Region {
    /// The region's name as a `&str`. Used as the lookup key.
    fn name(&self) -> &str;
}

impl Region for Slot {
    fn name(&self) -> &str {
        Slot::name(*self)
    }
}

impl Region for &str {
    fn name(&self) -> &str {
        self
    }
}

impl Region for String {
    fn name(&self) -> &str {
        self.as_str()
    }
}

impl Region for &String {
    fn name(&self) -> &str {
        self.as_str()
    }
}
