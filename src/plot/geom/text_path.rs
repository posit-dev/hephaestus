//! `TextPathGeom` — text laid out along a polyline path.
//!
//! Per-mark text on per-mark polyline. Rows are grouped by key (same
//! pattern as [`LineGeom`](super::line::LineGeom)); each group's
//! `(x, y)` vertices define the curve and a per-mark `"text"` channel
//! carries the string. Each glyph is stamped at the per-glyph arc-length
//! advance along the curve via [`PolylineSampler::sample_at`].
//!
//! Limitations:
//!
//! - One line per label. No `max_width` / wrapping, and a markdown
//!   source that produced block structure is flattened rather than
//!   stacked (see `"markdown"` below).
//! - Glyphs whose computed arc-length distance falls outside the
//!   `[0, total_length]` range are dropped (no partial stamping).
//! - The mark must contribute at least two finite vertices; otherwise
//!   the whole mark is skipped silently.
//!
//! Channels consumed:
//!
//! - `"x"` / `"y"` — vertex position (required; data; numeric, per row).
//! - `"x_offset"` / `"y_offset"` — per-row absolute pt offset added to
//!   each projected vertex (per row). Positive `y_offset` shifts the
//!   vertex up (decrements pixel y). Distinct from the per-mark
//!   `"offset"` channel below, which shifts the text along the path.
//! - `"x_band"` / `"y_band"` — per-row band-fraction offset folded
//!   into the corresponding scale's `map_with_offset`. No effect on
//!   continuous scales.
//! - `"text"` — label string (required; per mark; resolved at the mark's
//!   first row).
//! - `"size"` — font size in pt (optional; default 12pt; per mark).
//! - `"weight"` — CSS font weight (optional; default 400; per mark).
//! - `"italic"` — boolean (optional; default false; per mark). Accepts a
//!   `Value::Bool` or the conventional `"italic"` / `"normal"` strings.
//! - `"family"` — font family name (optional; per mark).
//! - `"tracking"` — letter spacing in 1/1000 em (optional; per mark).
//!   `20.0` is `0.02 em`. Widens the arc length each glyph occupies.
//! - `"underline"` / `"strikethrough"` — booleans (optional; per
//!   mark). Drawn as a stroke that follows the curve at the font's own
//!   rule offset and thickness, so the rule bends with the text
//!   instead of rotating with `"angle"`.
//! - `"markdown"` — boolean (optional; per mark). When true the label
//!   is read as marquee-flavoured markdown and shaped through
//!   [`crate::text::rich`], then flattened to one line: per-span
//!   font, size, weight, italic, colour, superscript / subscript
//!   shift, and underline / strikethrough all survive. Block-level
//!   constructs do not — a source that produced several blocks or
//!   lines (headings, list items, paragraph breaks, hard breaks) has
//!   its segments joined into one line with a space between them,
//!   dropping block indents, margins, list markers, span backgrounds
//!   and borders, none of which have a meaning on a curve. Per-span
//!   `text_stroke` from the style sheet is not honoured either; the
//!   `"text_stroke"` / `"text_linewidth"` channels below outline every
//!   glyph. `with_rich_sheet` installs a per-geom style sheet, as on
//!   [`TextGeom`](super::TextGeom).
//! - `"text_stroke"` — per-glyph outline colour (optional; per mark).
//!   Drawn behind the fill.
//! - `"text_linewidth"` — outline thickness in pt (optional; per
//!   mark). No effect without `"text_stroke"`.
//! - `"fill"` — glyph colour (optional; default black; per mark). A
//!   markdown span that sets its own colour overrides it.
//! - `"fill_opacity"` — overrides the alpha component of `"fill"` (optional;
//!   `0..=1`; per mark).
//! - `"offset"` — pt offset along the path where the text layout starts
//!   (optional; default `0.0`; per mark). Positive values shift text
//!   forward along the path.
//! - `"justify_x"` — fraction in `[0, 1]` of the available whitespace
//!   (`path_length - text_width`) to pad at the start of the text
//!   (optional; default `0.0`; per mark). `0.0` = text starts at the
//!   offset point, `0.5` = centred, `1.0` = text ends at the offset
//!   point plus the path length. Values outside `[0, 1]` are honoured
//!   literally — out-of-range glyphs are dropped per the limitation above.
//! - `"upright"` — boolean (optional; default false; per mark). When
//!   true, the layout checks whether the majority of glyph tangents
//!   point into the left half-plane (i.e., the text would render
//!   upside-down as a whole). If so, the entire text is laid out
//!   against the *reversed* path with `justify_x` inverted — every glyph
//!   then reads right-side-up and reading direction along the path
//!   reverses. This is a per-mark decision (the whole text flips
//!   together or not at all); no mid-text orientation changes.
//!   Matches ggplot2 `geomtextpath::geom_textpath(upright = TRUE)`.
//! - `"anchor_y"` — vertical anchor as a fraction of the text's own
//!   height (optional; default `0.5`; per mark). `0` puts the text's
//!   top edge on the curve so the body hangs off the right-of-motion
//!   side, `0.5` centres the body on the curve, `1` puts the bottom
//!   edge on it. Same fraction-of-own-height convention as
//!   [`TextGeom`](super::TextGeom)'s `"anchor_y"`; values outside
//!   `[0, 1]` push the text clear of the curve.
//! - `"angle"` — additional per-mark rotation in radians, mathematical
//!   CCW (optional; default `0.0`). Applied on top of the per-glyph
//!   tangent rotation.
//! - `"pick_id"` — per-mark pick ticket (optional). Every glyph in the
//!   mark shares the same id; the mark's first row supplies the value.

use std::sync::Arc;

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point, Vec2};
use crate::plot::theme::HAlign;
use crate::plot::value::Value;
use crate::primitives::PolylineSampler;
use crate::scene::{Font, Glyph, GlyphRun, SceneBuilder};
use crate::text::rich::{
    flatten_rich_run, RichKey, RichShapeCache, RichTextRun, RichTextStyleSheet, RichTextWidth,
};
use crate::text::{run_layout_glyphs, run_layout_rules, TextRun, TextStyle};

use super::marks::{build_marks_from_column, unique_values_at_first_rows, MarkSlot};
use super::resolve::{
    override_alpha, pt_to_px, resolve_angle_channel, resolve_bool_channel_or,
    resolve_color_channel_or_theme, resolve_number_channel, resolve_number_channel_or,
    resolve_pick_id, resolve_position, resolve_str_channel_or,
};
use super::state::{finalize_state, require_x_and_siblings, GeomState, KeysStrategy};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext, Keys};

use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};

// ─── Defaults ────────────────────────────────────────────────────────────────

