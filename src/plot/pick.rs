//! The pick vocabulary the plot layer speaks.
//!
//! [`crate::pick`] is chart-agnostic on purpose: a [`PickScope`] carries a
//! `&'static str` kind and two optional fields, and nothing down there knows
//! what an axis is. This module supplies the meanings — the constructors
//! plot code pushes, and the typed view a consumer reads a hit back through.
//! It is the same split [`Slot::name`] and the `Region` trait already use
//! between the composition anatomy and the layout solver.
//!
//! # The grammar
//!
//! ```text
//! composition → plot? → region(Slot) → [axis|legend|geom]? → part → item?
//! ```
//!
//! Only `region` and `part` are always present. The middle group frame
//! appears where there is a sub-object with its own identity — an axis, a
//! legend block, a geom. `plot` is absent for composition-level chrome,
//! because the figure title has no owning plot.

use std::sync::Arc;

use crate::composition::Slot;
use crate::pick::{PickPath, PickScope};
use crate::plot::chrome::axis::AxisId;
use crate::plot::plot::GeomId;

/// Scope kinds. Interned as `&'static str` in the scope itself; these are
/// the names the typed accessors match on.
pub mod kind {
    /// A whole composition, named by its composition id.
    pub const COMPOSITION: &str = "composition";
    /// One plot, named by patch id and numbered within that patch.
    pub const PLOT: &str = "plot";
    /// An anatomical region, named by [`Slot::name`](crate::composition::Slot::name).
    pub const REGION: &str = "region";
    /// One axis, named by its scale and numbered by its `AxisId`.
    pub const AXIS: &str = "axis";
    /// One legend block, named by its domain scale.
    pub const LEGEND: &str = "legend";
    /// One geom, numbered by its `GeomId`.
    pub const GEOM: &str = "geom";
    /// A distinguishable piece of chrome — see [`PlotPart`].
    pub const PART: &str = "part";
    /// An ordinal within a part: a break index, a legend row, a key.
    pub const ITEM: &str = "item";
}

/// A distinguishable piece of chrome inside an anatomical region.
///
/// Finer-grained than [`Slot`], deliberately: a slot is a layout concept and
/// owns a rect, while a part is a drawing concept with no rect of its own.
/// Keeping them separate is what stops [`Slot::placement`] — which is total
/// over the anatomy — from having to answer for tick marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlotPart {
    // Panel and background.
    PlotBackground,
    PanelBackground,
    GridMajor,
    GridMinor,
    Graticule,
    PanelOutline,
    // Axis.
    AxisLine,
    AxisTick,
    AxisMinorTick,
    AxisTickLabel,
    AxisTitle,
    // Strip.
    StripBackground,
    StripLabel,
    // Legend.
    LegendBackground,
    LegendTitle,
    LegendKeyFrame,
    LegendKey,
    LegendLabel,
    ColorbarBar,
    ColorbarTick,
    // Free text.
    Title,
    Subtitle,
    Caption,
}

