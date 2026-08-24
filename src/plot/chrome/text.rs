//! Text rendering shared by every chrome slot.
//!
//! Axes, legends, strips and the plot-level title band all need the
//! same three things: turn a themed [`TextElement`] into a shaped
//! style, reserve a layout cell whose measure includes the element's
//! margin, and draw the run inside a rect honoring alignment,
//! rotation, wrapping and the optional outline pass.
//!
//! Both the plain and markdown paths live here, so a slot opts into
//! rich text by setting `markdown` on its element rather than by
//! calling a different function.
//!
//! [`TextElement`]: crate::plot::theme::TextElement

use crate::geometry::Rect;
use crate::layout::Cell;
use crate::scales::chrome::AxisSide;
use crate::scene::SceneBuilder;

/// Build a chrome text cell whose measure includes both the shaped
/// run **and** the element's margin. The slot the layout solver
/// reserves is therefore sized to text + margin; the draw helper
/// then insets back to position the text inside.
pub(crate) fn text_cell_for_element(
    s: &str,
    el: &crate::plot::theme::TextElement,
    parent_pt: f64,
    dpi: f64,
    theme: &crate::plot::theme::Theme,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
) -> Cell {
    use crate::plot::theme::text_concrete_defaults;
    let style = text_style_from(el, parent_pt);
    let run = measure_for_element(s, el, &style, dpi, theme, images);
    let margin = el
        .margin
        .or(text_concrete_defaults().margin)
        .expect("text_concrete_defaults sets margin");
    let (mt, mr, mb, ml) = margin.resolve(parent_pt);
    let pt_to_px = dpi / 72.0;
    let margins_px = (mt * pt_to_px, mr * pt_to_px, mb * pt_to_px, ml * pt_to_px);
    if margins_px.0 == 0.0 && margins_px.1 == 0.0 && margins_px.2 == 0.0 && margins_px.3 == 0.0 {
        Cell::measured_boxed(run)
    } else {
        Cell::measured(crate::text::WithMargin::new(run, margins_px))
    }
}

/// Shape `s` the same way the draw pass will, so a slot measures at
/// the size it renders at. A markdown slot measures through
/// [`crate::text::rich::RichTextRun`]; anything else through
/// [`crate::text::TextRun`].
pub(crate) fn measure_for_element(
    s: &str,
    el: &crate::plot::theme::TextElement,
    style: &crate::text::TextStyle,
    dpi: f64,
    theme: &crate::plot::theme::Theme,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
) -> Box<dyn crate::layout::Measure> {
    use crate::plot::theme::text_concrete_defaults;
    if matches!(el.markdown, Some(true)) {
        let color = el
            .color
            .clone()
            .or_else(|| text_concrete_defaults().color.clone())
            .expect("color default");
        return Box::new(crate::text::rich::RichTextRun::new_with_images(
            s,
            style,
            color.resolve(&theme.palette),
            &theme.rich_text,
            &theme.palette,
            dpi,
            images,
        ));
    }
    Box::new(crate::text::TextRun::new(s, style, dpi))
}

