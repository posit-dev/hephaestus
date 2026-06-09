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
//! - Single line only. No `max_width` / wrapping; multi-line text on a
//!   curve is not supported.
//! - Glyphs whose computed arc-length distance falls outside the
//!   `[0, total_length]` range are dropped (no partial stamping).
//! - The mark must contribute at least two finite vertices; otherwise
//!   the whole mark is skipped silently.
//!
//! Channels consumed:
//!
//! - `"x"` / `"y"` — vertex position (required; data; numeric, per row).
//! - `"text"` — label string (required; per mark; resolved at the mark's
//!   first row).
//! - `"size"` — font size in pt (optional; default 12pt; per mark).
//! - `"weight"` — CSS font weight (optional; default 400; per mark).
//! - `"italic"` — boolean (optional; default false; per mark). Accepts a
//!   `Value::Bool` or the conventional `"italic"` / `"normal"` strings.
//! - `"family"` — font family name (optional; per mark).
//! - `"fill"` — glyph colour (optional; default black; per mark).
//! - `"alpha"` — overrides the alpha component of `"fill"` (optional;
//!   `0..=1`; per mark).
//! - `"offset"` — pt offset along the path where the text layout starts
//!   (optional; default `0.0`; per mark). Positive values shift text
//!   forward along the path.
//! - `"hjust"` — fraction in `[0, 1]` of the available whitespace
//!   (`path_length - text_width`) to pad at the start of the text
//!   (optional; default `0.0`; per mark). `0.0` = text starts at the
//!   offset point, `0.5` = centred, `1.0` = text ends at the offset
//!   point plus the path length. Values outside `[0, 1]` are honoured
//!   literally — out-of-range glyphs are dropped per the limitation above.
//! - `"upright"` — boolean (optional; default false; per mark). When
//!   true, the layout checks whether the majority of glyph tangents
//!   point into the left half-plane (i.e., the text would render
//!   upside-down as a whole). If so, the entire text is laid out
//!   against the *reversed* path with `hjust` inverted — every glyph
//!   then reads right-side-up and reading direction along the path
//!   reverses. This is a per-mark decision (the whole text flips
//!   together or not at all); no mid-text orientation changes.
//!   Matches ggplot2 `geomtextpath::geom_textpath(upright = TRUE)`.
//! - `"vjust"` — perpendicular offset in pt from the curve (optional;
//!   default `0.0`; per mark). `0` = glyph baseline on the curve,
//!   negative = above (screen-space), positive = below.
//! - `"angle"` — additional per-mark rotation in radians, mathematical
//!   CCW (optional; default `0.0`). Applied on top of the per-glyph
//!   tangent rotation.
//! - `"pick_id"` — per-mark pick ticket (optional). Every glyph in the
//!   mark shares the same id; the mark's first row supplies the value.

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point, Vec2};
use crate::plot::value::Value;
use crate::primitives::PolylineSampler;
use crate::scene::{Glyph, GlyphRun, SceneBuilder};
use crate::text::{run_layout_glyphs, TextRun, TextStyle};

use super::marks::{build_marks_from_column, MarkSlot};
use super::resolve::{
    override_alpha, pt_to_px, resolve_angle_channel, resolve_bool_channel_or,
    resolve_color_channel, resolve_number_channel, resolve_number_channel_or, resolve_pick_id,
    resolve_position, resolve_str_channel_or,
};
use super::state::{
    filter_declared, require_data_column, validate_channel_lengths, validate_pick_id_channel,
    GeomState, KeysStrategy,
};
use super::{
    empty_datacolumn_like, BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext,
    Keys,
};

use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::value::DataColumn;

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_SIZE_PT: f64 = 12.0;
const DEFAULT_WEIGHT: u16 = 400;
fn default_fill() -> Color {
    Color::new([0.0, 0.0, 0.0, 1.0])
}

