//! `TextGeom` — vectorised text labels drawn at scaled `(x, y)` anchors.
//!
//! One label per row (PointGeom-style: row == mark). Each row carries
//! its own string + font properties + colour, shaped per-draw via the
//! parley-backed `crate::text` module.
//!
//! Channels consumed:
//!
//! - `"x"`, `"y"` — anchor position (required; data; numeric). Standard
//!   `x_offset` / `y_offset` / `x_band` / `y_band` companions apply.
//! - `"text"` — the label string (required; data; strings).
//! - `"size"` — font size in **pt** (optional; default 12pt). Converted
//!   to px at draw via `dpi / 72`.
//! - `"weight"` — CSS font weight 100..=900 (optional; default 400).
//!   Common values: 400 (normal), 700 (bold). Non-integer values round
//!   to the nearest 100.
//! - `"italic"` — boolean (optional; default false). Channels can bind
//!   a Boolean DataColumn or use scaled string outputs like
//!   `"italic"` / `"normal"`.
//! - `"family"` — font family name (optional; default system sans-serif).
//! - `"anchor_x"` — horizontal anchor as a fraction of the label's
//!   width in `[0, 1]` (optional; default 0.5). `0` = anchor at left
//!   edge, `0.5` = centred, `1` = anchor at right edge.
//! - `"anchor_y"` — vertical anchor as a fraction of the label's
//!   height in `[0, 1]` (optional; default 0.5). `0` = anchor at top
//!   edge, `1` = anchor at bottom edge. Note: the fraction is in
//!   pixel-y direction; `anchor_y = 0` puts the top of the text at the
//!   anchor (label extends downward), matching the SVG / CSS
//!   convention.
//! - `"fill"` — text colour (optional; default black).
//! - `"fill_opacity"` — overrides the alpha component of `"fill"`
//!   (optional; expects `0..=1`).
//! - `"width"` — soft-wrap width in pt (optional; default 0 = no wrap).
//!   When positive, the text is laid out with this as the maximum line
//!   width and lines break at word boundaries.
//! - `"width_band"` — soft-wrap width as a fraction of the x scale's
//!   band width at the row's x value (optional; default 0). For text
//!   inside a categorical cell: bind x to a discrete scale and set
//!   `width_band = 1.0` to wrap at the band's full width. `width` and
//!   `width_band` sum in pixel space, so `width = 4, width_band = 1.0`
//!   gives "fill the band, minus 4pt padding on each side" when used
//!   with negative pt values (or just add positive pt to extend
//!   beyond the band).
//! - `"bg_fill"` — background-rect fill colour (optional; unset means
//!   no background rect). Resolved at the geom's first row of the
//!   mark; rect dimensions come from the laid-out text plus padding.
//!   Drawn *before* the glyphs so it sits behind the text.
//! - `"bg_fill_opacity"` — overrides alpha of `"bg_fill"`.
//! - `"bg_stroke"`, `"bg_stroke_opacity"`, `"bg_linewidth"` — outline
//!   styling for the background rect. Set without `"bg_fill"` for an
//!   unfilled outlined label.
//! - `"bg_corner_radius"` — uniform corner radius in pt (default 0).
//! - `"bg_padding"` — uniform padding in pt between the text and the
//!   background rect edge (default 0).
//!
//! ### Background-rect vertical balance
//!
//! When a background is drawn, vertical padding goes through the
//! ggplot2 `geom_label` rebalance trick: top padding bumps up to at
//! least the font descender, bottom padding shrinks by the same
//! amount. This shifts the descender allocation from below the
//! baseline to above the ascender, so the visible glyphs end up
//! centred inside the rect even when the last line has no descenders
//! (the word "men" sits as well-centred as "jay"). Net total height
//! is unchanged when `bg_padding ≥ descender`, so the trick is
//! invisible at typical padding values and only kicks in for tight
//! / zero-padding badges.
//!
//! Horizontal padding stays symmetric — there's no equivalent
//! left/right asymmetry in font metrics.
//!
//! With a background, the anchor positions the *label* (text + bg).
//! Without a background, the anchor positions the text metric box
//! directly. `anchor_x = 0.5, anchor_y = 0.5` therefore centres
//! whichever the user actually sees.
//!
//! - `"angle"` — rotation in **radians** around the resolved
//!   **alignment** anchor `(anchor_px, anchor_py)`, mathematical CCW
//!   (positive rotates the label counter-clockwise in the rendered
//!   image). Default `0.0`. The alignment anchor (set via
//!   `anchor_x` / `anchor_y` channels) is the rotation pivot — line
//!   justification within the laid-out box does not move the pivot.
//!   Both the laid-out text and any background rect rotate together.
//! - `"justify_x"` — **line justification** within the wrap box.
//!   Strings: `"start"` (default), `"center"`, `"end"`, `"justify"`.
//!   Orthogonal to `anchor_x` / `anchor_y` (which is alignment — where
//!   the box itself sits relative to the placement point). Only has
//!   visible effect when `"width"` causes wrap; a single-line label
//!   has nothing to justify against. Unknown values fall back to
//!   `"start"`.
//!
//! Picking: each row gets its own pick ticket allocated by the
//! orchestrator; every glyph in that row tags itself with the row's id.
//! Hit-testing falls out of the standard rasterised-pick path (alpha
//! coverage in the pick scene).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Rect};
use crate::path::FillRule;
use crate::plot::theme::HAlign;
use crate::plot::value::Value;
use crate::primitives::{rect as rect_path, rounded_rect};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};
use crate::style_vocab::ThemeColor;
use crate::text::rich::{
    draw_rich_text, pt as rich_pt, HAnchor, RichAnchor, RichKey, RichShapeCache, RichTextRun,
    RichTextStyleSheet, RichTextWidth, StyleDelta as RichStyleDelta, VAnchor,
};
use crate::text::{draw_text, TextRun, TextStyle};

use super::resolve::{
    band_width_at, override_alpha, pt_to_px, resolve_angle_channel, resolve_bool_channel_or,
    resolve_color_channel, resolve_color_channel_or_theme, resolve_number_channel,
    resolve_number_channel_or, resolve_pick_id, resolve_position,
};
use super::state::{
    finalize_state, require_data_column, require_x_and_siblings, GeomState, KeysStrategy,
};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────

// Style defaults (size, weight, anchor, bg linewidth) live on
// `theme.geom.text` and are read via `ctx.theme.geom.text.*`.
fn default_fill() -> crate::color::Color {
    crate::color::Color::new([0.0, 0.0, 0.0, 1.0])
}

