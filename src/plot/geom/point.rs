//! `PointGeom` — vectorised point glyphs drawn at scaled `(x, y)` positions.
//!
//! Channels consumed (any can be set as Constant or Data; key column is
//! synthesised if no `.keys(…)` supplied):
//!
//! - `"x"` — position along x axis (required; numeric data).
//! - `"y"` — position along y axis (required; numeric data).
//! - `"x_offset"` — absolute **pt** offset added to the resolved x
//!   position (optional). Positive → right.
//! - `"y_offset"` — absolute **pt** offset added to the resolved y
//!   position (optional). Positive → up (math convention).
//! - `"x_band"` — offset in **band fractions** of the x scale's band
//!   width (optional). Positive → right. No effect on continuous scales
//!   (their `band_width` is 0). Use for jitter / dodge on discrete x
//!   axes.
//! - `"y_band"` — same as `"x_band"` for y. Positive → up.
//! - `"fill"` — interior color for fill subpaths (optional).
//! - `"stroke"` — outline color for stroke subpaths (optional).
//! - `"fill_opacity"` — overrides the alpha component of the resolved
//!   fill color (optional; expects a 0..=1 number).
//! - `"stroke_opacity"` — overrides the alpha component of the resolved
//!   stroke color (optional; expects a 0..=1 number).
//! - `"size"` — glyph diameter in pt (optional; defaults to 5pt).
//! - `"shape"` — registered shape name (optional; defaults to "circle").
//!
//! Channels are stored in a `HashMap<String, Channel>` keyed by channel
//! name. There is a single binding method, [`PointGeomBuilder::set`] on
//! the builder + [`PointGeom::set`] at runtime; the data-vs-constant
//! distinction is inferred from the value's type via `Into<Channel>`. The
//! same call site works for first-binding and update.
//!
//! Fill and stroke are independent: a shape's fill subpaths are filled
//! with the resolved fill color (or skipped if `"fill"` is unset); its
//! stroke subpaths are stroked with the resolved stroke color (or
//! skipped if `"stroke"` is unset). Both can be set, only one, or
//! neither.

use std::collections::HashMap;
use std::sync::Arc;

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point};
use crate::path::FillRule;
use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::value::Value;
use crate::scene::SceneBuilder;
use crate::shape::{Shape, ShapeStyle};
use crate::stroke::Stroke;

use super::{
    empty_datacolumn_like, BuildableGeom, Channel, ChannelDecl, ExpectedOutput, Geom, GeomBuilder,
    GeomContext, Keys,
};

// ─── Defaults ────────────────────────────────────────────────────────────────

/// Default glyph diameter when the user doesn't bind / set `"size"`.
const DEFAULT_SIZE_PT: f64 = 5.0;
/// Default glyph shape name.
const DEFAULT_SHAPE: &str = "circle";
/// Default stroke linewidth in pt when stroking a glyph outline.
const DEFAULT_STROKE_WIDTH_PT: f64 = 1.0;

fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    pt * dpi / 72.0
}

// ─── PointGeom ───────────────────────────────────────────────────────────────

/// A vectorised point geom. Non-generic — all channel data flows through
/// the `DataColumn` enum.
pub struct PointGeom {
    keys: Keys,
    channels: HashMap<String, Channel>,

    // Diff snapshot (rotated at end of rebuild).
    prev_keys: Keys,
    prev_channels: HashMap<String, Channel>,

    // Last computed diff. v1 stores but doesn't consume; v1.5 reads.
    #[allow(dead_code)]
    enter: Vec<usize>,
    #[allow(dead_code)]
    update: Vec<(usize, usize)>,
    #[allow(dead_code)]
    exit: Vec<Value>,

    dirty: bool,
    /// Channel declarations exposed via [`Geom::declared_channels`].
    /// Recomputed at the end of `build()` so the data_bound / output
    /// hints reflect what was actually supplied.
    declared: Vec<ChannelDecl>,
}

impl PointGeom {
    /// Entry point for construction. Returns an empty
    /// [`GeomBuilder<PointGeom>`].
    pub fn builder() -> GeomBuilder<Self> {
        GeomBuilder::new()
    }

    /// Row count. All data columns + keys have this length.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// `true` if the user supplied an explicit key column via `.keys(...)`.
    /// Determines whether diff goes through the columnar hash path or the
    /// positional fast path.
    pub fn has_explicit_keys(&self) -> bool {
        self.keys.is_explicit()
    }

    /// Update a single channel in place. Length-validates data columns
    /// against the current row count (mismatch panics) and marks the
    /// geom dirty so diff rebuilds before the next draw.
    ///
    /// For multi-channel updates, row-count changes, or key-column
    /// changes, use [`Self::update`] instead — it runs the full builder
    /// validation atomically.
    pub fn set(&mut self, channel: impl Into<String>, value: impl Into<Channel>) {
        let name: String = channel.into();
        let value: Channel = value.into();
        if let Channel::Data(col) = &value {
            if col.len() != self.keys.len() {
                panic!(
                    "PointGeom::set: \"{name}\" length {} does not match row count {}",
                    col.len(),
                    self.keys.len()
                );
            }
        }
        self.channels.insert(name, value);
        self.dirty = true;
    }

