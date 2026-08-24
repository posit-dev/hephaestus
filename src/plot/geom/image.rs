//! `ImageGeom` — vectorised raster images placed at scaled positions.
//!
//! One image per row. The pixels themselves live in the plot's
//! [`ImageRegistry`](crate::plot::ImageRegistry); the `"image"` channel
//! carries the *name* to look up. A discrete scale can therefore map a
//! category column to an image the same way it maps one to a colour, and one
//! registry entry drawn on many rows is one texture as far as a backend is
//! concerned.
//!
//! ## Two sizing modes, one per axis
//!
//! Each axis picks its extent from which channels are supplied, so the two
//! modes mix freely:
//!
//! - **Data-space extent** — `x2` (or `y2`) supplied: the image spans from
//!   `x` to `x2`, scaled like any other position. What a basemap tile or a
//!   precomputed heatmap bitmap wants.
//! - **Absolute extent** — `x2` (or `y2`) absent: `x` anchors the image and
//!   `"width"` (or `"height"`) gives its size in pt, independent of the
//!   projection and of the panel's size. What a logo or a per-category
//!   thumbnail wants.
//!
//! With neither `"width"` nor `"height"` supplied, an absolute extent falls
//! back to the image's own pixel dimensions read as pt — the same
//! `pt * dpi / 72` conversion every other absolute graphical size in this
//! crate uses. With one of the two supplied, the other is derived from the
//! image's pixel aspect ratio, so a width alone scales the image
//! proportionally.
//!
//! Channels consumed:
//!
//! - `"image"` — registry name (required). May be a constant, which covers
//!   "one image behind the whole panel". A name the registry doesn't hold is
//!   read as a location — a file path, or a URL with the `image-url` feature —
//!   so a column of paths works without registering anything. A name that
//!   resolves neither way draws nothing for that row.
//! - `"x"`, `"y"` — the image's anchor (required; data; numeric).
//! - `"x2"`, `"y2"` — the opposite edge, switching that axis to a data-space
//!   extent.
//! - `"x_offset"`, `"y_offset"`, `"x2_offset"`, `"y2_offset"` — absolute pt
//!   offsets added per edge after scale resolution.
//! - `"x_band"`, `"y_band"`, `"x2_band"`, `"y2_band"` — band-fraction offsets
//!   folded into the scale's `map_with_offset` per edge. **All four default to
//!   `0.0`**, unlike [`RectGeom`](crate::plot::RectGeom), whose non-zero `x`
//!   defaults encode bar-chart intent. An image that should fill its band sets
//!   `x_band = -0.5` and `x2_band = 0.5` explicitly.
//! - `"width"`, `"height"` — absolute extent in pt, consulted only on an axis
//!   with no `2` channel.
//! - `"anchor_x"`, `"anchor_y"` — where the image sits relative to `(x, y)`,
//!   as a fraction of the image's own box. `0.5` (the default) centres it;
//!   `0.0` puts its left / top edge on the anchor. Also distributes the slack
//!   under `fit = "contain"` and the overflow under `"cover"`.
//! - `"angle"` — rotation in **radians** around the image's centre,
//!   mathematical CCW (positive rotates counter-clockwise in the rendered
//!   image). Default `0.0`.
//! - `"fit"` — how an image whose aspect ratio differs from its box behaves:
//!   `"stretch"` (default) fills the box exactly and distorts, `"contain"`
//!   scales uniformly to fit inside, `"cover"` scales uniformly to fill and
//!   clips the overflow to the box.
//! - `"sampling"` — `"nearest"` or `"bilinear"` (default). The whole
//!   vocabulary every backend implements natively.
//! - `"opacity"` — multiplied with the image's own alpha. Default `1.0`.
//! - `"pick_id"` — the id this row reports when picked.
//!
//! There is no `"fill"` / `"stroke"` block: a framed image is an `ImageGeom`
//! with a [`RectGeom`](crate::plot::RectGeom) drawn over it, which reuses the
//! whole outline vocabulary instead of duplicating a narrower copy here.
//!
//! ## Non-linear projections (Polar / future Ternary)
//!
//! [`SceneBuilder::draw_image`] takes an affine transform, so an image cannot
//! follow a projected geodesic the way `RectGeom`'s polygon fallback does.
//! Under a non-linear projection a data-space extent projects its corners and
//! takes their axis-aligned bounding box, which is an approximation rather
//! than the correct annular sector. An absolute extent is unaffected: its
//! anchor projects exactly and a pt size does not depend on the projection.
//!
//! ## Picking
//!
//! The hit area is the whole transformed image quad, transparent pixels
//! included — every backend records an image's pick id over its own bounds
//! rect rather than its coverage. And because ids composite with `SrcOver`, a
//! translucent image *fully* occludes picks beneath it on every backend (see
//! [`crate::pick`]), so an `ImageGeom` laid over marks makes them unpickable
//! even while they stay visible through it.

