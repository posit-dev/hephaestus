//! Glyph runs to `<text>` — the reason this backend exists.
//!
//! # Why `textLength` and not per-glyph positions
//!
//! A run is placed by one `x`/`y` anchor plus `textLength`, following
//! svglite, rather than by pinning every character's x.
//!
//! Per-character positioning would assert something we have no basis
//! for. We know what *our* shaping did; we cannot know what a fallback
//! face on another machine ligates, how it clusters, or how it kerns.
//! Positions derived from our shaping would be wrong there — and the
//! spec additionally requires a renderer to break a ligature whose
//! characters carry absolute positions, so the text would come apart in
//! exactly the case the extra precision was meant to serve.
//!
//! `textLength` claims only the thing that is true regardless of face:
//! this run occupies this width. Everything inside it is the viewer's
//! own correct shaping.
//!
//! # Why one element per block
//!
//! Every run of a block goes into one `<text>` as sibling `<tspan>`s,
//! which is the canonical multi-line SVG idiom and Inkscape's own
//! representation. An editor imports one `<text>` as a **single text
//! object**; N sibling `<text>` elements import as N unrelated objects.
//! For output whose point is editability that difference is the whole
//! game. [`crate::scene::TextGroup`] is what says which runs belong
//! together.

use super::defs::Defs;
use super::paint;
use super::writer::{escape_attr, escape_text, num, transform_attr};
use super::{SvgWarning, Warnings};
use crate::geometry::Affine;
use crate::pick::PickId;
use crate::scene::{GlyphRun, TextGroup};
use crate::style_vocab::{
    FontFamilyEntry, FontFeatureSetting, FontSpec, FontStyleKind, FontVariationSetting,
    GenericFamilyKind,
};

/// One run held for emission, owned so a block can be assembled across
/// several `draw_glyphs` calls.
pub(crate) struct PendingRun {
    pub group: TextGroup,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub advance: f32,
    pub font_size: f32,
    pub spec: FontSpec,
    pub rtl: bool,
    pub link: Option<String>,
    pub underline: bool,
    pub strikethrough: bool,
    /// Rects the decorations above will arrive as, so they can be
    /// dropped rather than drawn twice.
    pub rules: Vec<crate::geometry::Rect>,
    /// Fill paint, when a fill pass contributed this run.
    pub fill: Option<paint::Paint>,
    /// Stroke paint and width, when an outline pass contributed it.
    pub stroke: Option<(paint::Paint, f64)>,
    pub transform: Affine,
    pub pick: PickId,
}

/// A block of runs accumulating toward one `<text>` element.
#[derive(Default)]
pub(crate) struct TextBlock {
    pub group: Option<TextGroup>,
    pub runs: Vec<PendingRun>,
}

impl TextBlock {
    /// True when `rect` is a decoration one of these runs declared.
    ///
    /// Compared with a tolerance because the rect is rebuilt from the
    /// same floats by a different route.
    pub(crate) fn claims_rule(&self, rect: crate::geometry::Rect) -> bool {
        const EPS: f64 = 0.05;
        self.runs.iter().flat_map(|r| r.rules.iter()).any(|r| {
            (r.x0 - rect.x0).abs() < EPS
                && (r.x1 - rect.x1).abs() < EPS
                && (r.y0 - rect.y0).abs() < EPS
                && (r.y1 - rect.y1).abs() < EPS
        })
    }

    /// True when `run` belongs to the block currently accumulating.
    pub(crate) fn accepts(&self, group: TextGroup, transform: Affine) -> bool {
        match (self.group, self.runs.first()) {
            (Some(g), Some(first)) => g == group && first.transform == transform,
            _ => false,
        }
    }

    /// Add `run`, merging it into an existing run at the same place.
    ///
    /// The outline pass and the fill pass draw the same characters at
    /// the same position; merging them gives one element with both a
    /// `stroke` and a `fill` rather than two stacked copies of the
    /// text. Two stacked copies are two objects to an editor, so
    /// retyping the visible one would leave the outline behind still
    /// spelling the old string.
    pub(crate) fn push(&mut self, run: PendingRun) {
        if let Some(existing) = self
            .runs
            .iter_mut()
            .find(|r| r.x == run.x && r.y == run.y && r.text == run.text)
        {
            if run.fill.is_some() {
                existing.fill = run.fill;
                // The fill pass owns picking and decorations.
                existing.pick = run.pick;
                existing.underline |= run.underline;
                existing.strikethrough |= run.strikethrough;
            }
            if run.stroke.is_some() {
                existing.stroke = run.stroke;
            }
            return;
        }
        self.group = Some(run.group);
        self.runs.push(run);
    }
}

