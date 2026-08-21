//! Hit-testing primitives.
//!
//! Picking is opt-in at scene/renderer construction. When enabled, every
//! drawing call carries a [`PickId`] that tells the backend whether (and with
//! what id) the call should appear in a parallel "hitmap" buffer. After
//! rendering, the hitmap is read back to CPU once and indexed directly to
//! answer "which item is at pixel (x, y)?" — no per-event GPU round-trip.
//!
//! The id space is 24-bit (1..=0xFF_FFFF, ~16M items), with `0` reserved as the
//! "no hit" sentinel, alongside a fully transparent pixel — nothing was drawn
//! there, so its colour channels are not an id. Callers manage their own id assignment (typically a row
//! index or item index). The encoding packs the id into the RGB channels of an
//! `Rgba8Unorm` texture with alpha forced to 255, which round-trips cleanly
//! through default SrcOver compositing without any per-draw blend-mode plumbing.
//!
//! # Limitation: blended ids where picked content meets picked content
//!
//! This one depends on the backend. A rasteriser that cannot disable
//! antialiasing produces a pick pass whose edge pixels are a coverage blend
//! of what is above and below them. Over empty space that is harmless: the
//! rasteriser unpremultiplies, so the fringe divides back out to the mark's
//! exact id. Where a mark's edge falls on **other picked content**, the blend
//! mixes two ids and the result is a third, entirely plausible id at full
//! alpha, which [`decode`] cannot tell from a real hit.
//!
//! Two arrangements trigger it: overlapping marks (a boundary between ids
//! 100 and 200 reports values across that range) and a mark drawn over a
//! [`PickId::Block`] fill (the fringe ramps from 0 up to the mark's id,
//! producing low ids that alias onto low-numbered rows).
//!
//! Keeping decorative chrome on [`PickId::Skip`] — the default, and what the
//! plot layer does for panel backgrounds and gridlines — keeps marks
//! compositing over nothing and avoids the conflation entirely. The affected
//! band is one pixel wide at each boundary.
//!
//! A backend that computes coverage on the CPU can paint the pick pass with
//! binary coverage instead, which rules the whole failure out: a pixel is
//! covered by exactly one primitive, so every id read back is an id that was
//! drawn. The `backend::hybrid` backend does this.
//!
//! # Limitation: alpha-insensitive picking
//!
//! Picking ignores display alpha. A semi-transparent layer or image fully
//! occludes picks of content beneath it, even though the same content remains
//! visible in the rasterised image. This keeps the encoded id intact under
//! SrcOver and avoids decoding ambiguity, at the cost of a known mismatch
//! between visual appearance and hit behaviour for translucent overlays.

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
pub fn id_to_color(id: u32) -> Color {
    let r = (id & 0xFF) as f32 / 255.0;
    let g = ((id >> 8) & 0xFF) as f32 / 255.0;
    let b = ((id >> 16) & 0xFF) as f32 / 255.0;
    Color::new([r, g, b, 1.0])
}

/// Decode a u32 pixel sampled from the hitmap into the originating id, or
/// `None` for a miss.
///
/// A pixel misses when nothing was drawn over it (alpha `0`) or when what was
/// drawn carries the no-hit sentinel (`id == 0`). The alpha test is what makes
/// the RGB payload meaningful: every recorded id composites at alpha `255`
/// (see [`id_to_color`]), so an alpha of `0` means the RGB channels hold
/// whatever the rasteriser left behind rather than an id, and reading them as
/// one reports hits on empty space.
///
/// Public because a caller doing bulk queries over
/// [`VelloRenderer::hitmap`](crate::backend::vello::VelloRenderer::hitmap)
/// reads raw pixels and needs this to interpret them.
pub fn decode(px: u32) -> Option<u32> {
    if px >> 24 == 0 {
        return None;
    }
    let id = px & 0x00FF_FFFF;
    (id != 0).then_some(id)
}

