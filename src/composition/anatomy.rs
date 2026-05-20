//! Anatomical layout of a single patch — the 11×14 slot grid every patch
//! shares.
//!
//! The grid is symmetric in all four directions out through the legend ring:
//!
//! ```text
//! panel | axis | axis-title | strip | legend
//! ```
//!
//! Beyond the legend, top/bottom carry title/subtitle/caption; the left/right
//! sides carry nothing additional. Margin is the outermost track on every
//! side.
//!
//! All slot positions are 1-indexed to match [`crate::layout::Placement`].

/// Per-patch grid dimensions: 11 columns × 14 rows.
pub const TABLE_COLS: usize = 11;
/// Per-patch grid dimensions: 11 columns × 14 rows.
pub const TABLE_ROWS: usize = 14;

/// 1-indexed row of the panel.
pub const PANEL_ROW: u16 = 8;
/// 1-indexed column of the panel.
pub const PANEL_COL: u16 = 6;

/// Leftmost column of the symmetric ring (LegendLeft).
pub const PLOT_LEFT: u16 = 2;
/// Rightmost column of the symmetric ring (LegendRight).
pub const PLOT_RIGHT: u16 = 10;
/// Topmost row of the symmetric ring (LegendTop).
pub const PLOT_TOP: u16 = 4;
/// Bottommost row of the symmetric ring (LegendBottom).
pub const PLOT_BOTTOM: u16 = 12;

const PLOT_COL_SPAN: u16 = PLOT_RIGHT - PLOT_LEFT + 1;
const PLOT_ROW_SPAN: u16 = PLOT_BOTTOM - PLOT_TOP + 1;

/// Anatomical slots in a patch.
///
/// Every slot has a fixed `(row, col, row_span, col_span)` position; see
/// [`Slot::placement`]. The mapping is total — to put content outside this
/// fixed anatomy use [`crate::composition::Patch::place_at`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The panel area itself — single Fr(1)×Fr(1) cell.
    Panel,
    /// Spans the entire 11×14 grid; painted first.
    Background,

    // ── Top symmetric ring (panel-outward: axis → axis-title → strip → legend)
    AxisTop,
    AxisTopTitle,
    StripTop,
    LegendTop,

    // ── Bottom symmetric ring
    AxisBottom,
    AxisBottomTitle,
    StripBottom,
    LegendBottom,

    // ── Left symmetric ring
    AxisLeft,
    AxisLeftTitle,
    StripLeft,
    LegendLeft,

    // ── Right symmetric ring
    AxisRight,
    AxisRightTitle,
    StripRight,
    LegendRight,

    // ── Outside the legend ring, top/bottom only
    /// Outermost top text track (above subtitle).
    Title,
    /// Between title and LegendTop.
    Subtitle,
    /// Outermost bottom text track.
    Caption,
}