// Style defaults (size, weight) live on `theme.geom.text_path`.
fn default_fill() -> Color {
    Color::new([0.0, 0.0, 0.0, 1.0])
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
    ("tracking", ExpectedOutput::Numbers),
    ("markdown", ExpectedOutput::Any),
    ("underline", ExpectedOutput::Any),
    ("strikethrough", ExpectedOutput::Any),
    ("text_stroke", ExpectedOutput::Colors),
    ("text_linewidth", ExpectedOutput::Numbers),
    ("fill", ExpectedOutput::Colors),
    ("fill_opacity", ExpectedOutput::Numbers),
    ("offset", ExpectedOutput::Numbers),
    ("justify_x", ExpectedOutput::Numbers),
    ("upright", ExpectedOutput::Any),
    ("anchor_y", ExpectedOutput::Numbers),
    ("angle", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── Shaped intermediates ────────────────────────────────────────────────────

/// One glyph ready to be stamped on the curve. Both shapers — plain
/// [`TextRun`] and flattened markdown — reduce to this, so the
/// placement walk below is written once.
struct Stamp {
    /// Glyph id in `font`.
    id: u32,
    /// Distance from the label's start along the flow axis.
    x: f64,
    /// Flow distance the glyph occupies.
    advance: f64,
    /// Offset from the label's baseline, screen y-down — a
    /// superscript's lift, for instance.
    dy: f64,
    font: Font,
    font_size: f32,
    color: Color,
}

/// One underline or strikethrough rule over a span of the label,
/// stroked along the curve rather than as a rectangle.
struct Rule {
    /// Start of the rule along the flow axis.
    x0: f64,
    /// End of the rule along the flow axis.
    x1: f64,
    /// Centreline offset from the label's baseline, screen y-down.
    dy: f64,
    /// Stroke width, taken from the font's own rule thickness.
    thickness: f64,
    color: Color,
}

// ─── TextPathGeom ────────────────────────────────────────────────────────────

/// A vectorised text-on-curve geom. One label per mark, positioned
/// glyph-by-glyph along the mark's polyline.
pub struct TextPathGeom {
    pub(crate) state: GeomState,
    pub(crate) marks: Vec<MarkSlot>,
    /// Optional per-geom style sheet used when the `"markdown"`
    /// channel resolves `true`. `None` falls back to the theme's
    /// `rich_text` sheet.
    pub(crate) rich_sheet: Option<Arc<RichTextStyleSheet>>,
    /// Shaped markdown labels, reused across frames. Cleared whenever
    /// the geom's data is replaced.
    pub(crate) rich_cache: RichShapeCache,
}

crate::impl_geom_inherents_grouped!(TextPathGeom);

impl TextPathGeom {
    /// Build the per-mark slot index from the current keys. Each
    /// contiguous run of equal keys becomes one mark.
    pub(crate) fn build_marks(&self) -> Vec<MarkSlot> {
        super::marks::build_marks(&self.state.keys)
    }

    /// Install a rich-text style sheet used for every label this geom
    /// renders as markdown. Overrides the theme's default sheet.
    /// Chains for builder-style construction.
    pub fn with_rich_sheet(mut self, sheet: Arc<RichTextStyleSheet>) -> Self {
        self.rich_sheet = Some(sheet);
        self
    }

    /// Same as [`Self::with_rich_sheet`] for mutation through
    /// `Plot::update_geom(&mut TextPathGeom)`.
    pub fn set_rich_sheet(&mut self, sheet: Arc<RichTextStyleSheet>) {
        self.rich_sheet = Some(sheet);
    }

    /// Clear any per-geom rich-text sheet override — falls back to
    /// the theme default.
    pub fn clear_rich_sheet(&mut self) {
        self.rich_sheet = None;
    }

    /// Clear the shaped-markdown cache. The key covers the sheet's
    /// identity, so swapping sheets doesn't require this — it exists
    /// for callers that mutate a sheet in place despite the
    /// immutable-once-installed convention.
    pub fn clear_rich_cache(&mut self) {
        self.rich_cache.clear();
    }
}

// ─── BuildableGeom ───────────────────────────────────────────────────────────

impl BuildableGeom for TextPathGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();
        let n = require_x_and_siblings(&channels, &["y"], "TextPathGeom");
        // `"text"` may be a constant (one string for all marks) or per-
        // mark data, so we don't require_data_column here. But it must be
        // present — the geom has no useful default text.
        if !channels.contains_key("text") {
            panic!("TextPathGeom::build: missing required channel \"text\"");
        }
        let state = finalize_state(
            keys_opt,
            channels,
            n,
            KeysStrategy::OneMark,
            CHANNELS,
            "TextPathGeom",
        );
        TextPathGeom {
            state,
            marks: Vec::new(),
            rich_sheet: None,
            rich_cache: RichShapeCache::new(),
        }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for TextPathGeom {
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
        Some("text-path")
    }

    fn mark_count(&self) -> usize {
        if self.marks.is_empty() && !self.is_empty() {
            return self.build_marks().len();
        }
        self.marks.len()
    }

    fn invalidate_caches(&mut self) {
        self.marks.clear();
        self.rich_cache.clear();
    }

    fn rebuild_diff_against_previous(&mut self) {
        if !self.state.dirty {
            return;
        }
        let next_marks = self.build_marks();
        let prev_marks = match &self.state.prev_keys {
            Keys::Explicit(col) if !col.is_empty() => build_marks_from_column(col),
            _ => Vec::new(),
        };
        let (enter, update, exit) = match (&self.state.prev_keys, &self.state.keys) {
            (Keys::Explicit(prev_col), Keys::Explicit(next_col)) => {
                let prev_unique = unique_values_at_first_rows(
                    prev_col,
                    prev_marks.iter().map(|m| m.first_row),
                    "TextPathGeom",
                );
                let next_unique = unique_values_at_first_rows(
                    next_col,
                    next_marks.iter().map(|m| m.first_row),
                    "TextPathGeom",
                );
                let idx = KeyIndex::build(&prev_unique);
                diff_columns(&prev_unique, &idx, &next_unique)
            }
            _ => diff_positional(prev_marks.len(), next_marks.len()),
        };
        self.state.enter = enter;
        self.state.update = update;
        self.state.exit = exit;
        self.marks = next_marks;
        self.state.prev_keys = self.state.keys.clone();
        self.state.prev_channels = self.state.channels.clone();
        self.state.dirty = false;
    }

    fn draw(&self, scene: &mut dyn SceneBuilder, ctx: &GeomContext<'_>) {
        let panel = ctx.panel_rect;
        let panel_w = panel.x1 - panel.x0;
        let panel_h = panel.y1 - panel.y0;
        if panel_w <= 0.0 || panel_h <= 0.0 {
            return;
        }

        let owned_marks;
        let marks: &[MarkSlot] = if self.marks.is_empty() && !self.is_empty() {
            owned_marks = self.build_marks();
            &owned_marks
        } else {
            &self.marks
        };
        if marks.is_empty() {
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
        let tracking_scale = ctx.scale_for("tracking");
        let markdown_scale = ctx.scale_for("markdown");
        let underline_scale = ctx.scale_for("underline");
        let strikethrough_scale = ctx.scale_for("strikethrough");
        let text_stroke_scale = ctx.scale_for("text_stroke");
        let text_linewidth_scale = ctx.scale_for("text_linewidth");
        let fill_scale = ctx.scale_for("fill");
        let fill_opacity_scale = ctx.scale_for("fill_opacity");
        let offset_scale = ctx.scale_for("offset");
        let hjust_scale = ctx.scale_for("justify_x");
        let upright_scale = ctx.scale_for("upright");
        let anchor_y_scale = ctx.scale_for("anchor_y");
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

        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let text_ch = channels.get("text");
        let size_ch = channels.get("size");
        let weight_ch = channels.get("weight");
        let italic_ch = channels.get("italic");
        let family_ch = channels.get("family");
        let tracking_ch = channels.get("tracking");
        let markdown_ch = channels.get("markdown");
        // Kept as an `Arc` so the shape cache can key on its identity.
        let rich_sheet: &Arc<RichTextStyleSheet> =
            self.rich_sheet.as_ref().unwrap_or(&ctx.theme.rich_text);
        let underline_ch = channels.get("underline");
        let strikethrough_ch = channels.get("strikethrough");
        let text_stroke_ch = channels.get("text_stroke");
        let text_linewidth_ch = channels.get("text_linewidth");
        let fill_ch = channels.get("fill");
        let fill_opacity_ch = channels.get("fill_opacity");
        let offset_ch = channels.get("offset");
        let hjust_ch = channels.get("justify_x");
        let upright_ch = channels.get("upright");
        let anchor_y_ch = channels.get("anchor_y");
        let angle_ch = channels.get("angle");
        let pick_id_ch = channels.get("pick_id");

        for mark in marks.iter() {
            let i0 = mark.first_row;

            // ── Resolve per-mark text + style. ──
            let text = resolve_str_channel_or(text_ch, text_scale, i0, "");
            if text.is_empty() {
                continue;
            }
            let size_pt = resolve_number_channel_or(
                size_ch,
                size_scale,
                i0,
                ctx.theme.geom.text_path.size_pt,
            );
            if !size_pt.is_finite() || size_pt <= 0.0 {
                continue;
            }
            let weight = resolve_number_channel(weight_ch, weight_scale, i0)
                .map(|w| (w.round() as i64).clamp(1, 1000) as u16)
                .unwrap_or(ctx.theme.geom.text_path.weight);
            let italic = resolve_italic(italic_ch, italic_scale, i0);
            let family = resolve_str_opt(family_ch, family_scale, i0);
            let tracking = resolve_number_channel_or(
                tracking_ch,
                tracking_scale,
                i0,
                ctx.theme.geom.text_path.tracking,
            ) as f32;
            let underline = resolve_bool_channel_or(
                underline_ch,
                underline_scale,
                i0,
                ctx.theme.geom.text_path.underline,
            );
            let strikethrough = resolve_bool_channel_or(
                strikethrough_ch,
                strikethrough_scale,
                i0,
                ctx.theme.geom.text_path.strikethrough,
            );
            let markdown = resolve_bool_channel_or(
                markdown_ch,
                markdown_scale,
                i0,
                ctx.theme.geom.text_path.markdown,
            );
            let text_stroke_color = resolve_color_channel_or_theme(
                text_stroke_ch,
                text_stroke_scale,
                i0,
                ctx.theme.geom.text_path.text_stroke.as_ref(),
                &ctx.theme.palette,
            );
            let text_linewidth_pt = resolve_number_channel_or(
                text_linewidth_ch,
                text_linewidth_scale,
                i0,
                ctx.theme.geom.text_path.text_linewidth_pt,
            );
            let outline_stroke = match (text_stroke_color, text_linewidth_pt) {
                (Some(col), pt) if pt > 0.0 => {
                    let stroke_width_px = pt_to_px(pt, ctx.dpi);
                    if stroke_width_px > 0.0 {
                        Some((col, crate::stroke::Stroke::new(stroke_width_px)))
                    } else {
                        None
                    }
                }
                _ => None,
            };

            let fill_color = override_alpha(
                resolve_color_channel_or_theme(
                    fill_ch,
                    fill_scale,
                    i0,
                    ctx.theme.geom.text_path.fill.as_ref(),
                    &ctx.theme.palette,
                ),
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i0),
            )
            .unwrap_or_else(default_fill);

            let offset_pt = resolve_number_channel_or(offset_ch, offset_scale, i0, 0.0);
            let justify_x = resolve_number_channel_or(hjust_ch, hjust_scale, i0, 0.0);
            let upright = resolve_bool_channel_or(upright_ch, upright_scale, i0, false);
            let anchor_y = resolve_number_channel_or(anchor_y_ch, anchor_y_scale, i0, 0.5);
            let angle_user = resolve_angle_channel(angle_ch, angle_scale, i0);
            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);

            // ── Build polyline in panel pixel space. ──
            // Under non-linear projections, edges are densified so the
            // text follows the projected geodesic rather than chords
            // between sample vertices. Cartesian's `interpolate_segment`
            // is a no-op so `points` is identical to the per-row build.
            let is_linear = ctx.projection.is_linear();
            let mut interior: Vec<(f64, f64)> = Vec::new();
            let mut prev_channels: Option<[f64; 2]> = None;
            let mut points: Vec<Point> = Vec::with_capacity(mark.rows.len());
            for &i in &mark.rows {
                let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
                let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
                let x_frac = resolve_position(x_col.get(i), x_scale, x_band);
                let y_frac = resolve_position(y_col.get(i), y_scale, y_band);
                if !x_frac.is_finite() || !y_frac.is_finite() {
                    continue;
                }
                let curr_channels = [x_frac, y_frac];
                if !is_linear {
                    if let Some(prev) = prev_channels {
                        interior.clear();
                        ctx.projection.interpolate_segment(
                            panel,
                            &prev,
                            &curr_channels,
                            &mut interior,
                        );
                        for (ipx, ipy) in &interior {
                            points.push(Point::new(*ipx, *ipy));
                        }
                    }
                }
                let (mut px, mut py) = ctx.projection.project_to_panel_px(panel, &curr_channels);
                if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                    px += pt_to_px(off, ctx.dpi);
                }
                if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                    py -= pt_to_px(off, ctx.dpi);
                }
                points.push(Point::new(px, py));
                prev_channels = Some(curr_channels);
            }
            if points.len() < 2 {
                continue;
            }
            let sampler = PolylineSampler::from_polyline(&points);
            let path_length = sampler.total_length();
            if path_length <= 0.0 {
                continue;
            }

            // ── Shape the text. One line either way: the plain path
            //    never sets a wrap width, and a markdown source is
            //    flattened to a single line of glyphs. ──
            let mut style = TextStyle::new(size_pt as f32)
                .weight(weight)
                .italic(italic)
                .tracking(tracking)
                .underline(underline)
                .strikethrough(strikethrough);
            if let Some(fam) = family {
                style = style.family(fam);
            }
            let (stamps, rules, text_w, ascent_px, descent_px) = if markdown {
                let key = RichKey::new(
                    &text,
                    &style,
                    fill_color,
                    rich_sheet,
                    &ctx.theme.palette,
                    ctx.dpi,
                    RichTextWidth::Natural,
                    HAlign::Start,
                );
                let rich = self.rich_cache.get_or_shape(key, || {
                    RichTextRun::new(
                        &text,
                        &style,
                        fill_color,
                        rich_sheet,
                        &ctx.theme.palette,
                        ctx.dpi,
                    )
                });
                let flat = flatten_rich_run(&rich);
                let stamps: Vec<Stamp> = flat
                    .glyphs
                    .iter()
                    .map(|g| Stamp {
                        id: g.id,
                        x: g.x as f64,
                        advance: g.advance as f64,
                        dy: g.dy as f64,
                        font: g.font.clone(),
                        font_size: g.font_size,
                        color: g.color,
                    })
                    .collect();
                let rules: Vec<Rule> = flat
                    .rules
                    .iter()
                    .map(|r| Rule {
                        x0: r.x0 as f64,
                        x1: r.x1 as f64,
                        dy: r.dy as f64,
                        thickness: r.thickness as f64,
                        color: r.color,
                    })
                    .collect();
                (
                    stamps,
                    rules,
                    flat.width as f64,
                    flat.ascent as f64,
                    flat.descent as f64,
                )
            } else {
                let run = TextRun::new(&text, &style, ctx.dpi);
                let glyphs = run_layout_glyphs(&run);
                if glyphs.is_empty() {
                    continue;
                }
                // Parley's `g.y` includes the line's baseline offset from
                // the layout's top. For text-on-path we want
                // `offset_perp = 0` to mean "glyph baseline sits on the
                // curve", so subtract the line baseline (taken from the
                // first glyph; with single-line single-style text every
                // glyph's y matches).
                let baseline_ref = glyphs[0].y as f64;
                let stamps: Vec<Stamp> = glyphs
                    .iter()
                    .map(|g| Stamp {
                        id: g.id,
                        x: g.x as f64,
                        advance: g.advance as f64,
                        dy: g.y as f64 - baseline_ref,
                        font: g.font.clone(),
                        font_size: g.font_size,
                        color: fill_color,
                    })
                    .collect();
                let rules: Vec<Rule> = run_layout_rules(&run)
                    .iter()
                    .map(|r| Rule {
                        x0: r.x0 as f64,
                        x1: r.x1 as f64,
                        dy: r.dy as f64,
                        thickness: r.thickness as f64,
                        color: fill_color,
                    })
                    .collect();
                // Body metrics for the upright-flip baseline shift. The
                // body extends from y = -ascent (top) to y = +descent
                // (bottom) in glyph-local y-down coords.
                let descent = run.last_line_descender();
                (
                    stamps,
                    rules,
                    run.natural_width(),
                    run.natural_height() - descent,
                    descent,
                )
            };
            if stamps.is_empty() {
                continue;
            }

            // ── Compute global shifts. ──
            let offset_px = pt_to_px(offset_pt, ctx.dpi);
            // `anchor_y` is a fraction of the text's own height, like
            // `TextGeom`'s: 0 puts the top edge on the curve, 1 the
            // bottom. The baseline shift that produces is measured from
            // the curve along the right-of-motion normal.
            let body_h_px = ascent_px + descent_px;

            // ── Upright detection (per-mark, not per-glyph). ──
            //
            // ggplot2's geomtextpath: lay the text out in the natural
            // path direction; if the majority of glyph tangents point
            // into the left half-plane, the text is upside-down → flip
            // the WHOLE TEXT by reversing the path and inverting justify_x.
            // Re-layout against the reversed path. Reading direction
            // along the path is reversed, but every glyph reads
            // right-side-up and the text remains contiguous.
            //
            // We implement the path reversal by remapping the sampled
            // arc-length distance (`d_orig = path_length - d`) and
            // negating the tangent — no second sampler needed.
            let flipped = if upright {
                let natural_shift = justify_x * (path_length - text_w);
                let mut upside_down = 0usize;
                let mut counted = 0usize;
                for g in &stamps {
                    let half_advance = g.advance * 0.5;
                    let d = offset_px + natural_shift + g.x + half_advance;
                    if !d.is_finite() {
                        continue;
                    }
                    let d_clamped = d.clamp(0.0, path_length);
                    if let Some(s) = sampler.sample_at(d_clamped) {
                        counted += 1;
                        if s.tangent.x < 0.0 {
                            upside_down += 1;
                        }
                    }
                }
                counted > 0 && upside_down * 2 > counted
            } else {
                false
            };

            let hjust_shift = if flipped {
                (1.0 - justify_x) * (path_length - text_w)
            } else {
                justify_x * (path_length - text_w)
            };
            // When the whole text is reversed for the upright flip,
            // two perpendicular effects need compensation so the
            // glyph BODY ends up at the same world position as the
            // unflipped case:
            //
            // 1. The right-of-motion normal flips with the reading
            //    direction — `offset_perp` is in that normal's direction,
            //    so negate it to keep the baseline on the same world
            //    side of the curve.
            // 2. Rendered upside-down, the body extends "downward"
            //    from baseline in world (because R(π) maps glyph
            //    local -ascent to world +ascent). Rendered upright,
            //    the body extends "upward" from baseline. To put the
            //    flipped body in the same world bounding box, shift
            //    the baseline by `(ascent - descent)` toward the
            //    region the upside-down body would occupy.
            //
            // Rendered upside-down, the glyph body's top and bottom
            // swap in world space, so the anchor fraction mirrors to
            // `1 - anchor_y` — which is what the second form below
            // expands to.
            let perp_px = if flipped {
                anchor_y * body_h_px - descent_px
            } else {
                ascent_px - anchor_y * body_h_px
            };

            // ── Decoration rules, drawn under the glyphs. Each one
            //    follows the curve at its own perpendicular offset,
            //    stroked at the thickness the font reports. ──
            let geometry = RuleGeometry {
                path_length,
                flipped,
                baseline_perp: perp_px,
                shift: offset_px + hjust_shift,
            };
            for rule in &rules {
                emit_curved_rule(scene, &sampler, &geometry, rule, pick);
            }

            // ── Per-glyph emission. ──
            for g in &stamps {
                let brush = Brush::Solid(g.color);
                let half_advance = g.advance * 0.5;
                let d_glyph = offset_px + hjust_shift + g.x + half_advance;
                if !d_glyph.is_finite() || d_glyph < 0.0 || d_glyph > path_length {
                    continue;
                }
                let d_sample = if flipped {
                    path_length - d_glyph
                } else {
                    d_glyph
                };
                let sample = match sampler.sample_at(d_sample) {
                    Some(s) => s,
                    None => continue,
                };
                // Effective tangent: natural in the non-flipped case;
                // negated when the whole text is reversed. The
                // resulting rotation aligns the glyph's baseline with
                // the (reversed) reading direction, so every glyph
                // reads right-side-up without per-glyph mirroring.
                let tangent = if flipped {
                    -sample.tangent
                } else {
                    sample.tangent
                };
                let theta_tangent = tangent.y.atan2(tangent.x);
                // The user `angle` channel is math CCW. Screen y-down
                // inverts that → negate.
                let theta = theta_tangent + (-angle_user);

                let xform = Affine::translate(Vec2::new(sample.point.x, sample.point.y))
                    * Affine::rotate(theta)
                    * Affine::translate(Vec2::new(-half_advance, perp_px + g.dy));

                let glyph = Glyph {
                    id: g.id,
                    x: 0.0,
                    y: 0.0,
                };
                if let Some((stroke_color, stroke)) = &outline_stroke {
                    let stroke_brush = Brush::Solid(*stroke_color);
                    let stroke_run = GlyphRun {
                        font: &g.font,
                        font_size: g.font_size,
                        transform: xform,
                        glyph_transform: None,
                        brush: &stroke_brush,
                        brush_alpha: 1.0,
                        hint: false,
                        glyphs: std::slice::from_ref(&glyph),
                        style: Some(stroke),
                    };
                    scene.draw_glyphs(&stroke_run, crate::pick::PickId::Skip);
                }
                let glyph_run = GlyphRun {
                    font: &g.font,
                    font_size: g.font_size,
                    transform: xform,
                    glyph_transform: None,
                    brush: &brush,
                    brush_alpha: 1.0,
                    hint: false,
                    glyphs: std::slice::from_ref(&glyph),
                    style: None,
                };
                scene.draw_glyphs(&glyph_run, pick);
            }
        }
    }
}