/// Resolve a [`PickId`] to the raw id that should land in the hitmap, or
/// `None` if the call should not be recorded at all.
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

    /// The u32 a little-endian `Rgba8Unorm` readback yields for an
    /// encoded pick colour: red in the low byte, alpha in the high one.
    fn readback_u32(c: Color) -> u32 {
        let px = c.to_rgba8();
        ((px.a as u32) << 24) | ((px.b as u32) << 16) | ((px.g as u32) << 8) | px.r as u32
    }

    /// Ids that bracket each byte boundary of the 24-bit space.
    const BOUNDARIES: [u32; 10] = [
        1, 0x7F, 0xFF, 0x100, 0x101, 0xFFFF, 0x1_0000, 0x1_0001, 0xFE_FFFF, 0xFF_FFFF,
    ];

    #[test]
    fn boundary_ids_round_trip_through_the_encoded_pixel() {
        for id in BOUNDARIES {
            let got = decode(readback_u32(id_to_color(id)));
            assert_eq!(got, Some(id), "id {id:#x} did not round-trip");
        }
    }

    #[test]
    fn every_sampled_id_across_the_24_bit_space_round_trips() {
        // Prime stride so the sweep hits every byte value in each of the
        // three channels rather than aliasing to a fixed pattern.
        let mut id = 1u32;
        while id <= 0xFF_FFFF {
            assert_eq!(decode(readback_u32(id_to_color(id))), Some(id));
            id += 7919;
        }
    }

    #[test]
    fn encoded_pixel_is_opaque_with_the_id_in_the_low_24_bits() {
        for id in BOUNDARIES {
            assert_eq!(
                readback_u32(id_to_color(id)),
                (0xFF << 24) | (id & 0x00FF_FFFF),
                "id {id:#x} encoded to an unexpected pixel"
            );
        }
    }

    #[test]
    fn id_zero_encodes_the_same_pixel_as_block() {
        let block = id_to_color(raw_id(PickId::Block).unwrap());
        let id_zero = id_to_color(raw_id(PickId::Id(0)).unwrap());
        assert_eq!(raw_id(PickId::Id(0)), raw_id(PickId::Block));
        assert_eq!(readback_u32(id_zero), readback_u32(block));
        // Opaque black — the no-hit sentinel, which still occludes.
        assert_eq!(readback_u32(block), 0xFF00_0000);
        assert_eq!(decode(readback_u32(block)), None);
    }

    #[test]
    fn ids_past_24_bits_lose_their_high_byte() {
        assert_eq!(
            readback_u32(id_to_color(0x0100_0001)),
            readback_u32(id_to_color(1))
        );
        assert_eq!(decode(readback_u32(id_to_color(0x0100_0001))), Some(1));
        assert_eq!(decode(readback_u32(id_to_color(u32::MAX))), Some(0xFF_FFFF));
        // A caller id whose payload is entirely in the high byte collides
        // with the no-hit sentinel and becomes unhittable.
        assert_eq!(decode(readback_u32(id_to_color(0x0100_0000))), None);
    }

    #[test]
    fn a_fully_transparent_pixel_misses_whatever_its_rgb_holds() {
        // Nothing composited here, so the RGB channels are residue, not an id.
        assert_eq!(decode(0x0000_1234), None);
        assert_eq!(decode(0x0000_0000), None);
    }

    #[test]
    fn any_coverage_at_all_makes_the_id_payload_count() {
        assert_eq!(decode(0xFF00_1234), Some(0x1234));
        assert_eq!(decode(0xFF00_0000), None);
        // Partial coverage still carries an exact id: the rasteriser
        // unpremultiplies, so a fringe pixel reports the mark it belongs to.
        assert_eq!(decode(0x0100_1234), Some(0x1234));
    }

    #[test]
    fn raw_id_omits_skip_and_maps_block_to_zero() {
        assert_eq!(raw_id(PickId::Skip), None);
        assert_eq!(raw_id(PickId::Block), Some(0));
        assert_eq!(raw_id(PickId::Id(7)), Some(7));
    }

    #[test]
    fn default_pick_id_stays_out_of_the_hitmap() {
        assert_eq!(PickId::default(), PickId::Skip);
        assert_eq!(raw_id(PickId::default()), None);
    }
}