/// Convert a theme [`TextElement`](crate::plot::theme::TextElement)
/// into a shaper-facing [`crate::text::TextStyle`]. Resolves
/// `size_pt` against `parent_pt` (typically the root text size) and
/// translates every `FontSpec` axis into the matching `TextStyle`
/// field: family chain (named + generic fallbacks), weight, width,
/// style (italic / oblique angle), OpenType feature toggles, and
/// variable-font axis assignments. Empty / `None` `FontSpec` fields
/// leave the corresponding `TextStyle` field at its default.
pub(crate) fn text_style_from(
    el: &crate::plot::theme::TextElement,
    parent_pt: f64,
) -> crate::text::TextStyle {
    use crate::plot::theme::{text_concrete_defaults, FontFamily, FontStyle, FontWidth, Length};
    use crate::text::{
        FontFamilyEntry, FontFeatureSetting, FontStyleKind, FontVariationSetting,
        GenericFamilyKind, LineHeight,
    };
    let defaults = text_concrete_defaults();
    let size_len = el.size_pt.or(defaults.size_pt).expect("size_pt default");
    let size = size_len.resolve(parent_pt) as f32;
    let mut style = crate::text::TextStyle::new(size);
    // Line height: `Length::Rel(m)` → font-size multiplier; `Abs(pt)`
    // → absolute pt. Preserves the resolved-vs-relative semantics
    // across DPI changes.
    let lineheight = el
        .lineheight
        .or(defaults.lineheight)
        .expect("lineheight default");
    style = style.line_height(match lineheight {
        Length::Rel(mult) => LineHeight::Relative(mult as f32),
        Length::Abs(pt) => LineHeight::Absolute(pt as f32),
    });
    let tracking = el.tracking.or(defaults.tracking).expect("tracking default");
    // The shaper takes 1/1000 em: `Rel(m)` is m em already, and an
    // absolute pt value becomes the fraction of this size that it is.
    let tracking_per_mille = match tracking {
        Length::Rel(mult) => mult * 1000.0,
        Length::Abs(pt) if size > 0.0 => pt / size as f64 * 1000.0,
        Length::Abs(_) => 0.0,
    };
    style = style.tracking(tracking_per_mille as f32);
    let underline = el
        .underline
        .or(defaults.underline)
        .expect("underline default");
    style = style.underline(underline);
    let strikethrough = el
        .strikethrough
        .or(defaults.strikethrough)
        .expect("strikethrough default");
    style = style.strikethrough(strikethrough);
    if let Some(weight) = el.font.weight {
        style = style.weight(weight.0);
    }
    if let Some(width) = el.font.width {
        style = style.width(match width {
            FontWidth::UltraCondensed => 0.5,
            FontWidth::ExtraCondensed => 0.625,
            FontWidth::Condensed => 0.75,
            FontWidth::SemiCondensed => 0.875,
            FontWidth::Normal => 1.0,
            FontWidth::SemiExpanded => 1.125,
            FontWidth::Expanded => 1.25,
            FontWidth::ExtraExpanded => 1.5,
            FontWidth::UltraExpanded => 2.0,
        });
    }
    style = style.style(match el.font.style {
        Some(FontStyle::Italic) => FontStyleKind::Italic,
        Some(FontStyle::Oblique(angle)) => FontStyleKind::Oblique(angle),
        Some(FontStyle::Normal) | None => FontStyleKind::Normal,
    });
    if let Some(family) = &el.font.family {
        let entries: Vec<FontFamilyEntry> = match family {
            FontFamily::Named(names) => names
                .iter()
                .map(|n| FontFamilyEntry::Named(n.clone()))
                .collect(),
            FontFamily::Serif => vec![FontFamilyEntry::Generic(GenericFamilyKind::Serif)],
            FontFamily::SansSerif => vec![FontFamilyEntry::Generic(GenericFamilyKind::SansSerif)],
            FontFamily::Mono => vec![FontFamilyEntry::Generic(GenericFamilyKind::Mono)],
            FontFamily::Cursive => vec![FontFamilyEntry::Generic(GenericFamilyKind::Cursive)],
            FontFamily::Fantasy => vec![FontFamilyEntry::Generic(GenericFamilyKind::Fantasy)],
            FontFamily::SystemUi => vec![FontFamilyEntry::Generic(GenericFamilyKind::SystemUi)],
        };
        style = style.families(entries);
    }
    if !el.font.features.is_empty() {
        let features: Vec<FontFeatureSetting> = el
            .font
            .features
            .iter()
            .map(|f| FontFeatureSetting {
                tag: f.tag,
                // Theme stores feature values as u32 to accommodate any
                // future encoding; parley uses u16, which covers every
                // OpenType feature value in practice.
                value: f.value.min(u16::MAX as u32) as u16,
            })
            .collect();
        style = style.features(features);
    }
    if !el.font.variations.is_empty() {
        let variations: Vec<FontVariationSetting> = el
            .font
            .variations
            .iter()
            .map(|v| FontVariationSetting {
                tag: v.tag,
                value: v.value,
            })
            .collect();
        style = style.variations(variations);
    }
    style
}

/// A resolved per-glyph outline for chrome text — palette and dpi
/// already applied, ready for [`crate::text::draw_text_outline`].
#[derive(Debug, Clone)]
pub(crate) struct TextOutline {
    /// Brush the outline pass paints with.
    pub brush: crate::brush::Brush,
    /// Glyph outline pen, width in device pixels.
    pub stroke: crate::stroke::Stroke,
}

/// Resolve a [`TextElement`](crate::plot::theme::TextElement)'s outline
/// fields into a concrete brush + pen. `None` when `text_stroke` names
/// no color or the width resolves to a non-positive pixel count — in
/// both cases the caller emits no outline pass.
///
/// `text_linewidth_pt` resolves against
/// [`DEFAULT_LINEWIDTH_PT`](crate::plot::theme::DEFAULT_LINEWIDTH_PT),
/// so no text-size parent needs threading here.
pub(crate) fn text_outline_from(
    el: &crate::plot::theme::TextElement,
    palette: &crate::plot::theme::Palette,
    dpi: f64,
) -> Option<TextOutline> {
    let color = el.text_stroke.as_ref()?.resolve(palette);
    let width_pt = el
        .text_linewidth_pt
        .or_else(|| crate::plot::theme::text_concrete_defaults().text_linewidth_pt)
        .expect("text_concrete_defaults sets text_linewidth_pt")
        .resolve(crate::plot::theme::DEFAULT_LINEWIDTH_PT);
    let width_px = width_pt * dpi / 72.0;
    if !width_px.is_finite() || width_px <= 0.0 {
        return None;
    }
    Some(TextOutline {
        brush: crate::brush::Brush::Solid(color),
        stroke: crate::stroke::Stroke::new(width_px),
    })
}

/// Emit the stroke-only glyph pass for `run` when `outline` is present.
///
/// Call immediately before the matching [`crate::text::draw_text`] with
/// identical `x`, `y` and `transform` so the outline registers behind
/// the fill. The fill pass owns picking, so this pass records
/// [`PickId::Skip`](crate::pick::PickId::Skip).
pub(crate) fn draw_text_outline_pass(
    scene: &mut dyn SceneBuilder,
    outline: Option<&TextOutline>,
    run: &crate::text::TextRun,
    x: f64,
    y: f64,
    transform: crate::geometry::Affine,
) {
    if let Some(o) = outline {
        crate::text::draw_text_outline(
            scene,
            run,
            x,
            y,
            &o.brush,
            &o.stroke,
            transform,
            crate::pick::PickId::Skip,
        );
    }
}

