//! Hit-testing primitives.
//!
//! Every drawing call on a [`SceneBuilder`](crate::scene::SceneBuilder)
//! carries a [`PickId`], the authoring layer's own handle for whatever the
//! call draws. A scene wrapped in [`PickIndexScene`] records each call's
//! geometry into a [`PickIndex`] as it goes past, and the index answers point,
//! rectangle and lasso queries afterwards — on the CPU, with no second
//! rasterisation and nothing read back from a GPU.
//!
//! Ids are caller-managed and span the full `u32` range; `0` is reserved as
//! the no-hit sentinel, which is what [`PickId::Block`] encodes.
//!
//! # Beyond ids: scopes
//!
//! An id is a leaf. [`PickScope`] records the *tree* a drawing sits in —
//! pushed and popped like a layer, but with no visual effect — so a hit can
//! report the axis, panel and plot it belongs to and not merely a number.
//! That is what makes chrome pickable: chrome has no id of its own, and
//! carving a range out of a namespace the caller owns was never safe.
//! The vocabulary the plot layer pushes lives in [`crate::plot::pick`].
//!
//! # Known limits
//!
//! - **Dashed strokes are hittable along their gaps.** A hit target follows
//!   the path, not the dash pattern.
//! - **Glyph runs pick as layout boxes, not ink.** Leading and side bearings
//!   are hittable, which is what a text target should be; a glyph-backed
//!   marker shape is correspondingly looser than its outline.
//! - **Stroke ends and joins are round** whatever the cap and join say. The
//!   error is sub-pixel to a few pixels, and on the generous side.
//! - **There is no canvas.** A mark drawn partly outside the frame is
//!   hittable at coordinates outside it; the index answers about geometry,
//!   not about a framebuffer.

mod clip;
mod geom;
mod hilbert;
mod index;
mod rtree;
mod scene;
mod scope;

pub use index::{Hit, PickIndex};
pub use scene::PickIndexScene;
pub use scope::{PickPath, PickScope, ScopeMode};

/// The authoring layer's handle for whatever a draw call draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PickId {
    /// Carry no authoring id.
    ///
    /// The primitive is not indexed on its own, and whatever is beneath it
    /// stays hittable through it. The default, and what all decorative chrome
    /// passes — chrome becomes pickable by sitting inside a
    /// [`ScopeMode::Target`] scope, not by taking an id.
    #[default]
    Skip,
    /// Occlude without reporting.
    ///
    /// A point query stops here and reports nothing, so an opaque panel can
    /// hide what is under it without being interactive itself. Region queries
    /// are unaffected: a marquee is a spatial query, not a ray.
    Block,
    /// Carry the given id. `Id(0)` is treated identically to [`Self::Block`],
    /// `0` being the no-hit sentinel; every other `u32` is reported as given.
    Id(u32),
}

/// Resolve a [`PickId`] to the raw id it reports, or `None` if the call
/// should not be indexed at all.
pub fn raw_id(pick: PickId) -> Option<u32> {
    match pick {
        PickId::Skip => None,
        PickId::Block => Some(0),
        PickId::Id(n) => Some(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_id_omits_skip_and_maps_block_to_zero() {
        assert_eq!(raw_id(PickId::Skip), None);
        assert_eq!(raw_id(PickId::Block), Some(0));
        assert_eq!(raw_id(PickId::Id(7)), Some(7));
        // `Id(0)` and `Block` are the same request: occlude, report nothing.
        assert_eq!(raw_id(PickId::Id(0)), raw_id(PickId::Block));
    }

    #[test]
    fn ids_span_the_whole_u32_range() {
        // Nothing packs an id into colour channels any more, so there is no
        // width to truncate to.
        assert_eq!(raw_id(PickId::Id(0x0100_0001)), Some(0x0100_0001));
        assert_eq!(raw_id(PickId::Id(u32::MAX)), Some(u32::MAX));
        assert_eq!(raw_id(PickId::Id(0x0100_0000)), Some(0x0100_0000));
    }

    #[test]
    fn default_pick_id_carries_no_id() {
        assert_eq!(PickId::default(), PickId::Skip);
        assert_eq!(raw_id(PickId::default()), None);
    }
}