const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("text", ExpectedOutput::Strings),
    ("size", ExpectedOutput::Numbers),
    ("weight", ExpectedOutput::Numbers),
    ("italic", ExpectedOutput::Any),
    ("family", ExpectedOutput::Strings),
    ("letter_spacing", ExpectedOutput::Numbers),
    ("underline", ExpectedOutput::Any),
    ("strikethrough", ExpectedOutput::Any),
    ("markdown", ExpectedOutput::Any),
    ("text_stroke", ExpectedOutput::Colors),
    ("text_linewidth", ExpectedOutput::Numbers),
    ("anchor_x", ExpectedOutput::Numbers),
    ("anchor_y", ExpectedOutput::Numbers),
    ("fill", ExpectedOutput::Colors),
    ("fill_opacity", ExpectedOutput::Numbers),
    ("width", ExpectedOutput::Numbers),
    ("width_band", ExpectedOutput::Numbers),
    ("bg_fill", ExpectedOutput::Colors),
    ("bg_fill_opacity", ExpectedOutput::Numbers),
    ("bg_stroke", ExpectedOutput::Colors),
    ("bg_stroke_opacity", ExpectedOutput::Numbers),
    ("bg_linewidth", ExpectedOutput::Numbers),
    ("bg_corner_radius", ExpectedOutput::Numbers),
    ("bg_padding", ExpectedOutput::Numbers),
    ("angle", ExpectedOutput::Numbers),
    ("justify_x", ExpectedOutput::Strings),
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── TextGeom ────────────────────────────────────────────────────────────────

/// A vectorised text-label geom. One label per row.
pub struct TextGeom {
    pub(crate) state: GeomState,
    /// Optional per-geom style sheet used when the `"markdown"`
    /// channel resolves `true`. `None` falls back to the theme's
    /// `rich_text` sheet.
    pub(crate) rich_sheet: Option<Arc<RichTextStyleSheet>>,
    /// Shaped markdown rows, reused across frames. Cleared whenever
    /// the geom's data is replaced.
    pub(crate) rich_cache: RichShapeCache,
    /// Sheets derived from the base one by folding a row's
    /// `text_stroke` / `text_linewidth` onto its root selector, keyed
    /// by `(base sheet identity, colour, width)`. Held so the derived
    /// sheet keeps one identity across frames — [`RichShapeCache`]
    /// keys on that identity, and a fresh `Arc` per frame would miss
    /// every time.
    pub(crate) rich_outline_sheets: RefCell<HashMap<(usize, u128, u64), Arc<RichTextStyleSheet>>>,
}

crate::impl_geom_inherents!(TextGeom);

impl TextGeom {
    /// Install a rich-text style sheet used for every row this geom
    /// renders as markdown. Overrides the theme's default sheet.
    /// Chains for builder-style construction.
    pub fn with_rich_sheet(mut self, sheet: Arc<RichTextStyleSheet>) -> Self {
        self.rich_sheet = Some(sheet);
        self
    }

    /// Same as [`Self::with_rich_sheet`] for mutation through
    /// `Plot::update_geom(&mut TextGeom)`.
    pub fn set_rich_sheet(&mut self, sheet: Arc<RichTextStyleSheet>) {
        self.rich_sheet = Some(sheet);
    }

    /// Clear the shaped-markdown cache. The key covers the sheet's
    /// identity, so swapping sheets doesn't require this — it exists
    /// for callers that mutate a sheet in place despite the
    /// immutable-once-installed convention.
    pub fn clear_rich_cache(&mut self) {
        self.rich_cache.clear();
    }

