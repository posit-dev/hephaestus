//! Glyph outlines as `<path>`, for runs that cannot be text.
//!
//! A run arrives with no source text when the caller positioned the
//! glyphs itself — a glyph-backed marker shape, or a downstream crate
//! building a [`GlyphRun`] by hand. Dropping it would be the worst
//! failure a rendering backend can have: silent, invisible, and
//! undiagnosable. Outlines are also the right answer for a marker
//! specifically, since a scatterplot's ★ is a *shape* to move and
//! recolor rather than text to retype — and because a marker's size and
//! centring are derived from the face's own metrics, so a viewer that
//! substituted a different face would move the datum.

use super::writer::num;
use super::{SvgWarning, Warnings};
use crate::scene::GlyphRun;
use skrifa::instance::{LocationRef, Size as SkSize};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

/// Collects a glyph's contours into SVG path data.
///
/// Skrifa reports outlines in font-typography convention — Y up from
/// the baseline — and the scene is Y down, so every y is negated on the
/// way in.
struct PathPen<'a> {
    out: &'a mut String,
    dx: f64,
    dy: f64,
    decimals: u8,
    wrote: bool,
}

impl PathPen<'_> {
    fn pt(&mut self, x: f32, y: f32) {
        num(self.out, self.dx + f64::from(x), self.decimals);
        self.out.push(' ');
        num(self.out, self.dy - f64::from(y), self.decimals);
    }
}

impl OutlinePen for PathPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.out.push('M');
        self.pt(x, y);
        self.wrote = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.out.push('L');
        self.pt(x, y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.out.push('Q');
        self.pt(cx, cy);
        self.out.push(' ');
        self.pt(x, y);
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.out.push('C');
        self.pt(c1x, c1y);
        self.out.push(' ');
        self.pt(c2x, c2y);
        self.out.push(' ');
        self.pt(x, y);
    }

    fn close(&mut self) {
        self.out.push('Z');
    }
}

/// Path data for every glyph in `run`, positioned, or `None` when the
/// face yields no outlines.
pub(crate) fn outline_d(
    run: &GlyphRun<'_>,
    decimals: u8,
    warnings: &mut Warnings,
) -> Option<String> {
    let data = run.font.data();
    let font = FontRef::from_index(data.data.as_ref(), data.index).ok()?;
    let outlines = font.outline_glyphs();
    // A variable font would otherwise render its default instance. The
    // axis values travel on the run's `FontSpec` precisely for this.
    let coords: Vec<skrifa::instance::NormalizedCoord> = run
        .source
        .map(|src| {
            let axes = font.axes();
            let settings: Vec<(skrifa::Tag, f32)> = src
                .font
                .variations
                .iter()
                .map(|v| (skrifa::Tag::new(&v.tag), v.value))
                .collect();
            axes.location(settings).coords().to_vec()
        })
        .unwrap_or_default();
    let location = if coords.is_empty() {
        LocationRef::default()
    } else {
        LocationRef::new(&coords)
    };
    let size = SkSize::new(run.font_size);

    let mut d = String::new();
    let mut any = false;
    for glyph in run.glyphs {
        let Some(g) = outlines.get(skrifa::GlyphId::new(glyph.id)) else {
            continue;
        };
        let mut pen = PathPen {
            out: &mut d,
            dx: f64::from(glyph.x),
            dy: f64::from(glyph.y),
            decimals,
            wrote: false,
        };
        if g.draw(DrawSettings::unhinted(size, location), &mut pen)
            .is_ok()
        {
            any |= pen.wrote;
        }
    }
    if !any {
        // A color font's glyphs are layers or bitmaps, not one contour
        // set; there is nothing monochrome to fall back to.
        warnings.note(SvgWarning::TextWithoutSource);
        return None;
    }
    Some(d)
}