// ─── Unwrapped chrome labels ────────────────────────────────────────────────
//
// Break labels, legend titles and polar labels all shape at natural
// width and never re-break, so they share one shaped-run type and one
// cross-frame memo. Slots that *do* wrap (the title band, axis titles,
// strip labels) go through `draw_text_element_in_rect` instead.

thread_local! {
    /// Shaped rich runs for unwrapped chrome labels, held across
    /// frames.
    ///
    /// Thread-local rather than owned by a `Plot` because chrome is
    /// drawn through free functions whose signatures are public API;
    /// the alternative is threading a cache reference into every one
    /// of them. `RichKey` covers everything that decides what a run
    /// looks like, so sharing one cache between plots on a thread
    /// only lets them dedupe labels they have in common. Rendering is
    /// single-threaded by design, which is what keeps `Rc` sound here.
    static CHROME_RICH_CACHE: crate::text::rich::RichShapeCache =
        crate::text::rich::RichShapeCache::new();
}

/// The markdown context one chrome text slot shapes through.
///
/// Resolved once per slot rather than once per label: every run a
/// slot shapes then shares a single sheet `Arc`, which is the
/// identity [`crate::text::rich::RichShapeCache`] keys on. Rebuilding
/// it per label would miss the cache every time.
#[derive(Clone)]
pub(crate) struct RichChrome {
    /// Sheet the slot's spans resolve through — the theme's, or a
    /// derivative carrying the element's outline on its `base`.
    pub(crate) sheet: std::sync::Arc<crate::text::rich::RichTextStyleSheet>,
    /// Palette the sheet's `ThemeColor` references resolve against.
    pub(crate) palette: crate::plot::theme::Palette,
    /// Fill the base style paints with.
    pub(crate) fill: crate::color::Color,
    /// Register the slot's image tags resolve against. Shared rather
    /// than borrowed so a resolved context can be stored — an axis
    /// keeps one for the whole of its draw.
    pub(crate) images: std::sync::Arc<crate::image_registry::ImageRegistry>,
}

/// The markdown context for `el`, or `None` when the element leaves
/// `markdown` off — in which case the slot shapes plain text and
/// keeps its separate [`TextOutline`] pass.
///
/// An element carrying `text_stroke` gets a derived sheet with the
/// outline folded onto `base`, since the rich pipeline paints glyph
/// outlines from the sheet rather than from a second pass. Per-span
/// `text_stroke` in the sheet still wins.
pub(crate) fn rich_chrome_for(
    el: &crate::plot::theme::TextElement,
    theme: &crate::plot::theme::Theme,
    dpi: f64,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
) -> Option<RichChrome> {
    use crate::plot::theme::text_concrete_defaults;
    if !matches!(el.markdown, Some(true)) {
        return None;
    }
    let fill = el
        .color
        .clone()
        .or_else(|| text_concrete_defaults().color.clone())
        .expect("text_concrete_defaults sets color")
        .resolve(&theme.palette);
    let sheet = match (&el.text_stroke, text_outline_from(el, &theme.palette, dpi)) {
        (Some(stroke_color), Some(o)) => {
            let mut s = (*theme.rich_text).clone();
            let base = s.get("base").cloned().unwrap_or_default();
            s.set(
                "base",
                crate::text::rich::StyleDelta {
                    text_stroke: Some(stroke_color.clone()),
                    text_stroke_width: Some(crate::text::rich::pt(o.stroke.width * 72.0 / dpi)),
                    ..base
                },
            );
            std::sync::Arc::new(s)
        }
        _ => std::sync::Arc::clone(&theme.rich_text),
    };
    Some(RichChrome {
        sheet,
        palette: theme.palette,
        fill,
        images: std::sync::Arc::clone(images),
    })
}

/// A chrome label shaped at its natural width, through whichever
/// pipeline its slot opted into.
///
/// Measure and draw both hold one of these, so a slot can't reserve a
/// box shaped one way and paint one shaped the other.
// The plain variant is the wider one and the one on the default
// path; boxing it to even the two out would put an allocation on
// every chrome label a figure without markdown draws.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ChromeRun {
    /// Plain shaping — the label's markers render literally.
    Plain(crate::text::TextRun),
    /// Marquee-flavoured markdown, memoized across frames.
    Rich(std::rc::Rc<crate::text::rich::RichTextRun>),
}

impl ChromeRun {
    /// Shape `text` unwrapped. `rich` opts the label into markdown;
    /// `None` shapes plain text.
    pub(crate) fn shape(
        text: &str,
        style: &crate::text::TextStyle,
        dpi: f64,
        rich: Option<&RichChrome>,
    ) -> Self {
        use crate::text::rich::{RichKey, RichTextRun, RichTextWidth};
        let Some(rc) = rich else {
            let run = crate::text::TextRun::new(text, style, dpi);
            let _ = run.set_max_width(f32::INFINITY, crate::plot::theme::HAlign::Start);
            return ChromeRun::Plain(run);
        };
        let key = RichKey::new(
            text,
            style,
            rc.fill,
            &rc.sheet,
            &rc.palette,
            dpi,
            RichTextWidth::Natural,
            crate::plot::theme::HAlign::Start,
            &rc.images,
        );
        let run = CHROME_RICH_CACHE.with(|cache| {
            cache.get_or_shape(key, || {
                RichTextRun::new_with_images(
                    text,
                    style,
                    rc.fill,
                    &rc.sheet,
                    &rc.palette,
                    dpi,
                    &rc.images,
                )
            })
        });
        ChromeRun::Rich(run)
    }