// ─── Decoration rules ────────────────────────────────────────────────────────

/// Where a label ended up on its curve — everything its decoration
/// rules need beyond the rule itself.
struct RuleGeometry {
    /// Arc length available on the curve.
    path_length: f64,
    /// Whether the upright flip reversed the reading direction.
    flipped: bool,
    /// Perpendicular offset of the label's baseline from the curve.
    baseline_perp: f64,
    /// Arc-length shift every glyph of the label received — the
    /// `"offset"` channel plus the justification slack.
    shift: f64,
}

/// Stroke one underline / strikethrough rule along the curve.
///
/// The rule bends with the path rather than rotating with the per-mark
/// `"angle"`, so it stays parallel to the curve the text sits on. Its
/// span is clamped to the path, and the polyline keeps every original
/// vertex inside it so a corner stays a corner.
fn emit_curved_rule(
    scene: &mut dyn SceneBuilder,
    sampler: &PolylineSampler,
    geometry: &RuleGeometry,
    rule: &Rule,
    pick: crate::pick::PickId,
) {
    let RuleGeometry {
        path_length,
        flipped,
        baseline_perp,
        shift,
    } = *geometry;
    let thickness = rule.thickness;
    let perp = baseline_perp + rule.dy;
    let (d0, d1) = (shift + rule.x0, shift + rule.x1);
    if !thickness.is_finite() || thickness <= 0.0 || !d0.is_finite() || !d1.is_finite() {
        return;
    }
    let lo = d0.clamp(0.0, path_length);
    let hi = d1.clamp(0.0, path_length);
    if hi <= lo {
        return;
    }
    // Reversing the reading direction also reverses the normal the
    // glyph transforms are offset along, so the rule mirrors with it.
    let (s0, s1, sign) = if flipped {
        (path_length - hi, path_length - lo, -1.0)
    } else {
        (lo, hi, 1.0)
    };
    let mut distances = Vec::with_capacity(4);
    distances.push(s0);
    distances.extend(sampler.segment_boundaries_between(s0, s1));
    distances.push(s1);
    let mut points: Vec<Point> = Vec::with_capacity(distances.len());
    for d in distances {
        let Some(sample) = sampler.sample_at(d) else {
            continue;
        };
        let t = sample.tangent;
        let normal = Vec2::new(-t.y, t.x);
        points.push(sample.point + normal * (sign * perp));
    }
    if points.len() < 2 {
        return;
    }
    let path = crate::primitives::polyline_path(&points);
    scene.stroke(
        &crate::stroke::Stroke::new(thickness),
        Affine::IDENTITY,
        &Brush::Solid(rule.color),
        None,
        &path,
        pick,
    );
}