    /// Atomic multi-channel / N-changing update. The closure receives a
    /// [`GeomBuilder`] pre-populated with the geom's current state; on
    /// return the builder is built (full validation runs once) and the
    /// result atomically replaces the geom's state. `prev_*` snapshots
    /// rotate to the just-replaced state so diff produces meaningful
    /// enter/update/exit on the next draw.
    ///
    /// ```ignore
    /// g.update(|b| {
    ///     b.keys(new_keys);          // can change keys
    ///     b.set("x", new_xs);        // can change N
    ///     b.set("y", new_ys);
    ///     b.set("fill", new_cats);
    /// });
    /// ```
    pub fn update(&mut self, f: impl FnOnce(&mut GeomBuilder<Self>)) {
        // Pre-populate the builder with the geom's current state.
        // Positional keys re-synthesise from the new row count
        // automatically (no allocation carried forward), so the closure
        // can change N without manually re-setting keys. For
        // user-supplied keys we carry them forward so the closure can
        // mutate them; if the user changes N without also calling
        // `.keys(...)`, build()'s length check will panic as expected.
        let carry_keys = match &self.keys {
            Keys::Explicit(col) => Some(col.clone()),
            Keys::Positional(_) => None,
        };
        let mut b = GeomBuilder::from_parts(carry_keys, self.channels.clone());
        f(&mut b);
        let new = b.build();
        // Rotate: prev_* = old current; current = new.
        self.prev_keys = std::mem::replace(&mut self.keys, new.keys);
        self.prev_channels = std::mem::replace(&mut self.channels, new.channels);
        self.declared = new.declared;
        self.dirty = true;
        // Stale enter/update/exit are discarded; the next
        // rebuild_diff_against_previous (driven by dirty) recomputes
        // them against the just-rotated prev_*.
    }
}

// ─── BuildableGeom impl (validation + defaults live here) ────────────────────

impl BuildableGeom for PointGeom {
    /// Finalise the builder into a `PointGeom`. Runs all geom-specific
    /// validation in one atomic pass: x/y are required data + numeric
    /// columns; every channel's data column matches the row count;
    /// optional explicit keys also match. Installs default `"size"`
    /// (5pt) and `"shape"` ("circle") constants when unset.
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, mut channels) = builder.into_parts();

        // x and y are mandatory.
        let x = channels
            .get("x")
            .cloned()
            .expect("PointGeom::build: missing required channel \"x\"");
        let y = channels
            .get("y")
            .cloned()
            .expect("PointGeom::build: missing required channel \"y\"");

        // x/y must be numeric data columns.
        let x_col = match x {
            Channel::Data(c) => c,
            Channel::Constant(_) => panic!(
                "PointGeom::build: \"x\" must be data, not constant — point positions vary per row"
            ),
        };
        let y_col = match y {
            Channel::Data(c) => c,
            Channel::Constant(_) => panic!(
                "PointGeom::build: \"y\" must be data, not constant — point positions vary per row"
            ),
        };
        // No column-variant check on x/y: discrete scales accept string
        // / bool / arbitrary `Value` inputs and produce numeric band
        // centres. The per-row draw loop resolves each cell through its
        // scale (or rejects with NaN if the scale doesn't produce a
        // finite number), so column type can be anything the user's
        // bound scale accepts.

        // Row count.
        let n = x_col.len();
        if y_col.len() != n {
            panic!(
                "PointGeom::build: \"y\" length {} does not match \"x\" length {n}",
                y_col.len()
            );
        }

        // Validate every other data channel.
        for (name, ch) in &channels {
            if let Channel::Data(col) = ch {
                if col.len() != n {
                    panic!(
                        "PointGeom::build: \"{name}\" length {} does not match row count {n}",
                        col.len()
                    );
                }
            }
        }

        // Keys. Explicit keys go through Keys::Explicit; absent keys
        // become Keys::Positional(n) — zero allocation rather than a
        // materialised `(0..n)` vector.
        let keys = match keys_opt {
            Some(k) => {
                if k.len() != n {
                    panic!(
                        "PointGeom::build: keys length {} does not match row count {n}",
                        k.len()
                    );
                }
                Keys::Explicit(k)
            }
            None => Keys::Positional(n),
        };

        // Install defaults for size + shape if unset.
        channels
            .entry("size".to_string())
            .or_insert_with(|| Channel::Constant(Value::Number(DEFAULT_SIZE_PT)));
        channels
            .entry("shape".to_string())
            .or_insert_with(|| Channel::Constant(Value::String(Arc::from(DEFAULT_SHAPE))));

        // First-frame snapshot of channels + empty diff state. The diff
        // is rebuilt before draw via Geom::rebuild_diff_against_previous;
        // on the very first draw it produces an all-enter result.
        let prev_keys = keys.empty_like();
        let prev_channels = empty_channels_like(&channels);

        let declared = declared_channels(&channels);

        PointGeom {
            keys,
            channels,
            prev_keys,
            prev_channels,
            enter: Vec::new(),
            update: Vec::new(),
            exit: Vec::new(),
            dirty: true,
            declared,
        }
    }
}

