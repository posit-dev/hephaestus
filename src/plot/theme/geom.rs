//! [`GeomTheme`] and its per-geom default sub-structs.
//!
//! Every geom's hardcoded `DEFAULT_*` constant has a counterpart
//! here. Geoms read defaults from `ctx.theme.geom.<geom>.<field>` at
//! draw time when no channel binding supplies the value.
//!
//! The defaults are intentionally minimal: only style-related values
//! that a theme might reasonably override. Geometric / semantic
//! defaults (rect band offsets, B-spline degree, partial-wedge
//! sweep) stay as constants in each geom — they describe meaning,
//! not appearance.
//!
//! Colour fields are `Option<ThemeColor>`. `None` preserves the
//! pre-theme "channel-or-nothing" semantic (no channel bound and no
//! theme default = nothing rendered for that aesthetic). `Some(...)`
//! sets a default rendered colour the geom uses when the channel is
//! unbound. Built-in themes (`Theme::default()` etc.) leave these at
//! `None`; the ggplot2-style defaults task populates them with
//! palette-anchored values.

use std::sync::Arc;

use crate::stroke::{Cap, Join};

use super::palette::ThemeColor;

/// Per-geom default style values. Read at draw time when a channel
/// binding doesn't supply the value.
#[derive(Debug, Clone, PartialEq)]
pub struct GeomTheme {
    /// Defaults for `PointGeom`.
    pub point: PointDefaults,
    /// Defaults for `LineGeom` (stroke-only).
    pub line: LineDefaults,
    /// Defaults for `SegmentGeom` (stroke-only).
    pub segment: LineDefaults,
    /// Defaults for `PolygonGeom` (fill + stroke).
    pub polygon: ShapeDefaults,
    /// Defaults for `RectGeom` (fill + stroke).
    pub rect: ShapeDefaults,
    /// Defaults for `EllipseGeom` (fill + stroke).
    pub ellipse: ShapeDefaults,
    /// Defaults for `WedgeGeom` (fill + stroke).
    pub wedge: ShapeDefaults,
    /// Defaults for `BSplineGeom` (stroke-only).
    pub bspline: LineDefaults,
    /// Defaults for `RibbonGeom` (fill + stroke).
    pub ribbon: ShapeDefaults,
    /// Defaults for `RibbonBSplineGeom` (fill + stroke). Independent
    /// from `ribbon` so smoothed ribbons can carry their own style.
    pub ribbon_bspline: ShapeDefaults,
    /// Defaults for `TextGeom`.
    pub text: TextDefaults,
    /// Defaults for `TextFitGeom`.
    pub text_fit: TextFitDefaults,
    /// Defaults for `TextPathGeom`.
    pub text_path: TextDefaults,
    /// Stroke width for shape markers stamped into linetype dash
    /// patterns (used by `LineGeom` and descendants when a
    /// `Marker(...)` step appears in the linetype), pt.
    pub marker_outline_pt: f64,
}

impl Default for GeomTheme {
    fn default() -> Self {
        Self {
            point: PointDefaults::default(),
            line: LineDefaults::default(),
            segment: LineDefaults::default(),
            polygon: ShapeDefaults::default(),
            rect: ShapeDefaults::default(),
            ellipse: ShapeDefaults::default(),
            wedge: ShapeDefaults::default(),
            bspline: LineDefaults::default(),
            ribbon: ShapeDefaults::default(),
            ribbon_bspline: ShapeDefaults::default(),
            text: TextDefaults::default(),
            text_fit: TextFitDefaults::default(),
            text_path: TextDefaults::default(),
            marker_outline_pt: 0.5,
        }
    }
}

/// Defaults for [`PointGeom`](crate::plot::geom::PointGeom).
#[derive(Debug, Clone, PartialEq)]
pub struct PointDefaults {
    /// Marker diameter in pt when no `"size"` channel is bound.
    pub size_pt: f64,
    /// Shape-registry name to use when no `"shape"` channel is
    /// bound.
    pub shape: Arc<str>,
    /// Default fill color when no `"fill"` channel is bound.
    /// `None` = no fill rendered.
    pub fill: Option<ThemeColor>,
    /// Default stroke color when no `"stroke"` channel is bound.
    /// `None` = no stroke rendered.
    pub stroke: Option<ThemeColor>,
    /// Stroke width in pt for the marker outline when no
    /// `"linewidth"` channel is bound.
    pub stroke_width_pt: f64,
}

impl Default for PointDefaults {
    /// 5pt circle marker with a 1pt outline. Colours default to
    /// `None` so the pre-theme "fill / stroke channel or nothing"
    /// semantic is preserved; a populated theme can override.
    fn default() -> Self {
        Self {
            size_pt: 5.0,
            shape: Arc::from("circle"),
            fill: None,
            stroke: None,
            stroke_width_pt: 1.0,
        }
    }
}