    /// Natural single-line width in pixels — what the label actually
    /// draws, and what an unwrapped slot reserves.
    pub(crate) fn width(&self) -> f64 {
        match self {
            ChromeRun::Plain(r) => r.natural_width(),
            ChromeRun::Rich(r) => r.natural_width(),
        }
    }

    /// Full line-box height in pixels, half-leading included.
    pub(crate) fn line_box_height(&self) -> f64 {
        match self {
            ChromeRun::Plain(r) => r.natural_height(),
            ChromeRun::Rich(r) => r.natural_height(),
        }
    }

    /// Height of the visible band — ascender top to descender bottom.
    pub(crate) fn inked_height(&self) -> f64 {
        match self {
            ChromeRun::Plain(r) => r.inked_height(),
            ChromeRun::Rich(r) => r.inked_height(),
        }
    }

    /// Offset from the run's top edge to its visible top.
    pub(crate) fn ink_top_offset(&self) -> f64 {
        match self {
            ChromeRun::Plain(r) => r.first_line_ascender_offset(),
            ChromeRun::Rich(r) => r.ink_top_offset(),
        }
    }

    /// Offset from the run's top edge to the first line's baseline.
    pub(crate) fn baseline_offset(&self) -> f64 {
        match self {
            ChromeRun::Plain(r) => r.baseline_offset(),
            ChromeRun::Rich(r) => r.baseline_offset(),
        }
    }

    /// Cap-height of the run's first glyph run, in pixels.
    pub(crate) fn cap_height(&self) -> f64 {
        match self {
            ChromeRun::Plain(r) => r.cap_height(),
            ChromeRun::Rich(r) => r.cap_height(),
        }
    }

    /// Draw the label with its top-left at `(x, y)` in the frame
    /// `transform` establishes.
    ///
    /// `outline` applies to the plain path only — a markdown slot
    /// carries its outline on the sheet's `base` selector (see
    /// [`rich_chrome_for`]), so the rich pipeline has already emitted
    /// it by the time the glyphs paint.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &self,
        scene: &mut dyn SceneBuilder,
        x: f64,
        y: f64,
        brush: &crate::brush::Brush,
        outline: Option<&TextOutline>,
        transform: crate::geometry::Affine,
        pick_id: crate::pick::PickId,
    ) {
        match self {
            ChromeRun::Plain(run) => {
                draw_text_outline_pass(scene, outline, run, x, y, transform);
                crate::text::draw_text(scene, run, x, y, brush, transform, pick_id);
            }
            ChromeRun::Rich(run) => {
                use crate::text::rich::{draw_rich_text, HAnchor, RichAnchor, VAnchor};
                draw_rich_text(
                    scene,
                    run,
                    x,
                    y,
                    RichAnchor {
                        h: HAnchor::Left,
                        v: VAnchor::Top,
                    },
                    transform,
                    pick_id,
                );
            }
        }
    }
}

/// Resolve the effective [`TextElement`](crate::plot::theme::TextElement)
/// for an `Element<TextElement>` slot. `Blank` short-circuits to
/// `None`; otherwise the slot's sparse fields cascade onto `root`,
/// producing an owned `TextElement` whose `Some`-set fields reflect
/// the per-field merge of override → root.
///
/// Callers must still fall through to
/// [`text_concrete_defaults`](crate::plot::theme::text_concrete_defaults)
/// for any field left `None` (typically by passing the resolved
/// element to [`text_style_from`], which handles the fallback).
pub(crate) fn effective_text(
    slot: &crate::plot::theme::Element<crate::plot::theme::TextElement>,
    root: &crate::plot::theme::TextElement,
) -> Option<crate::plot::theme::TextElement> {
    match slot {
        crate::plot::theme::Element::Blank => None,
        crate::plot::theme::Element::Inherit => Some(root.clone()),
        crate::plot::theme::Element::Set(el) => Some(el.cascade(root)),
    }
}

/// Build the `Cell` for a cartesian axis title slot. Vertical sides
/// (Left/Right) wrap the shaped run in a [`RotatedAxisTitleMeasure`]
/// so the slot's column width reflects the rotated text's footprint
/// (one font line height) rather than the natural string width.
/// Horizontal sides reuse the unrotated `TextRun` measure directly.
pub(crate) fn axis_title_cell(
    title: &str,
    side: AxisSide,
    theme: &crate::plot::theme::Theme,
    dpi: f64,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
) -> Cell {
    let (ch, side_idx) = crate::plot::chrome::axis::axis_side_to_channel_side(side);
    let resolved = theme.resolved_axis(ch, side_idx);
    let root_pt = crate::plot::chrome::root_text_pt(theme);
    let Some(el) = resolved.title else {
        return Cell::empty();
    };
    let style = text_style_from(&el, root_pt);
    let run = measure_for_element(title, &el, &style, dpi, theme, images);
    if side.is_vertical() {
        Cell::measured(RotatedAxisTitleMeasure {
            rotated_w: run.height_at(f64::INFINITY, dpi),
        })
    } else {
        Cell::measured_boxed(run)
    }
}