/// Describe a glyph run for emission, or report why it cannot be.
pub(crate) fn prepare(
    run: &GlyphRun<'_>,
    pick_id: PickId,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) -> Option<PendingRun> {
    let Some(src) = run.source else {
        warnings.note(SvgWarning::TextWithoutSource);
        return None;
    };
    let first = run.glyphs.first()?;
    // Parley adds the line's baseline into each glyph's y and the draw
    // origin is already folded in, so a glyph's y *is* the SVG
    // baseline. Applying a baseline offset again would double it.
    let paint = paint::resolve(run.brush, None, defs, doc_prefix, decimals, warnings);
    let paint = scale_alpha(paint, run.brush_alpha);
    let (fill, stroke) = match run.style {
        Some(s) => (None, Some((paint, s.width))),
        None => (Some(paint), None),
    };
    Some(PendingRun {
        group: src.group,
        text: src.text.to_string(),
        x: first.x as f64,
        y: first.y as f64,
        advance: src.advance,
        font_size: run.font_size,
        spec: src.font.clone(),
        rtl: src.rtl,
        link: src.link.map(str::to_string),
        underline: src.decorations.underline.is_some(),
        strikethrough: src.decorations.strikethrough.is_some(),
        rules: predicted_rules(&src, first.x as f64, first.y as f64),
        fill,
        stroke,
        transform: run.transform,
        pick: pick_id,
    })
}

/// The rects the shaper will emit for this run's decorations.
///
/// The rules are drawn as ordinary fills, which a rasteriser wants and
/// this backend does not: `text-decoration` is the semantic form, and a
/// separate rect would be a second object that editing the text leaves
/// behind at the old length. Suppression works by *predicting* the rect
/// rather than guessing what a passing rect might mean — a fill that
/// was not predicted is drawn, so a legend rule or a span background
/// near a label can never be swallowed.
///
/// The offset is in font-typography convention (Y-up from the
/// baseline), which is why it is subtracted to reach screen Y-down.
fn predicted_rules(
    src: &crate::scene::TextSource<'_>,
    x: f64,
    y: f64,
) -> Vec<crate::geometry::Rect> {
    let advance = src.advance as f64;
    [src.decorations.underline, src.decorations.strikethrough]
        .into_iter()
        .flatten()
        .filter(|r| r.thickness > 0.0)
        .map(|r| {
            let top = y - r.offset as f64;
            crate::geometry::Rect::new(x, top, x + advance, top + r.thickness as f64)
        })
        .collect()
}

/// Fold a run's brush alpha into its paint opacity.
fn scale_alpha(mut p: paint::Paint, alpha: f32) -> paint::Paint {
    if alpha < 1.0 {
        p.opacity = Some(p.opacity.unwrap_or(1.0) * alpha);
    }
    p
}

/// Emit one block as a single `<text>` element.
///
/// White-space handling is not written here: every `<text>` needs the
/// same declaration, so the root element carries it for all of them.
pub(crate) fn write_block(
    out: &mut String,
    block: &TextBlock,
    decimals: u8,
    pick_ids: bool,
    root: &mut RootFont,
) {
    let Some(first) = block.runs.first() else {
        return;
    };
    let shared = Shared::of(block);

    out.push_str("<text");
    if let Some(spec) = shared.spec {
        write_font_attrs(out, spec, shared.font_size, decimals, root);
    }
    if let Some(p) = shared.fill {
        write_paint_attr(out, "fill", p, decimals);
    }
    transform_attr(out, first.transform, decimals);
    out.push('>');

    for run in &block.runs {
        write_tspan(out, run, &shared, decimals, pick_ids, root);
    }
    out.push_str("</text>");
}

/// The font named on the document's root element, which every `<text>`
/// inherits.
///
/// The first run to draw claims it; text agreeing with it leaves the
/// attribute off rather than repeating it once per element, which makes
/// restyling a whole figure a one-place edit. Family and size are
/// claimed separately, so a run differing in one still inherits the
/// other.
///
/// Inheritance rather than a `<style>` rule deliberately: a presentation
/// attribute loses to any CSS rule whatever its specificity, so a
/// catch-all selector would override the elements that need their own
/// font instead of merely defaulting them.
#[derive(Default, Clone)]
pub(crate) struct RootFont {
    family: Option<String>,
    size: Option<f32>,
}

