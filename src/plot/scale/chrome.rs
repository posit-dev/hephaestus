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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LegendSide {
    Left,
    Right,
    Top,
    Bottom,
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
