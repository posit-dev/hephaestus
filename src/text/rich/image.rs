//! Image tags: resolving the location one names to pixels, sizing the
//! box a layout reserves for it, and painting the placeholder for one
//! that gave nothing.
//!
//! Sizing follows marquee. An **inline** image stands one em tall — the
//! em of the `img` element, so a `{.24 ![](logo.png)}` span makes it
//! 24pt — and takes its width from the pixel aspect ratio. A **block**
//! image, meaning a tag that was the whole content of its paragraph,
//! fills the block's width instead and takes its height from the
//! aspect ratio. A block image in a run shaped at natural width has no
//! container to fill, so it falls back to its own pixel dimensions read
//! as pt, the same reading [`ImageGeom`](crate::plot::ImageGeom) gives
//! an absolute extent.
//!
//! A location that gives no pixels is drawn as marquee's placeholder: a
//! framed box with a diagonal cross, one em square. A block image with
//! nothing behind it stays that square rather than filling the column —
//! there is no aspect ratio to give a stretched box a height, and a
//! missing picture has no claim on the space a real one would take.

use crate::brush::{Brush, Image};
use crate::color::Color;
use crate::geometry::{Affine, Point, Rect};
use crate::image_registry::ImageRegistry;
use crate::path::Path;
use crate::pick::PickId;
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;
use crate::style_vocab::Palette;
use crate::text::rich::reduce::InlineObject;

/// Base of the inline-box id space image objects use. Span padding
/// numbers its boxes from zero upward, so images take the high bit and
/// the draw pass tells the two apart by it.
pub(crate) const OBJECT_ID_BASE: u64 = 1 << 63;

/// Points per inch, for the pt ↔ px conversions here.
const PT_PER_INCH: f64 = 72.0;

/// One image tag resolved against a register: the pixels it found and
/// the box the layout reserves.
#[derive(Debug, Clone)]
pub(crate) struct ObjectLayout {
    /// Byte offset into the block's own text where the box sits.
    pub(crate) index: usize,
    /// The pixels, or `None` for a location that gave none — which is
    /// what makes this a placeholder.
    pub(crate) image: Option<Image>,
    /// Box width in pixels.
    pub(crate) width_px: f32,
    /// Box height in pixels.
    pub(crate) height_px: f32,
    /// How far below the box position the picture actually draws.
    /// marquee centres an inline image's em box on the font's ink band
    /// rather than sitting it on the baseline; this is that offset.
    pub(crate) dy_px: f32,
    /// Whether the tag filled its own paragraph. A block image's width
    /// follows its container, so it is re-sized whenever the run
    /// re-breaks.
    pub(crate) block: bool,
    /// Em of the element, in pixels — what an inline box measures one
    /// of, kept for the re-size a block image needs.
    pub(crate) em_px: f32,
    /// The image's own pixel width read as pt and back to pixels, which
    /// is the size a block image takes when it has no column to fill.
    /// Zero when there is no image.
    pub(crate) natural_width_px: f32,
    /// What the placeholder is painted with.
    pub(crate) placeholder: Placeholder,
}

/// Frame and cross of the placeholder an unreadable location draws,
/// resolved from the sheet's `broken_image` selector.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Placeholder {
    /// Frame colour.
    pub(crate) frame: Color,
    /// Frame stroke width in pixels.
    pub(crate) frame_width_px: f32,
    /// Cross colour.
    pub(crate) cross: Color,
    /// Cross stroke width in pixels.
    pub(crate) cross_width_px: f32,
}

impl ObjectLayout {
    /// The image's own pixel aspect ratio, or `None` when there is no
    /// image to read one from — which is what makes a placeholder a
    /// square.
    fn aspect(&self) -> Option<f32> {
        let image = self.image.as_ref()?;
        if image.width == 0 || image.height == 0 {
            return None;
        }
        Some(image.width as f32 / image.height as f32)
    }

    /// Re-size against the width now available to the block. Only a
    /// block image moves: an inline one is one em tall whatever the
    /// column does.
    pub(crate) fn resize_to_block(&mut self, avail_px: f32) {
        if !self.block {
            return;
        }
        let (w, h) = block_box(self, Some(avail_px));
        self.width_px = w;
        self.height_px = h;
    }
}