// ─── Channel helpers (mirrored from TextGeom) ────────────────────────────────

fn resolve_str_opt(
    channel: Option<&Channel>,
    scale: Option<&crate::plot::scale::Scale>,
    i: usize,
) -> Option<String> {
    let ch = channel?;
    let (raw, bypass) = match ch {
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

fn resolve_italic(
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
    use crate::geometry::Rect;
    use crate::plot::geom::{DirectScaleResolver, Raw};
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

    fn drained(g: &TextPathGeom) -> RecordingScene {
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 400.0, 400.0), &shapes, &scales),
        );
        scene
    }

    fn glyph_ops(scene: &RecordingScene) -> Vec<&crate::scene::recording::OwnedGlyphRun> {
        scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(run) => Some(run),
                _ => None,
            })
            .collect()
    }

    /// Every stroked decoration rule in the scene, as
    /// `(stroke width, vertices)`.
    fn rule_ops(scene: &RecordingScene) -> Vec<(f64, Vec<Point>)> {
        scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, path, .. } => Some((
                    stroke.width,
                    path.elements()
                        .iter()
                        .filter_map(|el| match el {
                            crate::path::PathEl::MoveTo(p) | crate::path::PathEl::LineTo(p) => {
                                Some(*p)
                            }
                            _ => None,
                        })
                        .collect(),
                )),
                _ => None,
            })
            .collect()
    }

    /// Baseline y of every emitted glyph, in scene order.
    fn glyph_baselines(scene: &RecordingScene) -> Vec<f64> {
        glyph_ops(scene)
            .iter()
            .map(|r| r.transform.as_coeffs()[5])
            .collect()
    }

    // ── build() validation ──

    #[test]
    #[should_panic(expected = "missing required channel \"x\"")]
    fn builder_missing_x_panics() {
        TextPathGeom::builder()
            .set("y", vec![0.0_f64, 1.0])
            .set("text", "hi")
            .build();
    }

    #[test]
    #[should_panic(expected = "missing required channel \"text\"")]
    fn builder_missing_text_panics() {
        TextPathGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_mismatched_xy_panics() {
        TextPathGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64])
            .set("text", "hi")
            .build();
    }

    // ── Draw output ──

    /// Panel is 400×400, so x_frac=0.25→px=100, x_frac=0.75→px=300,
    /// y_frac=0.5→py=200. A horizontal path from (100, 200) to (300, 200).
    fn horizontal_path_geom(text: &'static str) -> TextPathGeom {
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", text)
            .set("size", 20.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        g
    }

    /// Same path as [`horizontal_path_geom`], with the label read as
    /// markdown.
    fn markdown_path_geom(text: &'static str) -> TextPathGeom {
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", text)
            .set("size", 20.0_f64)
            .set("markdown", true)
            .build();
        g.rebuild_diff_against_previous();
        g
    }

    #[test]
    fn fill_opacity_overrides_the_glyph_fill_alpha() {
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "AB")
            .set("size", 20.0_f64)
            .set("fill", crate::color::Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("fill_opacity", 0.35_f64)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let runs = glyph_ops(&scene);
        assert!(!runs.is_empty(), "expected glyph runs");
        for run in runs {
            let crate::brush::Brush::Solid(c) = run.brush else {
                panic!("expected a solid glyph brush");
            };
            assert!((c.components[3] - 0.35).abs() < 1e-6, "fill alpha {c:?}");
        }
    }

    #[test]
    fn single_glyph_anchor_on_horizontal_path() {
        // Path runs from (100, 200) to (300, 200) — horizontal, length
        // 200 px. Single-char text with justify_x = 0: glyph's CENTRE lands
        // at offset + half_advance (the glyph is centred on its
        // arc-length sample point). The composite affine's translation
        // is the glyph's LEFT-baseline position, which therefore equals
        // sample.point - (half_advance, 0). For the first glyph at
        // d_glyph = half_advance, that's exactly (100, 200).
        let g = horizontal_path_geom("A");
        let scene = drained(&g);
        let runs = glyph_ops(&scene);
        assert_eq!(runs.len(), 1);
        let coeffs = runs[0].transform.as_coeffs();
        // a = cos(theta), b = sin(theta), c = -sin(theta), d = cos(theta)
        assert!(coeffs[0] > 0.99, "cos(theta) = {}", coeffs[0]);
        assert!(coeffs[1].abs() < 0.01, "sin(theta) = {}", coeffs[1]);
        // The default `anchor_y = 0.5` centres the glyph body on the
        // path rather than putting the baseline on it, so the baseline
        // sits below by half the body height (descenders excepted).
        let ty = coeffs[5];
        assert!(
            ty > 200.0 && ty < 200.0 + 20.0 * 96.0 / 72.0,
            "expected the baseline just below the path at y = 200, got {ty}"
        );
        // Translation x component = left edge of glyph = sample.point.x
        // - half_advance. For glyph-0 with justify_x=0, offset=0:
        // sample.point.x = 100 + half_advance, so tx ≈ 100.
        let tx = coeffs[4];
        assert!(
            (tx - 100.0).abs() < 1.0,
            "expected left-edge x ~= 100, got {tx}"
        );
    }

    #[test]
    fn vertical_path_rotates_glyphs_by_quarter_turn() {
        // Path running downward (screen +y) — tangent (0, +1).
        // y_frac=0.75→py=100 (top), y_frac=0.25→py=300 (bottom).
        // theta = atan2(1, 0) = π/2. Affine::rotate(π/2) has
        // a=cos(π/2)≈0, b=sin(π/2)=1, c=-sin(π/2)=-1, d=cos(π/2)≈0.
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.5_f64, 0.5]))
            .set("y", Raw(vec![0.75_f64, 0.25]))
            .set("text", "X")
            .set("size", 20.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let runs = glyph_ops(&scene);
        assert_eq!(runs.len(), 1);
        let coeffs = runs[0].transform.as_coeffs();
        assert!(coeffs[0].abs() < 0.01, "cos(theta) = {}", coeffs[0]);
        assert!((coeffs[1] - 1.0).abs() < 0.01, "sin(theta) = {}", coeffs[1]);
    }

    #[test]
    fn hjust_zero_packs_text_to_start() {
        // Multi-char text on a long horizontal path with justify_x = 0. The
        // first glyph's transform x component should sit close to the
        // path's start (100 px) plus its own half-advance.
        let g = horizontal_path_geom("hello");
        let scene = drained(&g);
        let runs = glyph_ops(&scene);
        assert!(runs.len() >= 5);
        let first_tx = runs[0].transform.as_coeffs()[4];
        // half_advance of an 'h' at 20pt is typically ~6-8 px; allow a
        // generous tolerance and check the first glyph lands close to
        // x=100, not near the centre or end.
        assert!(
            (100.0..130.0).contains(&first_tx),
            "first glyph tx = {first_tx} (expected ~[100, 130))"
        );
    }

    #[test]
    fn hjust_half_centers_text() {
        // justify_x = 0.5 should centre the text around the path midpoint.
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "centerme")
            .set("size", 20.0_f64)
            .set("justify_x", 0.5_f64)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let runs = glyph_ops(&scene);
        assert!(!runs.is_empty());
        // Midpoint of text is at index runs.len() / 2; its x position
        // should be near 200 (path midpoint).
        let mid_idx = runs.len() / 2;
        let mid_tx = runs[mid_idx].transform.as_coeffs()[4];
        assert!(
            (mid_tx - 200.0).abs() < 25.0,
            "midpoint glyph tx = {mid_tx} (expected near 200)"
        );
    }

    #[test]
    fn hjust_one_packs_text_to_end() {
        // justify_x = 1.0 should place the LAST glyph near the path end (x ~= 300).
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "abc")
            .set("size", 20.0_f64)
            .set("justify_x", 1.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let runs = glyph_ops(&scene);
        assert!(runs.len() >= 3);
        let last_tx = runs.last().unwrap().transform.as_coeffs()[4];
        assert!(
            last_tx > 270.0 && last_tx <= 300.0,
            "last glyph tx = {last_tx} (expected near 300)"
        );
    }

    #[test]
    fn offset_shifts_layout_along_path() {
        // offset = 50 pt at 96 dpi → 66.7 px shift along the path.
        // First glyph should land further down the path than offset=0.
        let baseline = horizontal_path_geom("ab");
        let mut shifted = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "ab")
            .set("size", 20.0_f64)
            .set("offset", 50.0_f64)
            .build();
        shifted.rebuild_diff_against_previous();
        let s0 = drained(&baseline);
        let s1 = drained(&shifted);
        let tx0 = glyph_ops(&s0)[0].transform.as_coeffs()[4];
        let tx1 = glyph_ops(&s1)[0].transform.as_coeffs()[4];
        let expected_delta_px = 50.0 * 96.0 / 72.0;
        assert!(
            (tx1 - tx0 - expected_delta_px).abs() < 1.0,
            "expected shift {expected_delta_px}, got {}",
            tx1 - tx0
        );
    }

    #[test]
    fn anchor_y_moves_the_text_across_the_path_by_its_own_height() {
        // `anchor_y` is a fraction of the text's height, so 0 (top edge
        // on the curve) and 1 (bottom edge on it) must straddle the
        // path, and the gap between the two baselines is exactly the
        // text's height.
        let baseline_at = |anchor: f64| {
            let mut g = TextPathGeom::builder()
                .set("x", Raw(vec![0.25_f64, 0.75]))
                .set("y", Raw(vec![0.5_f64, 0.5]))
                .set("text", "X")
                .set("size", 20.0_f64)
                .set("anchor_y", anchor)
                .build();
            g.rebuild_diff_against_previous();
            let scene = drained(&g);
            glyph_ops(&scene)[0].transform.as_coeffs()[5]
        };
        let top = baseline_at(0.0);
        let mid = baseline_at(0.5);
        let bottom = baseline_at(1.0);
        // Path sits at y = 200. Anchoring the top edge on it pushes the
        // baseline below; anchoring the bottom edge lifts it above.
        assert!(top > 200.0, "top-anchored baseline {top} should sit below");
        assert!(
            bottom < 200.0,
            "bottom-anchored baseline {bottom} should sit above"
        );
        // The centred case is the midpoint of the two extremes.
        assert!(
            (mid - 0.5 * (top + bottom)).abs() < 0.5,
            "centred baseline {mid} should be midway between {top} and {bottom}"
        );
    }

    #[test]
    fn upright_reverses_reading_along_path() {
        // Path runs right-to-left in screen (start x=300, end x=100,
        // length 200 px). With upright off, glyph 0 sits at the
        // START of the path (rightmost, x≈300) and reads upside-down
        // because the tangent points left. With upright on, the whole
        // text is laid out against the REVERSED path: the text still
        // occupies the same physical arc-length region (since justify_x=0
        // is preserved as 1-justify_x=1 on the reversed walk, which
        // brings the text back to the same physical span), but
        // reading direction reverses. Glyph 0 now sits at what was
        // the FAR END of the natural text region (around x≈250 for a
        // ~50px-wide text), reading left-to-right toward x≈300.
        let common = |upright: bool| -> RecordingScene {
            let mut g = TextPathGeom::builder()
                .set("x", Raw(vec![0.75_f64, 0.25]))
                .set("y", Raw(vec![0.5_f64, 0.5]))
                .set("text", "abcde")
                .set("size", 20.0_f64)
                .set("upright", upright)
                .build();
            g.rebuild_diff_against_previous();
            drained(&g)
        };
        let s_off = common(false);
        let s_on = common(true);
        let off = glyph_ops(&s_off);
        let on = glyph_ops(&s_on);
        assert!(off.len() >= 5 && on.len() >= 5);

        // Without upright: glyph 0's tangent rotation aligns local +x
        // with -x_world. cos(theta) ≈ -1 — upside-down glyph.
        let off0 = off[0].transform.as_coeffs();
        assert!(off0[0] < -0.95, "without upright cos = {}", off0[0]);
        // With upright: effective tangent is reversed, cos(theta) ≈ +1.
        // Every glyph in the run reads upright (no per-glyph flips).
        for r in &on {
            let c = r.transform.as_coeffs();
            assert!(
                c[0] > 0.95,
                "every upright glyph reads upright: cos = {}",
                c[0]
            );
        }

        // Glyph 0 swaps its end of the text region: without upright it's
        // near the path start (x≈300); with upright it's at the FAR
        // end of the same text region (lower x). Glyph N (last) sits
        // near x≈300 in the upright case — that's where reading starts
        // from for the reversed walk.
        let off_x = off0[4];
        let on_x = on[0].transform.as_coeffs()[4];
        let on_last_x = on.last().unwrap().transform.as_coeffs()[4];
        assert!(
            off_x > 280.0,
            "without upright, glyph 0 near start of path: off_x = {off_x}"
        );
        assert!(
            on_x < off_x - 30.0,
            "with upright, glyph 0 should land toward the far end of \
             the natural text region (smaller x than off_x): \
             on_x = {on_x}, off_x = {off_x}"
        );
        assert!(
            on_last_x > on_x + 30.0,
            "with upright, glyph N reads further along the reversed \
             walk (larger x in world): on_last_x = {on_last_x}, \
             on_x = {on_x}"
        );
    }

    #[test]
    fn upright_flips_glyphs_on_backwards_tangent() {
        // Path running right-to-left (tangent points -x) — without
        // upright, the glyph rotates by π (text upside-down). With
        // upright, the glyph adds another π, returning rotation to 0.
        // x_frac=0.75→px=300 (start), x_frac=0.25→px=100 (end).
        let mut without = TextPathGeom::builder()
            .set("x", Raw(vec![0.75_f64, 0.25]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "X")
            .set("size", 20.0_f64)
            .build();
        without.rebuild_diff_against_previous();
        let mut with_ = TextPathGeom::builder()
            .set("x", Raw(vec![0.75_f64, 0.25]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "X")
            .set("size", 20.0_f64)
            .set("upright", true)
            .build();
        with_.rebuild_diff_against_previous();
        let s0 = drained(&without);
        let s1 = drained(&with_);
        let c0 = glyph_ops(&s0)[0].transform.as_coeffs();
        let c1 = glyph_ops(&s1)[0].transform.as_coeffs();
        // Without upright: cos(theta)≈-1, sin(theta)≈0.
        assert!(c0[0] < -0.99, "without upright cos = {}", c0[0]);
        // With upright: cos(theta)≈+1, sin(theta)≈0.
        assert!(c1[0] > 0.99, "with upright cos = {}", c1[0]);
    }

    #[test]
    fn glyphs_outside_path_range_are_dropped() {
        // Very short path (10 px), long text. Most glyphs should fall
        // beyond the path end and be dropped.
        // x_frac=0.25→px=100, x_frac=0.275→px=110, span 10 px.
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.275]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "this is way too long for that path")
            .set("size", 20.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let n_rendered = glyph_ops(&scene).len();
        // At 20pt the text would naturally be hundreds of px wide; only
        // a handful of glyphs should fit in 10 px.
        assert!(
            n_rendered < 5,
            "expected few glyphs to fit in a 10px path; got {n_rendered}"
        );
    }

    #[test]
    fn empty_text_skips_mark() {
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "")
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        assert_eq!(glyph_ops(&scene).len(), 0);
    }

    #[test]
    fn single_vertex_mark_skipped() {
        // Two rows of the same key but identical positions → zero-length
        // polyline → mark skipped.
        let mut g = TextPathGeom::builder()
            .keys(vec!["A", "A"])
            .set("x", Raw(vec![0.25_f64, 0.25]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "label")
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        assert_eq!(glyph_ops(&scene).len(), 0);
    }

    #[test]
    fn per_mark_grouping_emits_one_label_per_key() {
        // Two keys A and B, each defining a separate horizontal path
        // (parallel at different y values). Each mark gets its own text.
        // A at y=100 (y_frac=0.75), B at y=300 (y_frac=0.25). x spans
        // 100→200 (x_frac 0.25→0.5).
        let mut g = TextPathGeom::builder()
            .keys(vec!["A", "A", "B", "B"])
            .set("x", Raw(vec![0.25_f64, 0.5, 0.25, 0.5]))
            .set("y", Raw(vec![0.75_f64, 0.75, 0.25, 0.25]))
            .set("text", vec!["one", "one", "two", "two"])
            .set("size", 16.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        // Each mark contributes its own glyphs; total ≥ 3 + 3 = 6.
        assert!(glyph_ops(&scene).len() >= 6);
        // Confirm vertical separation between the two marks.
        let ys: Vec<f64> = glyph_ops(&scene)
            .iter()
            .map(|r| r.transform.as_coeffs()[5])
            .collect();
        let min_y = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (max_y - min_y - 200.0).abs() < 5.0,
            "expected ~200px y separation between marks, got {}",
            max_y - min_y
        );
    }

    #[test]
    fn pick_id_propagates_to_all_glyphs() {
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "abc")
            .set("pick_id", 42_i64)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let runs = glyph_ops(&scene);
        assert!(!runs.is_empty());
        for r in &runs {
            match r.pick_id {
                crate::pick::PickId::Id(n) => assert_eq!(n, 42),
                other => panic!("expected PickId::Id(42), got {other:?}"),
            }
        }
    }

    // ── Markdown labels ──

    #[test]
    fn markdown_consumes_the_inline_markup() {
        // `**AB**` is six characters plain, two glyphs as markdown.
        let plain = drained(&horizontal_path_geom("**AB**"));
        let rich = drained(&markdown_path_geom("**AB**"));
        assert_eq!(glyph_ops(&plain).len(), 6);
        assert_eq!(glyph_ops(&rich).len(), 2);
    }

    #[test]
    fn markdown_span_colour_reaches_the_glyph_brush() {
        let scene = drained(&markdown_path_geom("{#ff0000 A}"));
        let runs = glyph_ops(&scene);
        assert_eq!(runs.len(), 1);
        let Brush::Solid(c) = &runs[0].brush else {
            panic!("expected a solid brush, got {:?}", runs[0].brush);
        };
        assert!(
            c.components[0] > 0.9 && c.components[1] < 0.1 && c.components[2] < 0.1,
            "expected red, got {:?}",
            c.components
        );
    }

    #[test]
    fn markdown_blocks_join_with_a_space() {
        // Two paragraphs flatten onto one line, separated by a space —
        // so the gap between the two glyphs exceeds the first glyph's
        // own advance.
        let joined = drained(&markdown_path_geom("a\n\nb"));
        let contiguous = drained(&markdown_path_geom("ab"));
        let gap = |scene: &RecordingScene| {
            let runs = glyph_ops(scene);
            assert_eq!(runs.len(), 2);
            runs[1].transform.as_coeffs()[4] - runs[0].transform.as_coeffs()[4]
        };
        let joined_gap = gap(&joined);
        let contiguous_gap = gap(&contiguous);
        assert!(
            joined_gap > contiguous_gap + 3.0,
            "block join should insert a space: {joined_gap} vs {contiguous_gap}"
        );
    }

    #[test]
    fn superscript_lifts_its_glyph_off_the_baseline() {
        // Pulldown-cmark wants word boundaries around the carets.
        let scene = drained(&markdown_path_geom("a ^2^ b"));
        let baselines = glyph_baselines(&scene);
        assert!(baselines.len() >= 3, "got {baselines:?}");
        let body = baselines.iter().cloned().fold(f64::MIN, f64::max);
        let lifted = baselines.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            lifted < body - 3.0,
            "the superscript should sit above the body baseline: {baselines:?}"
        );
    }

    // ── Decoration rules ──

    #[test]
    fn underline_strokes_one_rule_along_the_path() {
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "AB")
            .set("size", 20.0_f64)
            .set("underline", true)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let rules = rule_ops(&scene);
        assert_eq!(rules.len(), 1, "expected exactly one underline");
        let (width, points) = &rules[0];
        // Thickness comes from the font, so it is positive and well
        // under the em size.
        assert!(
            *width > 0.0 && *width < 20.0 * 96.0 / 72.0,
            "unexpected rule thickness {width}"
        );
        assert!(points.len() >= 2, "got {points:?}");
        // The rule starts where the text starts and sits below the
        // baseline the glyphs were stamped on.
        assert!(
            (points[0].x - 100.0).abs() < 1.0,
            "rule should start at the path start, got {:?}",
            points[0]
        );
        let baseline = glyph_baselines(&scene)[0];
        assert!(
            points[0].y > baseline,
            "underline {:?} should sit below the baseline {baseline}",
            points[0]
        );
    }

    #[test]
    fn underline_and_strikethrough_emit_a_rule_each() {
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "AB")
            .set("size", 20.0_f64)
            .set("underline", true)
            .set("strikethrough", true)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let rules = rule_ops(&scene);
        assert_eq!(rules.len(), 2);
        // The strikethrough crosses the body, the underline sits under
        // it, so they land on opposite sides of the baseline.
        let baseline = glyph_baselines(&scene)[0];
        let above = rules.iter().filter(|(_, p)| p[0].y < baseline).count();
        let below = rules.iter().filter(|(_, p)| p[0].y > baseline).count();
        assert_eq!((above, below), (1, 1), "rules at {rules:?}");
    }

    #[test]
    fn a_markdown_rule_covers_only_its_own_span() {
        // Only the trailing span is underlined. On a straight path the
        // rule's start therefore lands on the left edge of the span's
        // first glyph — glyph 3 of `A A space B B` — rather than under
        // the whole label.
        let scene = drained(&markdown_path_geom("AA _BB_"));
        let rules = rule_ops(&scene);
        assert_eq!(rules.len(), 1);
        let glyphs = glyph_ops(&scene);
        assert_eq!(glyphs.len(), 5, "expected `AA BB` to shape to 5 glyphs");
        let span_start = glyphs[3].transform.as_coeffs()[4];
        assert!(
            (rules[0].1[0].x - span_start).abs() < 2.0,
            "rule should start at the span, not the label: {:?} vs {span_start}",
            rules[0].1[0]
        );
        // …and end at the label's end, since the span runs to it.
        let last = rules[0].1.last().unwrap();
        assert!(
            last.x > span_start,
            "rule should run forward from {span_start}, got {last:?}"
        );
    }

    #[test]
    fn an_upright_flip_keeps_the_rule_on_the_same_side() {
        // Forward path: the underline sits a fixed distance under the
        // baseline. A backwards path with `upright` reverses reading
        // direction, and the rule has to mirror with the glyphs so it
        // stays on the same side of the curve.
        let rule_offset = |xs: Vec<f64>, upright: bool| {
            let mut g = TextPathGeom::builder()
                .set("x", Raw(xs))
                .set("y", Raw(vec![0.5_f64, 0.5]))
                .set("text", "AB")
                .set("size", 20.0_f64)
                .set("underline", true)
                .set("upright", upright)
                .build();
            g.rebuild_diff_against_previous();
            let scene = drained(&g);
            let rules = rule_ops(&scene);
            assert_eq!(rules.len(), 1);
            let baseline = glyph_baselines(&scene)[0];
            rules[0].1[0].y - baseline
        };
        let forward = rule_offset(vec![0.25, 0.75], false);
        let flipped = rule_offset(vec![0.75, 0.25], true);
        assert!(
            (forward - flipped).abs() < 0.5,
            "rule offset should survive the flip: {forward} vs {flipped}"
        );
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = TextPathGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0]))
            .set("y", Raw(vec![0.0_f64, 1.0]))
            .set("text", "x")
            .set("justify_x", 0.0_f64)
            .set("upright", false)
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(names.contains(&"text"));
        assert!(names.contains(&"justify_x"));
        assert!(names.contains(&"upright"));
    }
}