impl Slot {
    /// Stable snake_case identifier used as the region name in lookups via
    /// [`crate::composition::CompositionLayout::get`].
    pub const fn name(self) -> &'static str {
        match self {
            Slot::Panel => "panel",
            Slot::Background => "background",

            Slot::AxisTop => "axis_top",
            Slot::AxisTopTitle => "axis_top_title",
            Slot::StripTop => "strip_top",
            Slot::LegendTop => "legend_top",

            Slot::AxisBottom => "axis_bottom",
            Slot::AxisBottomTitle => "axis_bottom_title",
            Slot::StripBottom => "strip_bottom",
            Slot::LegendBottom => "legend_bottom",

            Slot::AxisLeft => "axis_left",
            Slot::AxisLeftTitle => "axis_left_title",
            Slot::StripLeft => "strip_left",
            Slot::LegendLeft => "legend_left",

            Slot::AxisRight => "axis_right",
            Slot::AxisRightTitle => "axis_right_title",
            Slot::StripRight => "strip_right",
            Slot::LegendRight => "legend_right",

            Slot::Title => "title",
            Slot::Subtitle => "subtitle",
            Slot::Caption => "caption",
        }
    }

    /// (row, col, row_span, col_span), 1-indexed within the per-patch
    /// 11×14 anatomy.
    pub const fn placement(self) -> (u16, u16, u16, u16) {
        match self {
            Slot::Panel => (PANEL_ROW, PANEL_COL, 1, 1),
            Slot::Background => (1, 1, TABLE_ROWS as u16, TABLE_COLS as u16),

            // Top symmetric ring — single cell at the panel column.
            Slot::AxisTop => (7, PANEL_COL, 1, 1),
            Slot::AxisTopTitle => (6, PANEL_COL, 1, 1),
            Slot::StripTop => (5, PANEL_COL, 1, 1),
            // Legends span horizontally across the symmetric ring.
            Slot::LegendTop => (PLOT_TOP, PLOT_LEFT, 1, PLOT_COL_SPAN),

            Slot::AxisBottom => (9, PANEL_COL, 1, 1),
            Slot::AxisBottomTitle => (10, PANEL_COL, 1, 1),
            Slot::StripBottom => (11, PANEL_COL, 1, 1),
            Slot::LegendBottom => (PLOT_BOTTOM, PLOT_LEFT, 1, PLOT_COL_SPAN),

            // Left symmetric ring — single cell at the panel row.
            Slot::AxisLeft => (PANEL_ROW, 5, 1, 1),
            Slot::AxisLeftTitle => (PANEL_ROW, 4, 1, 1),
            Slot::StripLeft => (PANEL_ROW, 3, 1, 1),
            // Side legends span vertically across the symmetric ring.
            Slot::LegendLeft => (PLOT_TOP, PLOT_LEFT, PLOT_ROW_SPAN, 1),

            Slot::AxisRight => (PANEL_ROW, 7, 1, 1),
            Slot::AxisRightTitle => (PANEL_ROW, 8, 1, 1),
            Slot::StripRight => (PANEL_ROW, 9, 1, 1),
            Slot::LegendRight => (PLOT_TOP, PLOT_RIGHT, PLOT_ROW_SPAN, 1),

            // Asymmetric text tracks outside the legend ring.
            Slot::Title => (2, PLOT_LEFT, 1, PLOT_COL_SPAN),
            Slot::Subtitle => (3, PLOT_LEFT, 1, PLOT_COL_SPAN),
            Slot::Caption => (13, PLOT_LEFT, 1, PLOT_COL_SPAN),
        }
    }

    /// True if this slot's anatomical column falls in the left chrome (cols
    /// strictly less than `PANEL_COL`).
    #[allow(dead_code)] // used by the flattener once it lands
    pub(crate) const fn is_left_chrome(self) -> bool {
        self.placement().1 < PANEL_COL
    }
    /// True if this slot's anatomical column falls in the right chrome.
    #[allow(dead_code)]
    pub(crate) const fn is_right_chrome(self) -> bool {
        let (_, c, _, cs) = self.placement();
        c + cs - 1 > PANEL_COL && c > PANEL_COL
    }
    /// True if this slot's anatomical row falls in the top chrome.
    #[allow(dead_code)]
    pub(crate) const fn is_top_chrome(self) -> bool {
        let (r, _, rs, _) = self.placement();
        r + rs - 1 < PANEL_ROW
    }
    /// True if this slot's anatomical row falls in the bottom chrome.
    #[allow(dead_code)]
    pub(crate) const fn is_bottom_chrome(self) -> bool {
        let (r, _, _, _) = self.placement();
        r > PANEL_ROW
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let all = [
            Slot::Panel,
            Slot::Background,
            Slot::AxisTop,
            Slot::AxisTopTitle,
            Slot::StripTop,
            Slot::LegendTop,
            Slot::AxisBottom,
            Slot::AxisBottomTitle,
            Slot::StripBottom,
            Slot::LegendBottom,
            Slot::AxisLeft,
            Slot::AxisLeftTitle,
            Slot::StripLeft,
            Slot::LegendLeft,
            Slot::AxisRight,
            Slot::AxisRightTitle,
            Slot::StripRight,
            Slot::LegendRight,
            Slot::Title,
            Slot::Subtitle,
            Slot::Caption,
        ];
        let mut names: Vec<&str> = all.iter().map(|s| s.name()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "slot names collide");
    }

    #[test]
    fn placements_stay_within_grid() {
        for s in [
            Slot::Panel,
            Slot::Background,
            Slot::AxisTop,
            Slot::AxisTopTitle,
            Slot::StripTop,
            Slot::LegendTop,
            Slot::AxisBottom,
            Slot::AxisBottomTitle,
            Slot::StripBottom,
            Slot::LegendBottom,
            Slot::AxisLeft,
            Slot::AxisLeftTitle,
            Slot::StripLeft,
            Slot::LegendLeft,
            Slot::AxisRight,
            Slot::AxisRightTitle,
            Slot::StripRight,
            Slot::LegendRight,
            Slot::Title,
            Slot::Subtitle,
            Slot::Caption,
        ] {
            let (r, c, rs, cs) = s.placement();
            assert!(
                r >= 1 && c >= 1,
                "{} starts at ({r}, {c}) — must be 1-indexed",
                s.name()
            );
            assert!(
                r + rs - 1 <= TABLE_ROWS as u16,
                "{} extends past TABLE_ROWS",
                s.name()
            );
            assert!(
                c + cs - 1 <= TABLE_COLS as u16,
                "{} extends past TABLE_COLS",
                s.name()
            );
        }
    }

    #[test]
    fn chrome_side_classification() {
        // Top chrome.
        assert!(Slot::Title.is_top_chrome());
        assert!(Slot::AxisTop.is_top_chrome());
        assert!(Slot::LegendTop.is_top_chrome());
        assert!(!Slot::Panel.is_top_chrome());
        assert!(!Slot::AxisBottom.is_top_chrome());

        // Bottom chrome.
        assert!(Slot::Caption.is_bottom_chrome());
        assert!(Slot::AxisBottom.is_bottom_chrome());
        assert!(Slot::LegendBottom.is_bottom_chrome());
        assert!(!Slot::Panel.is_bottom_chrome());

        // Left/right chrome.
        assert!(Slot::AxisLeft.is_left_chrome());
        assert!(Slot::AxisLeftTitle.is_left_chrome());
        assert!(!Slot::Panel.is_left_chrome());
        assert!(Slot::AxisRight.is_right_chrome());
        assert!(!Slot::Panel.is_right_chrome());
    }
}