impl RootFont {
    /// The family list the root element should name, once text claimed one.
    pub(crate) fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }

    /// The size the root element should name, once text claimed one.
    pub(crate) fn size(&self) -> Option<f32> {
        self.size
    }

    /// True when `family` is what the root names, claiming it if nothing has.
    fn claim_family(&mut self, family: &str) -> bool {
        match &self.family {
            Some(f) => f == family,
            None => {
                self.family = Some(family.to_string());
                true
            }
        }
    }

    /// True when `size` is what the root names, claiming it if nothing has.
    fn claim_size(&mut self, size: f32) -> bool {
        match self.size {
            Some(s) => s == size,
            None => {
                self.size = Some(size);
                true
            }
        }
    }
}

/// Attributes every run in a block agrees on, hoisted to the parent.
struct Shared<'a> {
    spec: Option<&'a FontSpec>,
    font_size: f32,
    fill: Option<&'a paint::Paint>,
}

impl<'a> Shared<'a> {
    fn of(block: &'a TextBlock) -> Self {
        let first = block.runs.first();
        let same_font = block
            .runs
            .windows(2)
            .all(|w| w[0].spec == w[1].spec && w[0].font_size == w[1].font_size);
        let same_fill = block
            .runs
            .windows(2)
            .all(|w| paint_eq(w[0].fill.as_ref(), w[1].fill.as_ref()));
        Self {
            spec: if same_font {
                first.map(|r| &r.spec)
            } else {
                None
            },
            font_size: first.map(|r| r.font_size).unwrap_or(0.0),
            fill: if same_fill {
                first.and_then(|r| r.fill.as_ref())
            } else {
                None
            },
        }
    }
}

fn paint_eq(a: Option<&paint::Paint>, b: Option<&paint::Paint>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x.value == y.value && x.opacity == y.opacity,
        _ => false,
    }
}

/// Emit one run as a `<tspan>`, wrapped in `<a>` when it carries a link.
fn write_tspan(
    out: &mut String,
    run: &PendingRun,
    shared: &Shared<'_>,
    decimals: u8,
    pick_ids: bool,
    root: &mut RootFont,
) {
    let link = run.link.as_deref().filter(|u| safe_href(u));
    if let Some(url) = link {
        out.push_str("<a href=\"");
        escape_attr(out, url);
        // An SVG opened from a page would otherwise navigate the
        // embedding document away.
        out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\">");
    }
    out.push_str("<tspan x=\"");
    num(out, run.x, decimals);
    out.push_str("\" y=\"");
    num(out, run.y, decimals);
    out.push('"');
    if run.advance > 0.0 {
        out.push_str(" textLength=\"");
        num(out, run.advance as f64, decimals);
        // Guarantees the run occupies the box the layout solved for,
        // whatever face the viewer resolves.
        out.push_str("\" lengthAdjust=\"spacingAndGlyphs\"");
    }
    if shared.spec.is_none() {
        write_font_attrs(out, &run.spec, run.font_size, decimals, root);
    }
    if shared.fill.is_none() {
        if let Some(p) = &run.fill {
            write_paint_attr(out, "fill", p, decimals);
        }
    }
    if let Some((p, width)) = &run.stroke {
        write_paint_attr(out, "stroke", p, decimals);
        out.push_str(" stroke-width=\"");
        num(out, *width, decimals);
        // The stroke is a halo behind the glyph, not over it.
        out.push_str("\" paint-order=\"stroke fill\"");
        if run.fill.is_none() {
            out.push_str(" fill=\"none\"");
        }
    }
    let mut deco = String::new();
    if run.underline {
        deco.push_str("underline");
    }
    if run.strikethrough {
        if !deco.is_empty() {
            deco.push(' ');
        }
        deco.push_str("line-through");
    }
    if !deco.is_empty() {
        out.push_str(" text-decoration-line=\"");
        out.push_str(&deco);
        out.push('"');
    }
    if run.rtl {
        out.push_str(" direction=\"rtl\" unicode-bidi=\"bidi-override\"");
    }
    if pick_ids {
        match run.pick {
            PickId::Skip => out.push_str(" pointer-events=\"none\""),
            PickId::Block => out.push_str(" data-pick-id=\"0\""),
            PickId::Id(n) => {
                out.push_str(" data-pick-id=\"");
                out.push_str(&n.to_string());
                out.push('"');
            }
        }
    }
    out.push('>');
    escape_text(out, &run.text);
    out.push_str("</tspan>");
    if link.is_some() {
        out.push_str("</a>");
    }
}