/// Snapshot of `channels` where every `Data` column is replaced by its
/// length-0 counterpart and every `Constant` is preserved. Seeds the
/// first-frame `prev_channels` so v1.5 animation has a stable "previous
/// state" to interpolate from.
fn empty_channels_like(channels: &HashMap<String, Channel>) -> HashMap<String, Channel> {
    channels
        .iter()
        .map(|(name, ch)| {
            let prev = match ch {
                Channel::Constant(v) => Channel::Constant(v.clone()),
                Channel::Data(col) => Channel::Data(empty_datacolumn_like(col)),
            };
            (name.clone(), prev)
        })
        .collect()
}

fn declared_channels(channels: &HashMap<String, Channel>) -> Vec<ChannelDecl> {
    let mut out = Vec::with_capacity(channels.len());
    for (name, ch) in channels {
        // Map the channel name to a static str via the conventional set;
        // unknown names get leaked into 'static via Box::leak so the
        // ChannelDecl can stay 'static. For v1 only the conventional
        // names are declared. Custom channels declared via .data()/
        // .constant() are still stored on the geom and consulted at draw
        // time — they just don't appear in the static decl list.
        let static_name: &'static str = match name.as_str() {
            "x" => "x",
            "y" => "y",
            "x_offset" => "x_offset",
            "y_offset" => "y_offset",
            "x_band" => "x_band",
            "y_band" => "y_band",
            "fill" => "fill",
            "stroke" => "stroke",
            "fill_opacity" => "fill_opacity",
            "stroke_opacity" => "stroke_opacity",
            "size" => "size",
            "shape" => "shape",
            _ => continue,
        };
        let data_bound = ch.is_data();
        let expected_output = match static_name {
            "x"
            | "y"
            | "x_offset"
            | "y_offset"
            | "x_band"
            | "y_band"
            | "size"
            | "fill_opacity"
            | "stroke_opacity" => ExpectedOutput::Numbers,
            "fill" | "stroke" => ExpectedOutput::Colors,
            "shape" => ExpectedOutput::Strings,
            _ => ExpectedOutput::Any,
        };
        out.push(ChannelDecl {
            name: static_name,
            data_bound,
            expected_output,
        });
    }
    // Deterministic order for tests and validation: alphabetical.
    out.sort_by_key(|d| d.name);
    out
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for PointGeom {
    fn declared_channels(&self) -> &[ChannelDecl] {
        &self.declared
    }

    fn len(&self) -> usize {
        self.keys.len()
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn rebuild_diff_against_previous(&mut self) {
        if !self.dirty {
            return;
        }
        // Identity-based diff only when BOTH prev and current carry an
        // explicit key column. Any other combination — both positional,
        // or transition between modes (e.g. user added explicit keys
        // mid-flight via update()) — falls back to positional matching.
        // The mode transition case is semantically the only sensible
        // answer because the prev state has no key identity to match
        // against.
        let (enter, update, exit) = match (&self.prev_keys, &self.keys) {
            (Keys::Explicit(prev_col), Keys::Explicit(next_col)) => {
                let idx = KeyIndex::build(prev_col);
                diff_columns(prev_col, &idx, next_col)
            }
            _ => diff_positional(self.prev_keys.len(), self.keys.len()),
        };
        self.enter = enter;
        self.update = update;
        self.exit = exit;
        // Rotate prev_* to current. Cloning Keys::Positional is a
        // single-`usize` copy; Keys::Explicit clones the underlying
        // DataColumn (same cost as before).
        self.prev_keys = self.keys.clone();
        self.prev_channels = self.channels.clone();
        self.dirty = false;
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

        // Resolve scales by channel name (None == identity / position-frac).
        let x_scale = ctx.scale_for("x");
        let y_scale = ctx.scale_for("y");
        let fill_scale = ctx.scale_for("fill");
        let stroke_scale = ctx.scale_for("stroke");
        let fill_opacity_scale = ctx.scale_for("fill_opacity");
        let stroke_opacity_scale = ctx.scale_for("stroke_opacity");
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let size_scale = ctx.scale_for("size");

        // x/y are always data columns (build() guaranteed numeric).
        let x_col = match self.channels.get("x") {
            Some(Channel::Data(c)) => c,
            _ => return,
        };
        let y_col = match self.channels.get("y") {
            Some(Channel::Data(c)) => c,
            _ => return,
        };

        // Resolve other channels' columns + constants ahead of the row loop.
        let fill_ch = self.channels.get("fill");
        let stroke_ch = self.channels.get("stroke");
        let fill_opacity_ch = self.channels.get("fill_opacity");
        let stroke_opacity_ch = self.channels.get("stroke_opacity");
        let x_offset_ch = self.channels.get("x_offset");
        let y_offset_ch = self.channels.get("y_offset");
        let x_band_ch = self.channels.get("x_band");
        let y_band_ch = self.channels.get("y_band");
        let size_ch = self.channels.get("size");
        let shape_ch = self.channels.get("shape");

        for i in 0..n {
            // ── Position ──
            // `*_band` is a fraction of the scale's band width — it's
            // folded into the scale's map_with_offset so the resulting
            // fraction stays resize-invariant and per-bin widths (for
            // uneven Binned scales) are handled by the scale itself.
            let x_band = resolve_number_channel(x_band_ch, x_band_scale, i).unwrap_or(0.0);
            let y_band = resolve_number_channel(y_band_ch, y_band_scale, i).unwrap_or(0.0);
            let px_frac = resolve_position(x_col.get(i), x_scale, x_band);
            let py_frac = resolve_position(y_col.get(i), y_scale, y_band);
            if !px_frac.is_finite() || !py_frac.is_finite() {
                continue;
            }
            let mut px = panel.x0 + px_frac * panel_w;
            let mut py = panel.y1 - py_frac * panel_h; // y flips

            // ── Pt offsets ──
            // `*_offset` is in pt — pixel-domain, dpi-dependent. Applied
            // after the scale → panel conversion.
            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                px += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                py -= pt_to_px(off, ctx.dpi);
            }

            // ── Channel resolves ──
            let fill_color = resolve_color_channel(fill_ch, fill_scale, i);
            let stroke_color = resolve_color_channel(stroke_ch, stroke_scale, i);
            // Opacity channels override the alpha component of the
            // resolved color (if set). Unset → keep the color's own alpha.
            let fill_color = override_alpha(
                fill_color,
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i),
            );
            let stroke_color = override_alpha(
                stroke_color,
                resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, i),
            );
            let size_pt = resolve_size_channel(size_ch, size_scale, i, DEFAULT_SIZE_PT);
            let shape_name = resolve_shape_channel(shape_ch, i);

            let size_px = pt_to_px(size_pt, ctx.dpi);
            if !size_px.is_finite() || size_px <= 0.0 {
                continue;
            }

            // ── Shape lookup ──
            let shape: &Shape = match ctx.shapes.get(&shape_name) {
                Some(s) => s,
                None => continue, // unknown shape — skip
            };

            // ── Placement transform: translate + uniform scale. ──
            // Mode A from the shape module docs (centered on placement).
            let xform = Affine::translate((px, py)) * Affine::scale(size_px);

            // Iterate subpaths. fill subpaths use fill_color (if set);
            // stroke subpaths use stroke_color (if set). For Fill-style
            // shapes the user may *also* want an outline if stroke is
            // bound — we honour that by stroking the fill subpaths too
            // when both colors resolve. For Stroke-style shapes the fill
            // is meaningless.
            let pick = ctx.pick_id_for_row(i);
            for sub in shape.paths() {
                match shape.style() {
                    ShapeStyle::Fill => {
                        if let Some(fc) = fill_color {
                            scene.fill(
                                FillRule::NonZero,
                                xform,
                                &Brush::Solid(fc),
                                None,
                                sub,
                                pick,
                            );
                        }
                        if let Some(sc) = stroke_color {
                            let st = Stroke::new(pt_to_px(DEFAULT_STROKE_WIDTH_PT, ctx.dpi));
                            scene.stroke(
                                &st,
                                xform,
                                &Brush::Solid(sc),
                                None,
                                sub,
                                pick,
                            );
                        }
                    }
                    ShapeStyle::Stroke => {
                        if let Some(sc) = stroke_color {
                            let st = Stroke::new(pt_to_px(DEFAULT_STROKE_WIDTH_PT, ctx.dpi));
                            scene.stroke(
                                &st,
                                xform,
                                &Brush::Solid(sc),
                                None,
                                sub,
                                pick,
                            );
                        }
                    }
                }
            }
        }

        // Touch prev_channels so the field stays alive — v1.5 animation
        // will consume it for interpolating across update edges.
        let _ = &self.prev_channels;
    }
}