/// Box of a block image: the container's width, height from the aspect
/// ratio. With no container — a run shaped at natural width — the
/// image's own pixels read as pt instead.
fn block_box(object: &ObjectLayout, avail_px: Option<f32>) -> (f32, f32) {
    // No pixels means no ratio, so there is no height to stretch a
    // column-wide box to: the placeholder stays the square an inline one
    // would be.
    let Some(asp) = object.aspect().map(|a| a.max(f32::EPSILON)) else {
        return (object.em_px.max(1.0), object.em_px.max(1.0));
    };
    let width = avail_px.unwrap_or(object.natural_width_px);
    (width.max(1.0), (width / asp).max(1.0))
}

/// Resolve every tag in `objects` against `images` and size its box.
///
/// `avail_px` is the width a block image fills, or `None` for a run
/// shaped at natural width. `dpi` converts the element's pt sizes.
pub(crate) fn resolve_objects(
    objects: &[InlineObject],
    images: &ImageRegistry,
    palette: &Palette,
    dpi: f64,
    avail_px: Option<f32>,
) -> Vec<ObjectLayout> {
    objects
        .iter()
        .map(|object| {
            let em_px = (object.style.size_pt * dpi / PT_PER_INCH) as f32;
            let image = images.resolve(&object.dest);
            let natural_width_px = image
                .as_ref()
                .map(|i| (f64::from(i.width) * dpi / PT_PER_INCH) as f32)
                .unwrap_or(0.0);
            let mut out = ObjectLayout {
                index: object.index,
                image,
                width_px: 0.0,
                height_px: 0.0,
                dy_px: 0.0,
                block: object.block,
                em_px: em_px.max(1.0),
                natural_width_px,
                placeholder: placeholder_of(&object.placeholder_style, palette, dpi),
            };
            let (w, h) = if out.block {
                block_box(&out, avail_px)
            } else {
                // Inline: one em tall, width from the aspect ratio.
                // Nothing to take a ratio from makes it a square.
                let height = out.em_px;
                (height * out.aspect().unwrap_or(1.0), height)
            };
            out.width_px = w;
            out.height_px = h;
            out
        })
        .collect()
}

/// Read the placeholder's paint off a resolved `broken_image` style.
fn placeholder_of(
    style: &crate::text::rich::style::ResolvedStyle,
    palette: &Palette,
    dpi: f64,
) -> Placeholder {
    let scale = dpi / PT_PER_INCH;
    // The frame reads the widest side it was given: the box is one
    // rect, so four different widths have nothing to describe.
    let frame_width_pt = style
        .border_width_pt
        .iter()
        .fold(0.0f64, |acc, w| acc.max(*w));
    Placeholder {
        frame: style
            .border_color
            .as_ref()
            .map(|c| c.resolve(palette))
            .unwrap_or(Color::TRANSPARENT),
        frame_width_px: (frame_width_pt * scale) as f32,
        cross: style
            .color
            .as_ref()
            .map(|c| c.resolve(palette))
            .unwrap_or(Color::TRANSPARENT),
        cross_width_px: (style.text_stroke_width_pt * scale) as f32,
    }
}

/// The resolved objects positioned inside `range`, rebased to it —
/// what a block re-shaped at a new width hands to the continuation
/// half of its split.
pub(crate) fn slice_object_layouts(
    objects: &[ObjectLayout],
    range: &std::ops::Range<usize>,
) -> Vec<ObjectLayout> {
    objects
        .iter()
        .filter(|o| range.contains(&o.index))
        .map(|o| ObjectLayout {
            index: o.index - range.start,
            ..o.clone()
        })
        .collect()
}

/// The object a given inline-box id belongs to, or `None` when the box
/// is one of the span-padding spacers sharing the layout.
pub(crate) fn object_for_box(objects: &[ObjectLayout], id: u64) -> Option<&ObjectLayout> {
    let index = id.checked_sub(OBJECT_ID_BASE)?;
    objects.get(index as usize)
}

/// Push one in-flow box per object, so the shaper reserves the space
/// and grows the line to fit.
pub(crate) fn push_object_boxes(
    builder: &mut parley::RangedBuilder<'_, super::run::RichBrush>,
    objects: &[ObjectLayout],
    text_len: usize,
) {
    for (i, object) in objects.iter().enumerate() {
        if object.index > text_len {
            continue;
        }
        builder.push_inline_box(parley::InlineBox {
            id: OBJECT_ID_BASE + i as u64,
            kind: parley::InlineBoxKind::InFlow,
            index: object.index,
            width: object.width_px.max(0.0),
            height: object.height_px.max(0.0),
        });
    }
}