impl PlotPart {
    /// Stable snake_case identifier. Same contract as [`Slot::name`]: it is
    /// the wire form, so it may be matched on and must not drift.
    pub const fn name(self) -> &'static str {
        match self {
            PlotPart::PlotBackground => "plot_background",
            PlotPart::PanelBackground => "panel_background",
            PlotPart::GridMajor => "grid_major",
            PlotPart::GridMinor => "grid_minor",
            PlotPart::Graticule => "graticule",
            PlotPart::PanelOutline => "panel_outline",
            PlotPart::AxisLine => "axis_line",
            PlotPart::AxisTick => "axis_tick",
            PlotPart::AxisMinorTick => "axis_minor_tick",
            PlotPart::AxisTickLabel => "axis_tick_label",
            PlotPart::AxisTitle => "axis_title",
            PlotPart::StripBackground => "strip_background",
            PlotPart::StripLabel => "strip_label",
            PlotPart::LegendBackground => "legend_background",
            PlotPart::LegendTitle => "legend_title",
            PlotPart::LegendKeyFrame => "legend_key_frame",
            PlotPart::LegendKey => "legend_key",
            PlotPart::LegendLabel => "legend_label",
            PlotPart::ColorbarBar => "colorbar_bar",
            PlotPart::ColorbarTick => "colorbar_tick",
            PlotPart::Title => "title",
            PlotPart::Subtitle => "subtitle",
            PlotPart::Caption => "caption",
        }
    }

    /// The part a [`Self::name`] identifier came from.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "plot_background" => PlotPart::PlotBackground,
            "panel_background" => PlotPart::PanelBackground,
            "grid_major" => PlotPart::GridMajor,
            "grid_minor" => PlotPart::GridMinor,
            "graticule" => PlotPart::Graticule,
            "panel_outline" => PlotPart::PanelOutline,
            "axis_line" => PlotPart::AxisLine,
            "axis_tick" => PlotPart::AxisTick,
            "axis_minor_tick" => PlotPart::AxisMinorTick,
            "axis_tick_label" => PlotPart::AxisTickLabel,
            "axis_title" => PlotPart::AxisTitle,
            "strip_background" => PlotPart::StripBackground,
            "strip_label" => PlotPart::StripLabel,
            "legend_background" => PlotPart::LegendBackground,
            "legend_title" => PlotPart::LegendTitle,
            "legend_key_frame" => PlotPart::LegendKeyFrame,
            "legend_key" => PlotPart::LegendKey,
            "legend_label" => PlotPart::LegendLabel,
            "colorbar_bar" => PlotPart::ColorbarBar,
            "colorbar_tick" => PlotPart::ColorbarTick,
            "title" => PlotPart::Title,
            "subtitle" => PlotPart::Subtitle,
            "caption" => PlotPart::Caption,
            _ => return None,
        })
    }

    /// Every part, in declaration order.
    pub const ALL: [PlotPart; 23] = [
        PlotPart::PlotBackground,
        PlotPart::PanelBackground,
        PlotPart::GridMajor,
        PlotPart::GridMinor,
        PlotPart::Graticule,
        PlotPart::PanelOutline,
        PlotPart::AxisLine,
        PlotPart::AxisTick,
        PlotPart::AxisMinorTick,
        PlotPart::AxisTickLabel,
        PlotPart::AxisTitle,
        PlotPart::StripBackground,
        PlotPart::StripLabel,
        PlotPart::LegendBackground,
        PlotPart::LegendTitle,
        PlotPart::LegendKeyFrame,
        PlotPart::LegendKey,
        PlotPart::LegendLabel,
        PlotPart::ColorbarBar,
        PlotPart::ColorbarTick,
        PlotPart::Title,
        PlotPart::Subtitle,
        PlotPart::Caption,
    ];
}

// ─── Scope constructors ──────────────────────────────────────────────────
//
// The only way plot code builds a frame. Group vs Target lives here rather
// than at the call sites, so "chrome is a target, structure is not" cannot
// be got wrong one call site at a time.

/// A whole composition, named by its composition id.
pub fn composition_scope(id: &str) -> PickScope {
    PickScope::group(kind::COMPOSITION).with_name(id)
}

/// One plot, addressed the way
/// [`PlotComposition::update_plot_at`](crate::plot::PlotComposition::update_plot_at)
/// addresses it.
pub fn plot_scope(patch_id: &Arc<str>, index_in_patch: u32) -> PickScope {
    PickScope::group(kind::PLOT)
        .with_name(patch_id.clone())
        .with_index(index_in_patch)
}

/// An anatomical region. Its name is the layout lookup key, so a hit
/// round-trips into `CompositionLayout::get` to recover the region's rect.
pub fn region_scope(slot: Slot) -> PickScope {
    PickScope::group(kind::REGION).with_name(slot.name())
}

/// A region placed with `place_at` rather than into the fixed anatomy.
pub fn named_region_scope(name: &str) -> PickScope {
    PickScope::group(kind::REGION).with_name(name)
}

/// One geom within a plot.
pub fn geom_scope(id: GeomId) -> PickScope {
    PickScope::group(kind::GEOM).with_index(id.raw())
}

/// One axis, carrying the scale that drives it so a consumer can re-derive a
/// break value from an [`PlotPath::item`] ordinal.
pub fn axis_scope(id: Option<AxisId>, scale_name: Option<&str>) -> PickScope {
    let mut s = PickScope::group(kind::AXIS);
    if let Some(name) = scale_name {
        s = s.with_name(name);
    }
    if let Some(id) = id {
        s = s.with_index(id.raw());
    }
    s
}

/// One legend block, numbered within its side's stack.
pub fn legend_scope(block: u32, domain_scale: &str) -> PickScope {
    PickScope::group(kind::LEGEND)
        .with_name(domain_scale)
        .with_index(block)
}

/// A piece of chrome. **A target**: whatever is drawn directly inside is
/// indexed even though chrome carries no [`PickId`](crate::pick::PickId).
pub fn part_scope(part: PlotPart) -> PickScope {
    PickScope::target(kind::PART).with_name(part.name())
}

/// A piece of chrome that repeats per channel — gridlines, mostly. The index
/// is the `PerChannel` coordinate, i.e. the theme's own addressing.
pub fn part_scope_for_channel(part: PlotPart, channel: u8) -> PickScope {
    PickScope::target(kind::PART)
        .with_name(part.name())
        .with_index(u32::from(channel))
}