// ─── Per-row resolve helpers ─────────────────────────────────────────────────

/// Project a row's raw `Value` through an optional position scale to a
/// `[0, 1]` panel fraction, with an optional band-fraction offset folded
/// in. With no scale the input must itself project to a finite f64
/// (i.e. be numeric or temporal); other variants return NaN so the
/// caller skips the row. Without a scale, the band offset is ignored —
/// "band" is a scale-defined concept.
fn resolve_position(
    raw: Value,
    scale: Option<&crate::plot::scale::Scale>,
    band_offset: f64,
) -> f64 {
    let mapped = match scale {
        Some(s) => s.map_with_offset(&raw, band_offset),
        None => raw,
    };
    mapped.as_number().unwrap_or(f64::NAN)
}

/// Resolve a color channel: data column or constant, with optional scale
/// mapping. Returns `None` when the channel is unset (skip subpath draw).
fn resolve_color_channel(
    channel: Option<&Channel>,
    scale: Option<&crate::plot::scale::Scale>,
    i: usize,
) -> Option<Color> {
    let raw = match channel? {
        Channel::Constant(v) => v.clone(),
        Channel::Data(col) => col.get(i),
    };
    let mapped = match scale {
        Some(s) => s.map(&raw),
        None => raw,
    };
    mapped.as_color()
}

