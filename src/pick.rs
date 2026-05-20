//! Hit-testing primitives.
//!
//! Picking is opt-in at scene/renderer construction. When enabled, every
//! drawing call carries a [`PickId`] that tells the backend whether (and with
//! what id) the call should appear in a parallel "hitmap" buffer. After
//! rendering, the hitmap is read back to CPU once and indexed directly to
//! answer "which item is at pixel (x, y)?" — no per-event GPU round-trip.
//!
//! The id space is 24-bit (1..=0xFF_FFFF, ~16M items), with `0` reserved as the
//! "no hit" sentinel. Callers manage their own id assignment (typically a row
//! index or item index). The encoding packs the id into the RGB channels of an
//! `Rgba8Unorm` texture with alpha forced to 255, which round-trips cleanly
//! through default SrcOver compositing without any per-draw blend-mode plumbing.

#[cfg(feature = "vello")]
use crate::color::Color;

/// Per-draw-call hitmap directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PickId {
    /// Don't record into the hitmap. Items beneath remain hittable through
    /// this primitive. Sensible default for decorative chrome (gridlines,
    /// axis ticks, etc.).
    #[default]
    Skip,
    /// Record with id 0 — occludes whatever is beneath in the hitmap, but is
    /// itself reported as "no hit". Useful for opaque panels/backgrounds
    /// that should block picks without being interactive themselves.
    Block,
    /// Record with the given id. id 0 is reserved internally for "no hit"
    /// and `Id(0)` is treated identically to `Block`. Ids above `0xFF_FFFF`
    /// are truncated to 24 bits — the high byte is discarded.
    Id(u32),
}

/// Encode a 24-bit id into the [`Color`] that will be written to the pick
/// texture. Bytes land in the `Rgba8Unorm` target as
/// `(R = id & 0xFF, G = (id>>8) & 0xFF, B = (id>>16) & 0xFF, A = 255)`, so a
/// `u32` lifted off the little-endian readback buffer equals
/// `(0xFF << 24) | (id & 0x00FF_FFFF)`.
#[cfg(feature = "vello")]
pub(crate) fn id_to_color(id: u32) -> Color {
    let r = (id & 0xFF) as f32 / 255.0;
    let g = ((id >> 8) & 0xFF) as f32 / 255.0;
    let b = ((id >> 16) & 0xFF) as f32 / 255.0;
    Color::new([r, g, b, 1.0])
}

/// Decode a u32 pixel sampled from the hitmap into the originating id, or
/// `None` for the no-hit sentinel. The alpha byte is discarded; only the
/// low 24 bits carry id payload.
#[cfg(feature = "vello")]
pub(crate) fn decode(px: u32) -> Option<u32> {
    let id = px & 0x00FF_FFFF;
    (id != 0).then_some(id)
}

/// Resolve a [`PickId`] to the raw id that should land in the hitmap, or
/// `None` if the call should not be recorded at all.
#[cfg(feature = "vello")]
pub(crate) fn raw_id(pick: PickId) -> Option<u32> {
    match pick {
        PickId::Skip => None,
        PickId::Block => Some(0),
        PickId::Id(n) => Some(n),
    }
}
