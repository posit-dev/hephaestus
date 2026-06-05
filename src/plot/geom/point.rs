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
//! - `"size_band"` — additional glyph-diameter contribution, expressed
//!   as a fraction of the discrete-band width at the row's `(x, y)`
//!   position. Composes additively with `"size"`:
//!
//!   ```text
//!   diameter_px = pt_to_px(size_pt) + size_band * band_px
//!   ```
//!
//!   `band_px` is the smallest non-zero band width across x and y at the
//!   row's centre, in panel pixels — single-discrete → that axis's
//!   band, both-discrete → smaller of the two (so the glyph fits the
//!   cell on both axes), both-continuous → 0 (no contribution).
//!   Mirrors `WedgeGeom::radius_band`. Defaults to 0; the existing
//!   5pt `"size"` default is unchanged, so callers wanting pure band
//!   sizing also pass `size = 0`.
//! - `"shape"` — registered shape name (optional; defaults to "circle").
//!   Glyph-backed shapes (constructed via [`crate::shape::Shape::glyph`]
//!   or the [`crate::text::glyph_marker`] convenience) are valid here and
//!   render via `scene.draw_glyphs`. For glyph shapes, `"stroke"` has no
//!   effect — the glyph is filled with the resolved `"fill"` colour.
//!   Glyph height is normalised to the same bounding-box convention as
//!   the built-in vector shapes (`~1.6` units across), so a vector
//!   `"circle"` and a glyph `"letter-a"` at the same `"size"` render at
//!   comparable extent. Visible glyph ink still occupies only ~70% of
//!   its em-box (cap-height), so letters look slightly smaller than a
//!   solid disc of the same `"size"` — bump `"size"` if you need exact
//!   visual parity.
//! - `"angle"` — rotation in **radians** around the placement point,
//!   mathematical CCW (positive rotates the glyph counter-clockwise in
//!   the rendered image). Default `0.0` (no rotation). Applies after
//!   scale + pivot resolution and before the pt-space offsets — the
//!   offsets translate the rotated glyph by absolute pt, they aren't
//!   rotated themselves.
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

use std::sync::Arc;

use crate::brush::Brush;
use crate::geometry::Affine;
use crate::path::FillRule;
use crate::plot::value::Value;
use crate::scene::{Glyph, GlyphRun, SceneBuilder};
use crate::shape::{Shape, ShapeKind, ShapeStyle};
use crate::stroke::Stroke;

use super::resolve::{
    band_width_at, override_alpha, pt_to_px, resolve_angle_channel, resolve_color_channel,
    resolve_number_channel, resolve_number_channel_or, resolve_pick_id, resolve_position,
    resolve_str_channel_or, smallest_nonzero,
};
use super::state::{
    filter_declared, require_data_column, validate_channel_lengths, validate_pick_id_channel,
    GeomState, KeysStrategy,
};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────

/// Default glyph diameter when the user doesn't bind / set `"size"`.
const DEFAULT_SIZE_PT: f64 = 5.0;
/// Default glyph shape name.
const DEFAULT_SHAPE: &str = "circle";
/// Default stroke linewidth in pt when stroking a glyph outline.
const DEFAULT_STROKE_WIDTH_PT: f64 = 1.0;
/// Reference local-bbox height for built-in vector shapes (circle:
/// r=0.8 → bbox 1.6×1.6). The glyph branch scales font-size by
/// `GLYPH_BBOX_REFERENCE / em_bbox.height()` so a glyph shape at a given
/// `"size"` renders with a bounding-box height comparable to a vector
/// shape at the same `"size"`. (Visible glyph ink remains ~70% of its
/// em-box due to font cap-height; that residual mismatch is documented
/// but not corrected — would require per-font metric reads.)
const GLYPH_BBOX_REFERENCE: f64 = 1.6;