use crate::geometry::{Affine, Point, Rect};
use crate::primitives::rect as rect_path;
use crate::scene::SceneBuilder;

use super::resolve::{
    pt_to_px, resolve_angle_channel, resolve_number_channel, resolve_number_channel_or,
    resolve_pick_id, resolve_position, resolve_str_channel_or,
};
use super::state::{finalize_state, require_x_and_siblings, GeomState, KeysStrategy};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────
//
// These are geometric rather than stylistic, so they live here rather than on
// `theme.geom.*` — see `src/plot/theme/CLAUDE.md`.

/// Default anchor on both axes: the image is centred on `(x, y)`.
const DEFAULT_ANCHOR: f64 = 0.5;
/// Default band offset on every edge. Zero, so an image sits on the band's
/// centre until told otherwise.
const DEFAULT_BAND: f64 = 0.0;
/// Default opacity: fully opaque, so only the image's own alpha applies.
const DEFAULT_OPACITY: f64 = 1.0;
/// Default sampling mode. Bilinear suits photographic content; pixel art
/// asks for `"nearest"`.
const DEFAULT_SAMPLING: &str = "bilinear";
/// Default aspect-fit mode. Filling the box exactly matches what `RectGeom`
/// does with the same corner channels.
const DEFAULT_FIT: &str = "stretch";

const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("image", ExpectedOutput::Strings),
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
    ("width", ExpectedOutput::Numbers),
    ("height", ExpectedOutput::Numbers),
    ("anchor_x", ExpectedOutput::Numbers),
    ("anchor_y", ExpectedOutput::Numbers),
    ("angle", ExpectedOutput::Numbers),
    ("fit", ExpectedOutput::Strings),
    ("sampling", ExpectedOutput::Strings),
    ("opacity", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── Fit and sampling vocabularies ───────────────────────────────────────────

/// How an image whose aspect ratio differs from its target box is scaled
/// into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    /// Fill the box on both axes, distorting the image.
    Stretch,
    /// Scale uniformly until the image fits inside the box.
    Contain,
    /// Scale uniformly until the image fills the box, clipping the overflow.
    Cover,
}

/// The `"fit"` channel's string vocabulary. Anything unrecognised reads as
/// the default, matching how `"cap"` and `"join"` treat an unknown name.
fn fit_from_str(s: &str) -> Fit {
    match s {
        "contain" => Fit::Contain,
        "cover" => Fit::Cover,
        _ => Fit::Stretch,
    }
}

/// The `"sampling"` channel's string vocabulary.
fn sampling_from_str(s: &str) -> crate::brush::Sampling {
    match s {
        "nearest" => crate::brush::Sampling::Nearest,
        _ => crate::brush::Sampling::Bilinear,
    }
}

// ─── ImageGeom ───────────────────────────────────────────────────────────────

/// A vectorised raster-image geom. One image per row.
pub struct ImageGeom {
    pub(crate) state: GeomState,
}

