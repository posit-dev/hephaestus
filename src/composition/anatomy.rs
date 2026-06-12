//! Anatomical layout of a single patch — the 13×16 slot grid every patch
//! shares.
//!
//! The grid is symmetric in all four directions through the legend ring:
//!
//! ```text
//! margin | padding | legend | strip | axis-title | axis | panel | axis | axis-title | strip | legend | padding | margin
//! ```
//!
//! Beyond the legend, top/bottom carry title/subtitle/caption; the left/right
//! sides carry nothing additional.
//!
//! - **Outermost tracks** (row 1, row [`TABLE_ROWS`], col 1, col [`TABLE_COLS`]):
//!   *margin*. Sized by [`crate::composition::Patch::margin`]. The
//!   [`Slot::Background`] does **not** extend into these tracks — they are
//!   the gap between adjacent patches' backgrounds when patches are composed
//!   side-by-side.
//! - **Second-from-outermost tracks** (row 2, row `TABLE_ROWS - 1`, col 2,
//!   col `TABLE_COLS - 1`): *padding*. Sized by
//!   [`crate::composition::Patch::padding`]. Sits inside the background;
//!   chrome (title, legends, axes) sits inside the padding.
//!
//! All slot positions are 1-indexed to match [`crate::layout::Placement`].

/// Per-patch grid dimensions: 13 columns × 16 rows.
pub const TABLE_COLS: usize = 13;
/// Per-patch grid dimensions: 13 columns × 16 rows.
pub const TABLE_ROWS: usize = 16;

/// 1-indexed row of the panel.
pub const PANEL_ROW: u16 = 9;
/// 1-indexed column of the panel.
pub const PANEL_COL: u16 = 7;

/// Leftmost column of the symmetric ring (LegendLeft).
pub const PLOT_LEFT: u16 = 3;
/// Rightmost column of the symmetric ring (LegendRight).
pub const PLOT_RIGHT: u16 = 11;
/// Topmost row of the symmetric ring (LegendTop).
pub const PLOT_TOP: u16 = 5;
/// Bottommost row of the symmetric ring (LegendBottom).
pub const PLOT_BOTTOM: u16 = 13;

const PLOT_COL_SPAN: u16 = PLOT_RIGHT - PLOT_LEFT + 1;

/// 1-indexed row of the top margin track.
pub const MARGIN_TOP_ROW: u16 = 1;
/// 1-indexed row of the bottom margin track.
pub const MARGIN_BOTTOM_ROW: u16 = TABLE_ROWS as u16;
/// 1-indexed col of the left margin track.
pub const MARGIN_LEFT_COL: u16 = 1;
/// 1-indexed col of the right margin track.
pub const MARGIN_RIGHT_COL: u16 = TABLE_COLS as u16;

/// 1-indexed row of the top padding track.
pub const PADDING_TOP_ROW: u16 = 2;
/// 1-indexed row of the bottom padding track.
pub const PADDING_BOTTOM_ROW: u16 = TABLE_ROWS as u16 - 1;
/// 1-indexed col of the left padding track.
pub const PADDING_LEFT_COL: u16 = 2;
/// 1-indexed col of the right padding track.
pub const PADDING_RIGHT_COL: u16 = TABLE_COLS as u16 - 1;

