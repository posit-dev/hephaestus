//! Cascade containers — wrap a single `Element<T>` with optional
//! per-channel and per-(channel, side) overrides.
//!
//! These are used for chrome categories where the whole element
//! overrides as a unit — grid lines (per-channel) and strips
//! (per-channel, per-side). The axis-specific cascade is richer
//! (per-AxisTheme-field) and lives in [`super::axis`].

use super::element::Element;

/// Container for a single element type that varies per projection
/// channel (no per-side distinction). Used for grid lines, which span
/// the panel and aren't anchored to a particular side.
#[derive(Debug, Clone, PartialEq)]
pub struct PerChannel<T> {
    /// Applies to both channels unless overridden by `by_channel[i]`.
    pub all: Element<T>,
    /// Per-channel override. `by_channel[0]` overrides `all` for
    /// channel 0; `by_channel[1]` for channel 1. Channel 0 / 1
    /// meaning is projection-defined (Cartesian = x / y; Polar =
    /// theta / radius).
    pub by_channel: [Element<T>; 2],
}

impl<T> PerChannel<T> {
    /// Construct with `all = Set(value)` and both channel overrides
    /// inheriting.
    pub fn new(value: T) -> Self {
        Self {
            all: Element::Set(value),
            by_channel: [Element::Inherit, Element::Inherit],
        }
    }

    /// Resolve for channel `ch`. Walks `by_channel[ch]` → `all`.
    /// Returns the first non-`Inherit` element. `Blank` short-circuits
    /// to `None`.
    pub fn resolve(&self, ch: u8) -> Option<&T> {
        let i = ch as usize;
        debug_assert!(i < 2, "channel index out of range: {ch}");
        let parent = self.all.as_set();
        self.by_channel.get(i).and_then(|e| e.cascade(parent))
    }
}

impl<T: Default> Default for PerChannel<T> {
    fn default() -> Self {
        Self {
            all: Element::Set(T::default()),
            by_channel: [Element::Inherit, Element::Inherit],
        }
    }
}

/// Container for a single element type that varies per
/// (projection channel, side). Used for strip backgrounds / strip
/// text — strips on different facet sides can be styled
/// independently.
///
/// Side 0 / 1 follows the same convention as the axis container:
/// - Cartesian: ch 0 side 0 = bottom, side 1 = top;
///   ch 1 side 0 = left,   side 1 = right.
/// - Polar:     ch 0 side 0 = outer perimeter, side 1 = inner;
///   ch 1 side 0 = primary spoke,   side 1 = secondary.
#[derive(Debug, Clone, PartialEq)]
pub struct Sided<T> {
    /// Applies to every (channel, side) unless overridden.
    pub all: Element<T>,
    /// Per-channel override. `by_channel[ch]` applies to both sides
    /// of channel `ch` unless `by_channel_side[ch][side]` overrides
    /// further.
    pub by_channel: [Element<T>; 2],
    /// Per-(channel, side) override. Most specific.
    pub by_channel_side: [[Element<T>; 2]; 2],
}

impl<T> Sided<T> {
    /// Construct with `all = Set(value)` and every override
    /// inheriting.
    pub fn new(value: T) -> Self {
        Self {
            all: Element::Set(value),
            by_channel: [Element::Inherit, Element::Inherit],
            by_channel_side: [
                [Element::Inherit, Element::Inherit],
                [Element::Inherit, Element::Inherit],
            ],
        }
    }

    /// Resolve for `(ch, side)`. Walks
    /// `by_channel_side[ch][side]` → `by_channel[ch]` → `all`,
    /// returning the first non-`Inherit` element. `Blank` short-
    /// circuits to `None`.
    pub fn resolve(&self, ch: u8, side: u8) -> Option<&T> {
        let ci = ch as usize;
        let si = side as usize;
        debug_assert!(ci < 2 && si < 2, "channel/side out of range: {ch}, {side}");
        let all = self.all.as_set();
        let by_ch = self.by_channel.get(ci).and_then(|e| e.cascade(all)).or(all);
        self.by_channel_side
            .get(ci)
            .and_then(|row| row.get(si))
            .and_then(|e| e.cascade(by_ch))
    }
}

impl<T: Default> Default for Sided<T> {
    fn default() -> Self {
        Self {
            all: Element::Set(T::default()),
            by_channel: [Element::Inherit, Element::Inherit],
            by_channel_side: [
                [Element::Inherit, Element::Inherit],
                [Element::Inherit, Element::Inherit],
            ],
        }
    }
}