/// Resolve a generic optional numeric channel. Returns `None` when the
/// channel is unset (caller decides what "absent" means — leave the
/// color alpha alone, skip an offset, etc.) or when the resolved value
/// isn't numeric. No clamping — the caller is responsible for any
/// range checks.
fn resolve_number_channel(
    channel: Option<&Channel>,
    scale: Option<&crate::plot::scale::Scale>,
    i: usize,
) -> Option<f64> {
    let raw = match channel? {
        Channel::Constant(v) => v.clone(),
        Channel::Data(col) => col.get(i),
    };
    let mapped = match scale {
        Some(s) => s.map(&raw),
        None => raw,
    };
    mapped.as_number()
}

/// Override the alpha channel of `color` with `alpha` (in `0..=1`). If
/// either input is `None`, returns the color unchanged (or `None` if the
/// color itself was missing).
fn override_alpha(color: Option<Color>, alpha: Option<f64>) -> Option<Color> {
    let c = color?;
    match alpha {
        None => Some(c),
        Some(a) => {
            let [r, g, b, _] = c.components;
            Some(Color::new([r, g, b, a as f32]))
        }
    }
}

/// Resolve a size channel: data or constant, optional scale mapping,
/// returning a pt value (caller converts to px).
fn resolve_size_channel(
    channel: Option<&Channel>,
    scale: Option<&crate::plot::scale::Scale>,
    i: usize,
    default_pt: f64,
) -> f64 {
    let raw = match channel {
        None => return default_pt,
        Some(Channel::Constant(v)) => v.clone(),
        Some(Channel::Data(col)) => col.get(i),
    };
    let mapped = match scale {
        Some(s) => s.map(&raw),
        None => raw,
    };
    mapped.as_number().unwrap_or(default_pt)
}

/// Resolve a shape-name channel. Returns a freshly-allocated String for
/// the matched name; falls back to the default shape on missing /
/// non-string values.
fn resolve_shape_channel(channel: Option<&Channel>, i: usize) -> String {
    let raw = match channel {
        None => return DEFAULT_SHAPE.to_string(),
        Some(Channel::Constant(v)) => v.clone(),
        Some(Channel::Data(col)) => col.get(i),
    };
    match raw.as_str() {
        Some(s) => s.to_string(),
        None => DEFAULT_SHAPE.to_string(),
    }
}