/// Catalog of channels this geom recognises, with their expected scale
/// output type. New channels: add an entry here + handle the resolved
/// value in `draw`. The `filter_declared` helper turns this into the
/// per-instance `ChannelDecl` list reported to `view.validate()`.
const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("fill", ExpectedOutput::Colors),
    ("stroke", ExpectedOutput::Colors),
    ("fill_opacity", ExpectedOutput::Numbers),
    ("stroke_opacity", ExpectedOutput::Numbers),
    ("size", ExpectedOutput::Numbers),
    ("size_band", ExpectedOutput::Numbers),
    ("shape", ExpectedOutput::Strings),
    ("angle", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── PointGeom ───────────────────────────────────────────────────────────────

/// A vectorised point geom. Non-generic — all channel data flows through
/// the `DataColumn` enum.
pub struct PointGeom {
    pub(crate) state: GeomState,
}

crate::impl_geom_inherents!(PointGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for PointGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, mut channels) = builder.into_parts();

        // x and y are mandatory data columns. Row count = x.len(); all
        // other channels length-validated against it.
        let n = require_data_column("x", &channels, "PointGeom").len();
        let y_len = require_data_column("y", &channels, "PointGeom").len();
        if y_len != n {
            panic!("PointGeom::build: \"y\" length {y_len} does not match \"x\" length {n}");
        }
        validate_channel_lengths(&channels, n, "PointGeom");
        validate_pick_id_channel(&channels, "PointGeom");

        // Install defaults for size + shape if unset.
        channels
            .entry("size".to_string())
            .or_insert_with(|| Channel::Constant(Value::Number(DEFAULT_SIZE_PT)));
        channels
            .entry("shape".to_string())
            .or_insert_with(|| Channel::Constant(Value::String(Arc::from(DEFAULT_SHAPE))));

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::PerRow, declared);
        PointGeom { state }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for PointGeom {
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

        // Resolve scales by channel name (None == identity / position-frac).
        // `Channel::RawData` columns bypass the scale; we shadow the bound
        // scale to None after the column pattern-match below so position
        // resolution + band_width_at uniformly skip it.
        let x_scale_bound = ctx.scale_for("x");
        let y_scale_bound = ctx.scale_for("y");
        let fill_scale = ctx.scale_for("fill");
        let stroke_scale = ctx.scale_for("stroke");
        let fill_opacity_scale = ctx.scale_for("fill_opacity");
        let stroke_opacity_scale = ctx.scale_for("stroke_opacity");
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let size_scale = ctx.scale_for("size");
        let size_band_scale = ctx.scale_for("size_band");
        let angle_scale = ctx.scale_for("angle");
        let pick_id_scale = ctx.scale_for("pick_id");

        // x/y are always data columns (build_from guaranteed). RawData
        // columns supply pre-computed panel fractions and disable the
        // bound scale for that axis.
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

        let fill_ch = channels.get("fill");
        let stroke_ch = channels.get("stroke");
        let fill_opacity_ch = channels.get("fill_opacity");
        let stroke_opacity_ch = channels.get("stroke_opacity");
        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let size_ch = channels.get("size");
        let size_band_ch = channels.get("size_band");
        let shape_ch = channels.get("shape");
        let angle_ch = channels.get("angle");
        let pick_id_ch = channels.get("pick_id");

        for i in 0..n {
            // ── Position (per row) ──
            let x_raw = x_col.get(i);
            let y_raw = y_col.get(i);
            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
            let px_frac = resolve_position(x_raw.clone(), x_scale, x_band);
            let py_frac = resolve_position(y_raw.clone(), y_scale, y_band);
            if !px_frac.is_finite() || !py_frac.is_finite() {
                continue;
            }
            let mut px = panel.x0 + px_frac * panel_w;
            let mut py = panel.y1 - py_frac * panel_h; // y flips

            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                px += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                py -= pt_to_px(off, ctx.dpi);
            }

            // ── Channel resolves ──
            let fill_color = override_alpha(
                resolve_color_channel(fill_ch, fill_scale, i),
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i),
            );
            let stroke_color = override_alpha(
                resolve_color_channel(stroke_ch, stroke_scale, i),
                resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, i),
            );
            let size_pt = resolve_number_channel_or(size_ch, size_scale, i, DEFAULT_SIZE_PT);
            let shape_name = resolve_str_channel_or(shape_ch, None, i, DEFAULT_SHAPE);

            // Glyph diameter: pt contribution + band contribution. band_px
            // is the smallest non-zero band width across x and y at the
            // row's centre (matches WedgeGeom's radius_band semantics).
            let size_band = resolve_number_channel_or(size_band_ch, size_band_scale, i, 0.0);
            let x_band_px = band_width_at(x_scale, &x_raw) * panel_w;
            let y_band_px = band_width_at(y_scale, &y_raw) * panel_h;
            let band_px = smallest_nonzero(x_band_px, y_band_px);
            let size_px = pt_to_px(size_pt, ctx.dpi) + size_band * band_px;
            if !size_px.is_finite() || size_px <= 0.0 {
                continue;
            }

            // ── Shape lookup ──
            let shape: &Shape = match ctx.shapes.get(&shape_name) {
                Some(s) => s,
                None => continue,
            };

            // Rotation: math CCW from the user (positive = visible
            // counter-clockwise). Kurbo's `Affine::rotate` uses
            // mathematical convention where positive theta rotates +x
            // toward +y — in screen space (y-down) this looks clockwise.
            // Negate to get user-visible CCW. Rotation is around the
            // glyph's own centre, which is the path origin pre-translate.
            let angle = resolve_angle_channel(angle_ch, angle_scale, i);
            let xform = if angle == 0.0 {
                Affine::translate((px, py)) * Affine::scale(size_px)
            } else {
                Affine::translate((px, py)) * Affine::rotate(-angle) * Affine::scale(size_px)
            };

            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i);
            match shape.kind() {
                ShapeKind::Paths { paths, style } => {
                    for sub in paths {
                        match style {
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
                                    let st =
                                        Stroke::new(pt_to_px(DEFAULT_STROKE_WIDTH_PT, ctx.dpi));
                                    scene.stroke(&st, xform, &Brush::Solid(sc), None, sub, pick);
                                }
                            }
                            ShapeStyle::Stroke => {
                                if let Some(sc) = stroke_color {
                                    let st =
                                        Stroke::new(pt_to_px(DEFAULT_STROKE_WIDTH_PT, ctx.dpi));
                                    scene.stroke(&st, xform, &Brush::Solid(sc), None, sub, pick);
                                }
                            }
                        }
                    }
                }
                ShapeKind::Glyph {
                    font,
                    glyph_id,
                    em_bbox,
                    em_origin,
                } => {
                    let Some(fc) = fill_color else { continue };
                    // Normalise glyph height to the vector-shape bbox
                    // convention so vector and glyph markers at the same
                    // "size" render at comparable visual extent.
                    let h = em_bbox.height();
                    if h <= 0.0 || !h.is_finite() {
                        continue;
                    }
                    let bbox_norm = GLYPH_BBOX_REFERENCE / h;
                    let centring =
                        Affine::translate(em_origin.to_vec2() - em_bbox.center().to_vec2());
                    let glyphs = [Glyph {
                        id: glyph_id,
                        x: 0.0,
                        y: 0.0,
                    }];
                    let brush = Brush::Solid(fc);
                    let run = GlyphRun {
                        font,
                        font_size: 1.0,
                        transform: xform * Affine::scale(bbox_norm) * centring,
                        glyph_transform: None,
                        brush: &brush,
                        brush_alpha: 1.0,
                        hint: false,
                        glyphs: &glyphs,
                    };
                    scene.draw_glyphs(&run, pick);
                }
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::geometry::Rect;
    use crate::plot::geom::{DirectScaleResolver, Keys, Raw};
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
        match &g.state.keys {
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
    fn builder_temporal_x_ok() {
        let g = PointGeom::builder()
            .set(
                "x",
                vec![Date::from_ymd(2024, 1, 1), Date::from_ymd(2024, 1, 2)],
            )
            .set("y", vec![0.0_f64, 1.0])
            .build();
        assert_eq!(g.len(), 2);
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_mismatched_lengths_panic() {
        PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
    }

    #[test]
    fn builder_installs_default_size_and_shape() {
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .build();
        // Constants for size+shape live as Channel::Constant in state.
        match g.state.channels.get("size") {
            Some(Channel::Constant(Value::Number(n))) => assert_eq!(*n, 5.0),
            other => panic!("expected default size constant, got {other:?}"),
        }
        match g.state.channels.get("shape") {
            Some(Channel::Constant(Value::String(s))) => assert_eq!(&**s, "circle"),
            other => panic!("expected default shape constant, got {other:?}"),
        }
    }

    // ── Draw output ──

    fn no_scales<'a>() -> DirectScaleResolver<'a> {
        DirectScaleResolver::new()
    }

    fn red_solid() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    #[test]
    fn draw_emits_one_op_per_row() {
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, 0.5, 1.0])
            .set("y", vec![0.0_f64, 1.0, 0.0])
            .set("fill", red_solid())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 3);
    }

    fn synthetic_glyph_shape() -> crate::shape::Shape {
        let blob = peniko::Blob::new(std::sync::Arc::new(Vec::<u8>::new()));
        let font = crate::scene::Font::new(blob, 0);
        let em_bbox = crate::geometry::Rect::new(0.0, 0.0, 0.6, 1.0);
        let em_origin = crate::geometry::Point::new(0.05, 0.8);
        let anchor = crate::geometry::Point::new(-0.5, 0.0);
        crate::shape::Shape::glyph(font, 1, em_bbox, em_origin, anchor)
    }

    #[test]
    fn glyph_shape_emits_draw_glyphs() {
        let mut shapes = registry();
        shapes.insert("synthetic-glyph", synthetic_glyph_shape());
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, 0.5, 1.0])
            .set("y", vec![0.0_f64, 1.0, 0.0])
            .set("fill", red_solid())
            .set("shape", "synthetic-glyph")
            .set("size", 14.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);

        let glyph_ops: Vec<_> = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawGlyphs(_)))
            .collect();
        assert_eq!(glyph_ops.len(), 3, "one DrawGlyphs op per row");

        // Fills/strokes from glyph rows: none.
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(fills, 0);
        assert_eq!(strokes, 0);
    }

    #[test]
    fn glyph_shape_with_no_fill_emits_nothing() {
        // Stroke channel is ignored for glyph shapes; without a fill,
        // nothing should be emitted.
        let mut shapes = registry();
        shapes.insert("synthetic-glyph", synthetic_glyph_shape());
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .set("stroke", red_solid())
            .set("shape", "synthetic-glyph")
            .build();
        g.rebuild_diff_against_previous();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        assert!(
            scene.ops.is_empty(),
            "glyph shape with stroke-only should emit nothing, got {:?}",
            scene.ops
        );
    }

    #[test]
    fn mixed_path_and_glyph_shapes_both_render() {
        // Per-row mix: half the rows use the vector "circle", half use a
        // glyph shape. Recording should contain both Fill and DrawGlyphs.
        let mut shapes = registry();
        shapes.insert("synthetic-glyph", synthetic_glyph_shape());
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, 0.25, 0.5, 0.75])
            .set("y", vec![0.5_f64, 0.5, 0.5, 0.5])
            .set("fill", red_solid())
            .set(
                "shape",
                vec!["circle", "circle", "synthetic-glyph", "synthetic-glyph"],
            )
            .build();
        g.rebuild_diff_against_previous();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);

        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let glyphs = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawGlyphs(_)))
            .count();
        assert_eq!(fills, 2, "two vector-shape fills");
        assert_eq!(glyphs, 2, "two glyph draws");
    }

    #[test]
    fn draw_skips_non_finite_rows() {
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, f64::NAN, 1.0])
            .set("y", vec![0.0_f64, 1.0, 0.0])
            .set("fill", red_solid())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 2);
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .set("fill", red_solid())
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    // ── More build() validation ──

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

    // ── declared_channels ──

    #[test]
    fn declared_channels_sorted_and_classified() {
        use std::collections::HashMap;
        let g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("fill", Color::new([1.0, 0.0, 0.0, 1.0]))
            .build();
        let decls = g.declared_channels();
        let names: Vec<_> = decls.iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["fill", "shape", "size", "x", "y"]);

        let by_name: HashMap<_, _> = decls.iter().map(|d| (d.name, d)).collect();
        assert!(by_name["x"].data_bound);
        assert!(by_name["y"].data_bound);
        assert!(!by_name["fill"].data_bound);
        assert_eq!(by_name["x"].expected_output, ExpectedOutput::Numbers);
        assert_eq!(by_name["fill"].expected_output, ExpectedOutput::Colors);
        assert_eq!(by_name["shape"].expected_output, ExpectedOutput::Strings);
    }

    #[test]
    fn declared_opacity_channels_are_numeric() {
        use std::collections::HashMap;
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
        g.rebuild_diff_against_previous();
        assert_eq!(g.state.enter, vec![0, 1, 2]);
        assert!(g.state.update.is_empty());
        assert!(g.state.exit.is_empty());
        g.set("y", vec![10.0_f64, 20.0, 30.0]);
        g.rebuild_diff_against_previous();
        assert!(g.state.enter.is_empty());
        assert_eq!(g.state.update, vec![(0, 0), (1, 1), (2, 2)]);
        assert!(g.state.exit.is_empty());
    }

    #[test]
    fn update_closure_atomic_multi_channel() {
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![10.0_f64, 20.0, 30.0])
            .build();
        g.update(|b| {
            b.set("x", vec![100.0_f64, 200.0, 300.0, 400.0]);
            b.set("y", vec![1.0_f64, 2.0, 3.0, 4.0]);
        });
        assert_eq!(g.len(), 4);
        g.rebuild_diff_against_previous();
        assert_eq!(g.state.update, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(g.state.enter, vec![3]);
        assert!(g.state.exit.is_empty());
    }

    #[test]
    fn update_closure_can_change_keys() {
        let mut g = PointGeom::builder()
            .keys(vec!["a", "b", "c"])
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .build();
        g.rebuild_diff_against_previous();

        g.update(|b| {
            b.keys(vec!["c", "a", "d"]);
            b.set("x", vec![20.0_f64, 0.0, 99.0]);
            b.set("y", vec![20.0_f64, 0.0, 99.0]);
        });
        g.rebuild_diff_against_previous();
        assert_eq!(g.state.update, vec![(2, 0), (0, 1)]);
        assert_eq!(g.state.enter, vec![2]);
        assert_eq!(g.state.exit.len(), 1);
        assert_eq!(g.state.exit[0].as_str(), Some("b"));
    }

    #[test]
    #[should_panic(expected = "must be data, not constant")]
    fn update_closure_validation_panics_on_invalid_state() {
        let mut g = PointGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
        g.update(|b| {
            b.set("x", 5.0);
        });
    }

    #[test]
    fn diff_columns_path_with_reordered_keys() {
        let mut g = PointGeom::builder()
            .keys(vec!["a", "b", "c"])
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .build();
        g.rebuild_diff_against_previous();
        assert_eq!(g.state.enter, vec![0, 1, 2]);

        g.state.keys = Keys::Explicit(vec!["c", "a", "b"].into());
        g.state.dirty = true;
        g.rebuild_diff_against_previous();
        assert!(g.state.enter.is_empty());
        assert!(g.state.exit.is_empty());
        assert_eq!(g.state.update, vec![(2, 0), (0, 1), (1, 2)]);
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
            .set("fill", red_solid())
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert!(fills >= 1);
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
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert_eq!(fills, 0);
        assert!(strokes >= 1);
    }

    #[test]
    fn draw_both_fill_and_stroke() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("stroke", Color::new([0.0, 0.0, 0.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert!(fills >= 1);
        assert!(strokes >= 1);
    }

    #[test]
    fn draw_fill_opacity_overrides_alpha() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("fill_opacity", 0.25)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
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
        assert!(!alphas.is_empty());
        for a in &alphas {
            assert!((*a as f64 - 0.25).abs() < 1e-6);
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
        let resolver = no_scales();
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
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Color::new([1.0, 0.0, 0.0, 0.7]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
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
        let g = PointGeom::builder()
            .set("x", vec![0.25_f64, 0.75])
            .set("y", vec![0.5_f64, 0.5])
            .set("fill", Color::new([0.0, 0.0, 1.0, 1.0]))
            .set("fill_opacity", vec![0.2_f64, 0.8])
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
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
        assert_eq!(alphas.len(), 2);
        assert!((alphas[0] as f64 - 0.2).abs() < 1e-6);
        assert!((alphas[1] as f64 - 0.8).abs() < 1e-6);
    }

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
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("x_offset", 9.0)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (px, py) = first_fill_translation(&scene).expect("fill op");
        assert!((px - 62.0).abs() < 1e-6, "px = {px}");
        assert!((py - 50.0).abs() < 1e-6, "py = {py}");
    }

    #[test]
    fn draw_y_offset_positive_is_up() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("y_offset", 9.0)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (px, py) = first_fill_translation(&scene).expect("fill op");
        assert!((px - 50.0).abs() < 1e-6);
        assert!((py - 38.0).abs() < 1e-6, "py = {py}");
    }

    #[test]
    fn draw_x_band_offset_on_discrete_scale() {
        use crate::plot::scale;
        let x_scale = scale::discrete(
            ["a", "b", "c", "d"]
                .into_iter()
                .map(|s| crate::plot::value::Value::String(Arc::from(s))),
        );
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = PointGeom::builder()
            .set("x", vec!["b"])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("x_band", 0.5)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (px, _py) = first_fill_translation(&scene).expect("fill op");
        assert!((px - 50.0).abs() < 1e-6, "px = {px}");
    }

    #[test]
    fn draw_x_band_no_op_on_continuous_scale() {
        use crate::plot::scale;
        let x_scale = scale::continuous(0.0..=10.0);
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = PointGeom::builder()
            .set("x", vec![5.0_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("x_band", 0.5)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (px, _py) = first_fill_translation(&scene).expect("fill op");
        assert!((px - 50.0).abs() < 1e-6);
    }

    #[test]
    fn draw_per_row_offset_jitter() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64, 0.5, 0.5])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("fill", red_solid())
            .set("x_offset", vec![-9.0_f64, 0.0, 9.0])
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
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

    // ── Raw (scale-bypass) channels ──

    #[test]
    fn raw_position_bypasses_scale() {
        // x_scale maps domain [0..100] → [0,1] fraction. A Raw column
        // should bypass that mapping entirely — supplied values are
        // treated as panel fractions directly.
        use crate::plot::scale;
        let x_scale = scale::continuous(0.0..=100.0);
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = PointGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.5, 0.75]))
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("fill", red_solid())
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
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
        // Without bypass these would be domain values run through the
        // scale → 0.25, 0.5, 0.75 → 0.0025, 0.005, 0.0075 of the panel.
        // With bypass they're already fractions → 25, 50, 75 px.
        assert!((xs[0] - 25.0).abs() < 1e-6, "xs[0] = {}", xs[0]);
        assert!((xs[1] - 50.0).abs() < 1e-6, "xs[1] = {}", xs[1]);
        assert!((xs[2] - 75.0).abs() < 1e-6, "xs[2] = {}", xs[2]);
    }

    #[test]
    fn raw_color_bypasses_scale() {
        // Even when a colour scale is bound to "fill", a Raw colour
        // ignores it and uses the literal value.
        use crate::plot::scale;
        use crate::plot::value::Value;
        let fill_scale = scale::ordinal(["a", "b"].iter().map(|s| Value::String(Arc::from(*s))))
            .range_colors([
                Color::new([0.0, 0.0, 1.0, 1.0]),
                Color::new([0.0, 1.0, 0.0, 1.0]),
            ]);
        let resolver = DirectScaleResolver::new().with("fill", &fill_scale);
        let literal = Color::new([1.0, 0.0, 0.0, 1.0]);
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", Raw(literal))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let fill_color = scene.ops.iter().find_map(|op| match op {
            Op::Fill {
                brush: crate::brush::Brush::Solid(c),
                ..
            } => Some(*c),
            _ => None,
        });
        let c = fill_color.expect("fill op");
        assert!((c.components[0] - 1.0).abs() < 1e-6);
        assert!((c.components[1] - 0.0).abs() < 1e-6);
        assert!((c.components[2] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn raw_position_outside_panel_clips() {
        // Raw fractions outside [0, 1] are drawn at the corresponding
        // off-panel pixel; the panel clip in draw_panel_into handles
        // the visual cutoff. Here we just verify the geom emits the
        // op with the off-panel translation (no skip / no panic).
        let g = PointGeom::builder()
            .set("x", Raw(vec![-0.5_f64, 1.5]))
            .set("y", vec![0.5_f64, 0.5])
            .set("fill", red_solid())
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
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
        assert_eq!(xs.len(), 2);
        assert!((xs[0] - -50.0).abs() < 1e-6);
        assert!((xs[1] - 150.0).abs() < 1e-6);
    }

    #[test]
    fn raw_constant_size_bypasses_size_scale() {
        // size_scale maps domain values to pt; Raw("size", 20.0) skips
        // it and uses 20pt directly. 20pt at 96dpi = ~26.67 px.
        use crate::plot::scale;
        let size_scale = scale::continuous(0.0..=10.0).range_numbers([2.0, 10.0]);
        let resolver = DirectScaleResolver::new().with("size", &size_scale);
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("size", Raw(20.0_f64))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let scale_factor = scene.ops.iter().find_map(|op| match op {
            Op::Fill { transform, .. } => Some(transform.as_coeffs()[0]),
            _ => None,
        });
        let s = scale_factor.expect("fill");
        // 20pt → 20 * 96/72 = 26.6667 px.
        assert!((s - 20.0 * 96.0 / 72.0).abs() < 1e-6, "size = {s}");
    }

    #[test]
    fn raw_length_validated_at_build() {
        // RawData length mismatch panics just like Data length mismatch.
        let r = std::panic::catch_unwind(|| {
            PointGeom::builder()
                .set("x", vec![0.0_f64, 1.0])
                .set("y", Raw(vec![0.5_f64, 0.5, 0.5])) // wrong length
                .build()
        });
        assert!(r.is_err());
    }

    #[test]
    fn raw_data_x_is_required_position_data() {
        // require_data_column accepts RawData for required position
        // channels — building succeeds.
        let g = PointGeom::builder()
            .set("x", Raw(vec![0.25_f64, 0.75]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .build();
        assert_eq!(g.len(), 2);
    }

    #[test]
    #[should_panic(expected = "must be a non-negative integer")]
    fn raw_pick_id_validated_at_build() {
        // RawConstant pick_id is build-validated the same as Constant.
        PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("pick_id", Raw(0x100_0000_i64))
            .build();
    }

    #[test]
    fn raw_pick_id_passes_through_per_row() {
        let g = PointGeom::builder()
            .set("x", vec![0.2_f64, 0.5, 0.8])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("fill", red_solid())
            .set("pick_id", Raw(vec![5_i64, 6, 7]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let picks: Vec<u32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill {
                    pick_id: crate::pick::PickId::Id(n),
                    ..
                } => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(picks, vec![5, 6, 7]);
    }

    #[test]
    fn draw_neither_fill_nor_stroke_emits_nothing() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert_eq!(fills, 0);
        assert_eq!(strokes, 0);
    }

    #[test]
    fn draw_vectorised_n_rows() {
        let g = PointGeom::builder()
            .set("x", vec![0.1_f64, 0.3, 0.5, 0.7, 0.9])
            .set("y", vec![0.5_f64; 5])
            .set("fill", Color::new([0.0, 0.0, 1.0, 1.0]))
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, _) = count_ops(&scene.ops);
        assert!(fills >= 5);
    }

    #[test]
    fn draw_routes_x_through_scale() {
        use crate::plot::scale;
        let x_scale = scale::continuous(0.0..=100.0);
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = PointGeom::builder()
            .set("x", vec![50.0_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, _) = count_ops(&scene.ops);
        assert!(fills >= 1);
    }

    #[test]
    fn draw_skips_unknown_shape() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("shape", "definitely-not-a-shape")
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let (fills, strokes) = count_ops(&scene.ops);
        assert_eq!(fills, 0);
        assert_eq!(strokes, 0);
    }

    fn first_fill_scale(scene: &RecordingScene) -> Option<f64> {
        for op in &scene.ops {
            if let Op::Fill { transform, .. } = op {
                let m = transform.as_coeffs();
                // For Affine::translate(...) * Affine::scale(s), the
                // first coefficient is the x scale factor — equal to s.
                return Some(m[0]);
            }
        }
        None
    }

    #[test]
    fn draw_size_band_on_discrete_x_sizes_to_band() {
        use crate::plot::scale;
        let x_scale = scale::discrete(
            ["a", "b", "c", "d"]
                .into_iter()
                .map(|s| Value::String(Arc::from(s))),
        );
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        // Panel 100 wide, 4 bands → band = 25 px. size_band = 1.0,
        // size = 0 → diameter = 25 px.
        let g = PointGeom::builder()
            .set("x", vec!["b"])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("size", 0.0_f64)
            .set("size_band", 1.0_f64)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let s = first_fill_scale(&scene).expect("fill");
        assert!((s - 25.0).abs() < 1e-6, "diameter = {s}");
    }

    #[test]
    fn draw_size_band_no_op_on_continuous_axes() {
        // Both axes continuous → band contribution drops out; only the
        // pt size remains.
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("size", 6.0_f64)
            .set("size_band", 1.0_f64)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let s = first_fill_scale(&scene).expect("fill");
        // 6 pt at 96 dpi = 8 px.
        assert!((s - 8.0).abs() < 1e-6, "diameter = {s}");
    }

    #[test]
    fn draw_size_band_picks_smallest_when_both_axes_discrete() {
        use crate::plot::scale;
        // x: 4 bands over 100 px → 25 px; y: 2 bands over 100 px → 50 px.
        // smallest_nonzero picks 25 → diameter = 25 px at size_band = 1.0.
        let x_scale = scale::discrete(
            ["a", "b", "c", "d"]
                .into_iter()
                .map(|s| Value::String(Arc::from(s))),
        );
        let y_scale = scale::discrete(["p", "q"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new()
            .with("x", &x_scale)
            .with("y", &y_scale);
        let g = PointGeom::builder()
            .set("x", vec!["b"])
            .set("y", vec!["q"])
            .set("fill", red_solid())
            .set("size", 0.0_f64)
            .set("size_band", 1.0_f64)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let s = first_fill_scale(&scene).expect("fill");
        assert!((s - 25.0).abs() < 1e-6, "diameter = {s}");
    }

    #[test]
    fn draw_size_band_additive_with_size_pt() {
        use crate::plot::scale;
        // size = 6pt (= 8px at 96dpi), size_band = 0.5 over 25px band
        // → diameter = 8 + 12.5 = 20.5 px.
        let x_scale = scale::discrete(
            ["a", "b", "c", "d"]
                .into_iter()
                .map(|s| Value::String(Arc::from(s))),
        );
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = PointGeom::builder()
            .set("x", vec!["b"])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("size", 6.0_f64)
            .set("size_band", 0.5_f64)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let s = first_fill_scale(&scene).expect("fill");
        assert!((s - 20.5).abs() < 1e-6, "diameter = {s}");
    }

    #[test]
    fn angle_zero_produces_unrotated_recording() {
        // Regression guard: angle=0 must produce the same Affine as a
        // build with no `angle` channel at all.
        let g_no_angle = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("size", 10.0_f64)
            .build();
        let g_zero = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("size", 10.0_f64)
            .set("angle", 0.0_f64)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut s1 = RecordingScene::default();
        let mut s2 = RecordingScene::default();
        g_no_angle.draw(&mut s1, &ctx(panel, &shapes, &resolver));
        g_zero.draw(&mut s2, &ctx(panel, &shapes, &resolver));
        let t1 = first_fill_translation(&s1).unwrap();
        let t2 = first_fill_translation(&s2).unwrap();
        assert!((t1.0 - t2.0).abs() < 1e-9 && (t1.1 - t2.1).abs() < 1e-9);
    }

    #[test]
    fn angle_rotates_glyph_about_centre_math_ccw() {
        // triangle-up apex is at path-local (0, -0.92). After a math-CCW
        // rotation of π/2 about the placement point, the apex should
        // land to the LEFT of the placement point (because math CCW in
        // a y-up frame moves +y → -x; on the screen y-down frame, the
        // geom internally negates angle so the visible motion is +up →
        // -x = left).
        use std::f64::consts::FRAC_PI_2;
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .set("size", 10.0_f64)
            .set("shape", "triangle-up")
            .set("angle", FRAC_PI_2)
            .build();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        let xform = scene.ops.iter().find_map(|op| match op {
            Op::Fill { transform, .. } => Some(*transform),
            _ => None,
        });
        let xform = xform.expect("fill op");
        // Apex world position. size_px = pt_to_px(10) = 10*96/72 ≈ 13.33.
        // Apex path (0, -0.92). Math CCW by π/2 should put the apex at
        // (x_centre - 0.92*size_px, y_centre).
        let apex_path = crate::geometry::Point::new(0.0, -0.92);
        let apex_world = xform * apex_path;
        let size_px = 10.0 * 96.0 / 72.0;
        let expected_x = 50.0 - 0.92 * size_px;
        let expected_y = 50.0;
        assert!(
            (apex_world.x - expected_x).abs() < 0.5,
            "apex.x = {}, expected {}",
            apex_world.x,
            expected_x
        );
        assert!(
            (apex_world.y - expected_y).abs() < 0.5,
            "apex.y = {}, expected {}",
            apex_world.y,
            expected_y
        );
    }

    #[test]
    fn draw_silent_on_degenerate_panel() {
        let g = PointGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("fill", red_solid())
            .build();
        let panel = Rect::new(0.0, 0.0, 0.0, 0.0);
        let shapes = registry();
        let resolver = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &resolver));
        assert!(scene.ops.is_empty());
    }
}
