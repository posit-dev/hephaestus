//! Chrome-placement enums shared by axis and legend rendering.
//!
//! Kept feature-flag-agnostic so callers can match on `AxisSide` /
//! `LegendSide` without pulling in the text feature.

/// Where an axis is drawn relative to the panel rect.
///
/// `Left` / `Right` axes are vertical (tick labels horizontal, anchored
/// to one side of the panel). `Bottom` / `Top` axes are horizontal (tick
/// labels centred under/over their tick).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AxisSide {
    Left,
    Right,
    Bottom,
    Top,
}

impl AxisSide {
    /// `true` for [`Self::Left`] / [`Self::Right`]; the axis runs vertically
    /// and consumes a column of the patch.
    pub fn is_vertical(self) -> bool {
        matches!(self, AxisSide::Left | AxisSide::Right)
    }

    /// `true` for [`Self::Bottom`] / [`Self::Top`]; the axis runs horizontally
    /// and consumes a row of the patch.
    pub fn is_horizontal(self) -> bool {
        !self.is_vertical()
    }
}

/// Where a legend is drawn relative to the panel rect.
///
/// The four cardinal variants route the legend to the corresponding
/// anatomical chrome slot (one of `LegendLeft` / `LegendRight` /
/// `LegendTop` / `LegendBottom`), shrinking the panel to make room.
/// [`Self::InPanel`] places the legend *inside* the panel rect against
/// the chosen [`Anchor`] without consuming any chrome space — the
/// legend overlays data marks. `inset_pt` is the gap from the panel
/// boundary on both axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LegendSide {
    Left,
    Right,
    Top,
    Bottom,
    InPanel { anchor: Anchor, inset_pt: f64 },
}

impl Eq for LegendSide {}

impl std::hash::Hash for LegendSide {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        if let LegendSide::InPanel { anchor, inset_pt } = self {
            anchor.hash(state);
            inset_pt.to_bits().hash(state);
        }
    }
}

/// Nine reference points used by [`LegendSide::InPanel`] placement.
/// The named corner of the legend's bbox pins to the matching corner /
/// edge midpoint / centre of the panel rect, offset by the side's
/// `inset_pt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchor {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_side_orientation() {
        assert!(AxisSide::Left.is_vertical());
        assert!(AxisSide::Right.is_vertical());
        assert!(AxisSide::Bottom.is_horizontal());
        assert!(AxisSide::Top.is_horizontal());
        assert!(!AxisSide::Left.is_horizontal());
        assert!(!AxisSide::Bottom.is_vertical());
    }
}