/// Measure for an axis title rotated 90° onto a vertical chrome
/// column. The slot's horizontal contribution is the font's line
/// height (post-rotation width); the vertical extent is panel-driven,
/// so the cell reports no row contribution.
struct RotatedAxisTitleMeasure {
    rotated_w: f64,
}

impl crate::layout::Measure for RotatedAxisTitleMeasure {
    fn width_hint(&self, _dpi: f64) -> crate::layout::WidthHint {
        crate::layout::WidthHint::Min(self.rotated_w)
    }

    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        0.0
    }

    fn width_at(&self, _height: f64, _dpi: f64) -> f64 {
        self.rotated_w
    }
}

/// Wrap width for text rotated by `angle_rad` inside a `w` × `h`
/// rect: the rect's extent along the text's own advance direction.
/// Unrotated text wraps at `w` and quarter-turned text at `h`, so a
/// rotated block breaks against the edge it actually runs along
/// rather than the one that happens to be horizontal on screen.
pub(crate) fn rotated_wrap_width(w: f64, h: f64, angle_rad: f64) -> f64 {
    w * angle_rad.cos().abs() + h * angle_rad.sin().abs()
}

/// Render `text` styled by `el` inside `rect`, honoring every
/// layout-affecting field on the [`TextElement`]: `margin` insets the
/// rect before wrapping, `align` controls justification along the
/// text's advance direction, `valign` positions
/// the wrapped block across its stacked lines (Top / Middle / Bottom;
/// `Baseline` treated as Top), `angle` rotates the rendered block
/// around the inset's centre (only `Rotation::Degrees(_)` resolves
/// here — `Along` / `Across` need a baseline context and are deferred
/// to per-side helpers like [`draw_axis_title`]). `lineheight` flows
/// through the cached `TextRun` via [`text_style_from`].
///
/// Both alignments live in the **text's own frame**, so a rotated
/// block aligns against the rect's extents projected onto its advance
/// and stacking axes rather than against screen width and height: a
/// quarter-turned label centres along the edge it runs down, and its
/// `valign` moves it across that edge's thickness.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text_element_in_rect(
    scene: &mut dyn SceneBuilder,
    text: &str,
    el: &crate::plot::theme::TextElement,
    rect: Rect,
    palette: &crate::plot::theme::Palette,
    parent_pt: f64,
    dpi: f64,
    pick_id: crate::pick::PickId,
    // `Some` routes through [`crate::text::rich::draw_rich_text`]
    // when `el.markdown == Some(true)` — the sheet drives markdown
    // resolution, and the resolved `TextElement` feeds the base
    // style. `None` disables the rich path unconditionally (used at
    // callsites that don't want markdown, or feature gates that
    // prefer to opt out).
    sheet: Option<&std::sync::Arc<crate::text::rich::RichTextStyleSheet>>,
    // Register the slot's image tags resolve against, ignored when the
    // slot is not markdown.
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
) {
    use crate::brush::Brush;
    use crate::geometry::{Affine, Vec2};
    use crate::plot::theme::{text_concrete_defaults, HAlign, Rotation, VAlign};
    use crate::text::rich::{draw_rich_text, HAnchor, RichAnchor, RichTextRun, VAnchor};
    use crate::text::{draw_text, TextRun};

    let defaults = text_concrete_defaults();
    // Inset by margin (pt → px).
    let margin = el.margin.or(defaults.margin).expect("margin default");
    let (mt, mr, mb, ml) = margin.resolve(parent_pt);
    let pt_to_px = dpi / 72.0;
    let inset = Rect::new(
        rect.x0 + ml * pt_to_px,
        rect.y0 + mt * pt_to_px,
        (rect.x1 - mr * pt_to_px).max(rect.x0 + ml * pt_to_px),
        (rect.y1 - mb * pt_to_px).max(rect.y0 + mt * pt_to_px),
    );
    let style = text_style_from(el, parent_pt);
    let color = el
        .color
        .clone()
        .or_else(|| defaults.color.clone())
        .expect("color default");
    let brush = Brush::Solid(color.resolve(palette));
    let outline = text_outline_from(el, palette, dpi);

    // ── Markdown branch. ──
    //
    // When the slot opts into markdown *and* a style sheet is
    // available, shape the rich pipeline instead of plain text. The
    // resolved `TextElement` feeds `RichTextRun`'s base style so
    // font / size / colour still cascade the same way. Alignment
    // (align / valign / angle) uses the same anchor arithmetic as
    // the plain path — anchor_x/anchor_y derived from HAlign/VAlign,
    // wrap via the same rotated-projection width.
    let use_markdown = matches!(el.markdown, Some(true)) && sheet.is_some();
    if use_markdown {
        let sheet = sheet.expect("sheet checked above");
        let align_h = el.align.or(defaults.align).expect("align default");
        let align_v = el.valign.or(defaults.valign).expect("valign default");
        let angle = el.angle.or(defaults.angle).expect("angle default");
        let angle_rad = match angle {
            Rotation::Degrees(d) => (d as f64).to_radians(),
            Rotation::Along | Rotation::Across => 0.0,
        };
        let inner_w = inset.x1 - inset.x0;
        let inner_h = inset.y1 - inset.y0;
        let along_px = rotated_wrap_width(inner_w, inner_h, angle_rad);
        let cross_px = rotated_wrap_width(inner_h, inner_w, angle_rad);
        let base_brush_col = color.resolve(palette);
        // Fold the element's outline onto the base style so a themed
        // halo survives the markdown path; per-span `text_stroke` in
        // the sheet still overrides it.
        let outlined_sheet: Option<std::sync::Arc<_>> = match (&el.text_stroke, outline.as_ref()) {
            (Some(stroke_color), Some(o)) => {
                let mut s = (**sheet).clone();
                let base = s.get("base").cloned().unwrap_or_default();
                s.set(
                    "base",
                    crate::text::rich::StyleDelta {
                        text_stroke: Some(stroke_color.clone()),
                        text_stroke_width: Some(crate::text::rich::pt(o.stroke.width * 72.0 / dpi)),
                        ..base
                    },
                );
                Some(std::sync::Arc::new(s))
            }
            _ => None,
        };
        let sheet = outlined_sheet.as_ref().unwrap_or(sheet);
        let rich =
            RichTextRun::new_with_images(text, &style, base_brush_col, sheet, palette, dpi, images);
        rich.set_max_width(along_px as f32, align_h);
        let block_w = rich.content_width();
        // Inked band, not the stacked box — the same quantity
        // `measure_for_element` reserved. `ink_top` is the empty
        // band the box keeps above the first thing that paints; the
        // origin backs it out so the visible top lands flush with
        // the slot edge, mirroring the plain path's ascender shift.
        let block_h = rich.inked_height();
        let ink_top = rich.ink_top_offset();
        let hf = match align_h {
            HAlign::Start => 0.0,
            HAlign::Center | HAlign::Justify => 0.5,
            HAlign::End => 1.0,
        };
        let vf = match align_v {
            VAlign::Top | VAlign::Baseline => 0.0,
            VAlign::Middle => 0.5,
            VAlign::Bottom => 1.0,
        };
        if angle_rad.abs() < 1e-9 {
            let tx = inset.x0 + (along_px - block_w) * hf;
            let ty = inset.y0 + (cross_px - block_h) * vf - ink_top;
            draw_rich_text(
                scene,
                &rich,
                tx,
                ty,
                RichAnchor {
                    h: HAnchor::Left,
                    v: VAnchor::Top,
                },
                Affine::IDENTITY,
                pick_id,
            );
        } else {
            let centre = Vec2::new((inset.x0 + inset.x1) * 0.5, (inset.y0 + inset.y1) * 0.5);
            let transform = Affine::translate(centre)
                * Affine::rotate(angle_rad)
                * Affine::translate(Vec2::new(
                    -along_px * 0.5 + (along_px - block_w) * hf,
                    -cross_px * 0.5 + (cross_px - block_h) * vf - ink_top,
                ));
            draw_rich_text(
                scene,
                &rich,
                0.0,
                0.0,
                RichAnchor {
                    h: HAnchor::Left,
                    v: VAnchor::Top,
                },
                transform,
                pick_id,
            );
        }
        return;
    }

    let run = TextRun::new(text, &style, dpi);
    let alignment = el.align.or(defaults.align).expect("align default");
    let angle = el.angle.or(defaults.angle).expect("angle default");
    let angle_rad = match angle {
        Rotation::Degrees(d) => (d as f64).to_radians(),
        // Along / Across need a baseline orientation — chrome that
        // knows the baseline (axis titles, polar rails) handles those
        // variants in its own helper. Default to no rotation here.
        Rotation::Along | Rotation::Across => 0.0,
    };
    let inner_w = inset.x1 - inset.x0;
    let inner_h = inset.y1 - inset.y0;
    // Alignment travels with the text, not with the screen box:
    // `align` runs along the advance direction and `valign` across
    // the stacked lines, whatever the rotation. The slot the block
    // gets is therefore the inset projected onto those two rotated
    // axes — `along_px` is the extent the wrap breaks against,
    // `cross_px` its complement.
    let along_px = rotated_wrap_width(inner_w, inner_h, angle_rad);
    let cross_px = rotated_wrap_width(inner_h, inner_w, angle_rad);
    let _ = run.set_max_width(along_px as f32, alignment);
    // Inked height (first-line ascender top → last-line descender
    // bottom) drives layout. `ascender_offset` is the half-leading
    // the parley layout reserves above the first line; the draw
    // helper compensates by shifting the layout up by that much so
    // the visible glyphs land flush with the slot edge.
    let block_h = run.inked_height();
    let ascender_offset = run.first_line_ascender_offset();
    let valign = el.valign.or(defaults.valign).expect("valign default");
    let cross_offset = match valign {
        VAlign::Top | VAlign::Baseline => 0.0,
        VAlign::Middle => ((cross_px - block_h) * 0.5).max(0.0),
        VAlign::Bottom => (cross_px - block_h).max(0.0),
    };
    if angle_rad.abs() < 1e-9 {
        let (tx, ty) = (inset.x0, inset.y0 + cross_offset - ascender_offset);
        draw_text_outline_pass(scene, outline.as_ref(), &run, tx, ty, Affine::IDENTITY);
        draw_text(scene, &run, tx, ty, &brush, Affine::IDENTITY, pick_id);
    } else {
        // Rotate about the inset's centre and place the layout in the
        // text's own frame. parley has already offset each line inside
        // a box `along_px` wide, so `align` is baked into the glyph
        // positions and the layout origin sits half that box back from
        // the centre. Measuring from the content width instead would
        // apply the alignment a second time and slide the block to one
        // end of the box.
        let centre = Vec2::new((inset.x0 + inset.x1) * 0.5, (inset.y0 + inset.y1) * 0.5);
        let transform = Affine::translate(centre)
            * Affine::rotate(angle_rad)
            * Affine::translate(Vec2::new(
                -along_px * 0.5,
                cross_offset - cross_px * 0.5 - ascender_offset,
            ));
        // Both passes take the same transform and origin, so the
        // outline lands exactly under the rotated fill.
        draw_text_outline_pass(scene, outline.as_ref(), &run, 0.0, 0.0, transform);
        draw_text(scene, &run, 0.0, 0.0, &brush, transform, pick_id);
    }
}