/// Append everything naming the face: `font-family`, `font-size`,
/// `font-weight`, `font-style`, `font-stretch`, `letter-spacing`, and
/// the OpenType features and variable-font axes.
///
/// Whatever matches the CSS default, or what the element already
/// inherits from the root, is left off.
fn write_font_attrs(
    out: &mut String,
    spec: &FontSpec,
    font_size: f32,
    decimals: u8,
    root: &mut RootFont,
) {
    let family = family_list(spec);
    if !root.claim_family(&family) {
        out.push_str(" font-family=\"");
        escape_attr(out, &family);
        out.push('"');
    }
    if !root.claim_size(font_size) {
        out.push_str(" font-size=\"");
        num(out, font_size as f64, decimals);
        out.push('"');
    }
    if spec.weight != 400 {
        out.push_str(" font-weight=\"");
        out.push_str(&spec.weight.to_string());
        out.push('"');
    }
    match spec.style {
        FontStyleKind::Normal => {}
        FontStyleKind::Italic => out.push_str(" font-style=\"italic\""),
        FontStyleKind::Oblique(angle) => {
            out.push_str(" font-style=\"oblique ");
            num(out, angle as f64, 1);
            out.push_str("deg\"");
        }
    }
    if spec.width != 1.0 {
        write_stretch(out, spec.width, decimals);
    }
    if spec.tracking != 0.0 {
        // Tracking is 1/1000 em, which is `em` in CSS once divided.
        out.push_str(" letter-spacing=\"");
        num(
            out,
            spec.tracking as f64 / 1000.0 * font_size as f64,
            decimals,
        );
        out.push('"');
    }
    write_font_settings(out, spec, decimals);
}

/// The `font-stretch` keywords, each paired with the width ratio it names.
const STRETCH_KEYWORDS: [(f32, &str); 9] = [
    (0.5, "ultra-condensed"),
    (0.625, "extra-condensed"),
    (0.75, "condensed"),
    (0.875, "semi-condensed"),
    (1.0, "normal"),
    (1.125, "semi-expanded"),
    (1.25, "expanded"),
    (1.5, "extra-expanded"),
    (2.0, "ultra-expanded"),
];

/// Append `font-stretch` for a width ratio.
///
/// Without it a viewer resolves the normal-width face and `textLength`
/// squeezes it into the condensed measurement, which is a mechanical
/// scale of the wrong face rather than the face that was asked for.
fn write_stretch(out: &mut String, width: f32, decimals: u8) {
    out.push_str(" font-stretch=\"");
    write_stretch_value(out, width, decimals);
    out.push('"');
}

/// Append a width ratio as CSS, for either an element or an
/// `@font-face` descriptor.
///
/// Shared so the two always spell a width the same way: a descriptor
/// that disagreed with the request would be a face the document asks
/// for and its own `@font-face` does not answer.
pub(crate) fn write_stretch_value(out: &mut String, width: f32, decimals: u8) {
    match STRETCH_KEYWORDS.iter().find(|(ratio, _)| *ratio == width) {
        // The keyword is what SVG 1.1 consumers and the desktop editors
        // read; percentages only arrived with CSS Fonts 4.
        Some((_, name)) => out.push_str(name),
        // A variable font's `wdth` axis lands anywhere between the
        // keywords, and only a percentage can say where.
        None => {
            num(out, width as f64 * 100.0, decimals);
            out.push('%');
        }
    }
}

/// Append `font-feature-settings` and `font-variation-settings` as a
/// `style` attribute.
///
/// In `style` rather than as presentation attributes because SVG 1.1
/// names neither, and a consumer that does not recognise an attribute
/// ignores it silently. Nothing else on a `<text>` or `<tspan>` writes
/// one, so there is no declaration to collide with.
fn write_font_settings(out: &mut String, spec: &FontSpec, decimals: u8) {
    let features: Vec<&FontFeatureSetting> = spec
        .features
        .iter()
        .filter(|f| writable_tag(&f.tag))
        .collect();
    let variations: Vec<&FontVariationSetting> = spec
        .variations
        .iter()
        .filter(|v| writable_tag(&v.tag))
        .collect();
    if features.is_empty() && variations.is_empty() {
        return;
    }
    let mut css = String::new();
    if !features.is_empty() {
        css.push_str("font-feature-settings:");
        for (i, f) in features.iter().enumerate() {
            if i > 0 {
                css.push(',');
            }
            write_tag(&mut css, &f.tag);
            css.push(' ');
            css.push_str(&f.value.to_string());
        }
    }
    if !variations.is_empty() {
        if !css.is_empty() {
            css.push(';');
        }
        css.push_str("font-variation-settings:");
        for (i, v) in variations.iter().enumerate() {
            if i > 0 {
                css.push(',');
            }
            write_tag(&mut css, &v.tag);
            css.push(' ');
            num(&mut css, v.value as f64, decimals);
        }
    }
    out.push_str(" style=\"");
    escape_attr(out, &css);
    out.push('"');
}