/// Defaults for a stroke-only geom — line, segment, B-spline.
#[derive(Debug, Clone, PartialEq)]
pub struct LineDefaults {
    /// Stroke colour when no `"stroke"` channel is bound. Named
    /// `stroke` to match the channel name used throughout the API.
    pub stroke: Option<ThemeColor>,
    /// Stroke width in pt when no `"linewidth"` channel is bound.
    pub linewidth_pt: f64,
    /// Stroke endpoint cap style.
    pub cap: Cap,
    /// Stroke segment join style.
    pub join: Join,
}

impl Default for LineDefaults {
    /// 1pt solid butt-cap miter-join. `stroke = None` preserves the
    /// pre-theme "stroke-channel-or-nothing" semantic.
    fn default() -> Self {
        Self {
            stroke: None,
            linewidth_pt: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
        }
    }
}

/// Defaults for a filled-shape geom — polygon, rect, ellipse, wedge,
/// ribbon. Carries both fill and stroke defaults; either can be
/// `None` to suppress that piece of the rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeDefaults {
    /// Default fill color when no `"fill"` channel is bound.
    /// `None` = no fill rendered.
    pub fill: Option<ThemeColor>,
    /// Default stroke color when no `"stroke"` channel is bound.
    /// `None` = no stroke rendered.
    pub stroke: Option<ThemeColor>,
    /// Stroke width in pt when no `"linewidth"` channel is bound.
    pub linewidth_pt: f64,
    /// Stroke endpoint cap style.
    pub cap: Cap,
    /// Stroke segment join style.
    pub join: Join,
}

impl Default for ShapeDefaults {
    /// 1pt solid butt-cap miter-join stroke; fill and stroke
    /// colours `None`.
    fn default() -> Self {
        Self {
            fill: None,
            stroke: None,
            linewidth_pt: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
        }
    }
}

/// Defaults for [`TextGeom`](crate::plot::geom::TextGeom) and
/// [`TextPathGeom`](crate::plot::geom::TextPathGeom).
#[derive(Debug, Clone, PartialEq)]
pub struct TextDefaults {
    /// Font size in pt when no `"size"` channel is bound.
    pub size_pt: f64,
    /// Font weight (CSS 100..=900) when no `"weight"` channel is
    /// bound.
    pub weight: u16,
    /// Default text fill colour when no `"fill"` channel is bound.
    /// `None` = no text rendered (rare; text usually needs a fill).
    pub fill: Option<ThemeColor>,
    /// `"anchor_x"` fallback (0 = left, 0.5 = center, 1 = right).
    pub anchor_x: f64,
    /// `"anchor_y"` fallback (0 = top, 0.5 = center, 1 = bottom).
    pub anchor_y: f64,
    /// Default background-rect fill colour. `None` = no background
    /// rendered.
    pub bg_fill: Option<ThemeColor>,
    /// Default background-rect stroke colour. `None` = no border
    /// rendered.
    pub bg_stroke: Option<ThemeColor>,
    /// Background outline stroke width in pt when no `"bg_linewidth"`
    /// channel is bound.
    pub bg_linewidth_pt: f64,
}

impl Default for TextDefaults {
    /// 12pt regular black text, centered, 1pt bg outline width. No
    /// background rect by default (caller-bound only). Note: `fill`
    /// is `Some(Ink)` since text without a fill renders nothing —
    /// the historic behaviour fell back to a hardcoded black fill.
    fn default() -> Self {
        Self {
            size_pt: 12.0,
            weight: 400,
            fill: Some(ThemeColor::Ink),
            anchor_x: 0.5,
            anchor_y: 0.5,
            bg_fill: None,
            bg_stroke: None,
            bg_linewidth_pt: 1.0,
        }
    }
}

/// Defaults for [`TextFitGeom`](crate::plot::geom::TextFitGeom).
#[derive(Debug, Clone, PartialEq)]
pub struct TextFitDefaults {
    /// Lower bound on the auto-fit font size in pt.
    pub min_font_pt: f64,
    /// Upper bound on the auto-fit font size in pt.
    pub max_font_pt: f64,
    /// Font weight when no `"weight"` channel is bound.
    pub weight: u16,
    /// Default text fill colour.
    pub fill: Option<ThemeColor>,
    /// Default background-rect fill colour.
    pub bg_fill: Option<ThemeColor>,
    /// Default background-rect stroke colour.
    pub bg_stroke: Option<ThemeColor>,
    /// Background outline stroke width in pt.
    pub bg_linewidth_pt: f64,
}

impl Default for TextFitDefaults {
    /// 6pt floor, 96pt ceiling, regular weight, ink fill, no bg
    /// fill/border by default.
    fn default() -> Self {
        Self {
            min_font_pt: 6.0,
            max_font_pt: 96.0,
            weight: 400,
            fill: Some(ThemeColor::Ink),
            bg_fill: None,
            bg_stroke: None,
            bg_linewidth_pt: 1.0,
        }
    }
}