    /// Clear any per-geom rich-text sheet override — falls back to
    /// the theme default.
    pub fn clear_rich_sheet(&mut self) {
        self.rich_sheet = None;
    }
}

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for TextGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();
        let n = require_x_and_siblings(&channels, &["y"], "TextGeom");
        require_data_column("text", &channels, "TextGeom");
        let state = finalize_state(
            keys_opt,
            channels,
            n,
            KeysStrategy::PerRow,
            CHANNELS,
            "TextGeom",
        );
        TextGeom {
            state,
            rich_sheet: None,
            rich_cache: RichShapeCache::new(),
            rich_outline_sheets: RefCell::new(HashMap::new()),
        }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for TextGeom {
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
        Some("text")
    }

    fn invalidate_caches(&mut self) {
        self.rich_cache.clear();
        self.rich_outline_sheets.borrow_mut().clear();
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
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let text_scale = ctx.scale_for("text");
        let size_scale = ctx.scale_for("size");
        let weight_scale = ctx.scale_for("weight");
        let italic_scale = ctx.scale_for("italic");
        let family_scale = ctx.scale_for("family");
        let letter_spacing_scale = ctx.scale_for("letter_spacing");
        let underline_scale = ctx.scale_for("underline");
        let strikethrough_scale = ctx.scale_for("strikethrough");
        let markdown_scale = ctx.scale_for("markdown");
        let text_stroke_scale = ctx.scale_for("text_stroke");
        let text_linewidth_scale = ctx.scale_for("text_linewidth");
        let anchor_x_scale = ctx.scale_for("anchor_x");
        let anchor_y_scale = ctx.scale_for("anchor_y");
        let fill_scale = ctx.scale_for("fill");
        let fill_opacity_scale = ctx.scale_for("fill_opacity");
        let width_scale = ctx.scale_for("width");
        let width_band_scale = ctx.scale_for("width_band");
        let bg_fill_scale = ctx.scale_for("bg_fill");
        let bg_fill_opacity_scale = ctx.scale_for("bg_fill_opacity");
        let bg_stroke_scale = ctx.scale_for("bg_stroke");
        let bg_stroke_opacity_scale = ctx.scale_for("bg_stroke_opacity");
        let bg_linewidth_scale = ctx.scale_for("bg_linewidth");
        let bg_corner_radius_scale = ctx.scale_for("bg_corner_radius");
        let bg_padding_scale = ctx.scale_for("bg_padding");
        let angle_scale = ctx.scale_for("angle");
        let justify_x_scale = ctx.scale_for("justify_x");
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
        let text_ch = channels.get("text");
        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let size_ch = channels.get("size");
        let weight_ch = channels.get("weight");
        let italic_ch = channels.get("italic");
        let family_ch = channels.get("family");
        let letter_spacing_ch = channels.get("letter_spacing");
        let underline_ch = channels.get("underline");
        let strikethrough_ch = channels.get("strikethrough");
        let markdown_ch = channels.get("markdown");
        // Kept as an `Arc` so the shape cache can key on its identity.
        let rich_sheet: &Arc<RichTextStyleSheet> =
            self.rich_sheet.as_ref().unwrap_or(&ctx.theme.rich_text);
        let text_stroke_ch = channels.get("text_stroke");
        let text_linewidth_ch = channels.get("text_linewidth");
        let anchor_x_ch = channels.get("anchor_x");
        let anchor_y_ch = channels.get("anchor_y");
        let fill_ch = channels.get("fill");
        let fill_opacity_ch = channels.get("fill_opacity");
        let width_ch = channels.get("width");
        let width_band_ch = channels.get("width_band");
        let bg_fill_ch = channels.get("bg_fill");
        let bg_fill_opacity_ch = channels.get("bg_fill_opacity");
        let bg_stroke_ch = channels.get("bg_stroke");
        let bg_stroke_opacity_ch = channels.get("bg_stroke_opacity");
        let bg_linewidth_ch = channels.get("bg_linewidth");
        let bg_corner_radius_ch = channels.get("bg_corner_radius");
        let bg_padding_ch = channels.get("bg_padding");
        let angle_ch = channels.get("angle");
        let justify_x_ch = channels.get("justify_x");
        let pick_id_ch = channels.get("pick_id");

        for i in 0..n {
            // ── Resolve text string. ──
            let text = match resolve_str_channel(text_ch, text_scale, i) {
                Some(s) if !s.is_empty() => s,
                _ => continue, // empty / missing text → skip
            };

            // ── Position (anchor in pixel space). ──
            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
            let x_frac = resolve_position(x_col.get(i), x_scale, x_band);
            let y_frac = resolve_position(y_col.get(i), y_scale, y_band);
            if !x_frac.is_finite() || !y_frac.is_finite() {
                continue;
            }
            let (apx0, apy0) = ctx.projection.project_to_panel_px(panel, &[x_frac, y_frac]);
            let mut anchor_px = apx0;
            let mut anchor_py = apy0;
            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                anchor_px += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                anchor_py -= pt_to_px(off, ctx.dpi);
            }

            // ── Resolve font style. ──
            let size_pt =
                resolve_number_channel_or(size_ch, size_scale, i, ctx.theme.geom.text.size_pt);
            if !size_pt.is_finite() || size_pt <= 0.0 {
                continue;
            }
            let weight = resolve_number_channel(weight_ch, weight_scale, i)
                .map(|w| (w.round() as i64).clamp(1, 1000) as u16)
                .unwrap_or(ctx.theme.geom.text.weight);
            let italic = resolve_bool_or_italic_string(italic_ch, italic_scale, i);
            let family = resolve_str_channel(family_ch, family_scale, i);
            let letter_spacing_pt = resolve_number_channel_or(
                letter_spacing_ch,
                letter_spacing_scale,
                i,
                ctx.theme.geom.text.letter_spacing_pt,
            );
            let underline = resolve_bool_channel_or(
                underline_ch,
                underline_scale,
                i,
                ctx.theme.geom.text.underline,
            );
            let strikethrough = resolve_bool_channel_or(
                strikethrough_ch,
                strikethrough_scale,
                i,
                ctx.theme.geom.text.strikethrough,
            );

            // ── Build TextStyle. ──
            let mut style = TextStyle::new(size_pt as f32)
                .weight(weight)
                .italic(italic)
                .letter_spacing_pt(letter_spacing_pt as f32)
                .underline(underline)
                .strikethrough(strikethrough);
            if let Some(fam) = family {
                style = style.family(fam);
            }

            // ── Markdown branch. ──
            //
            // When the row's `markdown` channel resolves `true` (or
            // the theme default is on), shape the row's text as
            // marquee-flavoured markdown via [`RichTextRun`]. The
            // geom's `bg_*` channels compose: they wrap the whole
            // label; markdown's block-level backgrounds paint inside.
            // The row's `text_stroke` / `text_linewidth` fold onto the
            // sheet's root selector so every span inherits the halo.
            let markdown = resolve_bool_channel_or(
                markdown_ch,
                markdown_scale,
                i,
                ctx.theme.geom.text.markdown,
            );
            if markdown {
                // Fill colour (resolved early — RichTextRun needs it
                // as the base brush for plain-styled runs).
                let fill_color = override_alpha(
                    resolve_color_channel_or_theme(
                        fill_ch,
                        fill_scale,
                        i,
                        ctx.theme.geom.text.fill.as_ref(),
                        &ctx.theme.palette,
                    ),
                    resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i),
                )
                .unwrap_or_else(default_fill);
                // The row's outline channels fold onto the sheet's
                // root selector so every span inherits them; a span
                // that sets its own `text_stroke` still wins.
                let row_stroke = resolve_color_channel_or_theme(
                    text_stroke_ch,
                    text_stroke_scale,
                    i,
                    ctx.theme.geom.text.text_stroke.as_ref(),
                    &ctx.theme.palette,
                );
                let row_sheet = match row_stroke {
                    None => Arc::clone(rich_sheet),
                    Some(c) => {
                        let width_pt = resolve_number_channel_or(
                            text_linewidth_ch,
                            text_linewidth_scale,
                            i,
                            ctx.theme.geom.text.text_linewidth_pt,
                        );
                        let [r, g, b, a] = c.components;
                        let color_bits = (r.to_bits() as u128) << 96
                            | (g.to_bits() as u128) << 64
                            | (b.to_bits() as u128) << 32
                            | a.to_bits() as u128;
                        let key = (
                            Arc::as_ptr(rich_sheet) as usize,
                            color_bits,
                            width_pt.to_bits(),
                        );
                        let mut sheets = self.rich_outline_sheets.borrow_mut();
                        Arc::clone(sheets.entry(key).or_insert_with(|| {
                            let mut s = (**rich_sheet).clone();
                            let base = s.get("base").cloned().unwrap_or_default();
                            s.set(
                                "base",
                                RichStyleDelta {
                                    text_stroke: Some(ThemeColor::Fixed(c)),
                                    text_stroke_width: Some(rich_pt(width_pt)),
                                    ..base
                                },
                            );
                            Arc::new(s)
                        }))
                    }
                };
                // Wrap.
                let x_raw = x_col.get(i);
                let x_band_width_px = band_width_at(x_scale, &x_raw) * panel_w;
                let width_pt = resolve_number_channel_or(width_ch, width_scale, i, 0.0);
                let width_band_frac =
                    resolve_number_channel_or(width_band_ch, width_band_scale, i, 0.0);
                let wrap_width_px = pt_to_px(width_pt, ctx.dpi) + width_band_frac * x_band_width_px;
                let justify_x = resolve_justify_channel(justify_x_ch, justify_x_scale, i);
                let wraps = wrap_width_px > 0.0 && wrap_width_px.is_finite();
                let align = justify_x;
                let width_spec = if wraps {
                    RichTextWidth::Fixed(wrap_width_px as f32)
                } else {
                    RichTextWidth::Natural
                };
                let key = RichKey::new(
                    &text,
                    &style,
                    fill_color,
                    &row_sheet,
                    &ctx.theme.palette,
                    ctx.dpi,
                    width_spec,
                    align,
                );
                let rich = self.rich_cache.get_or_shape(key, || {
                    let run = RichTextRun::new(
                        &text,
                        &style,
                        fill_color,
                        &row_sheet,
                        &ctx.theme.palette,
                        ctx.dpi,
                    );
                    if wraps {
                        run.set_max_width(wrap_width_px as f32, align);
                    }
                    run
                });
                let (text_w, text_h) = if wraps {
                    (rich.content_width(), rich.current_height())
                } else {
                    (rich.natural_width(), rich.natural_height())
                };
                let anchor_x = resolve_number_channel_or(
                    anchor_x_ch,
                    anchor_x_scale,
                    i,
                    ctx.theme.geom.text.anchor_x,
                );
                let anchor_y = resolve_number_channel_or(
                    anchor_y_ch,
                    anchor_y_scale,
                    i,
                    ctx.theme.geom.text.anchor_y,
                );
                let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i);
                // Background rect (from `bg_*` channels — wraps the
                // whole rich block).
                let bg_fill = override_alpha(
                    resolve_color_channel(bg_fill_ch, bg_fill_scale, i),
                    resolve_number_channel(bg_fill_opacity_ch, bg_fill_opacity_scale, i),
                );
                let bg_stroke = override_alpha(
                    resolve_color_channel(bg_stroke_ch, bg_stroke_scale, i),
                    resolve_number_channel(bg_stroke_opacity_ch, bg_stroke_opacity_scale, i),
                );
                let has_bg = bg_fill.is_some() || bg_stroke.is_some();
                let (draw_x, draw_y, bg_rect_opt) = if has_bg {
                    let padding_pt =
                        resolve_number_channel_or(bg_padding_ch, bg_padding_scale, i, 0.0);
                    let padding_px = pt_to_px(padding_pt, ctx.dpi);
                    let bg_w = text_w + 2.0 * padding_px;
                    let bg_h = text_h + 2.0 * padding_px;
                    let bg_left = anchor_px - anchor_x * bg_w;
                    let bg_top = anchor_py - anchor_y * bg_h;
                    let dx = bg_left + padding_px;
                    let dy = bg_top + padding_px;
                    let bg_rect = Rect::new(bg_left, bg_top, bg_left + bg_w, bg_top + bg_h);
                    (dx, dy, Some(bg_rect))
                } else {
                    let dx = anchor_px - anchor_x * text_w;
                    let dy = anchor_py - anchor_y * text_h;
                    (dx, dy, None)
                };
                // Rotation around the alignment anchor.
                let angle = resolve_angle_channel(angle_ch, angle_scale, i);
                let xform = if angle == 0.0 {
                    Affine::IDENTITY
                } else {
                    Affine::rotate_about(-angle, Point::new(anchor_px, anchor_py))
                };
                // Draw label bg first, then rich text on top.
                if let Some(bg_rect) = bg_rect_opt {
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
                        if let Some(fc) = bg_fill {
                            scene.fill(
                                FillRule::NonZero,
                                xform,
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
                                ctx.theme.geom.text.bg_linewidth_pt,
                            );
                            let lw_px = pt_to_px(lw_pt, ctx.dpi);
                            if lw_px.is_finite() && lw_px > 0.0 {
                                let stroke_spec = Stroke::new(lw_px)
                                    .with_caps(Cap::Butt)
                                    .with_join(Join::Miter);
                                scene.stroke(
                                    &stroke_spec,
                                    xform,
                                    &Brush::Solid(sc),
                                    None,
                                    &bg_path,
                                    pick,
                                );
                            }
                        }
                    }
                }
                // Emit rich text at (draw_x, draw_y) using top-left
                // anchor — the anchor_x/anchor_y math above already
                // positioned the top-left of the label there.
                draw_rich_text(
                    scene,
                    &rich,
                    draw_x,
                    draw_y,
                    RichAnchor {
                        h: HAnchor::Left,
                        v: VAnchor::Top,
                    },
                    xform,
                    pick,
                );
                continue;
            }

            let run = TextRun::new(&text, &style, ctx.dpi);

            // ── Soft-wrap. ──
            //
            // wrap_width_px = pt_to_px(width_pt) + width_band * x_band_width_px
            //
            // x_band_width_px is 0 on continuous x scales (band_width = 0),
            // so width_band degrades to "no contribution" outside discrete
            // scales. When wrap_width_px > 0, line-break the layout.
            //
            // The constraint is a *maximum*; parley wraps at word
            // boundaries so the actual content width is often less.
            // We use the actual content width (`run.content_width()`)
            // for anchor + bg calculations so the bg rect fits the
            // rendered text rather than the user-supplied bound.
            let x_raw = x_col.get(i);
            let x_band_width_px = band_width_at(x_scale, &x_raw) * panel_w;
            let width_pt = resolve_number_channel_or(width_ch, width_scale, i, 0.0);
            let width_band_frac =
                resolve_number_channel_or(width_band_ch, width_band_scale, i, 0.0);
            let wrap_width_px = pt_to_px(width_pt, ctx.dpi) + width_band_frac * x_band_width_px;
            // Justification (inner line placement). Only meaningful when
            // the layout wraps — single-line labels have nothing to
            // justify against.
            let justify_x = resolve_justify_channel(justify_x_ch, justify_x_scale, i);
            let (text_w, text_h) = if wrap_width_px > 0.0 && wrap_width_px.is_finite() {
                run.set_max_width(wrap_width_px as f32, justify_x);
                (run.content_width(), run.current_height())
            } else {
                (run.natural_width(), run.natural_height())
            };

            let anchor_x = resolve_number_channel_or(
                anchor_x_ch,
                anchor_x_scale,
                i,
                ctx.theme.geom.text.anchor_x,
            );
            let anchor_y = resolve_number_channel_or(
                anchor_y_ch,
                anchor_y_scale,
                i,
                ctx.theme.geom.text.anchor_y,
            );

            // ── Fill colour. ──
            let fill_color = override_alpha(
                resolve_color_channel_or_theme(
                    fill_ch,
                    fill_scale,
                    i,
                    ctx.theme.geom.text.fill.as_ref(),
                    &ctx.theme.palette,
                ),
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i),
            )
            .unwrap_or_else(default_fill);

            // ── Per-glyph outline. ──
            let text_stroke_color = resolve_color_channel_or_theme(
                text_stroke_ch,
                text_stroke_scale,
                i,
                ctx.theme.geom.text.text_stroke.as_ref(),
                &ctx.theme.palette,
            );
            let text_linewidth_pt = resolve_number_channel_or(
                text_linewidth_ch,
                text_linewidth_scale,
                i,
                ctx.theme.geom.text.text_linewidth_pt,
            );

            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i);

            // ── Background presence. ──
            let bg_fill = override_alpha(
                resolve_color_channel(bg_fill_ch, bg_fill_scale, i),
                resolve_number_channel(bg_fill_opacity_ch, bg_fill_opacity_scale, i),
            );
            let bg_stroke = override_alpha(
                resolve_color_channel(bg_stroke_ch, bg_stroke_scale, i),
                resolve_number_channel(bg_stroke_opacity_ch, bg_stroke_opacity_scale, i),
            );
            let has_bg = bg_fill.is_some() || bg_stroke.is_some();

            // ── Anchor + layout. ──
            //
            // Two regimes:
            //
            // - Without a background, the anchor positions the text
            //   metric box (font ascender..descender envelope).
            // - With a background, the anchor positions the *label*
            //   (text + padded rect) and we apply the ggplot2
            //   `geom_label` rebalance trick: top padding bumps up to
            //   at least the font descender, bottom padding shrinks by
            //   the same. Net total height is unchanged when
            //   `padding ≥ descender`, but the visible glyphs end up
            //   centred inside the bg rect even when the last line has
            //   no descenders ("men" looks centred like "jay" does).
            let (draw_x, draw_y, bg_rect_opt) = if has_bg {
                let padding_pt = resolve_number_channel_or(bg_padding_ch, bg_padding_scale, i, 0.0);
                let padding_px = pt_to_px(padding_pt, ctx.dpi);
                let descender_px = run.last_line_descender();
                let top_pad_eff = padding_px.max(descender_px);
                let bottom_pad_eff = (padding_px - descender_px).max(0.0);
                let bg_w = text_w + 2.0 * padding_px;
                let bg_h = text_h + top_pad_eff + bottom_pad_eff;
                let bg_left = anchor_px - anchor_x * bg_w;
                let bg_top = anchor_py - anchor_y * bg_h;
                let dx = bg_left + padding_px;
                let dy = bg_top + top_pad_eff;
                let bg_rect = Rect::new(bg_left, bg_top, bg_left + bg_w, bg_top + bg_h);
                (dx, dy, Some(bg_rect))
            } else {
                let dx = anchor_px - anchor_x * text_w;
                let dy = anchor_py - anchor_y * text_h;
                (dx, dy, None)
            };

            // ── Rotation transform. ──
            // Rotation pivots on the ALIGNMENT anchor — the user-visible
            // point that the text box's `anchor_x` / `anchor_y` fractions
            // pin to. Math CCW from the user → negate for kurbo (screen
            // y-down). Justification (line placement within the box) is
            // orthogonal: it changes where glyphs sit inside the layout
            // box, not the rotation pivot.
            let angle = resolve_angle_channel(angle_ch, angle_scale, i);
            let xform = if angle == 0.0 {
                Affine::IDENTITY
            } else {
                Affine::rotate_about(-angle, Point::new(anchor_px, anchor_py))
            };

            // ── Background rect (drawn before glyphs to sit behind). ──
            if let Some(bg_rect) = bg_rect_opt {
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
                    if let Some(fc) = bg_fill {
                        scene.fill(
                            FillRule::NonZero,
                            xform,
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
                            ctx.theme.geom.text.bg_linewidth_pt,
                        );
                        let lw_px = pt_to_px(lw_pt, ctx.dpi);
                        if lw_px.is_finite() && lw_px > 0.0 {
                            let stroke_spec = Stroke::new(lw_px)
                                .with_caps(Cap::Butt)
                                .with_join(Join::Miter);
                            scene.stroke(
                                &stroke_spec,
                                xform,
                                &Brush::Solid(sc),
                                None,
                                &bg_path,
                                pick,
                            );
                        }
                    }
                }
            }

            // ── Outline pass (under the fill). ──
            if let Some(stroke_color) = text_stroke_color {
                let stroke_width_px = pt_to_px(text_linewidth_pt, ctx.dpi);
                if stroke_width_px > 0.0 {
                    let stroke = crate::stroke::Stroke::new(stroke_width_px);
                    crate::text::draw_text_outline(
                        scene,
                        &run,
                        draw_x,
                        draw_y,
                        &Brush::Solid(stroke_color),
                        &stroke,
                        xform,
                        crate::pick::PickId::Skip,
                    );
                }
            }

            // ── Emit glyphs. ──
            draw_text(
                scene,
                &run,
                draw_x,
                draw_y,
                &Brush::Solid(fill_color),
                xform,
                pick,
            );
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

