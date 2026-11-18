//! Vector fill patterns: turn a [`FillPattern`] tile into scene ops, and
//! decompose a pattern fill into a clipped replay any backend can draw.
//!
//! The vector backends (SVG, PDF) express a pattern natively and do not
//! need the replay decomposition; both share [`bound`], which gives a
//! pattern that reaches a backend with its paint roles still unbound a
//! deterministic meaning.

use crate::blend::BlendMode;
use crate::brush::{bind_pattern, Brush, FillPattern, PatternElement, PatternPaint};
use crate::color::Color;
use crate::geometry::{Affine, Point, Rect};
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::{PickId, PickScope};
use crate::scene::{recording::RecordingScene, GlyphRun, SceneBuilder};
use crate::shape::{Shape, ShapeKind, ShapeRegistry, ShapeStyle};
use crate::stroke::Stroke;

/// Bind a pattern's paint roles to the backend defaults: `Foreground` to
/// opaque black, `Background` to transparent. Authors of the plot layer
/// bind roles to real brushes with [`bind_pattern`] before a pattern
/// reaches the scene; a pattern that arrives unbound — anything built on
/// the low-level scene API — still paints deterministically.
pub fn bound(pattern: &FillPattern) -> FillPattern {
    bind_pattern(
        pattern,
        &Brush::Solid(Color::BLACK),
        &Brush::Solid(Color::TRANSPARENT),
    )
}

/// Record a pattern's tile as scene ops in tile coordinates.
///
/// Path elements become fill/stroke ops; shape elements are resolved
/// against the built-in registry and stamped by their anchor. Text
/// elements are shaped into glyph runs: `size` (a multiple of the tile
/// height) becomes the em size in tile units, and `at` is the baseline
/// origin. Shaping happens against the process-global font context, so
/// a family name that resolves nowhere falls back like any other text.
/// Glyph-backed shapes are skipped.
pub fn tile_recording(pattern: &FillPattern) -> RecordingScene {
    let pattern = bound(pattern);
    let tile_h = pattern.tile_rect.height();
    let shapes = ShapeRegistry::with_builtins();
    let mut rec = RecordingScene::new();
    for el in &pattern.elements {
        match el {
            PatternElement::Path { path, fill, stroke } => {
                if let Some(PatternPaint::Fixed(b)) = fill {
                    rec.fill(
                        FillRule::NonZero,
                        Affine::IDENTITY,
                        b,
                        None,
                        path,
                        PickId::Skip,
                    );
                }
                if let Some(s) = stroke {
                    if let PatternPaint::Fixed(b) = &s.1 {
                        rec.stroke(&s.0, Affine::IDENTITY, b, None, path, PickId::Skip);
                    }
                }
            }
            PatternElement::Shape {
                name,
                at,
                size,
                paint,
            } => {
                let PatternPaint::Fixed(b) = paint else {
                    continue;
                };
                let Some(shape) = shapes.get(name.as_ref()) else {
                    continue;
                };
                stamp_shape(&mut rec, shape, *at, size * tile_h, b);
            }
            PatternElement::Text {
                content,
                at,
                size,
                family,
                paint,
            } => {
                let PatternPaint::Fixed(b) = paint else {
                    continue;
                };
                // dpi 72 so the point size is the em size in tile
                // units: parley's size_px = size_pt × dpi / 72.
                let mut style = crate::text::TextStyle::new((size * tile_h) as f32);
                if let Some(f) = family {
                    style = style.family(f.as_ref());
                }
                let run = crate::text::TextRun::new(content, &style, 72.0);
                crate::text::draw_text(
                    &mut rec,
                    &run,
                    at.x,
                    at.y,
                    b,
                    Affine::IDENTITY,
                    PickId::Skip,
                );
            }
        }
    }
    rec
}

/// Stamp `shape` centred on `at` at `height` px, painted with `brush`.
fn stamp_shape(rec: &mut RecordingScene, shape: &Shape, at: Point, height: f64, brush: &Brush) {
    let ShapeKind::Paths { paths, style } = shape.kind() else {
        return;
    };
    let bbox = shape.bounding_box();
    if !(bbox.height() > 0.0 && height.is_finite() && height > 0.0) {
        return;
    }
    let scale = height / bbox.height();
    let anchor = shape.anchor();
    let xform = Affine::translate(at.to_vec2())
        * Affine::scale(scale)
        * Affine::translate((-anchor.x, -anchor.y));
    match style {
        ShapeStyle::Fill => {
            for p in paths {
                rec.fill(FillRule::NonZero, xform, brush, None, p, PickId::Skip);
            }
        }
        ShapeStyle::Stroke => {
            // Stroked under `xform`, so divide the intended width by the
            // stamp's scale to land at it after scaling.
            let stroke = Stroke::new(height * 0.125 / scale);
            for p in paths {
                rec.stroke(&stroke, xform, brush, None, p, PickId::Skip);
            }
        }
    }
}