crate::impl_geom_inherents!(ImageGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for ImageGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();
        if !channels.contains_key("image") {
            panic!("ImageGeom::build: missing required channel \"image\"");
        }
        let n = require_x_and_siblings(&channels, &["y"], "ImageGeom");
        let state = finalize_state(
            keys_opt,
            channels,
            n,
            KeysStrategy::PerRow,
            CHANNELS,
            "ImageGeom",
        );
        ImageGeom { state }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for ImageGeom {
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
        Some("image")
    }

    fn draw(&self, scene: &mut dyn SceneBuilder, ctx: &GeomContext<'_>) {
        let panel = ctx.panel_rect;
        if panel.x1 - panel.x0 <= 0.0 || panel.y1 - panel.y0 <= 0.0 {
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
        let image_scale = ctx.scale_for("image");
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x2_offset_scale = ctx.scale_for("x2_offset");
        let y2_offset_scale = ctx.scale_for("y2_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let x2_band_scale = ctx.scale_for("x2_band");
        let y2_band_scale = ctx.scale_for("y2_band");
        let width_scale = ctx.scale_for("width");
        let height_scale = ctx.scale_for("height");
        let anchor_x_scale = ctx.scale_for("anchor_x");
        let anchor_y_scale = ctx.scale_for("anchor_y");
        let angle_scale = ctx.scale_for("angle");
        let fit_scale = ctx.scale_for("fit");
        let sampling_scale = ctx.scale_for("sampling");
        let opacity_scale = ctx.scale_for("opacity");
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
        // A `2` channel present switches that axis to a data-space extent.
        let x2_bind = match channels.get("x2") {
            Some(Channel::Data(c)) => Some((c, x2_scale_bound)),
            Some(Channel::RawData(c)) => Some((c, None)),
            _ => None,
        };
        let y2_bind = match channels.get("y2") {
            Some(Channel::Data(c)) => Some((c, y2_scale_bound)),
            Some(Channel::RawData(c)) => Some((c, None)),
            _ => None,
        };

        let image_ch = channels.get("image");
        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x2_offset_ch = channels.get("x2_offset");
        let y2_offset_ch = channels.get("y2_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let x2_band_ch = channels.get("x2_band");
        let y2_band_ch = channels.get("y2_band");
        let width_ch = channels.get("width");
        let height_ch = channels.get("height");
        let anchor_x_ch = channels.get("anchor_x");
        let anchor_y_ch = channels.get("anchor_y");
        let angle_ch = channels.get("angle");
        let fit_ch = channels.get("fit");
        let sampling_ch = channels.get("sampling");
        let opacity_ch = channels.get("opacity");
        let pick_id_ch = channels.get("pick_id");

        for i in 0..n {
            // ── The image itself. A name that resolves to nothing draws
            // nothing, matching how PointGeom treats an unknown shape name.
            let name = resolve_str_channel_or(image_ch, image_scale, i, "");
            let Some(image) = ctx.images.resolve(&name) else {
                continue;
            };
            let img_w = f64::from(image.width);
            let img_h = f64::from(image.height);
            if img_w <= 0.0 || img_h <= 0.0 {
                continue;
            }

            // ── Anchor position. ──
            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, DEFAULT_BAND);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, DEFAULT_BAND);
            let x_frac = resolve_position(x_col.get(i), x_scale, x_band);
            let y_frac = resolve_position(y_col.get(i), y_scale, y_band);
            if !x_frac.is_finite() || !y_frac.is_finite() {
                continue;
            }

            // Per-edge pixel offsets. Y is subtracted because screen y is
            // down while the user's offset convention is positive-up.
            let x_off_px = resolve_number_channel(x_offset_ch, x_offset_scale, i)
                .map(|o| pt_to_px(o, ctx.dpi))
                .unwrap_or(0.0);
            let y_off_px = resolve_number_channel(y_offset_ch, y_offset_scale, i)
                .map(|o| pt_to_px(o, ctx.dpi))
                .unwrap_or(0.0);
            let x2_off_px = resolve_number_channel(x2_offset_ch, x2_offset_scale, i)
                .map(|o| pt_to_px(o, ctx.dpi))
                .unwrap_or(0.0);
            let y2_off_px = resolve_number_channel(y2_offset_ch, y2_offset_scale, i)
                .map(|o| pt_to_px(o, ctx.dpi))
                .unwrap_or(0.0);

            let (ax, ay) = ctx.projection.project_to_panel_px(panel, &[x_frac, y_frac]);
            let anchor_px = ax + x_off_px;
            let anchor_py = ay - y_off_px;
            if !anchor_px.is_finite() || !anchor_py.is_finite() {
                continue;
            }

            // ── Data-space extents, where the `2` channels supply one. ──
            //
            // Both axes are resolved from the same projected corner so the
            // pair stays consistent under a non-linear projection, where the
            // pixel a fraction lands on depends on both coordinates. What
            // comes back is the corner's bounding box with the anchor, which
            // is the affine approximation this geom is limited to.
            let x2_frac = match x2_bind {
                Some((col, scale)) => {
                    let band =
                        resolve_number_channel_or(x2_band_ch, x2_band_scale, i, DEFAULT_BAND);
                    let f = resolve_position(col.get(i), scale, band);
                    if !f.is_finite() {
                        continue;
                    }
                    Some(f)
                }
                None => None,
            };
            let y2_frac = match y2_bind {
                Some((col, scale)) => {
                    let band =
                        resolve_number_channel_or(y2_band_ch, y2_band_scale, i, DEFAULT_BAND);
                    let f = resolve_position(col.get(i), scale, band);
                    if !f.is_finite() {
                        continue;
                    }
                    Some(f)
                }
                None => None,
            };

            let far = match (x2_frac, y2_frac) {
                (None, None) => None,
                (x2, y2) => {
                    let coords = [x2.unwrap_or(x_frac), y2.unwrap_or(y_frac)];
                    let (fx, fy) = ctx.projection.project_to_panel_px(panel, &coords);
                    let fx = fx + x2_off_px;
                    let fy = fy - y2_off_px;
                    if !fx.is_finite() || !fy.is_finite() {
                        continue;
                    }
                    Some((fx, fy))
                }
            };

            // The far edge in pixels, on whichever axes supplied a `2`
            // channel. Everything downstream reads these rather than `far`,
            // so an axis without its channel never sees a coordinate it has
            // no claim to.
            let far_x = far.filter(|_| x2_frac.is_some()).map(|(fx, _)| fx);
            let far_y = far.filter(|_| y2_frac.is_some()).map(|(_, fy)| fy);
            let data_w = far_x.map(|fx| (fx - anchor_px).abs());
            let data_h = far_y.map(|fy| (fy - anchor_py).abs());

            // ── Absolute extents, where an axis has no `2` channel. ──
            let width_px =
                resolve_number_channel(width_ch, width_scale, i).map(|w| pt_to_px(w, ctx.dpi));
            let height_px =
                resolve_number_channel(height_ch, height_scale, i).map(|h| pt_to_px(h, ctx.dpi));
            let aspect = img_w / img_h;

            // An axis with no `2` channel and no explicit size derives from
            // the other axis where that one resolved, and from the image's
            // own pixel dimensions read as pt where neither did.
            let box_w = match data_w.or(width_px) {
                Some(w) => w,
                None => match data_h.or(height_px) {
                    Some(h) => h * aspect,
                    None => pt_to_px(img_w, ctx.dpi),
                },
            };
            let box_h = match data_h.or(height_px) {
                Some(h) => h,
                None => match data_w.or(width_px) {
                    Some(w) => w / aspect,
                    None => pt_to_px(img_h, ctx.dpi),
                },
            };
            if !box_w.is_finite() || !box_h.is_finite() || box_w <= 0.0 || box_h <= 0.0 {
                continue;
            }

            let anchor_x =
                resolve_number_channel_or(anchor_x_ch, anchor_x_scale, i, DEFAULT_ANCHOR);
            let anchor_y =
                resolve_number_channel_or(anchor_y_ch, anchor_y_scale, i, DEFAULT_ANCHOR);

            // A data-space extent spans anchor→far edge; an absolute one is
            // placed around the anchor by the anchor fractions.
            let (bx0, bx1) = match far_x {
                Some(fx) => (anchor_px.min(fx), anchor_px.max(fx)),
                None => {
                    let left = anchor_px - anchor_x * box_w;
                    (left, left + box_w)
                }
            };
            let (by0, by1) = match far_y {
                Some(fy) => (anchor_py.min(fy), anchor_py.max(fy)),
                None => {
                    let top = anchor_py - anchor_y * box_h;
                    (top, top + box_h)
                }
            };
            let target = Rect::new(bx0, by0, bx1, by1);
            if !target.is_finite() || target.width() <= 0.0 || target.height() <= 0.0 {
                continue;
            }

            // ── Fit the image's pixel space into the target box. ──
            let fit = fit_from_str(&resolve_str_channel_or(fit_ch, fit_scale, i, DEFAULT_FIT));
            let fill_x = target.width() / img_w;
            let fill_y = target.height() / img_h;
            let (sx, sy) = match fit {
                Fit::Stretch => (fill_x, fill_y),
                Fit::Contain => {
                    let s = fill_x.min(fill_y);
                    (s, s)
                }
                Fit::Cover => {
                    let s = fill_x.max(fill_y);
                    (s, s)
                }
            };
            // Uniform scaling leaves slack (Contain) or overflow (Cover) in
            // the box; the anchor fractions distribute it, so the same knob
            // that placed the box also places the image inside it.
            let drawn_w = img_w * sx;
            let drawn_h = img_h * sy;
            let tl_x = target.x0 + anchor_x * (target.width() - drawn_w);
            let tl_y = target.y0 + anchor_y * (target.height() - drawn_h);

            // ── Rotation around the target box's centre. Math CCW from the
            // user → negate for kurbo (screen y-down). Identity when
            // angle == 0 keeps the recording byte-identical for unrotated
            // images.
            let angle = resolve_angle_channel(angle_ch, angle_scale, i);
            let centre = Point::new(0.5 * (target.x0 + target.x1), 0.5 * (target.y0 + target.y1));
            let rotation = if angle == 0.0 {
                Affine::IDENTITY
            } else {
                Affine::rotate_about(-angle, centre)
            };

            let sampling = sampling_from_str(&resolve_str_channel_or(
                sampling_ch,
                sampling_scale,
                i,
                DEFAULT_SAMPLING,
            ));
            let opacity = resolve_number_channel_or(opacity_ch, opacity_scale, i, DEFAULT_OPACITY)
                .clamp(0.0, 1.0) as f32;
            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i);

            // `draw_image`'s transform maps the image's own pixel space
            // (0, 0)–(width, height) onto the output, so the placement is a
            // scale into the box followed by the translation to its corner.
            let xform =
                rotation * Affine::translate((tl_x, tl_y)) * Affine::scale_non_uniform(sx, sy);

            // Only `Cover` overflows its box, so only it pays for a clip.
            let clipped =
                fit == Fit::Cover && (drawn_w > target.width() || drawn_h > target.height());
            if clipped {
                scene.push_layer(
                    crate::blend::BlendMode::NORMAL,
                    1.0,
                    rotation,
                    &rect_path(target),
                );
            }
            scene.draw_image(&image, xform, sampling, opacity, pick);
            if clipped {
                scene.pop_layer();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::brush::{Blob, Image, ImageAlphaType, ImageFormat, Sampling};
    use crate::geometry::Rect as GeomRect;
    use crate::pick::PickId;
    use crate::plot::geom::{DirectScaleResolver, Raw};
    use crate::plot::image_registry::ImageRegistry;
    use crate::plot::scale;
    use crate::scene::recording::{Op, RecordingScene};
    use std::sync::Arc;

    /// A solid image of the given pixel dimensions. Only the size matters to
    /// the geom — every placement decision reads `width` / `height`.
    fn image(width: u32, height: u32) -> Image {
        let px = vec![255u8; (width as usize) * (height as usize) * 4];
        Image {
            data: Blob::new(Arc::new(px)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        }
    }

    /// A registry holding a square `"sq"` (10x10) and a wide `"wide"`
    /// (20x10, aspect 2).
    fn registry() -> ImageRegistry {
        let mut r = ImageRegistry::new();
        r.insert("sq", image(10, 10));
        r.insert("wide", image(20, 10));
        r
    }

    fn shapes() -> crate::shape::ShapeRegistry {
        crate::shape::ShapeRegistry::with_builtins()
    }

    fn ctx<'a>(
        panel: GeomRect,
        shapes: &'a crate::shape::ShapeRegistry,
        images: &'a ImageRegistry,
        scales: &'a DirectScaleResolver<'a>,
    ) -> GeomContext<'a> {
        GeomContext::new(panel, 96.0, shapes, scales).with_images(images)
    }

    /// The one `Op::DrawImage` a single-row geom emitted.
    fn only_image(scene: &RecordingScene) -> (Affine, Sampling, f32, PickId) {
        let mut found = None;
        for op in &scene.ops {
            if let Op::DrawImage {
                transform,
                sampling,
                alpha,
                pick_id,
                ..
            } = op
            {
                assert!(found.is_none(), "expected exactly one image draw");
                found = Some((*transform, *sampling, *alpha, *pick_id));
            }
        }
        found.expect("expected an Op::DrawImage")
    }

    /// The axis-aligned pixel box an image transform maps its own pixel
    /// bounds onto. Only meaningful for an unrotated draw.
    fn drawn_box(transform: Affine, img_w: f64, img_h: f64) -> GeomRect {
        let tl = transform * Point::new(0.0, 0.0);
        let br = transform * Point::new(img_w, img_h);
        GeomRect::new(
            tl.x.min(br.x),
            tl.y.min(br.y),
            tl.x.max(br.x),
            tl.y.max(br.y),
        )
    }

    fn assert_close(got: f64, want: f64, what: &str) {
        assert!(
            (got - want).abs() < 1e-6,
            "{what}: got {got}, expected {want}"
        );
    }

    // ── build() validation ──

    #[test]
    #[should_panic(expected = "missing required channel \"image\"")]
    fn builder_requires_the_image_channel() {
        ImageGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .build();
    }

    #[test]
    fn builder_requires_x_and_y() {
        let r = std::panic::catch_unwind(|| {
            ImageGeom::builder()
                .set("image", vec!["sq"])
                .set("y", vec![0.0_f64])
                .build()
        });
        assert!(r.is_err(), "a geom with no \"x\" should not build");
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_y_length_mismatch_panics() {
        ImageGeom::builder()
            .set("image", vec!["sq", "sq", "sq"])
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_x2_length_mismatch_panics() {
        ImageGeom::builder()
            .set("image", vec!["sq", "sq"])
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("x2", vec![1.0_f64])
            .build();
    }

    #[test]
    fn builder_rejects_an_undeclared_channel_name() {
        let r = std::panic::catch_unwind(|| {
            ImageGeom::builder()
                .set("image", vec!["sq"])
                .set("x", vec![0.0_f64])
                .set("y", vec![0.0_f64])
                .set("smapling", vec!["nearest"])
                .build()
        });
        assert!(r.is_err(), "a misspelled channel should not build");
    }

    #[test]
    fn the_image_channel_may_be_a_constant() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn kind_is_image() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .build();
        assert_eq!(g.kind(), Some("image"));
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .set("width", 10.0_f64)
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    // ── registry lookup ──

    #[test]
    fn an_unregistered_name_draws_nothing() {
        let g = ImageGeom::builder()
            .set("image", "nope")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert!(!scene
            .ops
            .iter()
            .any(|op| matches!(op, Op::DrawImage { .. })));
    }

    #[test]
    fn an_empty_registry_draws_nothing() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .build();
        let (shapes, images) = (shapes(), ImageRegistry::new());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert!(scene.ops.is_empty());
    }

    #[test]
    fn only_the_registered_rows_draw() {
        let g = ImageGeom::builder()
            .set("image", vec!["sq", "nope", "wide"])
            .set("x", Raw(vec![0.2_f64, 0.5, 0.8]))
            .set("y", Raw(vec![0.5_f64, 0.5, 0.5]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let drawn = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawImage { .. }))
            .count();
        assert_eq!(drawn, 2);
    }

    // ── absolute (anchored) extent ──

    /// With no size channels the image is its own pixel dimensions read as
    /// pt, so at 96 dpi a 10x10 image is 10 * 96 / 72 px across.
    #[test]
    fn an_unsized_image_takes_its_pixel_dimensions_as_pt() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        let expected = 10.0 * 96.0 / 72.0;
        assert_close(b.width(), expected, "width");
        assert_close(b.height(), expected, "height");
        // Centred on the anchor by default.
        assert_close(0.5 * (b.x0 + b.x1), 50.0, "centre x");
        assert_close(0.5 * (b.y0 + b.y1), 50.0, "centre y");
    }

    #[test]
    fn width_and_height_in_pt_scale_with_dpi() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("width", 36.0_f64)
            .set("height", 18.0_f64)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        let mut c = ctx(
            GeomRect::new(0.0, 0.0, 100.0, 100.0),
            &shapes,
            &images,
            &resolver,
        );
        c.dpi = 144.0;
        g.draw(&mut scene, &c);
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        assert_close(b.width(), 36.0 * 2.0, "width at 144 dpi");
        assert_close(b.height(), 18.0 * 2.0, "height at 144 dpi");
    }

    /// A width alone scales the image proportionally: the wide image's
    /// aspect is 2, so a 40 px box is 20 px tall.
    #[test]
    fn a_width_alone_derives_the_height_from_the_aspect() {
        let g = ImageGeom::builder()
            .set("image", "wide")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("width", 30.0_f64)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 20.0, 10.0);
        let w = 30.0 * 96.0 / 72.0;
        assert_close(b.width(), w, "width");
        assert_close(b.height(), w / 2.0, "height derived from aspect 2");
    }

    #[test]
    fn a_height_alone_derives_the_width_from_the_aspect() {
        let g = ImageGeom::builder()
            .set("image", "wide")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("height", 18.0_f64)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 20.0, 10.0);
        let h = 18.0 * 96.0 / 72.0;
        assert_close(b.height(), h, "height");
        assert_close(b.width(), h * 2.0, "width derived from aspect 2");
    }

    #[test]
    fn the_anchor_places_the_box_relative_to_the_point() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("width", 36.0_f64)
            .set("height", 36.0_f64)
            .set("anchor_x", 0.0_f64)
            .set("anchor_y", 0.0_f64)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        // anchor 0 puts the box's top-left corner on the point.
        assert_close(b.x0, 50.0, "left edge on the anchor");
        assert_close(b.y0, 50.0, "top edge on the anchor");
    }

    #[test]
    fn pt_offsets_move_the_anchor_with_y_positive_up() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("width", 36.0_f64)
            .set("height", 36.0_f64)
            .set("x_offset", 9.0_f64)
            .set("y_offset", 9.0_f64)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        let off = 9.0 * 96.0 / 72.0;
        assert_close(0.5 * (b.x0 + b.x1), 50.0 + off, "x offset moves right");
        assert_close(0.5 * (b.y0 + b.y1), 50.0 - off, "y offset moves up");
    }

    // ── data-space extent ──

    #[test]
    fn x2_and_y2_span_the_image_across_a_data_rect() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.2_f64]))
            .set("y", Raw(vec![0.2_f64]))
            .set("x2", Raw(vec![0.8_f64]))
            .set("y2", Raw(vec![0.6_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        // Panel fractions run bottom-up, so y = 0.2 is pixel 80.
        assert_close(b.x0, 20.0, "left");
        assert_close(b.x1, 80.0, "right");
        assert_close(b.y0, 40.0, "top");
        assert_close(b.y1, 80.0, "bottom");
    }

    /// Corner ordering is irrelevant — the box is normalised, so swapping
    /// `x` with `x2` draws the same rectangle.
    #[test]
    fn reversed_corners_draw_the_same_box() {
        let build = |x: f64, x2: f64| {
            ImageGeom::builder()
                .set("image", "sq")
                .set("x", Raw(vec![x]))
                .set("y", Raw(vec![0.2_f64]))
                .set("x2", Raw(vec![x2]))
                .set("y2", Raw(vec![0.6_f64]))
                .build()
        };
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let panel = GeomRect::new(0.0, 0.0, 100.0, 100.0);

        let mut a = RecordingScene::new();
        build(0.2, 0.8).draw(&mut a, &ctx(panel, &shapes, &images, &resolver));
        let mut b = RecordingScene::new();
        build(0.8, 0.2).draw(&mut b, &ctx(panel, &shapes, &images, &resolver));

        let box_a = drawn_box(only_image(&a).0, 10.0, 10.0);
        let box_b = drawn_box(only_image(&b).0, 10.0, 10.0);
        assert_close(box_a.x0, box_b.x0, "left");
        assert_close(box_a.x1, box_b.x1, "right");
    }

    /// One axis in data space and the other in pt: the pt axis derives from
    /// the data axis through the image's aspect when no size is supplied.
    #[test]
    fn a_data_x_extent_drives_the_pt_y_extent_through_the_aspect() {
        let g = ImageGeom::builder()
            .set("image", "wide")
            .set("x", Raw(vec![0.2_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("x2", Raw(vec![0.6_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 20.0, 10.0);
        assert_close(b.width(), 40.0, "data-space width");
        assert_close(b.height(), 20.0, "height from aspect 2");
    }

    /// Band offsets default to zero on every edge, unlike `RectGeom`.
    /// Binding both `x` and `x2` to the same band therefore collapses the
    /// box rather than filling the band, and the row drops out.
    #[test]
    fn band_offsets_default_to_zero_on_every_edge() {
        let cats = scale::ordinal(["a", "b"]);
        let ys = scale::continuous(0.0..=1.0);
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", vec!["a"])
            .set("y", vec![0.5_f64])
            .set("x2", vec!["a"])
            .set("y2", vec![0.9_f64])
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new().with("x", &cats).with("y", &ys);
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert!(
            !scene
                .ops
                .iter()
                .any(|op| matches!(op, Op::DrawImage { .. })),
            "a zero-width box should draw nothing"
        );
    }

    /// Setting the band offsets explicitly is how an image fills its band.
    #[test]
    fn explicit_band_offsets_fill_the_band() {
        let cats = scale::ordinal(["a", "b"]);
        let ys = scale::continuous(0.0..=1.0);
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", vec!["a"])
            .set("y", vec![0.5_f64])
            .set("x2", vec!["a"])
            .set("y2", vec![0.9_f64])
            .set("x_band", -0.5_f64)
            .set("x2_band", 0.5_f64)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new().with("x", &cats).with("y", &ys);
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        // Two categories over 100 px: band "a" is the left half.
        assert_close(b.x0, 0.0, "band left edge");
        assert_close(b.x1, 50.0, "band right edge");
    }

    #[test]
    fn a_non_finite_position_drops_the_row() {
        let g = ImageGeom::builder()
            .set("image", vec!["sq", "sq"])
            .set("x", Raw(vec![f64::NAN, 0.5]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let drawn = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawImage { .. }))
            .count();
        assert_eq!(drawn, 1);
    }

    // ── fit ──

    /// The default fills the box on both axes, so a square image in a
    /// 2:1 box is scaled unevenly.
    #[test]
    fn stretch_fills_the_box_on_both_axes() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.0_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        assert_close(b.width(), 100.0, "stretched width");
        assert_close(b.height(), 50.0, "stretched height");
    }

    #[test]
    fn contain_scales_uniformly_and_letterboxes() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.0_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("fit", "contain")
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        // The box is 100x50, so a square image fits at 50x50, centred.
        assert_close(b.width(), 50.0, "contained width");
        assert_close(b.height(), 50.0, "contained height");
        assert_close(b.x0, 25.0, "centred horizontally in the box");
        assert_close(b.y0, 50.0, "flush with the box top");
        assert!(
            !scene
                .ops
                .iter()
                .any(|op| matches!(op, Op::PushLayer { .. })),
            "contain never overflows, so it must not clip"
        );
    }

    #[test]
    fn cover_scales_uniformly_and_clips_the_overflow() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.0_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("fit", "cover")
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let (xform, ..) = only_image(&scene);
        let b = drawn_box(xform, 10.0, 10.0);
        // The box is 100x50, so covering it needs 100x100.
        assert_close(b.width(), 100.0, "covering width");
        assert_close(b.height(), 100.0, "covering height");
        assert!(
            scene
                .ops
                .iter()
                .any(|op| matches!(op, Op::PushLayer { .. })),
            "cover overflows the box and must clip"
        );
        assert!(scene.ops.iter().any(|op| matches!(op, Op::PopLayer)));
    }

    /// The anchor distributes the slack `contain` leaves in the box, so it
    /// is also how a letterboxed image is pushed to one side.
    #[test]
    fn the_anchor_places_a_contained_image_inside_its_box() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.0_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("fit", "contain")
            .set("anchor_x", 1.0_f64)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let b = drawn_box(only_image(&scene).0, 10.0, 10.0);
        assert_close(b.x1, 100.0, "flush with the box's right edge");
    }

    /// A `cover` that happens to need no overflow — the box already matches
    /// the image's aspect — draws without paying for a clip layer.
    #[test]
    fn cover_on_a_matching_aspect_does_not_clip() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.0_f64]))
            .set("x2", Raw(vec![0.5_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("fit", "cover")
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert!(!scene
            .ops
            .iter()
            .any(|op| matches!(op, Op::PushLayer { .. })));
    }

    #[test]
    fn an_unknown_fit_name_falls_back_to_stretch() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.0_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("fit", "squish")
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let b = drawn_box(only_image(&scene).0, 10.0, 10.0);
        assert_close(b.width(), 100.0, "stretched width");
        assert_close(b.height(), 50.0, "stretched height");
    }

    // ── rotation ──

    /// An unrotated draw carries no rotation in its transform, which keeps
    /// the recording identical to what it was before `angle` existed.
    #[test]
    fn a_zero_angle_leaves_the_transform_axis_aligned() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("width", 36.0_f64)
            .set("height", 36.0_f64)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let coeffs = only_image(&scene).0.as_coeffs();
        assert_close(coeffs[1], 0.0, "no shear/rotation in b");
        assert_close(coeffs[2], 0.0, "no shear/rotation in c");
    }

    /// A quarter turn is counter-clockwise on screen and pivots on the
    /// box's centre, so the centre is where it was.
    #[test]
    fn a_quarter_turn_pivots_on_the_box_centre() {
        let g = ImageGeom::builder()
            .set("image", "wide")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("width", 60.0_f64)
            .set("angle", std::f64::consts::FRAC_PI_2)
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let xform = only_image(&scene).0;
        let mid = xform * Point::new(10.0, 5.0);
        assert_close(mid.x, 50.0, "centre stays on the anchor");
        assert_close(mid.y, 50.0, "centre stays on the anchor");
        // A CCW quarter turn sends the image's +x axis to screen -y.
        let along_x = xform * Point::new(20.0, 5.0);
        assert!(
            along_x.y < mid.y - 1.0,
            "the image's +x should run up the screen, got {along_x:?}"
        );
    }

    // ── sampling, opacity, picking ──

    #[test]
    fn sampling_defaults_to_bilinear() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert_eq!(only_image(&scene).1, Sampling::Bilinear);
    }

    #[test]
    fn the_sampling_channel_selects_nearest() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("sampling", "nearest")
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert_eq!(only_image(&scene).1, Sampling::Nearest);
    }

    #[test]
    fn opacity_rides_on_the_draw_and_clamps() {
        for (set, want) in [(0.4_f64, 0.4_f32), (2.0, 1.0), (-1.0, 0.0)] {
            let g = ImageGeom::builder()
                .set("image", "sq")
                .set("x", Raw(vec![0.5_f64]))
                .set("y", Raw(vec![0.5_f64]))
                .set("opacity", set)
                .build();
            let (shapes, images) = (shapes(), registry());
            let resolver = DirectScaleResolver::new();
            let mut scene = RecordingScene::new();
            g.draw(
                &mut scene,
                &ctx(
                    GeomRect::new(0.0, 0.0, 100.0, 100.0),
                    &shapes,
                    &images,
                    &resolver,
                ),
            );
            assert!((only_image(&scene).2 - want).abs() < 1e-6, "opacity {set}");
        }
    }

    #[test]
    fn no_pick_id_channel_means_skip() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert_eq!(only_image(&scene).3, PickId::Skip);
    }

    #[test]
    fn the_pick_id_channel_reaches_the_draw() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("pick_id", Raw(vec![7.0_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert_eq!(only_image(&scene).3, PickId::Id(7));
    }

    // ── scale integration ──

    /// The point of naming images rather than carrying them: a discrete
    /// scale maps a category column onto registry names.
    #[test]
    fn a_discrete_scale_maps_categories_to_image_names() {
        let names = scale::ordinal(["p", "q"]).range_strings([Arc::from("sq"), Arc::from("wide")]);
        let g = ImageGeom::builder()
            .set("image", vec!["p", "q"])
            .set("x", Raw(vec![0.3_f64, 0.7]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new().with("image", &names);
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        let widths: Vec<u32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawImage { image, .. } => Some(image.width),
                _ => None,
            })
            .collect();
        assert_eq!(widths, vec![10, 20], "each category resolved to its image");
    }

    #[test]
    fn an_empty_geom_draws_nothing() {
        let g = ImageGeom::builder()
            .set("image", Vec::<&str>::new())
            .set("x", Vec::<f64>::new())
            .set("y", Vec::<f64>::new())
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 100.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert!(scene.ops.is_empty());
    }

    #[test]
    fn a_degenerate_panel_draws_nothing() {
        let g = ImageGeom::builder()
            .set("image", "sq")
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .build();
        let (shapes, images) = (shapes(), registry());
        let resolver = DirectScaleResolver::new();
        let mut scene = RecordingScene::new();
        g.draw(
            &mut scene,
            &ctx(
                GeomRect::new(0.0, 0.0, 0.0, 100.0),
                &shapes,
                &images,
                &resolver,
            ),
        );
        assert!(scene.ops.is_empty());
    }
}
