//! Pre-built themes — `Theme::default()` / `dark()` / `minimal()` /
//! `classic()` / `bw()` / `void()`.
//!
//! Because element colors are palette references, most variants are
//! the same structure with a different palette; only minimal /
//! classic / void flip a few elements to `Blank` to reshape the
//! chrome.

use super::cascade::PerChannel;
use super::element::{Element, LineElement};
use super::palette::{Palette, ThemeColor};
use super::theme::Theme;
use crate::color::{rgb, Color};

impl Theme {
    /// Inverted dark theme. Equivalent to
    /// `Theme::default().invert()` — every chrome element that
    /// references the palette adapts automatically (paper → ink,
    /// ink → paper).
    pub fn dark() -> Self {
        Theme::default().invert()
    }

    /// A theme without panel borders or minor grid lines — just
    /// the major grid on a paper background. Mirrors ggplot2's
    /// `theme_minimal()` in spirit.
    #[allow(clippy::field_reassign_with_default)]
    pub fn minimal() -> Self {
        let mut t = Theme::default();
        t.palette.paper = rgb(1.0, 1.0, 1.0);
        t.panel_border = Element::Blank;
        t.panel_grid_minor = PerChannel {
            all: Element::Blank,
            by_channel: [Element::Inherit, Element::Inherit],
        };
        t
    }

    /// White panel + axis baselines, no grid. Mirrors ggplot2's
    /// `theme_classic()`.
    #[allow(clippy::field_reassign_with_default)]
    pub fn classic() -> Self {
        let mut t = Theme::default();
        t.palette.paper = rgb(1.0, 1.0, 1.0);
        t.panel_border = Element::Blank;
        t.panel_grid_major = PerChannel {
            all: Element::Blank,
            by_channel: [Element::Inherit, Element::Inherit],
        };
        t.panel_grid_minor = PerChannel {
            all: Element::Blank,
            by_channel: [Element::Inherit, Element::Inherit],
        };
        t
    }

    /// Black + white with grid. Like `theme_bw()` in ggplot2.
    #[allow(clippy::field_reassign_with_default)]
    pub fn bw() -> Self {
        let mut t = Theme::default();
        t.palette = Palette {
            paper: rgb(1.0, 1.0, 1.0),
            ink: rgb(0.0, 0.0, 0.0),
            accent: rgb(0.3, 0.3, 0.3),
        };
        // Re-derive grid colors against the new palette so they sit at
        // sensible intermediate grays.
        t.panel_grid_major = PerChannel::new(LineElement {
            color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.22)),
            ..LineElement::default()
        });
        t.panel_grid_minor = PerChannel::new(LineElement {
            color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.10)),
            ..LineElement::default()
        });
        t
    }

    /// Strip every panel / axis element to `Blank` — only the
    /// data marks render. Mirrors `theme_void()`.
    #[allow(clippy::field_reassign_with_default)]
    pub fn void() -> Self {
        let mut t = Theme::default();
        t.palette.paper = rgb(1.0, 1.0, 1.0);
        t.panel_background = Element::Blank;
        t.panel_border = Element::Blank;
        t.panel_grid_major = PerChannel {
            all: Element::Blank,
            by_channel: [Element::Inherit, Element::Inherit],
        };
        t.panel_grid_minor = PerChannel {
            all: Element::Blank,
            by_channel: [Element::Inherit, Element::Inherit],
        };
        t.axis = super::axis::PerAxis::new(super::axis::AxisTheme {
            title: Element::Blank,
            text: Element::Blank,
            line: Element::Blank,
            ticks: Element::Blank,
            ticks_minor: Element::Blank,
            ..super::axis::AxisTheme::default()
        });
        t
    }

    /// Replace the palette wholesale. Element references re-resolve
    /// at next render. Sugar for `.with_palette(Palette::new(paper,
    /// ink, accent))`.
    pub fn with_palette_anchors(self, paper: Color, ink: Color, accent: Color) -> Self {
        self.with_palette(Palette::new(paper, ink, accent))
    }
}