/// Anatomical slots in a patch.
///
/// Every slot has a fixed `(row, col, row_span, col_span)` position; see
/// [`Slot::placement`]. The mapping is total — to put content outside this
/// fixed anatomy use [`crate::composition::Patch::place_at`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The panel area itself — single Fr(1)×Fr(1) cell.
    Panel,
    /// Spans the padding + chrome area (rows 2–15, cols 2–12). Does **not**
    /// include the outermost margin tracks, so two adjacent patches'
    /// backgrounds are separated by `margin_a + margin_b` of empty
    /// composition space. Painted first.
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
    /// 13×16 anatomy.
    pub const fn placement(self) -> (u16, u16, u16, u16) {
        match self {
            Slot::Panel => (PANEL_ROW, PANEL_COL, 1, 1),
            // Background spans the padding + chrome area but **not** the
            // outermost margin tracks. The 14×11 span = 16 total rows minus
            // the 2 margin rows, 13 total cols minus the 2 margin cols.
            Slot::Background => (
                PADDING_TOP_ROW,
                PADDING_LEFT_COL,
                TABLE_ROWS as u16 - 2,
                TABLE_COLS as u16 - 2,
            ),

            // Top symmetric ring — single cell at the panel column.
            Slot::AxisTop => (8, PANEL_COL, 1, 1),
            Slot::AxisTopTitle => (7, PANEL_COL, 1, 1),
            Slot::StripTop => (6, PANEL_COL, 1, 1),
            // Legends sit at the panel column so they align with the
            // panel area, not the full plot row span. Stacking
            // multiple legends along the same side is the renderer's
            // responsibility (see `chrome::legend`).
            Slot::LegendTop => (PLOT_TOP, PANEL_COL, 1, 1),

            Slot::AxisBottom => (10, PANEL_COL, 1, 1),
            Slot::AxisBottomTitle => (11, PANEL_COL, 1, 1),
            Slot::StripBottom => (12, PANEL_COL, 1, 1),
            Slot::LegendBottom => (PLOT_BOTTOM, PANEL_COL, 1, 1),

            // Left symmetric ring — single cell at the panel row.
            Slot::AxisLeft => (PANEL_ROW, 6, 1, 1),
            Slot::AxisLeftTitle => (PANEL_ROW, 5, 1, 1),
            Slot::StripLeft => (PANEL_ROW, 4, 1, 1),
            // Side legends sit at the panel row so they align with
            // the panel area, not the full plot column span.
            Slot::LegendLeft => (PANEL_ROW, PLOT_LEFT, 1, 1),

            Slot::AxisRight => (PANEL_ROW, 8, 1, 1),
            Slot::AxisRightTitle => (PANEL_ROW, 9, 1, 1),
            Slot::StripRight => (PANEL_ROW, 10, 1, 1),
            Slot::LegendRight => (PANEL_ROW, PLOT_RIGHT, 1, 1),

            // Asymmetric text tracks outside the legend ring.
            Slot::Title => (3, PLOT_LEFT, 1, PLOT_COL_SPAN),
            Slot::Subtitle => (4, PLOT_LEFT, 1, PLOT_COL_SPAN),
            Slot::Caption => (14, PLOT_LEFT, 1, PLOT_COL_SPAN),
        }
    }

    /// True if this slot's anatomical column falls in the left chrome (cols
    /// strictly less than `PANEL_COL` and outside the margin/padding tracks).
    #[allow(dead_code)] // used by the flattener once it lands
    pub(crate) const fn is_left_chrome(self) -> bool {
        let c = self.placement().1;
        c < PANEL_COL && c > PADDING_LEFT_COL
    }
    /// True if this slot's anatomical column falls in the right chrome.
    #[allow(dead_code)]
    pub(crate) const fn is_right_chrome(self) -> bool {
        let (_, c, _, cs) = self.placement();
        let end = c + cs - 1;
        end > PANEL_COL && c > PANEL_COL && end < PADDING_RIGHT_COL
    }
    /// True if this slot's anatomical row falls in the top chrome.
    #[allow(dead_code)]
    pub(crate) const fn is_top_chrome(self) -> bool {
        let (r, _, rs, _) = self.placement();
        let end = r + rs - 1;
        end < PANEL_ROW && r > PADDING_TOP_ROW
    }
    /// True if this slot's anatomical row falls in the bottom chrome.
    #[allow(dead_code)]
    pub(crate) const fn is_bottom_chrome(self) -> bool {
        let (r, _, rs, _) = self.placement();
        let end = r + rs - 1;
        r > PANEL_ROW && end < PADDING_BOTTOM_ROW
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_SLOTS: &[Slot] = &[
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

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = ALL_SLOTS.iter().map(|s| s.name()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "slot names collide");
    }

    #[test]
    fn placements_stay_within_grid() {
        for s in ALL_SLOTS {
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
    fn background_excludes_margin_tracks() {
        // Background's row range is [2, 15] inclusive, col range [2, 12].
        // Margin tracks (rows 1, 16; cols 1, 13) are outside.
        let (r, c, rs, cs) = Slot::Background.placement();
        assert_eq!(r, PADDING_TOP_ROW);
        assert_eq!(c, PADDING_LEFT_COL);
        assert_eq!(r + rs - 1, PADDING_BOTTOM_ROW);
        assert_eq!(c + cs - 1, PADDING_RIGHT_COL);
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