const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("text", ExpectedOutput::Strings),
    ("size", ExpectedOutput::Numbers),
    ("weight", ExpectedOutput::Numbers),
    ("italic", ExpectedOutput::Any),
    ("family", ExpectedOutput::Strings),
    ("fill", ExpectedOutput::Colors),
    ("alpha", ExpectedOutput::Numbers),
    ("offset", ExpectedOutput::Numbers),
    ("hjust", ExpectedOutput::Numbers),
    ("upright", ExpectedOutput::Any),
    ("vjust", ExpectedOutput::Numbers),
    ("angle", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── TextPathGeom ────────────────────────────────────────────────────────────

/// A vectorised text-on-curve geom. One label per mark, positioned
/// glyph-by-glyph along the mark's polyline.
pub struct TextPathGeom {
    pub(crate) state: GeomState,
    pub(crate) marks: Vec<MarkSlot>,
}

crate::impl_geom_inherents_grouped!(TextPathGeom);

impl TextPathGeom {
    /// Build the per-mark slot index from the current keys. Each
    /// contiguous run of equal keys becomes one mark.
    pub(crate) fn build_marks(&self) -> Vec<MarkSlot> {
        super::marks::build_marks(&self.state.keys)
    }
}

// ─── BuildableGeom ───────────────────────────────────────────────────────────

impl BuildableGeom for TextPathGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "TextPathGeom").len();
        let y_len = require_data_column("y", &channels, "TextPathGeom").len();
        if y_len != n {
            panic!("TextPathGeom::build: \"y\" length {y_len} does not match \"x\" length {n}");
        }
        // `"text"` may be a constant (one string for all marks) or per-
        // mark data, so we don't require_data_column here. But it must be
        // present — the geom has no useful default text.
        if !channels.contains_key("text") {
            panic!("TextPathGeom::build: missing required channel \"text\"");
        }
        validate_channel_lengths(&channels, n, "TextPathGeom");
        validate_pick_id_channel(&channels, "TextPathGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::OneMark, declared);
        TextPathGeom {
            state,
            marks: Vec::new(),
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

    fn mark_count(&self) -> usize {
        if self.marks.is_empty() && !self.is_empty() {
            return self.build_marks().len();
        }
        self.marks.len()
    }

    fn invalidate_caches(&mut self) {
        self.marks.clear();
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
                let prev_unique = unique_keys_column(prev_col, &prev_marks);
                let next_unique = unique_keys_column(next_col, &next_marks);
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
        let text_scale = ctx.scale_for("text");
        let size_scale = ctx.scale_for("size");
        let weight_scale = ctx.scale_for("weight");
        let italic_scale = ctx.scale_for("italic");
        let family_scale = ctx.scale_for("family");
        let fill_scale = ctx.scale_for("fill");
        let alpha_scale = ctx.scale_for("alpha");
        let offset_scale = ctx.scale_for("offset");
        let hjust_scale = ctx.scale_for("hjust");
        let upright_scale = ctx.scale_for("upright");
        let vjust_scale = ctx.scale_for("vjust");
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

        let text_ch = channels.get("text");
        let size_ch = channels.get("size");
        let weight_ch = channels.get("weight");
        let italic_ch = channels.get("italic");
        let family_ch = channels.get("family");
        let fill_ch = channels.get("fill");
        let alpha_ch = channels.get("alpha");
        let offset_ch = channels.get("offset");
        let hjust_ch = channels.get("hjust");
        let upright_ch = channels.get("upright");
        let vjust_ch = channels.get("vjust");
        let angle_ch = channels.get("angle");
        let pick_id_ch = channels.get("pick_id");

        for mark in marks.iter() {
            let i0 = mark.first_row;

            // ── Resolve per-mark text + style. ──
            let text = resolve_str_channel_or(text_ch, text_scale, i0, "");
            if text.is_empty() {
                continue;
            }
            let size_pt = resolve_number_channel_or(size_ch, size_scale, i0, DEFAULT_SIZE_PT);
            let size_px = pt_to_px(size_pt, ctx.dpi);
            if !size_px.is_finite() || size_px <= 0.0 {
                continue;
            }
            let weight = resolve_number_channel(weight_ch, weight_scale, i0)
                .map(|w| (w.round() as i64).clamp(1, 1000) as u16)
                .unwrap_or(DEFAULT_WEIGHT);
            let italic = resolve_italic(italic_ch, italic_scale, i0);
            let family = resolve_str_opt(family_ch, family_scale, i0);

            let fill_color = override_alpha(
                resolve_color_channel(fill_ch, fill_scale, i0),
                resolve_number_channel(alpha_ch, alpha_scale, i0),
            )
            .unwrap_or_else(default_fill);

            let offset_pt = resolve_number_channel_or(offset_ch, offset_scale, i0, 0.0);
            let hjust = resolve_number_channel_or(hjust_ch, hjust_scale, i0, 0.0);
            let upright = resolve_bool_channel_or(upright_ch, upright_scale, i0, false);
            let vjust_pt = resolve_number_channel_or(vjust_ch, vjust_scale, i0, 0.0);
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
                let x_frac = resolve_position(x_col.get(i), x_scale, 0.0);
                let y_frac = resolve_position(y_col.get(i), y_scale, 0.0);
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
                let (px, py) = ctx.projection.project_to_panel_px(panel, &curr_channels);
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

            // ── Shape the text. Single-line only — no set_max_width. ──
            let mut style = TextStyle::new(size_px as f32).weight(weight).italic(italic);
            if let Some(fam) = family {
                style = style.family(fam);
            }
            let run = TextRun::new(&text, &style);
            let text_w = run.natural_width();
            let glyphs = run_layout_glyphs(&run);
            if glyphs.is_empty() {
                continue;
            }
            // Parley's `g.y` includes the line's baseline offset from the
            // layout's top. For text-on-path we want `vjust = 0` to mean
            // "glyph baseline sits on the curve", so subtract the line
            // baseline (taken from the first glyph; with single-line
            // single-style text every glyph's y matches).
            let baseline_ref = glyphs[0].y as f64;
            // Body metrics for the upright-flip baseline shift.
            // The body extends from y = -ascent (top) to y = +descent
            // (bottom) in glyph-local y-down coords; its centre is at
            // y = (descent - ascent) / 2.
            let descent_px = run.last_line_descender();
            let ascent_px = run.natural_height() - descent_px;

            // ── Compute global shifts. ──
            let offset_px = pt_to_px(offset_pt, ctx.dpi);
            let vjust_px = pt_to_px(vjust_pt, ctx.dpi);

            // ── Upright detection (per-mark, not per-glyph). ──
            //
            // ggplot2's geomtextpath: lay the text out in the natural
            // path direction; if the majority of glyph tangents point
            // into the left half-plane, the text is upside-down → flip
            // the WHOLE TEXT by reversing the path and inverting hjust.
            // Re-layout against the reversed path. Reading direction
            // along the path is reversed, but every glyph reads
            // right-side-up and the text remains contiguous.
            //
            // We implement the path reversal by remapping the sampled
            // arc-length distance (`d_orig = path_length - d`) and
            // negating the tangent — no second sampler needed.
            let flipped = if upright {
                let natural_shift = hjust * (path_length - text_w);
                let mut upside_down = 0usize;
                let mut counted = 0usize;
                for g in &glyphs {
                    let half_advance = g.advance as f64 * 0.5;
                    let d = offset_px + natural_shift + g.x as f64 + half_advance;
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
                (1.0 - hjust) * (path_length - text_w)
            } else {
                hjust * (path_length - text_w)
            };
            // When the whole text is reversed for the upright flip,
            // two perpendicular effects need compensation so the
            // glyph BODY ends up at the same world position as the
            // unflipped case:
            //
            // 1. The right-of-motion normal flips with the reading
            //    direction — `vjust` is in that normal's direction,
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
            // The combined adjustment is
            // `effective = -vjust + (ascent - descent)`.
            let effective_vjust_px = if flipped {
                -vjust_px + (ascent_px - descent_px)
            } else {
                vjust_px
            };

            let brush = Brush::Solid(fill_color);

            // ── Per-glyph emission. ──
            for g in &glyphs {
                let half_advance = g.advance as f64 * 0.5;
                let d_glyph = offset_px + hjust_shift + g.x as f64 + half_advance;
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

                let y_above_baseline = g.y as f64 - baseline_ref;
                let xform = Affine::translate(Vec2::new(sample.point.x, sample.point.y))
                    * Affine::rotate(theta)
                    * Affine::translate(Vec2::new(
                        -half_advance,
                        effective_vjust_px + y_above_baseline,
                    ));

                let glyph = Glyph {
                    id: g.id,
                    x: 0.0,
                    y: 0.0,
                };
                let glyph_run = GlyphRun {
                    font: &g.font,
                    font_size: g.font_size,
                    transform: xform,
                    glyph_transform: None,
                    brush: &brush,
                    brush_alpha: 1.0,
                    hint: false,
                    glyphs: std::slice::from_ref(&glyph),
                };
                scene.draw_glyphs(&glyph_run, pick);
            }
        }
    }
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

// ─── Diff helpers (mirrored from LineGeom) ───────────────────────────────────

fn unique_keys_column(col: &DataColumn, marks: &[MarkSlot]) -> DataColumn {
    let template = empty_datacolumn_like(col);
    push_values_into(template, marks.iter().map(|m| col.get(m.first_row)))
}

fn push_values_into(
    mut template: DataColumn,
    values: impl IntoIterator<Item = Value>,
) -> DataColumn {
    for v in values {
        match (&mut template, v) {
            (DataColumn::F64(vec), Value::Number(n)) => vec.push(n),
            (DataColumn::F32(vec), Value::Number(n)) => vec.push(n as f32),
            (DataColumn::I32(vec), Value::Number(n)) => vec.push(n as i32),
            (DataColumn::I64(vec), Value::Number(n)) => vec.push(n as i64),
            (DataColumn::Bool(vec), Value::Bool(b)) => vec.push(b),
            (DataColumn::String(vec), Value::String(s)) => vec.push(s),
            (DataColumn::Color(vec), Value::Color(c)) => vec.push(c),
            (DataColumn::Date(vec), Value::Date(d)) => vec.push(d),
            (DataColumn::DateTime(vec), Value::DateTime(us)) => vec.push(us),
            (DataColumn::Time(vec), Value::Time(us)) => vec.push(us),
            (DataColumn::Duration(vec), Value::Duration(us)) => vec.push(us),
            (DataColumn::Linetype(vec), Value::Linetype(p)) => vec.push(p),
            _ => panic!("TextPathGeom: unique-keys column variant mismatch"),
        }
    }
    template
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

    #[test]
    fn single_glyph_anchor_on_horizontal_path() {
        // Path runs from (100, 200) to (300, 200) — horizontal, length
        // 200 px. Single-char text with hjust = 0: glyph's CENTRE lands
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
        // Translation y component = baseline at sample point (200 px).
        let ty = coeffs[5];
        assert!(
            (ty - 200.0).abs() < 1.0,
            "expected baseline y ~= 200, got {ty}"
        );
        // Translation x component = left edge of glyph = sample.point.x
        // - half_advance. For glyph-0 with hjust=0, offset=0:
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
        // Multi-char text on a long horizontal path with hjust = 0. The
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
        // hjust = 0.5 should centre the text around the path midpoint.
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "centerme")
            .set("size", 20.0_f64)
            .set("hjust", 0.5_f64)
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
        // hjust = 1.0 should place the LAST glyph near the path end (x ~= 300).
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "abc")
            .set("size", 20.0_f64)
            .set("hjust", 1.0_f64)
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
    fn vjust_shifts_perpendicular_to_path() {
        // vjust = 10 pt = 13.33 px below the horizontal path → baseline
        // should sit at y = 200 + 13.33 ≈ 213.33.
        let mut g = TextPathGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("text", "X")
            .set("size", 20.0_f64)
            .set("vjust", 10.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let scene = drained(&g);
        let coeffs = glyph_ops(&scene)[0].transform.as_coeffs();
        let expected_y = 200.0 + 10.0 * 96.0 / 72.0;
        assert!(
            (coeffs[5] - expected_y).abs() < 1.0,
            "baseline y = {}, expected {expected_y}",
            coeffs[5]
        );
    }

    #[test]
    fn upright_reverses_reading_along_path() {
        // Path runs right-to-left in screen (start x=300, end x=100,
        // length 200 px). With upright off, glyph 0 sits at the
        // START of the path (rightmost, x≈300) and reads upside-down
        // because the tangent points left. With upright on, the whole
        // text is laid out against the REVERSED path: the text still
        // occupies the same physical arc-length region (since hjust=0
        // is preserved as 1-hjust=1 on the reversed walk, which
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

    #[test]
    fn declared_channels_alphabetical() {
        let g = TextPathGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0]))
            .set("y", Raw(vec![0.0_f64, 1.0]))
            .set("text", "x")
            .set("hjust", 0.0_f64)
            .set("upright", false)
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(names.contains(&"text"));
        assert!(names.contains(&"hjust"));
        assert!(names.contains(&"upright"));
    }
}
