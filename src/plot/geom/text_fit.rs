//! `TextFitGeom` — vectorised text labels that **scale font size** to
//! fit inside a target rect.
//!
//! Sibling of [`TextGeom`](super::TextGeom). The user supplies a target rect via
//! `(x, y) – (x2, y2)` corners (same convention as `RectGeom`) plus a
//! string; the geom runs a small binary search on font size between
//! `min_font_size` and `max_font_size` to find the largest size at
//! which the laid-out text fits within the rect (wrapping at the rect
//! width). When even `min_font_size` doesn't fit, the geom draws at
//! that minimum and pushes a clip rect so the overflow is cut at the
//! target rect edges.
//!
//! Use cases: callout labels that always fill their container,
//! dashboard tiles, faceted strip labels.
//!
//! Channels consumed:
//!
//! - `"x"`, `"y"` — one corner of the target rect (required; data; numeric).
//! - `"x2"`, `"y2"` — the opposite corner (required; data; numeric).
//! - `"x_offset"`, `"y_offset"`, `"x2_offset"`, `"y2_offset"` — per-edge
//!   absolute pt offsets after scale resolution.
//! - `"x_band"`, `"y_band"`, `"x2_band"`, `"y2_band"` — per-edge
//!   band-fraction offsets. All default to `0.0`.
//! - `"text"` — string content (required).
//! - `"family"`, `"weight"`, `"italic"` — font style (no `"size"`
//!   channel; the geom computes it).
//! - `"tracking"` — letter spacing in 1/1000 em (`20.0` = `0.02 em`),
//!   so it stays proportional as the fit scales the text.
//! - `"underline"`, `"strikethrough"` — booleans.
//! - `"text_stroke"`, `"text_linewidth"` — per-glyph outline colour and
//!   thickness in pt, drawn behind the fill.
//! - `"markdown"` — boolean; default `false`. When true the label is
//!   read as marquee-flavoured markdown and shaped through
//!   [`crate::text::rich`], and the fit measures that layout — block
//!   structure, span chrome and per-span fonts all take part in
//!   deciding the size, and all of them draw, since a rect is the
//!   shape rich text is laid out for. The row's `"text_stroke"` /
//!   `"text_linewidth"` fold onto the style sheet's root selector so
//!   every span inherits the halo; a span that sets its own
//!   `text_stroke` wins. `with_rich_sheet` installs a per-geom style
//!   sheet, as on [`TextGeom`](super::TextGeom). Unlike the other text
//!   channels this one has no theme default — the channel is the only
//!   switch.
//! - `"min_font_size"` — pt; lower bound on the binary search.
//!   Default `6.0`.
//! - `"max_font_size"` — pt; upper bound. Default `96.0`.
//! - `"justify_x"` — line justification within the wrap box. Strings:
//!   `"start"` (default), `"center"`, `"end"`, `"justify"`.
//! - `"justify_y"` — **vertical** placement of the fitted text block
//!   within the rect when the fit leaves vertical slack. Strings:
//!   `"start"` (default = top), `"center"`, `"end"`.
//! - `"fill"`, `"fill_opacity"` — text colour.
//! - `"bg_fill"`, `"bg_fill_opacity"`, `"bg_stroke"`, `"bg_stroke_opacity"`,
//!   `"bg_linewidth"`, `"bg_corner_radius"`, `"bg_padding"` — optional
//!   background rect hugging the fitted text block (separate from the
//!   target rect — the bg rect tracks where the text actually lands).
//! - `"angle"` — rotation in **radians** around the rect centre,
//!   mathematical CCW. Default `0.0`. Justification is orthogonal to
//!   rotation (the laid-out block is rotated as a rigid body around
//!   the rect's centre).
//! - `"pick_id"` — per-row picking ticket.
//!
//! **Cost**: each row pays up to `MAX_ITERS + 1` parley reshapes (full
//! glyph shape rebuild) — one per binary-search step plus the final
//! draw run. At default `[6, 96]` font-size bounds and `MAX_ITERS = 4`,
//! the final size is within `(96 - 6) / 2^4 ≈ 5.6` pt of the optimum.
//! A markdown row shapes through the geom's [`RichShapeCache`] instead,
//! one entry per probe, so a redraw at an unchanged rect walks the
//! search on cache hits alone.
//!
//! [`RichShapeCache`]: crate::text::rich::RichShapeCache

use std::rc::Rc;
use std::sync::Arc;

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Rect};
use crate::path::FillRule;
use crate::pick::PickId;
use crate::plot::theme::HAlign;
use crate::plot::value::Value;
use crate::primitives::{rect as rect_path, rounded_rect};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};
use crate::text::rich::{
    draw_rich_text, HAnchor, RichAnchor, RichKey, RichShapeCache, RichTextRun, RichTextStyleSheet,
    RichTextWidth, VAnchor,
};
use crate::text::{draw_text, TextRun, TextStyle};

use super::resolve::{
    override_alpha, pt_to_px, resolve_angle_channel, resolve_bool_channel_or,
    resolve_color_channel, resolve_color_channel_or_theme, resolve_number_channel,
    resolve_number_channel_or, resolve_pick_id, resolve_position,
};
use super::rich::{panel_space_transform, OutlineSheets};
use super::state::{
    finalize_state, require_data_column, require_x_and_siblings, GeomState, KeysStrategy,
};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────

// Style defaults (min/max font, weight, bg linewidth) live on
// `theme.geom.text_fit` and are read via `ctx.theme.geom.text_fit.*`.
/// Binary-search iteration count. At `[6, 96]` bounds the final font
/// size is within `(96 - 6) / 2^4 ≈ 5.6` pt of optimum — fine for
/// fitting visible text. Tighter bounds via `min_font_size` /
/// `max_font_size` narrow further.
const MAX_ITERS: usize = 4;
fn default_fill() -> crate::color::Color {
    crate::color::Color::new([0.0, 0.0, 0.0, 1.0])
}

