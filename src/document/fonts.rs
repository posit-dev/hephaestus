//! Embedding the fonts a document's text needs.
//!
//! Everywhere else, a document names a font rather than carrying it —
//! `FontSpec`, `TextStyle` and `StyleDelta` hold family names, and the
//! reader resolves them against its own font context. That is the right
//! shape when both ends share a system font set, and useless when they
//! don't: a browser's `FontContext::new()` has nothing to enumerate, so
//! every family would fall back and every glyph position would change.
//!
//! So a document *can* carry the font files for the families its text
//! mentions, and the reader registers them before anything shapes. This
//! is the one section whose contents are bytes rather than
//! configuration, and it is off by default: a system family dwarfs the
//! plot around it — 2.4 MB of Helvetica against 10 kB of plot, on macOS
//! — so the usual answer is for the consumer to register a subsetted web
//! font itself. See
//! [`WriteOptions::embed_fonts`](super::WriteOptions::embed_fonts).
//!
//! Generic families need care beyond the files. `sans-serif` is an
//! indirection through the font context, so shipping Helvetica's bytes
//! is not enough — the consumer has to be told that its `sans-serif`
//! now means Helvetica, which is what [`GenericMapping`] carries.

#[cfg(feature = "document-write")]
use std::collections::BTreeSet;

#[cfg(feature = "document-read")]
use super::codec::{Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;

use super::codec::impl_codec;
#[cfg(feature = "document-write")]
use crate::plot::theme::{FontFamily, Theme};
#[cfg(feature = "document-write")]
use crate::plot::PlotComposition;
use crate::text::GenericFamilyKind;

/// One embedded font face.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FontFace {
    /// Family name the reader should register it under.
    pub(crate) family: String,
    /// Face index within the file, for a collection (TTC / OTC).
    pub(crate) index: u32,
    /// The font file itself.
    pub(crate) bytes: Vec<u8>,
}

// Hand-written rather than through `impl_codec!`: the blanket `Vec<T>`
// impl would varint every byte, and a font file is around half bytes
// >= 128, each of which would then cost two. Raw length-prefixed bytes
// keep a file its own size.
#[cfg(feature = "document-write")]
impl Encode for FontFace {
    fn encode(&self, w: &mut Writer) {
        self.family.encode(w);
        self.index.encode(w);
        w.bytes(&self.bytes);
    }
}

#[cfg(feature = "document-read")]
impl Decode for FontFace {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(FontFace {
            family: String::decode(r)?,
            index: u32::decode(r)?,
            bytes: r.bytes()?.to_vec(),
        })
    }
}

/// Which generic families a document reinstates, and the concrete
/// families each resolved to on the writing machine.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenericMapping {
    /// The generic being pinned.
    pub(crate) kind: GenericFamilyKind,
    /// Concrete family names, best first.
    pub(crate) families: Vec<String>,
}

impl_codec! {
    record GenericMapping { kind, families }
}

/// The fonts a document carries: the faces themselves, plus what each
/// referenced generic family resolved to.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EmbeddedFonts {
    /// Font files, one entry per face.
    pub(crate) faces: Vec<FontFace>,
    /// Generic-family indirections to reinstate.
    pub(crate) generics: Vec<GenericMapping>,
}

impl_codec! {
    record EmbeddedFonts { faces, generics }
}

/// What a composition's text could ask for: named families, and the
/// generics whose meaning has to travel with them.
///
/// Over-collecting is the safe direction — a family named but never
/// drawn with costs its file, while one drawn with but not collected
/// changes the output. Both lists come out sorted, so the same plot
/// embeds the same fonts in the same order.
#[cfg(feature = "document-write")]
pub(crate) fn referenced_families(comp: &PlotComposition) -> (Vec<String>, Vec<GenericFamilyKind>) {
    let mut named = BTreeSet::new();
    let mut generic: Vec<GenericFamilyKind> = Vec::new();

    collect_theme_families(&comp.theme, &mut named, &mut generic);
    for plot in comp.plots.values().flatten() {
        if let Some(part) = plot.theme_override_ref() {
            if let Some(text) = &part.text {
                collect_font_spec(&text.font.family, &mut named, &mut generic);
            }
        }
        // The `"family"` channel on the text geoms, whether constant or
        // per row. A channel names a family directly — there is no
        // generic spelling at that level.
        for (_, geom) in plot.geoms() {
            if let Some(channel) = geom.state().channels.get("family") {
                collect_channel_families(channel, &mut named);
            }
        }
    }

    // The shaper falls back through the generic a style names, and every
    // `TextStyle` carries one, so a document that embeds nothing generic
    // has no text at all on a consumer with no system fonts.
    push_generic(&mut generic, GenericFamilyKind::SansSerif);

    (named.into_iter().collect(), generic)
}