/// Fill `path` with `pattern` by replaying its tile, clipped to the
/// path, once per tile the path's bounds cover. The rasterising
/// backends' answer to a vector pattern; the vector backends emit a
/// native pattern instead (see `backend/svg` and `backend/pdf`).
///
/// A singular pattern transform — one the tiling cannot be inverted
/// through — paints nothing. Picking reports the filled region, not
/// individual tiles: tile ops carry [`PickId::Skip`], and an invisible
/// fill of the whole path carries `pick_id`.
pub fn fill_tiled(
    scene: &mut dyn SceneBuilder,
    rule: FillRule,
    transform: Affine,
    pattern: &FillPattern,
    brush_transform: Option<Affine>,
    path: &Path,
    pick_id: PickId,
) {
    let p2u = transform * brush_transform.unwrap_or(Affine::IDENTITY) * pattern.transform;
    let [a, b, c, d, _, _] = p2u.as_coeffs();
    let det = a * d - b * c;
    if !(det.is_finite() && det.abs() > 1e-12) {
        return;
    }
    let u2p = p2u.inverse();

    // The path's bounds in user space, mapped back into pattern space
    // to find the tiles it covers. A transform's image of a rect is not
    // a rect, so the four corners bound it conservatively.
    let bbox = crate::path::bounding_box(path);
    let corners = [
        Point::new(bbox.x0, bbox.y0),
        Point::new(bbox.x1, bbox.y0),
        Point::new(bbox.x1, bbox.y1),
        Point::new(bbox.x0, bbox.y1),
    ];
    let mut area = Rect::new(
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for corner in corners {
        let p = u2p * (transform * corner);
        area = area.union(Rect::from_points(p, p));
    }
    if !area.is_finite() || area.width() <= 0.0 || area.height() <= 0.0 {
        return;
    }

    let tile = tile_recording(pattern);
    let tr = &pattern.tile_rect;
    let [sx, sy] = [pattern.spacing.x, pattern.spacing.y];
    let i0 = ((area.x0 - tr.x0) / sx).floor() as i64;
    let i1 = ((area.x1 - tr.x0) / sx).ceil() as i64;
    let j0 = ((area.y0 - tr.y0) / sy).floor() as i64;
    let j1 = ((area.y1 - tr.y0) / sy).ceil() as i64;

    scene.push_layer(BlendMode::NORMAL, 1.0, transform, path);
    for j in j0..j1 {
        for i in i0..i1 {
            let origin = Affine::translate((tr.x0 + i as f64 * sx, tr.y0 + j as f64 * sy));
            let mut shifted = TransformAdapter {
                inner: scene,
                pre: p2u * origin,
            };
            tile.replay(&mut shifted);
        }
    }
    scene.pop_layer();

    scene.fill(
        rule,
        transform,
        &Brush::Solid(Color::TRANSPARENT),
        None,
        path,
        pick_id,
    );
}

/// A `SceneBuilder` pass-through that pre-multiplies every transform by
/// `pre`. Used to replay a tile recording at each tile origin; tile
/// recordings hold only fill and stroke ops, so the glyph and image
/// paths forward unchanged.
struct TransformAdapter<'a> {
    inner: &'a mut dyn SceneBuilder,
    pre: Affine,
}