const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x2", ExpectedOutput::Numbers),
    ("y2", ExpectedOutput::Numbers),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x2_offset", ExpectedOutput::Numbers),
    ("y2_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("x2_band", ExpectedOutput::Numbers),
    ("y2_band", ExpectedOutput::Numbers),
    ("text", ExpectedOutput::Strings),
    ("family", ExpectedOutput::Strings),
    ("weight", ExpectedOutput::Numbers),
    ("italic", ExpectedOutput::Any),
    ("tracking", ExpectedOutput::Numbers),
    ("markdown", ExpectedOutput::Any),
    ("underline", ExpectedOutput::Any),
    ("strikethrough", ExpectedOutput::Any),
    ("text_stroke", ExpectedOutput::Colors),
    ("text_linewidth", ExpectedOutput::Numbers),
    ("min_font_size", ExpectedOutput::Numbers),
    ("max_font_size", ExpectedOutput::Numbers),
    ("fill", ExpectedOutput::Colors),
    ("fill_opacity", ExpectedOutput::Numbers),
    ("bg_fill", ExpectedOutput::Colors),
    ("bg_fill_opacity", ExpectedOutput::Numbers),
    ("bg_stroke", ExpectedOutput::Colors),
    ("bg_stroke_opacity", ExpectedOutput::Numbers),
    ("bg_linewidth", ExpectedOutput::Numbers),
    ("bg_corner_radius", ExpectedOutput::Numbers),
    ("bg_padding", ExpectedOutput::Numbers),
    ("justify_x", ExpectedOutput::Strings),
    ("justify_y", ExpectedOutput::Strings),
    ("angle", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── The fitted label ────────────────────────────────────────────────────────

/// A label shaped by whichever text path its row asked for. Both arms
/// answer the questions the fit search asks, so the search itself is
/// written once.
enum Fitted {
    Plain(Box<TextRun>),
    Rich(Rc<RichTextRun>),
}

impl Fitted {
    /// Width of the widest line at the current break.
    fn content_width(&self) -> f64 {
        match self {
            Fitted::Plain(run) => run.content_width(),
            Fitted::Rich(run) => run.content_width(),
        }
    }

    /// Stacked height at the current break.
    fn height(&self) -> f64 {
        match self {
            Fitted::Plain(run) => run.current_height(),
            Fitted::Rich(run) => run.current_height(),
        }
    }

    /// Font descender of the last line — the background rect's padding
    /// rebalance reads it.
    fn last_line_descender(&self) -> f64 {
        match self {
            Fitted::Plain(run) => run.last_line_descender(),
            Fitted::Rich(run) => run.last_line_descender(),
        }
    }

    /// Draw the label with its box's top-left corner at `(x, y)`.
    ///
    /// `brush` paints the plain arm; a markdown label carries the
    /// colours its spans resolved to, so the rich arm ignores it.
    fn draw(
        &self,
        scene: &mut dyn SceneBuilder,
        x: f64,
        y: f64,
        brush: &Brush,
        transform: Affine,
        pick: PickId,
    ) {
        match self {
            Fitted::Plain(run) => draw_text(scene, run.as_ref(), x, y, brush, transform, pick),
            Fitted::Rich(run) => draw_rich_text(
                scene,
                run,
                x,
                y,
                RichAnchor {
                    h: HAnchor::Left,
                    v: VAnchor::Top,
                },
                panel_space_transform(transform, x, y),
                pick,
            ),
        }
    }

    /// Stroke-only pass behind the fill, for the row's `"text_stroke"`.
    ///
    /// Only the plain arm draws anything: a markdown label's outline is
    /// folded onto its style sheet before shaping, so the rich draw
    /// pass has already emitted it.
    fn draw_outline(
        &self,
        scene: &mut dyn SceneBuilder,
        x: f64,
        y: f64,
        brush: &Brush,
        stroke: &Stroke,
        transform: Affine,
    ) {
        if let Fitted::Plain(run) = self {
            crate::text::draw_text_outline(
                scene,
                run.as_ref(),
                x,
                y,
                brush,
                stroke,
                transform,
                PickId::Skip,
            );
        }
    }
}

// ─── TextFitGeom ─────────────────────────────────────────────────────────────

/// A vectorised fit-text-to-rect geom. One fitted label per row.
pub struct TextFitGeom {
    pub(crate) state: GeomState,
    /// Optional per-geom style sheet used when the `"markdown"`
    /// channel resolves `true`. `None` falls back to the theme's
    /// `rich_text` sheet.
    pub(crate) rich_sheet: Option<Arc<RichTextStyleSheet>>,
    /// Shaped markdown rows, reused across frames — including the
    /// intermediate sizes the fit search probes, so a redraw at an
    /// unchanged rect re-walks the search without reshaping.
    pub(crate) rich_cache: RichShapeCache,
    /// Sheets derived from the base one by folding a row's
    /// `text_stroke` / `text_linewidth` onto its root selector.
    pub(crate) rich_outline_sheets: OutlineSheets,
}

crate::impl_geom_inherents!(TextFitGeom);

impl TextFitGeom {
    /// Install a rich-text style sheet used for every row this geom
    /// renders as markdown. Overrides the theme's default sheet.
    /// Chains for builder-style construction.
    pub fn with_rich_sheet(mut self, sheet: Arc<RichTextStyleSheet>) -> Self {
        self.rich_sheet = Some(sheet);
        self
    }

    /// Same as [`Self::with_rich_sheet`] for mutation through
    /// `Plot::update_geom(&mut TextFitGeom)`.
    pub fn set_rich_sheet(&mut self, sheet: Arc<RichTextStyleSheet>) {
        self.rich_sheet = Some(sheet);
    }

    /// Clear any per-geom rich-text sheet override — falls back to
    /// the theme default.
    pub fn clear_rich_sheet(&mut self) {
        self.rich_sheet = None;
    }

    /// Clear the shaped-markdown caches. The keys cover the sheet's
    /// identity, so swapping sheets doesn't require this — it exists
    /// for callers that mutate a sheet in place despite the
    /// immutable-once-installed convention.
    pub fn clear_rich_cache(&mut self) {
        self.rich_cache.clear();
        self.rich_outline_sheets.clear();
    }
}

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for TextFitGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();
        let n = require_x_and_siblings(&channels, &["y", "x2", "y2"], "TextFitGeom");
        require_data_column("text", &channels, "TextFitGeom");
        let state = finalize_state(
            keys_opt,
            channels,
            n,
            KeysStrategy::PerRow,
            CHANNELS,
            "TextFitGeom",
        );
        TextFitGeom {
            state,
            rich_sheet: None,
            rich_cache: RichShapeCache::new(),
            rich_outline_sheets: OutlineSheets::new(),
        }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for TextFitGeom {
    fn state(&self) -> &GeomState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut GeomState {
        &mut self.state
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn kind(&self) -> Option<&'static str> {
        Some("text-fit")
    }

    fn invalidate_caches(&mut self) {
        self.rich_cache.clear();
        self.rich_outline_sheets.clear();
    }

    fn draw(&self, scene: &mut dyn SceneBuilder, ctx: &GeomContext<'_>) {
        let panel = ctx.panel_rect;
        let panel_w = panel.x1 - panel.x0;
        let panel_h = panel.y1 - panel.y0;
        if panel_w <= 0.0 || panel_h <= 0.0 {
            return;
        }
        let n = self.len();
        if n == 0 {
            return;
        }

        let x_scale_bound = ctx.scale_for("x");
        let y_scale_bound = ctx.scale_for("y");
        let x2_scale_bound = ctx.scale_for("x2").or(x_scale_bound);
        let y2_scale_bound = ctx.scale_for("y2").or(y_scale_bound);
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x2_offset_scale = ctx.scale_for("x2_offset");
        let y2_offset_scale = ctx.scale_for("y2_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let x2_band_scale = ctx.scale_for("x2_band");
        let y2_band_scale = ctx.scale_for("y2_band");
        let text_scale = ctx.scale_for("text");
        let family_scale = ctx.scale_for("family");
        let weight_scale = ctx.scale_for("weight");
        let italic_scale = ctx.scale_for("italic");
        let tracking_scale = ctx.scale_for("tracking");
        let markdown_scale = ctx.scale_for("markdown");
        let underline_scale = ctx.scale_for("underline");
        let strikethrough_scale = ctx.scale_for("strikethrough");
        let text_stroke_scale = ctx.scale_for("text_stroke");
        let text_linewidth_scale = ctx.scale_for("text_linewidth");
        let min_font_scale = ctx.scale_for("min_font_size");
        let max_font_scale = ctx.scale_for("max_font_size");
        let fill_scale = ctx.scale_for("fill");
        let fill_opacity_scale = ctx.scale_for("fill_opacity");
        let bg_fill_scale = ctx.scale_for("bg_fill");
        let bg_fill_opacity_scale = ctx.scale_for("bg_fill_opacity");
        let bg_stroke_scale = ctx.scale_for("bg_stroke");
        let bg_stroke_opacity_scale = ctx.scale_for("bg_stroke_opacity");
        let bg_linewidth_scale = ctx.scale_for("bg_linewidth");
        let bg_corner_radius_scale = ctx.scale_for("bg_corner_radius");
        let bg_padding_scale = ctx.scale_for("bg_padding");
        let justify_x_scale = ctx.scale_for("justify_x");
        let justify_y_scale = ctx.scale_for("justify_y");
        let angle_scale = ctx.scale_for("angle");
        let pick_id_scale = ctx.scale_for("pick_id");

        let channels = &self.state.channels;
        let (x_col, x_scale) = match channels.get("x") {
            Some(Channel::Data(c)) => (c, x_scale_bound),
            Some(Channel::RawData(c)) => (c, None),
            _ => return,
        };
        let (y_col, y_scale) = match channels.get("y") {
            Some(Channel::Data(c)) => (c, y_scale_bound),
            Some(Channel::RawData(c)) => (c, None),
            _ => return,
        };
        let (x2_col, x2_scale) = match channels.get("x2") {
            Some(Channel::Data(c)) => (c, x2_scale_bound),
            Some(Channel::RawData(c)) => (c, None),
            _ => return,
        };
        let (y2_col, y2_scale) = match channels.get("y2") {
            Some(Channel::Data(c)) => (c, y2_scale_bound),
            Some(Channel::RawData(c)) => (c, None),
            _ => return,
        };

        let text_ch = channels.get("text");
        let family_ch = channels.get("family");
        let weight_ch = channels.get("weight");
        let italic_ch = channels.get("italic");
        let tracking_ch = channels.get("tracking");
        let markdown_ch = channels.get("markdown");
        // Kept as an `Arc` so the shape cache can key on its identity.
        let rich_sheet: &Arc<RichTextStyleSheet> =
            self.rich_sheet.as_ref().unwrap_or(&ctx.theme.rich_text);
        let underline_ch = channels.get("underline");
        let strikethrough_ch = channels.get("strikethrough");
        let text_stroke_ch = channels.get("text_stroke");
        let text_linewidth_ch = channels.get("text_linewidth");
        let min_font_ch = channels.get("min_font_size");
        let max_font_ch = channels.get("max_font_size");
        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x2_offset_ch = channels.get("x2_offset");
        let y2_offset_ch = channels.get("y2_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let x2_band_ch = channels.get("x2_band");
        let y2_band_ch = channels.get("y2_band");
        let fill_ch = channels.get("fill");
        let fill_opacity_ch = channels.get("fill_opacity");
        let bg_fill_ch = channels.get("bg_fill");
        let bg_fill_opacity_ch = channels.get("bg_fill_opacity");
        let bg_stroke_ch = channels.get("bg_stroke");
        let bg_stroke_opacity_ch = channels.get("bg_stroke_opacity");
        let bg_linewidth_ch = channels.get("bg_linewidth");
        let bg_corner_radius_ch = channels.get("bg_corner_radius");
        let bg_padding_ch = channels.get("bg_padding");
        let justify_x_ch = channels.get("justify_x");
        let justify_y_ch = channels.get("justify_y");
        let angle_ch = channels.get("angle");
        let pick_id_ch = channels.get("pick_id");

        for i in 0..n {
            // ── Resolve text. ──
            let text = match resolve_str_channel(text_ch, text_scale, i) {
                Some(s) if !s.is_empty() => s,
                _ => continue,
            };

            // ── Resolve target rect corners (band + offset). ──
            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
            let x2_band = resolve_number_channel_or(x2_band_ch, x2_band_scale, i, 0.0);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
            let y2_band = resolve_number_channel_or(y2_band_ch, y2_band_scale, i, 0.0);
            let x_frac = resolve_position(x_col.get(i), x_scale, x_band);
            let x2_frac = resolve_position(x2_col.get(i), x2_scale, x2_band);
            let y_frac = resolve_position(y_col.get(i), y_scale, y_band);
            let y2_frac = resolve_position(y2_col.get(i), y2_scale, y2_band);
            if !x_frac.is_finite()
                || !x2_frac.is_finite()
                || !y_frac.is_finite()
                || !y2_frac.is_finite()
            {
                continue;
            }

            let (px0, py0) = ctx.projection.project_to_panel_px(panel, &[x_frac, y_frac]);
            let (px20, py20) = ctx
                .projection
                .project_to_panel_px(panel, &[x2_frac, y2_frac]);
            let mut px = px0;
            let mut px2 = px20;
            let mut py = py0;
            let mut py2 = py20;
            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                px += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(x2_offset_ch, x2_offset_scale, i) {
                px2 += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                py -= pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y2_offset_ch, y2_offset_scale, i) {
                py2 -= pt_to_px(off, ctx.dpi);
            }
            let rx0 = px.min(px2);
            let rx1 = px.max(px2);
            let ry0 = py.min(py2);
            let ry1 = py.max(py2);
            let rect = Rect::new(rx0, ry0, rx1, ry1);
            if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            let rect_w = rect.width();
            let rect_h = rect.height();

            // ── Font style (size will be computed by the fit). ──
            let weight = resolve_number_channel(weight_ch, weight_scale, i)
                .map(|w| (w.round() as i64).clamp(1, 1000) as u16)
                .unwrap_or(ctx.theme.geom.text_fit.weight);
            let italic = resolve_bool_or_italic_string(italic_ch, italic_scale, i);
            let family = resolve_str_channel(family_ch, family_scale, i);
            let tracking = resolve_number_channel_or(
                tracking_ch,
                tracking_scale,
                i,
                ctx.theme.geom.text_fit.tracking,
            ) as f32;
            let underline = resolve_bool_channel_or(
                underline_ch,
                underline_scale,
                i,
                ctx.theme.geom.text_fit.underline,
            );
            let strikethrough = resolve_bool_channel_or(
                strikethrough_ch,
                strikethrough_scale,
                i,
                ctx.theme.geom.text_fit.strikethrough,
            );

            let min_pt = resolve_number_channel_or(
                min_font_ch,
                min_font_scale,
                i,
                ctx.theme.geom.text_fit.min_font_pt,
            )
            .max(0.5);
            let max_pt = resolve_number_channel_or(
                max_font_ch,
                max_font_scale,
                i,
                ctx.theme.geom.text_fit.max_font_pt,
            )
            .max(min_pt);
            let min_pt_f32 = min_pt as f32;
            let max_pt_f32 = max_pt as f32;

            // ── Justification — locked before the fit; affects line
            // alignment inside the wrap box at every search iteration.
            let justify_x = resolve_justify_x(justify_x_ch, justify_x_scale, i);
            let justify_y_frac = resolve_justify_y_frac(justify_y_ch, justify_y_scale, i);

            // ── Markdown, the outline sheet, and the fill colour are
            // all needed before shaping: the rich path bakes the fill
            // in as its base brush and takes its outline from the
            // sheet, both of which the shape cache keys on. ──
            let markdown = resolve_bool_channel_or(markdown_ch, markdown_scale, i, false);
            let fill_color = override_alpha(
                resolve_color_channel_or_theme(
                    fill_ch,
                    fill_scale,
                    i,
                    ctx.theme.geom.text_fit.fill.as_ref(),
                    &ctx.theme.palette,
                ),
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i),
            )
            .unwrap_or_else(default_fill);
            let text_stroke_color = resolve_color_channel_or_theme(
                text_stroke_ch,
                text_stroke_scale,
                i,
                ctx.theme.geom.text_fit.text_stroke.as_ref(),
                &ctx.theme.palette,
            );
            let text_linewidth_pt = resolve_number_channel_or(
                text_linewidth_ch,
                text_linewidth_scale,
                i,
                ctx.theme.geom.text_fit.text_linewidth_pt,
            );
            let row_sheet = if markdown {
                self.rich_outline_sheets
                    .resolve(rich_sheet, text_stroke_color, text_linewidth_pt)
            } else {
                Arc::clone(rich_sheet)
            };

            // ── Binary-search the font size. ──
            let make_style = |size_pt: f32| {
                let mut s = TextStyle::new(size_pt)
                    .weight(weight)
                    .italic(italic)
                    .tracking(tracking)
                    .underline(underline)
                    .strikethrough(strikethrough);
                if let Some(f) = &family {
                    s = s.family(f);
                }
                s
            };

            let measure = |size_pt: f32| {
                let style = make_style(size_pt);
                let fitted = if markdown {
                    // Every probe is its own cache entry, so a redraw
                    // at an unchanged rect walks the search on cache
                    // hits alone.
                    let key = RichKey::new(
                        &text,
                        &style,
                        fill_color,
                        &row_sheet,
                        &ctx.theme.palette,
                        ctx.dpi,
                        RichTextWidth::Fixed(rect_w as f32),
                        justify_x,
                        ctx.images,
                    );
                    let run = self.rich_cache.get_or_shape(key, || {
                        RichTextRun::new_with_images(
                            &text,
                            &style,
                            fill_color,
                            &row_sheet,
                            &ctx.theme.palette,
                            ctx.dpi,
                            ctx.images,
                        )
                    });
                    run.set_max_width(rect_w as f32, justify_x);
                    Fitted::Rich(run)
                } else {
                    let run = TextRun::new(&text, &style, ctx.dpi);
                    run.set_max_width(rect_w as f32, justify_x);
                    Fitted::Plain(Box::new(run))
                };
                let w = fitted.content_width();
                let h = fitted.height();
                (fitted, w, h)
            };

            // The maximum is tried first. The midpoint of a half-open
            // search never reaches its upper bound, so this is what makes
            // `max_font_size` itself attainable — and text that already
            // fits at full size skips the search entirely.
            let (run_at_max, w_at_max, h_at_max) = measure(max_pt_f32);
            let mut best: Option<(Fitted, f64, f64, f32)> =
                if w_at_max <= rect_w && h_at_max <= rect_h {
                    Some((run_at_max, w_at_max, h_at_max, max_pt_f32))
                } else {
                    None
                };
            if best.is_none() {
                let mut lo = min_pt_f32;
                let mut hi = max_pt_f32;
                for _ in 0..MAX_ITERS {
                    let mid = 0.5 * (lo + hi);
                    let (run, w, h) = measure(mid);
                    if w <= rect_w && h <= rect_h {
                        lo = mid;
                        best = Some((run, w, h, mid));
                    } else {
                        hi = mid;
                    }
                }
            }

            // If no candidate fit, draw at min and clip to the rect.
            let (run, content_w, content_h, _size_pt, fits) = match best {
                Some((r, w, h, s)) => (r, w, h, s, true),
                None => {
                    let (run, w, h) = measure(min_pt_f32);
                    (run, w, h, min_pt_f32, false)
                }
            };

            // ── Position the text block within the rect. ──
            // Horizontal: parley applies justify_x at wrap_width =
            // rect_w, so each line is positioned within the rect width
            // — there's no extra horizontal offset to apply. The block's
            // left edge is rect.x0.
            //
            // Vertical: justify_y picks where the block sits within
            // the rect's vertical slack. Slack = rect_h - content_h.
            let draw_x = rect.x0;
            let vslack = (rect_h - content_h).max(0.0);
            let draw_y = rect.y0 + justify_y_frac * vslack;

            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i);

            // ── Background presence — hugs the fitted text block,
            // not the target rect. (The user supplies the target rect
            // explicitly; the bg is a separate "make the text
            // readable" surface.) ──
            let bg_fill = override_alpha(
                resolve_color_channel(bg_fill_ch, bg_fill_scale, i),
                resolve_number_channel(bg_fill_opacity_ch, bg_fill_opacity_scale, i),
            );
            let bg_stroke = override_alpha(
                resolve_color_channel(bg_stroke_ch, bg_stroke_scale, i),
                resolve_number_channel(bg_stroke_opacity_ch, bg_stroke_opacity_scale, i),
            );

            // ── Rotation pivot: the target rect's centre. ──
            let angle = resolve_angle_channel(angle_ch, angle_scale, i);
            let xform = if angle == 0.0 {
                Affine::IDENTITY
            } else {
                let cx = 0.5 * (rect.x0 + rect.x1);
                let cy = 0.5 * (rect.y0 + rect.y1);
                Affine::rotate_about(-angle, Point::new(cx, cy))
            };

            // ── Clip on overflow. ──
            // If even min_font_size doesn't fit, push a clip rect at
            // the target rect so the laid-out text doesn't bleed out.
            let need_clip = !fits;
            if need_clip {
                let clip_path = rect_path(rect);
                scene.push_layer(crate::blend::BlendMode::NORMAL, 1.0, xform, &clip_path);
            }

            // ── Background rect (drawn before glyphs). ──
            if bg_fill.is_some() || bg_stroke.is_some() {
                let padding_pt = resolve_number_channel_or(bg_padding_ch, bg_padding_scale, i, 0.0);
                let padding_px = pt_to_px(padding_pt, ctx.dpi);
                let descender_px = run.last_line_descender();
                let top_pad_eff = padding_px.max(descender_px);
                let bottom_pad_eff = (padding_px - descender_px).max(0.0);
                let bg_w = content_w + 2.0 * padding_px;
                let bg_h = content_h + top_pad_eff + bottom_pad_eff;
                let bg_left = draw_x - padding_px;
                let bg_top = draw_y - top_pad_eff;
                let bg_rect = Rect::new(bg_left, bg_top, bg_left + bg_w, bg_top + bg_h);
                if bg_rect.is_finite() && bg_rect.width() > 0.0 && bg_rect.height() > 0.0 {
                    let bg_corner_radius_pt = resolve_number_channel_or(
                        bg_corner_radius_ch,
                        bg_corner_radius_scale,
                        i,
                        0.0,
                    );
                    let bg_corner_radius_px = pt_to_px(bg_corner_radius_pt, ctx.dpi).max(0.0);
                    let bg_path = if bg_corner_radius_px > 0.0 {
                        rounded_rect(bg_rect, bg_corner_radius_px)
                    } else {
                        rect_path(bg_rect)
                    };
                    let bg_xform = if need_clip { Affine::IDENTITY } else { xform };
                    if let Some(fc) = bg_fill {
                        scene.fill(
                            FillRule::NonZero,
                            bg_xform,
                            &Brush::Solid(fc),
                            None,
                            &bg_path,
                            pick,
                        );
                    }
                    if let Some(sc) = bg_stroke {
                        let lw_pt = resolve_number_channel_or(
                            bg_linewidth_ch,
                            bg_linewidth_scale,
                            i,
                            ctx.theme.geom.text_fit.bg_linewidth_pt,
                        );
                        let lw_px = pt_to_px(lw_pt, ctx.dpi);
                        if lw_px.is_finite() && lw_px > 0.0 {
                            let stroke_spec = Stroke::new(lw_px)
                                .with_caps(Cap::Butt)
                                .with_join(Join::Miter);
                            scene.stroke(
                                &stroke_spec,
                                bg_xform,
                                &Brush::Solid(sc),
                                None,
                                &bg_path,
                                pick,
                            );
                        }
                    }
                }
            }

            // ── Emit glyphs. ──
            // When clipping is active the rotation xform was pushed
            // into the clip layer; the glyphs draw with Identity. When
            // not clipping the rotation goes on the glyph run itself.
            let glyph_xform = if need_clip { Affine::IDENTITY } else { xform };

            // ── Outline pass (under the fill). ──
            if let Some(stroke_color) = text_stroke_color {
                let stroke_width_px = pt_to_px(text_linewidth_pt, ctx.dpi);
                if stroke_width_px > 0.0 {
                    let stroke = Stroke::new(stroke_width_px);
                    run.draw_outline(
                        scene,
                        draw_x,
                        draw_y,
                        &Brush::Solid(stroke_color),
                        &stroke,
                        glyph_xform,
                    );
                }
            }

            run.draw(
                scene,
                draw_x,
                draw_y,
                &Brush::Solid(fill_color),
                glyph_xform,
                pick,
            );

            if need_clip {
                scene.pop_layer();
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn resolve_str_channel(
    channel: Option<&Channel>,
    scale: Option<&crate::plot::scale::Scale>,
    i: usize,
) -> Option<String> {
    let (raw, bypass) = match channel? {
        Channel::Constant(v) => (v.clone(), false),
        Channel::Data(col) => (col.get(i), false),
        Channel::RawConstant(v) => (v.clone(), true),
        Channel::RawData(col) => (col.get(i), true),
    };
    let mapped = match (bypass, scale) {
        (true, _) | (false, None) => raw,
        (false, Some(s)) => s.map(&raw),
    };
    mapped.as_str().map(str::to_owned)
}

fn resolve_bool_or_italic_string(
    channel: Option<&Channel>,
    scale: Option<&crate::plot::scale::Scale>,
    i: usize,
) -> bool {
    let (raw, bypass) = match channel {
        None => return false,
        Some(Channel::Constant(v)) => (v.clone(), false),
        Some(Channel::Data(col)) => (col.get(i), false),
        Some(Channel::RawConstant(v)) => (v.clone(), true),
        Some(Channel::RawData(col)) => (col.get(i), true),
    };
    let mapped = match (bypass, scale) {
        (true, _) | (false, None) => raw,
        (false, Some(s)) => s.map(&raw),
    };
    match mapped {
        Value::Bool(b) => b,
        Value::String(s) => matches!(&*s, "italic" | "oblique"),
        _ => false,
    }
}

fn resolve_justify_x(
    channel: Option<&Channel>,
    scale: Option<&crate::plot::scale::Scale>,
    i: usize,
) -> HAlign {
    let s = match resolve_str_channel(channel, scale, i) {
        Some(s) => s,
        None => return HAlign::Start,
    };
    match s.as_str() {
        "start" => HAlign::Start,
        "center" | "centre" | "middle" => HAlign::Center,
        "end" => HAlign::End,
        "justify" | "justified" => HAlign::Justify,
        _ => HAlign::Start,
    }
}

/// Vertical placement fraction in `[0, 1]` — `start` → 0 (text at top
/// of rect), `center` → 0.5, `end` → 1 (text at bottom).
fn resolve_justify_y_frac(
    channel: Option<&Channel>,
    scale: Option<&crate::plot::scale::Scale>,
    i: usize,
) -> f64 {
    let s = match resolve_str_channel(channel, scale, i) {
        Some(s) => s,
        None => return 0.0,
    };
    match s.as_str() {
        "start" => 0.0,
        "center" | "centre" | "middle" => 0.5,
        "end" => 1.0,
        _ => 0.0,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::plot::geom::DirectScaleResolver;
    use crate::scene::recording::{Op, RecordingScene};

    fn shapes() -> crate::shape::ShapeRegistry {
        crate::shape::ShapeRegistry::with_builtins()
    }

    fn ctx<'a>(
        panel: Rect,
        registry: &'a crate::shape::ShapeRegistry,
        scales: &'a DirectScaleResolver<'a>,
    ) -> GeomContext<'a> {
        GeomContext::new(panel, 96.0, registry, scales)
    }

    #[test]
    fn build_requires_text() {
        let r = std::panic::catch_unwind(|| {
            TextFitGeom::builder()
                .set("x", vec![0.0_f64])
                .set("y", vec![0.0_f64])
                .set("x2", vec![1.0_f64])
                .set("y2", vec![1.0_f64])
                .build()
        });
        assert!(r.is_err());
    }

    #[test]
    fn fit_into_wide_rect_emits_glyphs() {
        let g = TextFitGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.3_f64])
            .set("x2", vec![0.9_f64])
            .set("y2", vec![0.7_f64])
            .set("text", vec!["abc"])
            .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 600.0, 200.0);
        let shapes = shapes();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let glyph_ops = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawGlyphs(_)))
            .count();
        assert!(glyph_ops >= 1, "expected at least one glyph op");
    }

    #[test]
    fn min_font_overflow_pushes_clip_layer() {
        // Tiny rect (~4 px wide) + min_font_size 8 → even at min the
        // text doesn't fit → clip path pushed.
        let g = TextFitGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.51_f64])
            .set("y2", vec![0.55_f64])
            .set("text", vec!["overflowing text"])
            .set("min_font_size", 8.0_f64)
            .set("max_font_size", 9.0_f64)
            .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 400.0, 200.0);
        let shapes = shapes();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let push_layers = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::PushLayer { .. }))
            .count();
        let pop_layers = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::PopLayer))
            .count();
        assert!(push_layers >= 1, "expected push_layer for clip");
        assert_eq!(push_layers, pop_layers, "push/pop must balance");
    }

    #[test]
    fn justify_y_end_shifts_text_to_bottom() {
        // A tall rect; "abc" (a short single line) sits at the top by
        // default (justify_y = "start"). With justify_y = "end" it
        // should sit at the bottom — the glyph y is larger.
        let mk = |justify: &'static str| {
            TextFitGeom::builder()
                .set("x", vec![0.2_f64])
                .set("y", vec![0.05_f64])
                .set("x2", vec![0.8_f64])
                .set("y2", vec![0.95_f64])
                .set("text", vec!["abc"])
                .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
                .set("justify_y", justify)
                .set("max_font_size", 14.0_f64)
                .build()
        };
        let panel = Rect::new(0.0, 0.0, 400.0, 300.0);
        let shapes = shapes();
        let resolver = DirectScaleResolver::new();

        let mut s_start = RecordingScene::default();
        mk("start").draw(&mut s_start, &ctx(panel, &shapes, &resolver));
        let mut s_end = RecordingScene::default();
        mk("end").draw(&mut s_end, &ctx(panel, &shapes, &resolver));

        let first_glyph_y = |scene: &RecordingScene| {
            scene.ops.iter().find_map(|op| match op {
                Op::DrawGlyphs(gr) => gr.glyphs.first().map(|g| g.y),
                _ => None,
            })
        };
        let y_start = first_glyph_y(&s_start).expect("start case glyph");
        let y_end = first_glyph_y(&s_end).expect("end case glyph");
        assert!(
            y_end > y_start,
            "justify_y=end should place glyphs lower (larger y in screen): start={} end={}",
            y_start,
            y_end
        );
    }

    // ── Font-size search ──

    /// Font size in px of the first glyph run a draw emitted.
    fn first_glyph_font_size(scene: &RecordingScene) -> Option<f32> {
        scene.ops.iter().find_map(|op| match op {
            Op::DrawGlyphs(gr) => Some(gr.font_size),
            _ => None,
        })
    }

    #[test]
    fn text_that_already_fits_draws_at_the_maximum_font_size() {
        // The search tries the maximum before bisecting, so the upper
        // bound is attainable rather than merely approached.
        let g = TextFitGeom::builder()
            .set("x", vec![0.05_f64])
            .set("y", vec![0.05_f64])
            .set("x2", vec![0.95_f64])
            .set("y2", vec![0.95_f64])
            .set("text", vec!["ab"])
            .set("min_font_size", 6.0_f64)
            .set("max_font_size", 12.0_f64)
            .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 600.0, 400.0);
        let shapes = shapes();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        // 12pt at 96 dpi = 16px.
        let size = first_glyph_font_size(&scene).expect("expected glyphs");
        assert!((size - 16.0).abs() < 1e-3, "{size}");
    }

    #[test]
    fn text_that_never_fits_draws_at_the_minimum_font_size() {
        // A rect too small for even `min_font_size`: the search finds no
        // candidate and the geom falls back to the minimum.
        let g = TextFitGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.51_f64])
            .set("y2", vec![0.55_f64])
            .set("text", vec!["overflowing text"])
            .set("min_font_size", 9.0_f64)
            .set("max_font_size", 40.0_f64)
            .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 400.0, 200.0);
        let shapes = shapes();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        // 9pt at 96 dpi = 12px.
        let size = first_glyph_font_size(&scene).expect("expected glyphs");
        assert!((size - 12.0).abs() < 1e-3, "{size}");
    }

    #[test]
    fn the_search_lands_between_the_two_bounds_when_only_part_of_the_range_fits() {
        // A rect that takes the text at some sizes but not at the
        // maximum: the result is smaller than the bound it could not
        // reach and no smaller than the floor it never needed.
        let g = TextFitGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.4_f64])
            .set("x2", vec![0.9_f64])
            .set("y2", vec![0.6_f64])
            .set("text", vec!["a fitted label"])
            .set("min_font_size", 6.0_f64)
            .set("max_font_size", 200.0_f64)
            .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 400.0, 200.0);
        let shapes = shapes();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let size = first_glyph_font_size(&scene).expect("expected glyphs");
        let px = |pt: f64| (pt * 96.0 / 72.0) as f32;
        assert!(size > px(6.0) && size < px(200.0), "{size}");
        // Fitting text pushes no clip layer.
        assert!(!scene
            .ops
            .iter()
            .any(|op| matches!(op, Op::PushLayer { .. })));
    }

    #[test]
    fn justify_y_center_places_the_block_midway_through_the_slack() {
        // `justify_y` distributes the vertical slack, so "center" lands
        // exactly halfway between the "start" and "end" placements.
        let mk = |justify: &'static str| {
            TextFitGeom::builder()
                .set("x", vec![0.2_f64])
                .set("y", vec![0.05_f64])
                .set("x2", vec![0.8_f64])
                .set("y2", vec![0.95_f64])
                .set("text", vec!["abc"])
                .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
                .set("justify_y", justify)
                .set("max_font_size", 14.0_f64)
                .build()
        };
        let panel = Rect::new(0.0, 0.0, 400.0, 300.0);
        let shapes = shapes();
        let resolver = DirectScaleResolver::new();
        let glyph_y = |justify: &'static str| {
            let mut scene = RecordingScene::default();
            mk(justify).draw(&mut scene, &ctx(panel, &shapes, &resolver));
            scene
                .ops
                .iter()
                .find_map(|op| match op {
                    Op::DrawGlyphs(gr) => gr.glyphs.first().map(|g| g.y),
                    _ => None,
                })
                .expect("expected glyphs")
        };
        let (start, center, end) = (glyph_y("start"), glyph_y("center"), glyph_y("end"));
        assert!(end > start, "start={start} end={end}");
        let expected = 0.5 * (start + end);
        assert!(
            (center - expected).abs() < 1e-3,
            "center={center} expected={expected}"
        );
    }

    // ── Justification vocabulary ──

    #[test]
    fn justify_y_frac_covers_the_alignment_vocabulary() {
        let frac = |s: &'static str| {
            let ch = Channel::Constant(Value::from(s));
            resolve_justify_y_frac(Some(&ch), None, 0)
        };
        assert_eq!(frac("start"), 0.0);
        assert_eq!(frac("center"), 0.5);
        assert_eq!(frac("centre"), 0.5);
        assert_eq!(frac("middle"), 0.5);
        assert_eq!(frac("end"), 1.0);
        // Unrecognised names and an unset channel both sit at the top.
        assert_eq!(frac("sideways"), 0.0);
        assert_eq!(resolve_justify_y_frac(None, None, 0), 0.0);
    }

    #[test]
    fn justify_x_covers_the_alignment_vocabulary() {
        let align = |s: &'static str| {
            let ch = Channel::Constant(Value::from(s));
            resolve_justify_x(Some(&ch), None, 0)
        };
        assert!(matches!(align("start"), HAlign::Start));
        assert!(matches!(align("center"), HAlign::Center));
        assert!(matches!(align("middle"), HAlign::Center));
        assert!(matches!(align("end"), HAlign::End));
        assert!(matches!(align("justify"), HAlign::Justify));
        assert!(matches!(align("justified"), HAlign::Justify));
        assert!(matches!(align("sideways"), HAlign::Start));
        assert!(matches!(resolve_justify_x(None, None, 0), HAlign::Start));
    }

    // ── Markdown labels ──

    /// Draw one row into a 600×200 panel and hand back the scene.
    fn drained(g: &TextFitGeom) -> RecordingScene {
        let panel = Rect::new(0.0, 0.0, 600.0, 200.0);
        let shapes = shapes();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        scene
    }

    /// One row filling most of the panel, with `markdown` on or off.
    fn fit_geom(text: &'static str, markdown: bool) -> TextFitGeom {
        TextFitGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.3_f64])
            .set("x2", vec![0.9_f64])
            .set("y2", vec![0.7_f64])
            .set("text", vec![text])
            .set("markdown", markdown)
            .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build()
    }

    fn glyph_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(run) => Some(run.glyphs.len()),
                _ => None,
            })
            .sum()
    }

    #[test]
    fn markdown_consumes_the_inline_markup() {
        // `**AB**` is six glyphs read literally, two as markdown.
        assert_eq!(glyph_count(&drained(&fit_geom("**AB**", false))), 6);
        assert_eq!(glyph_count(&drained(&fit_geom("**AB**", true))), 2);
    }

    #[test]
    fn a_markdown_span_paints_its_own_background() {
        // Block and span chrome work in a rect — the thing a curve
        // cannot have. The `code` selector carries a background, so a
        // code span fills one behind its glyphs.
        let plain = drained(&fit_geom("a `b` c", false));
        let rich = drained(&fit_geom("a `b` c", true));
        let fills = |scene: &RecordingScene| {
            scene
                .ops
                .iter()
                .filter(|op| matches!(op, Op::Fill { .. }))
                .count()
        };
        assert_eq!(fills(&plain), 0, "no background without markdown");
        assert!(
            fills(&rich) >= 1,
            "the code span should paint a background rect"
        );
    }

    #[test]
    fn markdown_fits_the_rect_by_shrinking() {
        // A long markdown label in the same rect has to come out at a
        // smaller size than a short one, which shows the search is
        // measuring the rich layout rather than ignoring it.
        let font_size = |text: &'static str| {
            let scene = drained(&fit_geom(text, true));
            scene
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::DrawGlyphs(run) => Some(run.font_size),
                    _ => None,
                })
                .fold(0.0_f32, f32::max)
        };
        let short = font_size("**hi**");
        let long = font_size(
            "**a** much longer label that has to shrink a good deal to fit inside the same rect",
        );
        assert!(short > 0.0 && long > 0.0, "{short} / {long}");
        assert!(
            long < short,
            "the longer label should fit at a smaller size: {long} vs {short}"
        );
    }

    #[test]
    fn markdown_that_never_fits_is_clipped() {
        let g = TextFitGeom::builder()
            .set("x", vec![0.45_f64])
            .set("y", vec![0.45_f64])
            .set("x2", vec![0.55_f64])
            .set("y2", vec![0.55_f64])
            .set("text", vec!["**far** too much text for this tiny rect"])
            .set("markdown", true)
            .set("min_font_size", vec![20.0_f64])
            .build();
        let scene = drained(&g);
        let push_layers = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::PushLayer { .. }))
            .count();
        assert!(push_layers >= 1, "expected a clip layer for the overflow");
    }

    #[test]
    fn justify_y_end_shifts_markdown_to_the_bottom() {
        let baseline_of = |justify_y: &'static str| {
            let g = TextFitGeom::builder()
                .set("x", vec![0.1_f64])
                .set("y", vec![0.1_f64])
                .set("x2", vec![0.9_f64])
                .set("y2", vec![0.9_f64])
                .set("text", vec!["*hi*"])
                .set("markdown", true)
                .set("max_font_size", vec![12.0_f64])
                .set("justify_y", vec![justify_y])
                .build();
            let scene = drained(&g);
            // The rich path carries its position in the run transform
            // rather than in glyph coordinates, so the screen baseline
            // is the sum of the two.
            scene
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::DrawGlyphs(run) => {
                        Some(run.transform.as_coeffs()[5] + run.glyphs[0].y as f64)
                    }
                    _ => None,
                })
                .next()
                .expect("a glyph")
        };
        let top = baseline_of("start");
        let bottom = baseline_of("end");
        assert!(
            bottom > top + 10.0,
            "justify_y = end should push the block down: {top} → {bottom}"
        );
    }

    #[test]
    fn a_rotated_markdown_label_lands_where_the_plain_one_does() {
        // Rotation pivots on the target rect's centre for both paths,
        // and a plain string shapes the same either way, so the glyphs
        // have to end up in the same place.
        let extent = |markdown: bool| {
            let g = TextFitGeom::builder()
                .set("x", vec![0.1_f64])
                .set("y", vec![0.3_f64])
                .set("x2", vec![0.9_f64])
                .set("y2", vec![0.7_f64])
                .set("text", vec!["abc"])
                .set("markdown", markdown)
                .set("max_font_size", vec![24.0_f64])
                .set("angle", vec![std::f64::consts::FRAC_PI_2])
                .build();
            let scene = drained(&g);
            let mut points: Vec<(f64, f64)> = Vec::new();
            for op in &scene.ops {
                if let Op::DrawGlyphs(run) = op {
                    let c = run.transform.as_coeffs();
                    for glyph in &run.glyphs {
                        let (gx, gy) = (glyph.x as f64, glyph.y as f64);
                        points.push((c[0] * gx + c[2] * gy + c[4], c[1] * gx + c[3] * gy + c[5]));
                    }
                }
            }
            assert!(!points.is_empty(), "expected glyphs");
            let x0 = points.iter().map(|p| p.0).fold(f64::MAX, f64::min);
            let y0 = points.iter().map(|p| p.1).fold(f64::MAX, f64::min);
            (x0, y0)
        };
        let (px, py) = extent(false);
        let (rx, ry) = extent(true);
        assert!(
            (px - rx).abs() < 2.0 && (py - ry).abs() < 2.0,
            "rotated markdown should land with the plain text: ({px}, {py}) vs ({rx}, {ry})"
        );
    }

    #[test]
    fn the_shape_cache_survives_a_redraw_and_clears_with_the_data() {
        let mut g = fit_geom("**AB** *cd*", true);
        assert!(g.rich_cache.is_empty());
        let first = drained(&g);
        let cached = g.rich_cache.len();
        assert!(cached > 0, "the fit should have cached its probes");
        // A second draw reuses the entries rather than adding more.
        let second = drained(&g);
        assert_eq!(g.rich_cache.len(), cached, "a redraw should not re-shape");
        assert_eq!(first.ops.len(), second.ops.len());
        g.invalidate_caches();
        assert!(g.rich_cache.is_empty(), "new data drops the shaped runs");
    }
}