/// Draw an axis title into `rect`, honoring `angle` from the theme.
/// `Along` and `Across` resolve against the per-side baseline
/// direction: Top / Bottom baselines run horizontally (0°), Left
/// rotates -90° (text reads bottom-to-top), Right rotates +90°. A
/// concrete `Rotation::Degrees(_)` bypasses that and uses the
/// absolute angle.
///
/// `outline`, when present, is emitted as a stroke-only pass behind
/// the fill.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_axis_title(
    scene: &mut dyn SceneBuilder,
    run: &crate::text::TextRun,
    rect: Rect,
    side: AxisSide,
    brush: &crate::brush::Brush,
    outline: Option<&TextOutline>,
    angle: crate::plot::theme::Rotation,
) {
    use crate::geometry::{Affine, Vec2};
    use crate::plot::theme::HAlign;
    use crate::text::draw_text;
    let cx = (rect.x0 + rect.x1) * 0.5;
    let cy = (rect.y0 + rect.y1) * 0.5;
    let pid = crate::pick::PickId::Skip;
    let baseline_deg: f32 = match side {
        AxisSide::Top | AxisSide::Bottom => 0.0,
        AxisSide::Left => -90.0,
        AxisSide::Right => 90.0,
    };
    let resolved_deg = angle.resolve(baseline_deg);
    let theta = (resolved_deg as f64).to_radians();
    if theta.abs() < 1e-9 {
        let w = (rect.x1 - rect.x0) as f32;
        run.set_max_width(w, HAlign::Center);
        draw_text_outline_pass(scene, outline, run, rect.x0, rect.y0, Affine::IDENTITY);
        draw_text(scene, run, rect.x0, rect.y0, brush, Affine::IDENTITY, pid);
    } else {
        // Lay out unconstrained so the run stays single-line; the
        // surrounding slot drives how much the rotated text can grow.
        let h = run.set_max_width(f32::INFINITY, HAlign::Start) as f64;
        let w = run.content_width();
        let transform = Affine::translate(Vec2::new(cx, cy))
            * Affine::rotate(theta)
            * Affine::translate(Vec2::new(-w * 0.5, -h * 0.5));
        draw_text_outline_pass(scene, outline, run, 0.0, 0.0, transform);
        draw_text(scene, run, 0.0, 0.0, brush, transform, pid);
    }
}