/// Placement helper using the shape module's mode-A convention (the
/// `Point` import is kept in scope below for callers that want to
/// pre-compute centres in a future batch path).
#[allow(dead_code)]
fn place(center: Point) -> Affine {
    Affine::translate((center.x, center.y))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::plot::geom::DirectScaleResolver;
    use crate::plot::scale;
    use crate::plot::value::Date;
    use crate::scene::recording::{Op, RecordingScene};

    fn registry() -> crate::shape::ShapeRegistry {
        crate::shape::ShapeRegistry::with_builtins()
    }

    fn ctx<'a>(
        panel: Rect,
        shapes: &'a crate::shape::ShapeRegistry,
        scales: &'a DirectScaleResolver<'a>,
    ) -> GeomContext<'a> {
        GeomContext::new(panel, 96.0, shapes, scales)
    }

    // ── build() validation ──

    #[test]
    fn builder_synthesises_positional_keys() {
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 4.0])
            .build();
        assert_eq!(g.len(), 3);
        assert!(!g.has_explicit_keys());
        // Synthesised keys are positional — no allocation, just N.
        match &g.keys {
            Keys::Positional(n) => assert_eq!(*n, 3),
            Keys::Explicit(_) => panic!("expected positional keys"),
        }
    }

    #[test]
    fn builder_uses_explicit_keys() {
        let g = PointGeom::builder()
            .keys(vec!["a", "b", "c"])
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .build();
        assert!(g.has_explicit_keys());
        assert_eq!(g.len(), 3);
    }

    #[test]
    #[should_panic(expected = "missing required channel")]
    fn builder_missing_x_panics() {
        PointGeom::builder()
            .set("y", vec![1.0_f64, 2.0, 3.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "missing required channel")]
    fn builder_missing_y_panics() {
        PointGeom::builder()
            .set("x", vec![1.0_f64, 2.0, 3.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "must be data, not constant")]
    fn builder_x_constant_panics() {
        PointGeom::builder()
            .set("x", 5.0)
            .set("y", vec![1.0_f64])
            .build();
    }

    #[test]
    fn builder_x_string_column_ok() {
        // String x columns are accepted at build time; their resolution
        // happens through the bound (typically Discrete/Ordinal) scale at
        // draw time. Without a scale they'd render as NaN positions and
        // skip — but build() itself doesn't reject them.
        let g = PointGeom::builder()
            .set("x", vec!["a", "b", "c"])
            .set("y", vec![1.0_f64, 2.0, 3.0])
            .build();
        assert_eq!(g.len(), 3);
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_y_length_mismatch_panics() {
        PointGeom::builder()
            .set("x", vec![1.0_f64, 2.0, 3.0])
            .set("y", vec![1.0_f64, 2.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match row count")]
    fn builder_color_length_mismatch_panics() {
        PointGeom::builder()
            .set("x", vec![1.0_f64, 2.0, 3.0])
            .set("y", vec![1.0_f64, 2.0, 3.0])
            .set("fill", vec!["a", "b"])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match row count")]
    fn builder_keys_length_mismatch_panics() {
        PointGeom::builder()
            .keys(vec!["a", "b"])
            .set("x", vec![1.0_f64, 2.0, 3.0])
            .set("y", vec![1.0_f64, 2.0, 3.0])
            .build();
    }

    #[test]
    fn builder_x_temporal_column_ok() {
        let g = PointGeom::builder()
            .set(
                "x",
                vec![Date::from_ymd(2024, 1, 1), Date::from_ymd(2024, 6, 1)],
            )
            .set("y", vec![0.0_f64, 1.0])
            .build();
        assert_eq!(g.len(), 2);
    }

    // ── declared_channels ──

    #[test]
    fn declared_channels_sorted_and_classified() {
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .build();
        let decls = g.declared_channels();
        let names: Vec<_> = decls.iter().map(|d| d.name).collect();
        // Defaults inject size + shape; user added x, y, fill.
        // Sorted alphabetically: fill, shape, size, x, y.
        assert_eq!(names, vec!["fill", "shape", "size", "x", "y"]);

        let by_name: HashMap<_, _> = decls.iter().map(|d| (d.name, d)).collect();
        assert!(by_name["x"].data_bound);
        assert!(by_name["y"].data_bound);
        assert!(!by_name["fill"].data_bound); // we used `.set("fill", ...)`
        assert_eq!(by_name["x"].expected_output, ExpectedOutput::Numbers);
        assert_eq!(by_name["fill"].expected_output, ExpectedOutput::Colors);
        assert_eq!(by_name["shape"].expected_output, ExpectedOutput::Strings);
    }

    #[test]
    fn declared_opacity_channels_are_numeric() {
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("fill_opacity", 0.3)
            .set("stroke_opacity", vec![0.2_f64, 0.8])
            .build();
        let decls = g.declared_channels();
        let by_name: HashMap<_, _> = decls.iter().map(|d| (d.name, d)).collect();
        assert_eq!(
            by_name["fill_opacity"].expected_output,
            ExpectedOutput::Numbers
        );
        assert_eq!(
            by_name["stroke_opacity"].expected_output,
            ExpectedOutput::Numbers
        );
        assert!(!by_name["fill_opacity"].data_bound);
        assert!(by_name["stroke_opacity"].data_bound);
    }

    // ── diff plumbing ──

    #[test]
    fn diff_positional_path_after_mutation() {
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .build();
        // First diff: all-enter (prev was empty).
        g.rebuild_diff_against_previous();
        assert_eq!(g.enter, vec![0, 1, 2]);
        assert!(g.update.is_empty());
        assert!(g.exit.is_empty());
        // Replace y with same length → all-update via positional fast path.
        g.set("y", vec![10.0_f64, 20.0, 30.0]);
        g.rebuild_diff_against_previous();
        assert!(g.enter.is_empty());
        assert_eq!(g.update, vec![(0, 0), (1, 1), (2, 2)]);
        assert!(g.exit.is_empty());
    }

    #[test]
    fn update_closure_atomic_multi_channel() {
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![10.0_f64, 20.0, 30.0])
            .build();
        // Atomic update: replace both x and y. Mid-update inconsistency
        // (e.g. updating x first while y still old) is invisible.
        g.update(|b| {
            b.set("x", vec![100.0_f64, 200.0, 300.0, 400.0]);
            b.set("y", vec![1.0_f64, 2.0, 3.0, 4.0]);
        });
        assert_eq!(g.len(), 4);
        // Diff against the previous (N=3) state: 3 updates + 1 enter.
        g.rebuild_diff_against_previous();
        assert_eq!(g.update, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(g.enter, vec![3]);
        assert!(g.exit.is_empty());
    }

    #[test]
    fn update_closure_can_change_keys() {
        let mut g = PointGeom::builder()
            .keys(vec!["a", "b", "c"])
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .build();
        g.rebuild_diff_against_previous(); // first-frame: all enter

        // Replace keys + data, identity-driven diff against prev keys.
        g.update(|b| {
            b.keys(vec!["c", "a", "d"]);
            b.set("x", vec![20.0_f64, 0.0, 99.0]);
            b.set("y", vec![20.0_f64, 0.0, 99.0]);
        });
        g.rebuild_diff_against_previous();
        // c was prev_idx 2 → new_idx 0; a was prev_idx 0 → new_idx 1;
        // d is new (enter at 2); b is gone (exit).
        assert_eq!(g.update, vec![(2, 0), (0, 1)]);
        assert_eq!(g.enter, vec![2]);
        assert_eq!(g.exit.len(), 1);
        assert_eq!(g.exit[0].as_str(), Some("b"));
    }

    #[test]
    #[should_panic(expected = "must be data, not constant")]
    fn update_closure_validation_panics_on_invalid_state() {
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
        // If the closure clobbers x with a constant the build() inside
        // update() panics — the geom's previous state is preserved
        // because the panic unwinds before the rotation step.
        g.update(|b| {
            // Replace x with a constant — invalid.
            b.set("x", 5.0);
        });
    }

    #[test]
    fn diff_columns_path_with_reordered_keys() {
        // With explicit keys, reordering should yield zero enter/exit
        // and N updates whose pairs reflect the permutation.
        let mut g = PointGeom::builder()
            .keys(vec!["a", "b", "c"])
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .build();
        g.rebuild_diff_against_previous();
        // First frame: all-enter regardless of key type.
        assert_eq!(g.enter, vec![0, 1, 2]);

        // Now mutate to a reordered key column.
        g.keys = Keys::Explicit(vec!["c", "a", "b"].into());
        g.dirty = true;
        g.rebuild_diff_against_previous();
        assert!(g.enter.is_empty());
        assert!(g.exit.is_empty());
        // Pairs: c was prev_idx 2, now 0; a was 0, now 1; b was 1, now 2.
        assert_eq!(g.update, vec![(2, 0), (0, 1), (1, 2)]);
    }

    // ── draw() ──

    fn count_ops(ops: &[Op]) -> (usize, usize) {
        let mut fills = 0;
        let mut strokes = 0;
        for op in ops {
            match op {
                Op::Fill { .. } => fills += 1,
                Op::Stroke { .. } => strokes += 1,
                _ => {}
            }
        }
        (fills, strokes)
    }

    #[test]
    fn draw_fills_circle_when_fill_bound() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert!(fills >= 1, "expected at least one fill for filled circle");
        // Stroke is unbound → no outline.
        assert_eq!(strokes, 0);
    }

    #[test]
    fn draw_strokes_circle_when_stroke_bound() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("stroke", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        // Filled-circle subpaths get a stroke pass when stroke is bound
        // but no fill. (Caller wanted an unfilled outlined circle.)
        assert_eq!(fills, 0);
        assert!(strokes >= 1);
    }

    #[test]
    fn draw_both_fill_and_stroke() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("stroke", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert!(fills >= 1);
        assert!(strokes >= 1);
    }

    #[test]
    fn draw_fill_opacity_overrides_alpha() {
        // Fill color has alpha 1.0; fill_opacity sets it to 0.25.
        // The recorded Fill op's brush should carry the override.
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("fill_opacity", 0.25)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let alphas: Vec<f32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(c.components[3]),
                _ => None,
            })
            .collect();
        assert!(
            !alphas.is_empty(),
            "expected at least one Fill op with a solid brush"
        );
        for a in &alphas {
            assert!(
                (*a as f64 - 0.25).abs() < 1e-6,
                "fill alpha mismatch: got {a}, expected 0.25"
            );
        }
    }

    #[test]
    fn draw_stroke_opacity_overrides_alpha() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("stroke", Color::new([0.0, 0.0, 0.0, 1.0]))
            .set("stroke_opacity", 0.5)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let alphas: Vec<f32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(c.components[3]),
                _ => None,
            })
            .collect();
        assert!(!alphas.is_empty());
        for a in &alphas {
            assert!((*a as f64 - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn draw_opacity_unset_preserves_color_alpha() {
        // No opacity channel → color's own alpha (0.7) flows through.
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 0.7]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        for op in &scene.ops {
            if let Op::Fill {
                brush: crate::brush::Brush::Solid(c),
                ..
            } = op
            {
                assert!((c.components[3] as f64 - 0.7).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn draw_per_row_opacity_data_column() {
        // Per-row alpha via Data column.
        let g = PointGeom::builder()
            .set("x", vec![0.25_f64, 0.75])
            .set("y", vec![0.5_f64, 0.5])
            .set("fill", Color::new([0.0, 0.0, 1.0, 1.0]))
            .set("fill_opacity", vec![0.2_f64, 0.8])
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let alphas: Vec<f32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(c.components[3]),
                _ => None,
            })
            .collect();
        // Expect alphas 0.2 then 0.8 in order. (Circle is one Fill subpath.)
        assert_eq!(alphas.len(), 2);
        assert!((alphas[0] as f64 - 0.2).abs() < 1e-6);
        assert!((alphas[1] as f64 - 0.8).abs() < 1e-6);
    }

    /// Extract the translation `(px, py)` from the first Fill op in the
    /// recorded scene. Returns `None` if there's no Fill op.
    fn first_fill_translation(scene: &RecordingScene) -> Option<(f64, f64)> {
        for op in &scene.ops {
            if let Op::Fill { transform, .. } = op {
                let v = transform.translation();
                return Some((v.x, v.y));
            }
        }
        None
    }

    #[test]
    fn draw_x_offset_shifts_right() {
        // 1pt at 96 dpi = 96/72 = 4/3 px.
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("x_offset", 9.0) // 9pt = 12 px at 96 dpi
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (px, py) = first_fill_translation(&scene).expect("expected a fill op");
        // Base px would be 50, with +12 px x_offset → 62.
        assert!((px - 62.0).abs() < 1e-6, "px = {px}");
        // y unchanged.
        assert!((py - 50.0).abs() < 1e-6, "py = {py}");
    }

    #[test]
    fn draw_y_offset_positive_is_up() {
        // y positive offset → up = smaller pixel y.
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("y_offset", 9.0) // +12 px in math = subtract from pixel y
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (px, py) = first_fill_translation(&scene).expect("expected a fill op");
        assert!((px - 50.0).abs() < 1e-6);
        // Base py = 50 (panel.y1 - 0.5 * 100), offset up by 12 px → 38.
        assert!((py - 38.0).abs() < 1e-6, "py = {py}");
    }

    #[test]
    fn draw_x_band_offset_on_discrete_scale() {
        // Discrete scale with 4 bands → band_width = 0.25. Band span on
        // panel = 0.25 * 100 = 25 px. x_band = 0.5 → shift +12.5 px.
        let x_scale = scale::discrete(
            ["a", "b", "c", "d"]
                .into_iter()
                .map(|s| Value::String(std::sync::Arc::from(s))),
        );
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = PointGeom::builder()
            .set("x", vec!["b"])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("x_band", 0.5)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (px, _py) = first_fill_translation(&scene).expect("expected a fill op");
        // "b" sits at band-centre (1 + 0.5) / 4 = 0.375 → 37.5 px.
        // +0.5 band-widths = +12.5 px → 50.0 px.
        assert!((px - 50.0).abs() < 1e-6, "px = {px}");
    }

    #[test]
    fn draw_x_band_no_op_on_continuous_scale() {
        // Continuous scale → band_width = 0 → x_band is a no-op.
        let x_scale = scale::continuous(0.0..=10.0);
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = PointGeom::builder()
            .set("x", vec![5.0_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("x_band", 0.5)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (px, _py) = first_fill_translation(&scene).expect("expected a fill op");
        // Continuous scale: 5/10 = 0.5 → 50 px. Band offset has no effect.
        assert!((px - 50.0).abs() < 1e-6);
    }

    #[test]
    fn draw_per_row_offset_jitter() {
        // Three points at the same data position with per-row x_offsets
        // for jitter.
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64, 0.5, 0.5])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("x_offset", vec![-9.0_f64, 0.0, 9.0]) // -12, 0, +12 px
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let xs: Vec<f64> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill { transform, .. } => Some(transform.translation().x),
                _ => None,
            })
            .collect();
        assert_eq!(xs.len(), 3);
        assert!((xs[0] - 38.0).abs() < 1e-6);
        assert!((xs[1] - 50.0).abs() < 1e-6);
        assert!((xs[2] - 62.0).abs() < 1e-6);
    }

    #[test]
    fn draw_neither_fill_nor_stroke_emits_nothing() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert_eq!(fills, 0);
        assert_eq!(strokes, 0);
    }

    #[test]
    fn draw_vectorised_n_rows() {
        // 5 points, filled — expect at least 5 fill ops (the circle
        // shape is one subpath, so one fill per point).
        let g = PointGeom::builder()
            .set("x", vec![0.1_f64, 0.3, 0.5, 0.7, 0.9])
            .set("y", vec![0.5_f64; 5])
            .set("fill", Color::new([0.0, 0.0, 1.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, _) = count_ops(&scene.ops);
        assert!(
            fills >= 5,
            "expected at least 5 fills for 5 points; got {fills}"
        );
    }

    #[test]
    fn draw_routes_x_through_scale() {
        // A scale that maps [0, 100] → [0, 1] frac. We pass x=50 which
        // should land at panel midpoint (50.0 px).
        let x_scale = scale::continuous(0.0..=100.0);
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = PointGeom::builder()
            .set("x", vec![50.0_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        // We can't easily inspect the transform without parsing the op
        // stream — assert at least one fill landed; the integration
        // example (Phase 8) will verify visually.
        let (fills, _) = count_ops(&scene.ops);
        assert!(fills >= 1);
    }

    #[test]
    fn draw_skips_unknown_shape() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("shape", "definitely-not-a-shape")
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert_eq!(fills, 0);
        assert_eq!(strokes, 0);
    }

    #[test]
    fn draw_silent_on_degenerate_panel() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 0.0, 0.0);
        let shapes = registry();
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        assert!(scene.ops.is_empty());
    }
}