/// An ordinal within a part: a break index, a legend row, a key.
pub fn item_scope(index: u32) -> PickScope {
    PickScope::target(kind::ITEM).with_index(index)
}

// ─── Typed view ──────────────────────────────────────────────────────────

/// A [`PickPath`] read through the plot layer's vocabulary.
///
/// Every accessor searches inward-out, so the innermost frame of a kind wins
/// — which is what you want when a legend sits inside a panel.
#[derive(Debug, Clone, Copy)]
pub struct PlotPath<'a>(PickPath<'a>);

impl<'a> PlotPath<'a> {
    /// Read a raw path through the plot vocabulary.
    pub fn new(path: PickPath<'a>) -> Self {
        Self(path)
    }

    /// The underlying untyped path.
    pub fn raw(&self) -> PickPath<'a> {
        self.0
    }

    /// Id of the composition the hit belongs to.
    pub fn composition(&self) -> Option<&'a str> {
        self.0.find(kind::COMPOSITION).and_then(|s| s.name())
    }

    /// `(patch_id, index_in_patch)` — the pair
    /// [`PlotComposition::update_plot_at`](crate::plot::PlotComposition::update_plot_at)
    /// takes. `None` for composition-level chrome, which has no owning plot.
    pub fn plot(&self) -> Option<(&'a str, u32)> {
        let s = self.0.find(kind::PLOT)?;
        Some((s.name()?, s.index().unwrap_or(0)))
    }

    /// The anatomical region, when the hit is in one. `None` for a
    /// `place_at` region — see [`Self::region_name`].
    pub fn region(&self) -> Option<Slot> {
        Slot::from_name(self.region_name()?)
    }

    /// The region's lookup name, anatomical or not.
    pub fn region_name(&self) -> Option<&'a str> {
        self.0.find(kind::REGION).and_then(|s| s.name())
    }

    /// The geom the hit came from, if it was a mark.
    pub fn geom(&self) -> Option<GeomId> {
        self.0
            .find(kind::GEOM)
            .and_then(|s| s.index())
            .map(GeomId::new)
    }

    /// The axis the hit belongs to, if it was axis chrome.
    pub fn axis(&self) -> Option<AxisId> {
        self.0
            .find(kind::AXIS)
            .and_then(|s| s.index())
            .map(AxisId::new)
    }

    /// Name of the scale driving the enclosing axis or legend.
    ///
    /// The key for recovering a domain value: with [`Self::item`] it gives
    /// `registry.get(scale).breaks(n)[item]`. Carrying the name rather than
    /// the value is deliberate — a
    /// [`Value`](crate::scales::value::Value) is not hashable, and a name
    /// resolves against the live scale rather than a snapshot of it.
    pub fn scale(&self) -> Option<&'a str> {
        self.0
            .find(kind::AXIS)
            .or_else(|| self.0.find(kind::LEGEND))
            .and_then(|s| s.name())
    }

    /// Which legend block, within its side's stack.
    pub fn legend_block(&self) -> Option<u32> {
        self.0.find(kind::LEGEND).and_then(|s| s.index())
    }

    /// The piece of chrome that was hit.
    pub fn part(&self) -> Option<PlotPart> {
        PlotPart::from_name(self.0.find(kind::PART)?.name()?)
    }

    /// The channel a per-channel part belongs to — gridlines carry the
    /// `PerChannel` index the theme stores them under.
    pub fn part_channel(&self) -> Option<u8> {
        self.0
            .find(kind::PART)
            .and_then(|s| s.index())
            .and_then(|i| u8::try_from(i).ok())
    }

    /// Ordinal within the part: break index, legend row, key index.
    pub fn item(&self) -> Option<u32> {
        self.0.find(kind::ITEM).and_then(|s| s.index())
    }
}

/// The `(channel, side)` coordinate the theme files a region's axis chrome
/// under — the pair
/// [`Sided::resolve`](crate::plot::theme::cascade::Sided::resolve) and
/// [`Theme::resolved_axis`](crate::plot::theme::Theme::resolved_axis) take.
///
/// This is why there is no `ElementRef` type and no `"axis.text.x"` string
/// scheme: `region` plus `part` already *is* a theme address, and this turns
/// it into the coordinates the theme indexes by.
pub fn theme_channel_side(slot: Slot) -> Option<(u8, u8)> {
    Some(match slot {
        Slot::AxisBottom | Slot::AxisBottomTitle => (0, 0),
        Slot::AxisTop | Slot::AxisTopTitle => (0, 1),
        Slot::AxisLeft | Slot::AxisLeftTitle => (1, 0),
        Slot::AxisRight | Slot::AxisRightTitle => (1, 1),
        _ => return None,
    })
}