/// Draw an axis title as marquee-flavoured markdown. Mirrors
/// [`draw_axis_title`] but shapes the string via [`RichTextRun`] and
/// draws with [`draw_rich_text`]. `text_stroke` on the axis title's
/// `TextElement` is not applied here — set `text_stroke` on the
/// sheet's `paragraph` class if a haloed markdown axis title is
/// needed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_axis_title_markdown(
    scene: &mut dyn SceneBuilder,
    text: &str,
    style: &crate::text::TextStyle,
    fill: crate::color::Color,
    palette: &crate::plot::theme::Palette,
    sheet: &std::sync::Arc<crate::text::rich::RichTextStyleSheet>,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    dpi: f64,
    rect: Rect,
    side: AxisSide,
    angle: crate::plot::theme::Rotation,
) {
    use crate::geometry::{Affine, Vec2};
    use crate::plot::theme::HAlign;
    use crate::text::rich::{draw_rich_text, HAnchor, RichAnchor, RichTextRun, VAnchor};
    let cx = (rect.x0 + rect.x1) * 0.5;
    let cy = (rect.y0 + rect.y1) * 0.5;
    let pid = crate::pick::PickId::Skip;
    let baseline_deg: f32 = match side {
        AxisSide::Top | AxisSide::Bottom => 0.0,
        AxisSide::Left => -90.0,
        AxisSide::Right => 90.0,
    };
    let resolved_deg = angle.resolve(baseline_deg);
    let theta = (resolved_deg as f64).to_radians();
    let run = RichTextRun::new_with_images(text, style, fill, sheet, palette, dpi, images);
    if theta.abs() < 1e-9 {
        let w = (rect.x1 - rect.x0) as f32;
        run.set_max_width(w, HAlign::Center);
        draw_rich_text(
            scene,
            &run,
            rect.x0,
            rect.y0,
            RichAnchor {
                h: HAnchor::Left,
                v: VAnchor::Top,
            },
            Affine::IDENTITY,
            pid,
        );
    } else {
        let w = run.natural_width();
        let h = run.natural_height();
        let transform = Affine::translate(Vec2::new(cx, cy))
            * Affine::rotate(theta)
            * Affine::translate(Vec2::new(-w * 0.5, -h * 0.5));
        draw_rich_text(
            scene,
            &run,
            0.0,
            0.0,
            RichAnchor {
                h: HAnchor::Left,
                v: VAnchor::Top,
            },
            transform,
            pid,
        );
    }
}