/// Fill in each object's vertical offset from the shaped layout.
///
/// marquee moves an inline image down by the font's descent and back up
/// by half the difference between the font's full height and its point
/// size, which centres the em box on the ink the text around it makes.
/// The metrics come from the run holding the object's own placeholder
/// character, so they are the element's font at the element's size.
pub(crate) fn fill_object_offsets(
    layout: &parley::Layout<super::run::RichBrush>,
    objects: &mut [ObjectLayout],
) {
    for object in objects.iter_mut() {
        if object.block {
            // A block image is the line: there is no surrounding text
            // to centre against.
            object.dy_px = 0.0;
            continue;
        }
        let Some((ascent, descent, size)) = run_metrics_at(layout, object.index) else {
            continue;
        };
        if size <= 0.0 {
            continue;
        }
        let descent_ratio = descent / size;
        let height_ratio = (ascent + descent) / size;
        object.dy_px = descent_ratio * object.height_px - (height_ratio - 1.0) * size / 2.0;
    }
}

/// Ascent, descent and font size of the run covering `index`.
fn run_metrics_at(
    layout: &parley::Layout<super::run::RichBrush>,
    index: usize,
) -> Option<(f32, f32, f32)> {
    for line in layout.lines() {
        for item in line.items() {
            let parley::PositionedLayoutItem::GlyphRun(run) = item else {
                continue;
            };
            let range = run.run().text_range();
            if !range.contains(&index) && range.end != index {
                continue;
            }
            let metrics = run.run().metrics();
            return Some((metrics.ascent, metrics.descent, run.run().font_size()));
        }
    }
    None
}

/// Emit one object: the picture if it resolved, the placeholder if it
/// did not.
///
/// `rect` is the box in the same frame the caller draws glyphs in, and
/// `transform` is what maps that frame to the scene.
pub(crate) fn emit_object(
    scene: &mut dyn SceneBuilder,
    object: &ObjectLayout,
    rect: Rect,
    transform: Affine,
    pick_id: PickId,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    match &object.image {
        Some(image) if image.width > 0 && image.height > 0 => {
            let scale = Affine::scale_non_uniform(
                rect.width() / f64::from(image.width),
                rect.height() / f64::from(image.height),
            );
            scene.draw_image(
                image,
                transform * Affine::translate((rect.x0, rect.y0)) * scale,
                crate::brush::Sampling::Bilinear,
                1.0,
                pick_id,
            );
        }
        _ => emit_placeholder(scene, &object.placeholder, rect, transform, pick_id),
    }
}

/// Paint the framed cross that stands in for an image the register and
/// the location both failed to supply.
///
/// Both strokes are inset by half their width, which keeps the whole
/// mark inside the box the layout reserved without a clip layer.
fn emit_placeholder(
    scene: &mut dyn SceneBuilder,
    placeholder: &Placeholder,
    rect: Rect,
    transform: Affine,
    pick_id: PickId,
) {
    let cross_inset = f64::from(placeholder.cross_width_px) / 2.0;
    if placeholder.cross_width_px > 0.0 && placeholder.cross.components[3] > 0.0 {
        let inner = inset(rect, cross_inset);
        let mut cross = Path::new();
        cross.move_to(Point::new(inner.x0, inner.y0));
        cross.line_to(Point::new(inner.x1, inner.y1));
        cross.move_to(Point::new(inner.x0, inner.y1));
        cross.line_to(Point::new(inner.x1, inner.y0));
        scene.stroke(
            &Stroke::new(f64::from(placeholder.cross_width_px)),
            transform,
            &Brush::Solid(placeholder.cross),
            None,
            &cross,
            pick_id,
        );
    }
    if placeholder.frame_width_px > 0.0 && placeholder.frame.components[3] > 0.0 {
        let inner = inset(rect, f64::from(placeholder.frame_width_px) / 2.0);
        let mut frame = Path::new();
        frame.move_to(Point::new(inner.x0, inner.y0));
        frame.line_to(Point::new(inner.x1, inner.y0));
        frame.line_to(Point::new(inner.x1, inner.y1));
        frame.line_to(Point::new(inner.x0, inner.y1));
        frame.close_path();
        scene.stroke(
            &Stroke::new(f64::from(placeholder.frame_width_px)),
            transform,
            &Brush::Solid(placeholder.frame),
            None,
            &frame,
            pick_id,
        );
    }
}

/// Shrink `rect` by `by` on every side, never past its own centre.
fn inset(rect: Rect, by: f64) -> Rect {
    let by = by.min(rect.width() / 2.0).min(rect.height() / 2.0);
    Rect::new(
        rect.x0 + by,
        rect.y0 + by,
        (rect.x1 - by).max(rect.x0 + by),
        (rect.y1 - by).max(rect.y0 + by),
    )
}
