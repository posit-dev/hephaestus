//! `TextFitGeom` — vectorised text labels that **scale font size** to
//! fit inside a target rect.
//!
//! Sibling of [`TextGeom`]. The user supplies a target rect via
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

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Rect};
use crate::path::FillRule;
use crate::plot::value::Value;
use crate::primitives::{rect as rect_path, rounded_rect};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};
use crate::text::{draw_text, Alignment, TextRun, TextStyle};

use super::resolve::{
    override_alpha, pt_to_px, resolve_angle_channel, resolve_color_channel, resolve_number_channel,
    resolve_number_channel_or, resolve_pick_id, resolve_position,
};
use super::state::{
    filter_declared, require_data_column, validate_channel_lengths, validate_pick_id_channel,
    GeomState, KeysStrategy,
};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_MIN_FONT_PT: f64 = 6.0;
const DEFAULT_MAX_FONT_PT: f64 = 96.0;
const DEFAULT_WEIGHT: u16 = 400;
const DEFAULT_BG_LINEWIDTH_PT: f64 = 1.0;
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

// ─── TextFitGeom ─────────────────────────────────────────────────────────────

/// A vectorised fit-text-to-rect geom. One fitted label per row.
pub struct TextFitGeom {
    pub(crate) state: GeomState,
}

crate::impl_geom_inherents!(TextFitGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for TextFitGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "TextFitGeom").len();
        for name in ["y", "x2", "y2"] {
            let len = require_data_column(name, &channels, "TextFitGeom").len();
            if len != n {
                panic!(
                    "TextFitGeom::build: \"{name}\" length {len} does not match \"x\" length {n}"
                );
            }
        }
        require_data_column("text", &channels, "TextFitGeom");
        validate_channel_lengths(&channels, n, "TextFitGeom");
        validate_pick_id_channel(&channels, "TextFitGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::PerRow, declared);
        TextFitGeom { state }
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
                .unwrap_or(DEFAULT_WEIGHT);
            let italic = resolve_bool_or_italic_string(italic_ch, italic_scale, i);
            let family = resolve_str_channel(family_ch, family_scale, i);

            let min_pt =
                resolve_number_channel_or(min_font_ch, min_font_scale, i, DEFAULT_MIN_FONT_PT)
                    .max(0.5);
            let max_pt =
                resolve_number_channel_or(max_font_ch, max_font_scale, i, DEFAULT_MAX_FONT_PT)
                    .max(min_pt);
            let min_px = pt_to_px(min_pt, ctx.dpi) as f32;
            let max_px = pt_to_px(max_pt, ctx.dpi) as f32;

            // ── Justification — locked before the fit; affects line
            // alignment inside the wrap box at every search iteration.
            let justify_x = resolve_justify_x(justify_x_ch, justify_x_scale, i);
            let justify_y_frac = resolve_justify_y_frac(justify_y_ch, justify_y_scale, i);

            // ── Binary-search the font size. ──
            let make_style = |size_px: f32| {
                let mut s = TextStyle::new(size_px).weight(weight).italic(italic);
                if let Some(f) = &family {
                    s = s.family(f);
                }
                s
            };

            let mut lo = min_px;
            let mut hi = max_px;
            let mut best: Option<(TextRun, f64, f64, f32)> = None;
            for _ in 0..MAX_ITERS {
                let mid = 0.5 * (lo + hi);
                let style = make_style(mid);
                let run = TextRun::new(&text, &style);
                run.set_max_width(rect_w as f32, justify_x);
                let w = run.content_width();
                let h = run.current_height();
                if w <= rect_w && h <= rect_h {
                    lo = mid;
                    best = Some((run, w, h, mid));
                } else {
                    hi = mid;
                }
            }

            // If no candidate fit, draw at min and clip to the rect.
            let (run, content_w, content_h, _size_px, fits) = match best {
                Some((r, w, h, s)) => (r, w, h, s, true),
                None => {
                    let style = make_style(min_px);
                    let run = TextRun::new(&text, &style);
                    run.set_max_width(rect_w as f32, justify_x);
                    let w = run.content_width();
                    let h = run.current_height();
                    (run, w, h, min_px, false)
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

            // ── Fill colour. ──
            let fill_color = override_alpha(
                resolve_color_channel(fill_ch, fill_scale, i),
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i),
            )
            .unwrap_or_else(default_fill);

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
                            DEFAULT_BG_LINEWIDTH_PT,
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
            draw_text(
                scene,
                &run,
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
) -> Alignment {
    let s = match resolve_str_channel(channel, scale, i) {
        Some(s) => s,
        None => return Alignment::Start,
    };
    match s.as_str() {
        "start" => Alignment::Start,
        "center" | "centre" | "middle" => Alignment::Center,
        "end" => Alignment::End,
        "justify" | "justified" => Alignment::Justify,
        _ => Alignment::Start,
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
}