// ─── BoxMeasure shim ─────────────────────────────────────────────────────────
//
// `Cell::measured` takes `impl Measure + 'static`. The Scale axis path
// returns `Box<dyn Measure>`. Bridge it through a thin wrapper.

pub(crate) struct BoxMeasure(Box<dyn crate::layout::Measure>);

impl BoxMeasure {
    pub(crate) fn new(inner: Box<dyn crate::layout::Measure>) -> Self {
        Self(inner)
    }
}

impl crate::layout::Measure for BoxMeasure {
    fn width_hint(&self, dpi: f64) -> crate::layout::WidthHint {
        self.0.width_hint(dpi)
    }

    fn height_at(&self, width: f64, dpi: f64) -> f64 {
        self.0.height_at(width, dpi)
    }

    fn width_at(&self, height: f64, dpi: f64) -> f64 {
        self.0.width_at(height, dpi)
    }
}

/// Axis-aligned bbox of a single-line run rotated by `angle_deg`.
/// `text_w` / `text_h` are the run's natural (unrotated) pixel size.
pub(crate) fn rotated_bbox(text_w: f64, text_h: f64, angle_deg: f32) -> (f64, f64) {
    let theta = (angle_deg as f64).to_radians();
    let (cos_t, sin_t) = (theta.cos().abs(), theta.sin().abs());
    (
        text_w * cos_t + text_h * sin_t,
        text_w * sin_t + text_h * cos_t,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::theme::{TextElement, Theme};
    use crate::text::TextStyle;

    const DPI: f64 = 96.0;

    fn markdown_ctx(theme: &Theme) -> RichChrome {
        let el = TextElement {
            markdown: Some(true),
            ..Default::default()
        };
        rich_chrome_for(&el, theme, DPI, &crate::image_registry::no_images())
            .expect("markdown is on")
    }

    /// A slot that leaves `markdown` unset gets no context, so it
    /// shapes plain text and keeps its separate outline pass.
    #[test]
    fn an_element_without_markdown_gets_no_context() {
        let theme = Theme::default();
        assert!(rich_chrome_for(
            &TextElement::default(),
            &theme,
            DPI,
            &crate::image_registry::no_images()
        )
        .is_none());
    }

    /// Every label a slot shapes has to share the sheet's identity,
    /// or each one misses the cache.
    #[test]
    fn a_slot_reuses_one_shaped_run_across_labels() {
        let theme = Theme::default();
        let ctx = markdown_ctx(&theme);
        let style = TextStyle::new(11.0);
        let a = ChromeRun::shape("42", &style, DPI, Some(&ctx));
        let b = ChromeRun::shape("42", &style, DPI, Some(&ctx));
        let (ChromeRun::Rich(a), ChromeRun::Rich(b)) = (&a, &b) else {
            panic!("markdown context should produce rich runs");
        };
        assert!(
            std::rc::Rc::ptr_eq(a, b),
            "the same label at the same style must hit the cache"
        );
    }

    /// A different label is a different entry — the memo keys on the
    /// source, not just the style.
    #[test]
    fn a_different_label_shapes_its_own_run() {
        let theme = Theme::default();
        let ctx = markdown_ctx(&theme);
        let style = TextStyle::new(11.0);
        let a = ChromeRun::shape("42", &style, DPI, Some(&ctx));
        let b = ChromeRun::shape("43", &style, DPI, Some(&ctx));
        let (ChromeRun::Rich(a), ChromeRun::Rich(b)) = (&a, &b) else {
            panic!("markdown context should produce rich runs");
        };
        assert!(!std::rc::Rc::ptr_eq(a, b));
    }

    /// The metrics a break label anchors on agree between the two
    /// pipelines, which is what keeps a label from moving when a
    /// theme turns markdown on.
    #[test]
    fn both_pipelines_report_the_same_anchoring_metrics() {
        let theme = Theme::default();
        let ctx = markdown_ctx(&theme);
        let style = TextStyle::new(11.0);
        let plain = ChromeRun::shape("Hello", &style, DPI, None);
        let rich = ChromeRun::shape("Hello", &style, DPI, Some(&ctx));
        assert!(
            (plain.width() - rich.width()).abs() < 0.01,
            "widths differ: {} vs {}",
            plain.width(),
            rich.width()
        );
        assert!(
            (plain.cap_height() - rich.cap_height()).abs() < 0.01,
            "cap heights differ: {} vs {}",
            plain.cap_height(),
            rich.cap_height()
        );
        assert!(
            (plain.baseline_offset() - rich.baseline_offset()).abs() < 0.51,
            "baselines differ: {} vs {}",
            plain.baseline_offset(),
            rich.baseline_offset()
        );
    }
}