/// Resolve a `"justify_x"` channel to an [`HAlign`]. Recognises the
/// canonical string aliases — `"start"` / `"center"` / `"end"` /
/// `"justify"`. Unknown / non-string / unset → [`HAlign::Start`].
fn resolve_justify_channel(
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

/// Resolve `"italic"` as either a `Value::Bool` or a string ("italic" /
/// "normal"). Anything else → `false`.
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crate::color::Color;
    use crate::geometry::Rect;
    use crate::plot::geom::DirectScaleResolver;
    use crate::plot::scale;
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

    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    // ── build() ──

    #[test]
    #[should_panic(expected = "missing required channel \"text\"")]
    fn missing_text_panics() {
        TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .build();
    }

    #[test]
    #[should_panic(expected = "missing required channel \"x\"")]
    fn missing_x_panics() {
        TextGeom::builder()
            .set("y", vec![0.5_f64])
            .set("text", vec!["hi"])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn length_mismatch_panics() {
        TextGeom::builder()
            .set("x", vec![0.5_f64, 0.7])
            .set("y", vec![0.5_f64])
            .set("text", vec!["a", "b"])
            .build();
    }

    // ── Drawing ──

    fn glyph_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawGlyphs(_)))
            .count()
    }

    #[test]
    fn empty_text_skips_row() {
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec![""])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        assert_eq!(glyph_count(&scene), 0);
    }

    #[test]
    fn renders_one_label_per_row() {
        let g = TextGeom::builder()
            .set("x", vec![0.2_f64, 0.5, 0.8])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("text", vec!["alpha", "beta", "gamma"])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 400.0, 100.0), &shapes, &scales),
        );
        // Each label emits at least one glyph run.
        assert!(glyph_count(&scene) >= 3, "got {}", glyph_count(&scene));
    }

    #[test]
    fn nonfinite_position_skips_row() {
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64, f64::NAN])
            .set("y", vec![0.5_f64, 0.5])
            .set("text", vec!["a", "b"])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        // Only the first row should produce glyphs.
        assert!(glyph_count(&scene) >= 1);
    }

    #[test]
    fn default_fill_is_black_when_unbound() {
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["hello"])
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        // Should produce glyphs (no fill needed; we default to black).
        assert!(glyph_count(&scene) >= 1);
    }

    #[test]
    fn pick_id_channel_passes_through_per_row() {
        let g = TextGeom::builder()
            .set("x", vec![0.2_f64, 0.5, 0.8])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("text", vec!["A", "B", "C"])
            .set("fill", red())
            .set("pick_id", vec![41_i64, 42, 43])
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 400.0, 100.0), &shapes, &scales),
        );
        let picks: std::collections::HashSet<u32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(run) => match run.pick_id {
                    crate::pick::PickId::Id(n) => Some(n),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(picks, [41u32, 42, 43].into_iter().collect());
    }

    #[test]
    fn anchor_centre_is_default() {
        // Same anchor data point: anchor_x=0.5, anchor_y=0.5 means the
        // glyph run's bbox should be roughly centred on (50, 50).
        // We can't easily compute the run's box without exposing it,
        // so just verify the geom doesn't panic and emits glyphs.
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["centered"])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        assert!(glyph_count(&scene) >= 1);
    }

    #[test]
    fn size_scaled_by_dpi() {
        // 12pt at 96 dpi = 16 px. Emit a single label and check the
        // glyph run's font_size in the recorded op.
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["x"])
            .set("size", vec![12.0_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        for op in &scene.ops {
            if let Op::DrawGlyphs(run) = op {
                assert!(
                    (run.font_size as f64 - 12.0 * 96.0 / 72.0).abs() < 1e-3,
                    "font_size = {}, expected ~16.0",
                    run.font_size
                );
                return;
            }
        }
        panic!("no glyph run emitted");
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = TextGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .set("text", vec!["x"])
            .set("fill", red())
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(names.contains(&"text"));
        // No x2/y2/radius — those belong elsewhere.
        assert!(!names.contains(&"x2"));
        assert!(!names.contains(&"radius"));
    }

    #[test]
    fn italic_via_string() {
        // "italic" string maps to TextStyle.italic = true. We can't
        // observe the style directly from the recorded ops, but the
        // build path shouldn't panic.
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["x"])
            .set("italic", Value::String(Arc::from("italic")))
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        assert!(glyph_count(&scene) >= 1);
    }

    // ── Background rect ──

    fn fill_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count()
    }

    fn stroke_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count()
    }

    #[test]
    fn bg_fill_emits_rect_before_glyphs() {
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["hi"])
            .set("fill", red())
            .set("bg_fill", Color::new([0.9, 0.9, 0.7, 1.0]))
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        // Expect one fill (bg rect) and at least one glyph run.
        assert_eq!(fill_count(&scene), 1);
        assert!(glyph_count(&scene) >= 1);
        // Order: fill should come before the first DrawGlyphs.
        let fill_idx = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::Fill { .. }))
            .unwrap();
        let glyph_idx = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::DrawGlyphs(_)))
            .unwrap();
        assert!(fill_idx < glyph_idx, "bg fill should precede glyphs");
    }

    #[test]
    fn bg_stroke_only_emits_stroke_no_fill() {
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["hi"])
            .set("fill", red())
            .set("bg_stroke", Color::new([0.2, 0.2, 0.2, 1.0]))
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        assert_eq!(fill_count(&scene), 0);
        assert_eq!(stroke_count(&scene), 1);
    }

    #[test]
    fn bg_unbound_emits_no_rect() {
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["hi"])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        assert_eq!(fill_count(&scene), 0);
        assert_eq!(stroke_count(&scene), 0);
        assert!(glyph_count(&scene) >= 1);
    }

    #[test]
    fn bg_padding_extends_rect() {
        // Width grows by 2*padding (horizontal padding is symmetric).
        // Height growth depends on the geom_label rebalance trick: for
        // padding < descender the box is locked at the minimum (top =
        // descender, bottom = 0); for padding ≥ descender the box
        // grows by `2 * padding - descender` relative to padding=0.
        //
        // Comparing two padding values that are BOTH ≥ descender, the
        // descender allocation cancels and the height delta is just
        // 2 * (padding_high − padding_low). That's the cleanest
        // invariant to test.
        let g_low = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["hi"])
            .set("fill", red())
            .set("bg_fill", red())
            .set("bg_padding", 6.0_f64) // 8 px at 96 dpi, > typical descender
            .build();
        let g_high = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["hi"])
            .set("fill", red())
            .set("bg_fill", red())
            .set("bg_padding", 15.0_f64) // 20 px at 96 dpi
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut s_low = RecordingScene::default();
        let mut s_high = RecordingScene::default();
        let c = ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales);
        g_low.draw(&mut s_low, &c);
        g_high.draw(&mut s_high, &c);
        let bb_low = fill_bbox(&s_low).expect("fill low");
        let bb_high = fill_bbox(&s_high).expect("fill high");
        let expected_delta_px = 2.0 * (15.0 - 6.0) * 96.0 / 72.0; // = 24 px
        assert!(
            (bb_high.width() - bb_low.width() - expected_delta_px).abs() < 0.5,
            "width delta {} (expected {})",
            bb_high.width() - bb_low.width(),
            expected_delta_px
        );
        assert!(
            (bb_high.height() - bb_low.height() - expected_delta_px).abs() < 0.5,
            "height delta {} (expected {})",
            bb_high.height() - bb_low.height(),
            expected_delta_px
        );
    }

    #[test]
    fn bg_rebalance_reserves_descender_at_zero_padding() {
        // With padding=0 and the geom_label rebalance trick, the bg
        // should still reserve `descender` of space above the text
        // (and 0 below). Net: the bg is taller than the text by
        // exactly `descender_px`.
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["men"]) // no descenders
            .set("fill", red())
            .set("bg_fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        let bb = fill_bbox(&scene).expect("fill");
        // Build a TextRun the same way to get the metrics we expect.
        let style = TextStyle::new(12.0).weight(400);
        let probe = TextRun::new("men", &style, 96.0);
        let text_h = probe.natural_height();
        let descender = probe.last_line_descender();
        // bg height = text_h + descender + 0.
        let expected = text_h + descender;
        assert!(
            (bb.height() - expected).abs() < 0.5,
            "bg height {} (expected text_h={} + descender={} = {})",
            bb.height(),
            text_h,
            descender,
            expected
        );
    }

    #[test]
    fn bg_corner_radius_uses_rounded_path() {
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["hi"])
            .set("fill", red())
            .set("bg_fill", Color::new([0.9, 0.9, 0.7, 1.0]))
            .set("bg_corner_radius", 4.0_f64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let has_curves = path.elements().iter().any(|el| {
                    matches!(
                        el,
                        crate::path::PathEl::CurveTo(_, _, _) | crate::path::PathEl::QuadTo(_, _)
                    )
                });
                assert!(has_curves, "rounded rect should have curves");
                return;
            }
        }
        panic!("no fill emitted");
    }

    #[test]
    fn bg_shares_pick_id_with_glyphs() {
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["hi"])
            .set("fill", red())
            .set("bg_fill", red())
            .set("pick_id", 99_i64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales),
        );
        let bg_pick = scene.ops.iter().find_map(|op| match op {
            Op::Fill {
                pick_id: crate::pick::PickId::Id(n),
                ..
            } => Some(*n),
            _ => None,
        });
        let glyph_pick = scene.ops.iter().find_map(|op| match op {
            Op::DrawGlyphs(run) => match run.pick_id {
                crate::pick::PickId::Id(n) => Some(n),
                _ => None,
            },
            _ => None,
        });
        assert_eq!(bg_pick, Some(99));
        assert_eq!(glyph_pick, Some(99));
    }

    // ── Soft-wrap ──

    #[test]
    fn width_pt_constrains_layout_height() {
        // A long string wrapped should be taller AND narrower than the
        // same string unwrapped. Parley's word-wrap is best-effort: if
        // an individual word exceeds the constraint, that line overflows
        // — so we don't assert "≤ constraint", only "narrower than
        // unwrapped".
        let long = "Lorem ipsum dolor sit amet, consectetur adipiscing elit";
        let g_unwrapped = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec![long])
            .set("fill", red())
            .set("bg_fill", red())
            .build();
        let g_wrapped = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec![long])
            .set("fill", red())
            .set("bg_fill", red())
            .set("width", 100.0_f64) // 100 pt ≈ 133 px at 96 dpi
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut s0 = RecordingScene::default();
        let mut s1 = RecordingScene::default();
        let c = ctx(Rect::new(0.0, 0.0, 1000.0, 600.0), &shapes, &scales);
        g_unwrapped.draw(&mut s0, &c);
        g_wrapped.draw(&mut s1, &c);
        let bb0 = fill_bbox(&s0).expect("fill0");
        let bb1 = fill_bbox(&s1).expect("fill1");
        assert!(
            bb1.height() > bb0.height() + 5.0,
            "wrapped should be taller: bb0.h={}, bb1.h={}",
            bb0.height(),
            bb1.height()
        );
        assert!(
            bb1.width() < bb0.width(),
            "wrapped should be narrower than unwrapped: bb0.w={}, bb1.w={}",
            bb0.width(),
            bb1.width()
        );
    }

    #[test]
    fn width_band_wraps_within_discrete_band() {
        // Discrete x with 4 categories → band width = 50 px on a 200 px
        // panel. width_band = 1.0 should set the wrap constraint to 50
        // px. The bg rect matches the actual content (wrapped) width,
        // which is ≤ 50 px.
        let x_scale = scale::discrete(
            ["A", "B", "C", "D"]
                .into_iter()
                .map(|s| Value::String(Arc::from(s))),
        );
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = TextGeom::builder()
            .set("x", vec!["B"])
            .set("y", vec![0.5_f64])
            .set("text", vec!["wrapped within category band"])
            .set("width_band", 1.0_f64)
            .set("fill", red())
            .set("bg_fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &resolver),
        );
        let bb = fill_bbox(&scene).expect("fill");
        // bg_padding is 0, so bg.width = actual content width. Should
        // be positive and have triggered wrapping (taller than one
        // line of natural width).
        assert!(bb.width() > 0.0, "width = {}", bb.width());
        // Sanity: with no wrap, the text would be much wider than 50 px;
        // wrapping should reduce the width below the natural extent of
        // "wrapped within category band" (>= 150 px in typical fonts).
        assert!(
            bb.width() < 150.0,
            "wrapped should be narrower than natural: width = {}",
            bb.width()
        );
    }

    #[test]
    fn width_pt_and_band_sum_triggers_wrap() {
        // Discrete x band = 50 px; width_band = 1.0 → 50 px; width = -9 pt
        // → -12 px. Net wrap constraint = 38 px. Wrap should fire.
        // (Negative pt with positive band is a useful "band-width minus
        // margin" pattern.)
        let x_scale = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = TextGeom::builder()
            .set("x", vec!["A"])
            .set("y", vec![0.5_f64])
            .set("text", vec!["long-running label text"])
            .set("width", -9.0_f64)
            .set("width_band", 1.0_f64)
            .set("fill", red())
            .set("bg_fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let bb = fill_bbox(&scene).expect("fill");
        assert!(bb.width() > 0.0, "width = {}", bb.width());
        // Without wrap, the text would be >> 38 px wide. With wrap, the
        // content should be much narrower than natural.
        assert!(
            bb.width() < 80.0,
            "wrap should fire: width = {}",
            bb.width()
        );
    }

    #[test]
    fn bg_matches_content_width_not_wrap_constraint() {
        // Short text with a generous wrap constraint should produce a
        // bg rect sized to the text content, NOT to the constraint.
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["short"])
            .set("width", 500.0_f64) // generous constraint
            .set("fill", red())
            .set("bg_fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 1000.0, 200.0), &shapes, &scales),
        );
        let bb = fill_bbox(&scene).expect("fill");
        let wrap_px = 500.0 * 96.0 / 72.0;
        assert!(
            bb.width() < wrap_px * 0.5,
            "bg width {} should be much less than wrap constraint {}",
            bb.width(),
            wrap_px
        );
    }

    fn glyph_extent(scene: &RecordingScene) -> Option<(f32, f32)> {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for op in &scene.ops {
            if let Op::DrawGlyphs(run) = op {
                for g in &run.glyphs {
                    lo = lo.min(g.x);
                    hi = hi.max(g.x);
                }
            }
        }
        (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
    }

    fn count_fills(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count()
    }

    fn draw_text_geom_to_scene(g: TextGeom) -> RecordingScene {
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 400.0, 200.0), &shapes, &scales),
        );
        scene
    }

    #[test]
    fn text_stroke_emits_outline_pass() {
        let make_plain = || {
            let g = TextGeom::builder()
                .set("x", vec![0.5_f64])
                .set("y", vec![0.5_f64])
                .set("text", vec!["hello"])
                .set("fill", red())
                .set("text_linewidth", 1.5_f64)
                .build();
            draw_text_geom_to_scene(g)
        };
        let make_outlined = || {
            let g = TextGeom::builder()
                .set("x", vec![0.5_f64])
                .set("y", vec![0.5_f64])
                .set("text", vec!["hello"])
                .set("fill", red())
                .set("text_linewidth", 1.5_f64)
                .set("text_stroke", Color::new([0.0, 0.0, 1.0, 1.0]))
                .build();
            draw_text_geom_to_scene(g)
        };
        let count_stroked = |scene: &RecordingScene| {
            scene
                .ops
                .iter()
                .filter(|op| matches!(op, Op::DrawGlyphs(g) if g.style.is_some()))
                .count()
        };
        assert_eq!(count_stroked(&make_plain()), 0);
        assert!(count_stroked(&make_outlined()) >= 1);
    }

    #[test]
    fn underline_channel_adds_fill_per_label() {
        let make = |underline: bool| {
            let g = TextGeom::builder()
                .set("x", vec![0.5_f64])
                .set("y", vec![0.5_f64])
                .set("text", vec!["hello"])
                .set("fill", red())
                .set("underline", underline)
                .build();
            draw_text_geom_to_scene(g)
        };
        let base = count_fills(&make(false));
        let underlined = count_fills(&make(true));
        assert_eq!(underlined, base + 1);
    }

    #[test]
    fn strikethrough_channel_adds_fill_per_label() {
        let make = |strike: bool| {
            let g = TextGeom::builder()
                .set("x", vec![0.5_f64])
                .set("y", vec![0.5_f64])
                .set("text", vec!["hello"])
                .set("fill", red())
                .set("strikethrough", strike)
                .build();
            draw_text_geom_to_scene(g)
        };
        let base = count_fills(&make(false));
        let struck = count_fills(&make(true));
        assert_eq!(struck, base + 1);
    }

    #[test]
    fn letter_spacing_channel_widens_emitted_glyphs() {
        let make = |spacing: f64| {
            let g = TextGeom::builder()
                .set("x", vec![0.5_f64])
                .set("y", vec![0.5_f64])
                .set("text", vec!["MMMM"])
                .set("fill", red())
                .set("letter_spacing", spacing)
                .build();
            let shapes = shapes();
            let scales = DirectScaleResolver::new();
            let mut scene = RecordingScene::default();
            g.draw(
                &mut scene,
                &ctx(Rect::new(0.0, 0.0, 400.0, 200.0), &shapes, &scales),
            );
            let (lo, hi) = glyph_extent(&scene).expect("glyphs");
            (hi - lo) as f64
        };
        let base = make(0.0);
        let loose = make(8.0);
        assert!(
            loose > base + 5.0,
            "letter_spacing=8pt should widen glyph extent: base={base}, loose={loose}"
        );
    }

    // Helper used by the new tests.
    fn fill_bbox(scene: &RecordingScene) -> Option<Rect> {
        use crate::geometry::Shape as _;
        scene.ops.iter().find_map(|op| match op {
            Op::Fill { path, .. } => {
                let bb = path.bounding_box();
                Some(Rect::new(bb.x0, bb.y0, bb.x1, bb.y1))
            }
            _ => None,
        })
    }

    #[test]
    fn x_band_shifts_anchor() {
        // Discrete x scale; x_band offset moves the anchor within the
        // band. Smoke check that geom doesn't panic with band binding.
        let x = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x);
        let g = TextGeom::builder()
            .set("x", vec!["A"])
            .set("y", vec![0.5_f64])
            .set("text", vec!["L"])
            .set("x_band", vec![0.0_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &resolver),
        );
        assert!(glyph_count(&scene) >= 1);
    }

    #[test]
    fn markdown_channel_switches_to_rich_text_path() {
        // A `{.red xyz}` span shapes as rich text only when the
        // `markdown` channel is true — the rich path emits a red
        // glyph run while the plain path renders the braces / dots
        // literally with the fill colour.
        let make = |md: bool| {
            TextGeom::builder()
                .set("x", vec![0.5_f64])
                .set("y", vec![0.5_f64])
                .set("text", vec!["{.red hi}"])
                .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
                .set("markdown", md)
                .build()
        };
        let s_plain = draw_text_geom_to_scene(make(false));
        let s_md = draw_text_geom_to_scene(make(true));
        let has_red_glyphs = |scene: &RecordingScene| {
            scene.ops.iter().any(|op| match op {
                Op::DrawGlyphs(run) => match &run.brush {
                    Brush::Solid(c) => {
                        let [r, g, b, _] = c.components;
                        (r - 1.0).abs() < 1e-3 && g < 0.1 && b < 0.1
                    }
                    _ => false,
                },
                _ => false,
            })
        };
        assert!(
            !has_red_glyphs(&s_plain),
            "plain path shouldn't produce red glyphs"
        );
        assert!(
            has_red_glyphs(&s_md),
            "markdown path should produce red glyphs from {{.red hi}}"
        );
    }

    #[test]
    fn markdown_code_block_paints_inside_geom_bg() {
        // A geom-level bg_fill combined with markdown containing a
        // fenced code block should emit at least TWO fill ops:
        // 1. The geom's outer bg (rect wrapping the whole label).
        // 2. The code block's per-block background (inside the outer).
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["```\nlet x = 1;\n```"])
            .set("markdown", true)
            .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
            .set("bg_fill", Color::new([1.0, 1.0, 0.5, 1.0])) // outer yellow
            .build();
        let scene = draw_text_geom_to_scene(g);
        let fills = count_fills(&scene);
        assert!(
            fills >= 2,
            "expected outer bg fill + code_block fill (≥ 2), got {fills}"
        );
    }

    #[test]
    fn markdown_theme_default_toggles_rich_path() {
        // When the theme sets `geom.text.markdown = true`, unset
        // channels default to rich shaping. Compare glyph counts:
        // markdown on strips the `*` markers, so the run yields one
        // fewer glyph.
        let g = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["*hi*"])
            .set("fill", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        // Default theme: markdown = false → 4 glyphs (`*`, `h`, `i`, `*`).
        let ctx_plain = ctx(Rect::new(0.0, 0.0, 400.0, 200.0), &shapes, &scales);
        let mut s_plain = RecordingScene::default();
        g.draw(&mut s_plain, &ctx_plain);
        // Themed with markdown default on → 2 glyphs (`h`, `i`).
        let mut theme_md = crate::plot::theme::Theme::default();
        theme_md.geom.text.markdown = true;
        let ctx_md = ctx(Rect::new(0.0, 0.0, 400.0, 200.0), &shapes, &scales).with_theme(&theme_md);
        let mut s_md = RecordingScene::default();
        g.draw(&mut s_md, &ctx_md);
        let count_glyphs = |sc: &RecordingScene| {
            sc.ops
                .iter()
                .map(|op| match op {
                    Op::DrawGlyphs(r) => r.glyphs.len(),
                    _ => 0,
                })
                .sum::<usize>()
        };
        assert!(
            count_glyphs(&s_md) < count_glyphs(&s_plain),
            "markdown theme default should strip `*` markers (plain={}, md={})",
            count_glyphs(&s_plain),
            count_glyphs(&s_md)
        );
    }

    #[test]
    fn markdown_rows_shape_once_and_are_reused_next_frame() {
        let geom = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["**bold** and *italic*"])
            .set("markdown", vec![true])
            .build();
        let registry = shapes();
        let sx = scale::continuous(0.0..=1.0);
        let scales = DirectScaleResolver::new().with("x", &sx).with("y", &sx);
        let panel = Rect::new(0.0, 0.0, 200.0, 100.0);
        let mut scene = RecordingScene::default();
        geom.draw(&mut scene, &ctx(panel, &registry, &scales));
        assert_eq!(geom.rich_cache.len(), 1, "the row should be cached");
        let ops_after_first = scene.ops.len();
        geom.draw(&mut scene, &ctx(panel, &registry, &scales));
        assert_eq!(
            geom.rich_cache.len(),
            1,
            "the second frame must reuse the shaped run, not add another"
        );
        assert_eq!(
            scene.ops.len(),
            ops_after_first * 2,
            "both frames should emit the same drawing work"
        );
    }

    #[test]
    fn invalidate_caches_drops_shaped_markdown() {
        let mut geom = TextGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("text", vec!["**bold**"])
            .set("markdown", vec![true])
            .build();
        let registry = shapes();
        let sx = scale::continuous(0.0..=1.0);
        let scales = DirectScaleResolver::new().with("x", &sx).with("y", &sx);
        let mut scene = RecordingScene::default();
        geom.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &registry, &scales),
        );
        assert!(!geom.rich_cache.is_empty());
        geom.invalidate_caches();
        assert!(geom.rich_cache.is_empty());
    }
}