/// Add `kind` if it isn't already listed, preserving first-seen order.
#[cfg(feature = "document-write")]
fn push_generic(out: &mut Vec<GenericFamilyKind>, kind: GenericFamilyKind) {
    if !out.contains(&kind) {
        out.push(kind);
    }
}

#[cfg(feature = "document-write")]
fn collect_theme_families(
    theme: &Theme,
    named: &mut BTreeSet<String>,
    generic: &mut Vec<GenericFamilyKind>,
) {
    collect_font_spec(&theme.text.font.family, named, generic);
    // Rich-text style sheets name families per selector.
    for (_, delta) in theme.rich_text.iter() {
        if let Some(family) = &delta.family {
            named.insert(family.clone());
        }
    }
}

#[cfg(feature = "document-write")]
fn collect_font_spec(
    family: &Option<FontFamily>,
    named: &mut BTreeSet<String>,
    generic: &mut Vec<GenericFamilyKind>,
) {
    match family {
        Some(FontFamily::Named(names)) => named.extend(names.iter().cloned()),
        Some(FontFamily::Serif) => {
            push_generic(generic, GenericFamilyKind::Serif);
        }
        Some(FontFamily::SansSerif) => {
            push_generic(generic, GenericFamilyKind::SansSerif);
        }
        Some(FontFamily::Mono) => {
            push_generic(generic, GenericFamilyKind::Mono);
        }
        Some(FontFamily::Cursive) => {
            push_generic(generic, GenericFamilyKind::Cursive);
        }
        Some(FontFamily::Fantasy) => {
            push_generic(generic, GenericFamilyKind::Fantasy);
        }
        Some(FontFamily::SystemUi) => {
            push_generic(generic, GenericFamilyKind::SystemUi);
        }
        None => {}
    }
}

#[cfg(feature = "document-write")]
fn collect_channel_families(channel: &crate::plot::Channel, out: &mut BTreeSet<String>) {
    use crate::plot::Channel;
    use crate::scales::value::{DataColumn, Value};

    let mut push = |v: &Value| {
        if let Value::String(s) = v {
            out.insert(s.to_string());
        }
    };
    match channel {
        Channel::Constant(v) | Channel::RawConstant(v) => push(v),
        Channel::Data(col) | Channel::RawData(col) => {
            if let DataColumn::String(names) = col {
                for n in names {
                    out.insert(n.to_string());
                }
            }
        }
    }
}

/// Resolve each family to the font files backing it.
///
/// A family the writing machine can't resolve is skipped rather than
/// refused: a document is still useful where the reader happens to have
/// that family, and the alternative is failing a write over a font the
/// plot may not even draw with.
#[cfg(feature = "document-write")]
pub(crate) fn collect(named: &[String], generics: &[GenericFamilyKind]) -> EmbeddedFonts {
    // A generic resolves to concrete families, whose files are what
    // actually has to travel.
    let mut generic_mappings = Vec::new();
    let mut wanted: BTreeSet<String> = named.iter().cloned().collect();
    for &kind in generics {
        let families = crate::text::generic_family_names(kind);
        if families.is_empty() {
            continue;
        }
        // Only the best match's files: a generic on a typical system
        // resolves to a long fallback chain, and embedding all of it
        // would dwarf the plot.
        wanted.extend(families.first().cloned());
        generic_mappings.push(GenericMapping { kind, families });
    }

    let mut faces = Vec::new();
    for family in &wanted {
        for (bytes, index) in crate::text::font_faces_for_family(family) {
            faces.push(FontFace {
                family: family.clone(),
                index,
                bytes,
            });
        }
    }
    EmbeddedFonts {
        faces,
        generics: generic_mappings,
    }
}

/// Register every embedded face, so the families a document names
/// resolve before anything shapes.
///
/// Registration is process-global and permanent, which matches how
/// [`crate::text::register_font_bytes`] works: a consumer loads a
/// document once and renders it many times.
#[cfg(feature = "document-read")]
pub(crate) fn register(fonts: &EmbeddedFonts) {
    for face in &fonts.faces {
        // The index is carried for fidelity but registration takes whole
        // files — a collection registers all its faces at once, and the
        // shaper picks among them by the attributes it was asked for.
        crate::text::register_font_bytes(face.bytes.clone());
    }
    // After the files, so the names the mapping points at exist.
    for mapping in &fonts.generics {
        crate::text::set_generic_family(mapping.kind, &mapping.families);
    }
}