impl SceneBuilder for TransformAdapter<'_> {
    fn clear(&mut self) {}

    fn fill(
        &mut self,
        rule: FillRule,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    ) {
        self.inner.fill(
            rule,
            self.pre * transform,
            brush,
            brush_transform,
            path,
            pick_id,
        );
    }

    fn stroke(
        &mut self,
        stroke: &Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    ) {
        self.inner.stroke(
            stroke,
            self.pre * transform,
            brush,
            brush_transform,
            path,
            pick_id,
        );
    }

    fn draw_image(
        &mut self,
        image: &crate::brush::Image,
        transform: Affine,
        sampling: crate::brush::Sampling,
        alpha: f32,
        pick_id: PickId,
    ) {
        self.inner
            .draw_image(image, self.pre * transform, sampling, alpha, pick_id);
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        // Glyph positions are absolute in the run's own frame, and the
        // backend composes them with `run.transform`, so premultiplying
        // the transform is the whole job.
        let shifted = GlyphRun {
            font: run.font,
            font_size: run.font_size,
            transform: self.pre * run.transform,
            glyph_transform: run.glyph_transform,
            brush: run.brush,
            brush_alpha: run.brush_alpha,
            hint: run.hint,
            glyphs: run.glyphs,
            style: run.style,
            source: run.source,
        };
        self.inner.draw_glyphs(&shifted, pick_id);
    }

    fn draw_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId) {
        self.inner.draw_mesh(mesh, self.pre * transform, pick_id);
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        self.inner
            .push_layer(blend, alpha, self.pre * transform, clip);
    }

    fn pop_layer(&mut self) {
        self.inner.pop_layer();
    }

    fn push_pick_scope(&mut self, scope: &PickScope) {
        self.inner.push_pick_scope(scope);
    }

    fn pop_pick_scope(&mut self) {
        self.inner.pop_pick_scope();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::fill_pattern;
    use crate::scene::recording::Op;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Path {
        let mut p = Path::new();
        p.move_to((x0, y0));
        p.line_to((x1, y0));
        p.line_to((x1, y1));
        p.line_to((x0, y1));
        p.close_path();
        p
    }

    #[test]
    fn roles_bind_to_the_backend_defaults() {
        let p = fill_pattern("stripe", PatternPaint::Foreground).unwrap();
        let b = bound(&p);
        let PatternElement::Path {
            stroke: Some(s), ..
        } = &b.elements[0]
        else {
            panic!("expected a stroked path element")
        };
        assert!(matches!(
            s.1,
            PatternPaint::Fixed(Brush::Solid(c)) if c == Color::BLACK
        ));
    }

    #[test]
    fn a_stripe_tile_records_one_stroke_op() {
        let p = fill_pattern("stripe", Brush::Solid(Color::BLACK)).unwrap();
        let tile = tile_recording(&p);
        assert_eq!(tile.ops.len(), 1);
        assert!(matches!(tile.ops[0], Op::Stroke { .. }));
    }

    #[test]
    fn a_shape_element_resolves_against_the_builtin_registry() {
        let p = fill_pattern("circle", Brush::Solid(Color::BLACK)).unwrap();
        let tile = tile_recording(&p);
        assert!(!tile.ops.is_empty());
        assert!(tile.ops.iter().all(|op| matches!(op, Op::Fill { .. })));
    }

    #[test]
    fn a_text_element_records_glyph_ops() {
        let p = FillPattern::new(
            vec![PatternElement::Text {
                content: std::sync::Arc::from("ab"),
                at: Point::new(0.1, 0.85),
                size: 0.8,
                family: None,
                paint: PatternPaint::Foreground,
            }],
            Rect::new(0.0, 0.0, 1.0, 1.0),
            crate::geometry::Vec2::new(1.0, 1.0),
        );
        let tile = tile_recording(&p);
        assert!(
            tile.ops.iter().any(|op| matches!(op, Op::DrawGlyphs(_))),
            "{tile:?}"
        );
    }

    #[test]
    fn a_tiled_fill_covers_the_target_and_clips_once() {
        let p = fill_pattern("stripe", Brush::Solid(Color::BLACK)).unwrap();
        // Unit tile pitched at 1×1 with no rotation, so a rect from
        // 0.5 to 3.5 × 0.5 to 2.5 touches 4×3 = 12 tiles of one stroke
        // each.
        let p = FillPattern {
            transform: Affine::IDENTITY,
            ..p
        };
        let mut rec = RecordingScene::new();
        let target = square(0.5, 0.5, 3.5, 2.5);
        fill_tiled(
            &mut rec,
            FillRule::NonZero,
            Affine::IDENTITY,
            &p,
            None,
            &target,
            PickId::Skip,
        );
        let layers = rec
            .ops
            .iter()
            .filter(|op| matches!(op, Op::PushLayer { .. }))
            .count();
        let strokes = rec
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(layers, 1);
        assert_eq!(strokes, 12, "one stroke per touched tile: {rec:?}");
        // The trailing op is the pick overlay: a transparent fill of
        // the whole path.
        assert!(matches!(rec.ops.last(), Some(Op::Fill { .. })));
    }

    #[test]
    fn a_singular_transform_paints_nothing() {
        let p = fill_pattern("stripe", Brush::Solid(Color::BLACK))
            .unwrap()
            .with_transform(Affine::scale(0.0));
        let mut rec = RecordingScene::new();
        fill_tiled(
            &mut rec,
            FillRule::NonZero,
            Affine::IDENTITY,
            &p,
            None,
            &square(0.0, 0.0, 10.0, 10.0),
            PickId::Skip,
        );
        assert!(rec.ops.is_empty());
    }
}