/// True when a tag is the printable ASCII an OpenType tag should be, and
/// so can go in a CSS string without escaping.
fn writable_tag(tag: &[u8; 4]) -> bool {
    tag.iter()
        .all(|b| (b.is_ascii_graphic() || *b == b' ') && !matches!(b, b'\'' | b'\\'))
}

/// Append a tag as the quoted CSS string both settings properties take.
///
/// Single quotes because the attribute around it is delimited with
/// double ones.
fn write_tag(css: &mut String, tag: &[u8; 4]) {
    css.push('\'');
    for b in tag {
        css.push(*b as char);
    }
    css.push('\'');
}

fn write_paint_attr(out: &mut String, name: &str, p: &paint::Paint, decimals: u8) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    out.push_str(&p.value);
    out.push('"');
    if let Some(a) = p.opacity {
        out.push(' ');
        out.push_str(name);
        out.push_str("-opacity=\"");
        num(out, a as f64, decimals.max(3));
        out.push('"');
    }
}

/// The CSS `font-family` list for a spec, always ending in a generic so
/// the text still resolves when nothing else does.
pub(crate) fn family_list(spec: &FontSpec) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut has_generic = false;
    for entry in &spec.families {
        match entry {
            FontFamilyEntry::Named(name) => parts.push(quote_family(name)),
            FontFamilyEntry::Generic(kind) => {
                has_generic = true;
                parts.push(generic_keyword(*kind).to_string());
            }
        }
    }
    if !has_generic {
        parts.push("sans-serif".to_string());
    }
    parts.join(", ")
}

/// Quote a family name that is not a bare CSS identifier.
fn quote_family(name: &str) -> String {
    let bare = !name.is_empty()
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// CSS keyword for a generic family.
fn generic_keyword(kind: GenericFamilyKind) -> &'static str {
    match kind {
        GenericFamilyKind::Serif => "serif",
        GenericFamilyKind::SansSerif => "sans-serif",
        GenericFamilyKind::Mono => "monospace",
        GenericFamilyKind::Cursive => "cursive",
        GenericFamilyKind::Fantasy => "fantasy",
        GenericFamilyKind::SystemUi => "system-ui",
    }
}

/// True when a link destination is safe to put in an `href`.
///
/// Markdown here is layered over arbitrary user strings, and an inlined
/// SVG carrying a `javascript:` href is a script-injection vector.
/// Anything not on the allowed list falls back to plain styled text.
fn safe_href(url: &str) -> bool {
    let trimmed = url.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with('/') {
        return true;
    }
    match trimmed.split_once(':') {
        None => true, // relative
        Some((scheme, _)) => {
            matches!(
                scheme.trim().to_ascii_lowercase().as_str(),
                "http" | "https" | "mailto"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(families: Vec<FontFamilyEntry>, weight: u16) -> FontSpec {
        FontSpec {
            families,
            weight,
            style: FontStyleKind::Normal,
            width: 1.0,
            tracking: 0.0,
            size_pt: 12.0,
            features: vec![],
            variations: vec![],
        }
    }

    #[test]
    fn a_family_list_always_ends_in_a_generic() {
        let s = spec(vec![FontFamilyEntry::Named("Inter".into())], 400);
        assert_eq!(family_list(&s), "Inter, sans-serif");

        let s = spec(vec![FontFamilyEntry::Generic(GenericFamilyKind::Mono)], 400);
        assert_eq!(family_list(&s), "monospace");
    }

    #[test]
    fn family_names_needing_quotes_get_them() {
        let s = spec(vec![FontFamilyEntry::Named("Open Sans".into())], 400);
        assert_eq!(family_list(&s), "'Open Sans', sans-serif");
    }

    #[test]
    fn dangerous_link_schemes_are_refused() {
        assert!(safe_href("https://example.com"));
        assert!(safe_href("http://example.com"));
        assert!(safe_href("mailto:a@b.c"));
        assert!(safe_href("#anchor"));
        assert!(safe_href("relative/path"));
        assert!(!safe_href("javascript:alert(1)"));
        assert!(!safe_href("  JavaScript:alert(1)"));
        assert!(!safe_href("data:text/html,<script>"));
    }
}
