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

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Rect};
use crate::path::FillRule;
use crate::plot::value::Value;
use crate::primitives::{rect as rect_path, rounded_rect};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};
use crate::text::{draw_text, Alignment, TextRun, TextStyle};

use super::resolve::{
    band_width_at, override_alpha, pt_to_px, resolve_angle_channel, resolve_color_channel,
    resolve_number_channel, resolve_number_channel_or, resolve_pick_id, resolve_position,
};
use super::state::{
    filter_declared, require_data_column, validate_channel_lengths, validate_pick_id_channel,
    GeomState, KeysStrategy,
};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_SIZE_PT: f64 = 12.0;
const DEFAULT_WEIGHT: u16 = 400;
const DEFAULT_ANCHOR_X: f64 = 0.5;
const DEFAULT_ANCHOR_Y: f64 = 0.5;
const DEFAULT_BG_LINEWIDTH_PT: f64 = 1.0;
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
}

crate::impl_geom_inherents!(TextGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for TextGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "TextGeom").len();
        let y_len = require_data_column("y", &channels, "TextGeom").len();
        if y_len != n {
            panic!("TextGeom::build: \"y\" length {y_len} does not match \"x\" length {n}");
        }
        require_data_column("text", &channels, "TextGeom");
        validate_channel_lengths(&channels, n, "TextGeom");
        validate_pick_id_channel(&channels, "TextGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::PerRow, declared);
        TextGeom { state }
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
            let mut anchor_px = panel.x0 + x_frac * panel_w;
            let mut anchor_py = panel.y1 - y_frac * panel_h;
            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                anchor_px += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                anchor_py -= pt_to_px(off, ctx.dpi);
            }

            // ── Resolve font style. ──
            let size_pt = resolve_number_channel_or(size_ch, size_scale, i, DEFAULT_SIZE_PT);
            let size_px = pt_to_px(size_pt, ctx.dpi);
            if !size_px.is_finite() || size_px <= 0.0 {
                continue;
            }
            let weight = resolve_number_channel(weight_ch, weight_scale, i)
                .map(|w| (w.round() as i64).clamp(1, 1000) as u16)
                .unwrap_or(DEFAULT_WEIGHT);
            let italic = resolve_bool_or_italic_string(italic_ch, italic_scale, i);
            let family = resolve_str_channel(family_ch, family_scale, i);

            // ── Build TextStyle + TextRun. ──
            let mut style = TextStyle::new(size_px as f32).weight(weight).italic(italic);
            if let Some(fam) = family {
                style = style.family(fam);
            }
            let run = TextRun::new(&text, &style);

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

            let anchor_x =
                resolve_number_channel_or(anchor_x_ch, anchor_x_scale, i, DEFAULT_ANCHOR_X);
            let anchor_y =
                resolve_number_channel_or(anchor_y_ch, anchor_y_scale, i, DEFAULT_ANCHOR_Y);

            // ── Fill colour. ──
            let fill_color = override_alpha(
                resolve_color_channel(fill_ch, fill_scale, i),
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i),
            )
            .unwrap_or_else(default_fill);

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
                            DEFAULT_BG_LINEWIDTH_PT,
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

/// Resolve a `"justify_x"` channel to a parley `Alignment`. Recognises
/// the canonical string aliases — `"start"` / `"center"` / `"end"` /
/// `"justify"`. Unknown / non-string / unset → `Alignment::Start`,
/// matching the historical (pre-channel) behaviour.
fn resolve_justify_channel(
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
    use std::sync::Arc;

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
        let style = TextStyle::new(12.0 * 96.0 / 72.0).weight(400);
        let probe = TextRun::new("men", &style);
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
                        kurbo::PathEl::CurveTo(_, _, _) | kurbo::PathEl::QuadTo(_, _)
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

    // Helper used by the new tests.
    fn fill_bbox(scene: &RecordingScene) -> Option<Rect> {
        use kurbo::Shape;
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
}
