//! The container: a magic number, a version, a flags word, and a
//! sequence of tagged length-prefixed chunks.
//!
//! Chunks rather than one flat stream, for two reasons. A reader can
//! **skip a tag it doesn't know**, so a minor version can add a section
//! — animation frames, a pick-id-to-payload map — without invalidating
//! documents or readers already in the world. And chunks can be written
//! in the order a reader needs them while being *encoded* in the
//! opposite order: the string and geometry tables have to precede the
//! sections that index into them, but it's encoding those sections that
//! discovers what belongs in the tables.
//!
//! # Two growth rules the layout gives for free
//!
//! **A chunk body may grow at its tail.** The chunk's own length
//! delimits it, so a reader that decodes the sections it knows and
//! stops leaves whatever a newer writer appended untouched. Every
//! reader here must therefore *never* require a body to be fully
//! consumed — that tolerance is the mechanism, not an accident. Growth
//! inside a chunk, before its tail, needs a [`record`] instead.
//!
//! **An unknown tag is skipped only if it says it may be.** Skipping
//! a section that mattered rebuilds a plot that silently differs from
//! the one written, so criticality is encoded in the tag itself, the
//! way PNG does it: an **uppercase** initial means critical and an
//! unknown one is refused; a **lowercase** initial means ancillary and
//! an unknown one is skipped. Costs no bytes, and could not be
//! introduced once tags were in the wild.
//!
//! [`record`]: super::codec::impl_codec

/// Leading bytes of every plot document.
pub(crate) const MAGIC: &[u8; 8] = b"HEPHPLOT";

/// Incremented when a change would make an older reader misread a
/// document: a renumbered discriminant, a reordered or removed record
/// field, a new critical chunk. A reader refuses a major it doesn't
/// know.
pub(crate) const VERSION_MAJOR: u16 = 2;

/// Incremented for additive changes — a trailing field on a record, a
/// new section at a chunk body's tail, a new ancillary chunk.
///
/// Only the writer names it: a reader accepts any minor, which is what
/// makes such a change additive.
#[cfg(feature = "document-write")]
pub(crate) const VERSION_MINOR: u16 = 0;

/// Container-level flags. Every bit is reserved; a reader refuses any it
/// doesn't know, since a set bit means the body is encoded in a way it
/// cannot interpret.
///
/// Nothing needs one yet. It exists because whole-document compression,
/// or any other variation on how the chunks themselves are stored, has
/// nowhere to be announced otherwise — and two bytes now is cheaper
/// than a major bump later.
#[cfg(feature = "document-write")]
pub(crate) const FLAGS: u16 = 0;

/// Bits [`FLAGS`] may legally set. Any other bit is refused on read.
#[cfg(feature = "document-read")]
pub(crate) const KNOWN_FLAGS: u16 = 0;

// ─── Chunk tags ──────────────────────────────────────────────────────────────
//
// Uppercase initial = critical, lowercase = ancillary. See the module
// docs; `is_critical` is the one place the rule is read.

/// Header: root composition id, the render hints a consumer can use as
/// defaults, and the writer's own version.
pub(crate) const CHUNK_HEAD: &[u8; 4] = b"HEAD";
/// The interned string table.
pub(crate) const CHUNK_STRINGS: &[u8; 4] = b"STRS";
/// The interned geometry table.
pub(crate) const CHUNK_GEOMETRY: &[u8; 4] = b"GEOM";
/// The interned rich-text style-sheet table.
pub(crate) const CHUNK_SHEETS: &[u8; 4] = b"SHET";
/// The theme.
pub(crate) const CHUNK_THEME: &[u8; 4] = b"THEM";
/// The scale registry.
pub(crate) const CHUNK_SCALES: &[u8; 4] = b"SCAL";
/// The composition template and its composition-level chrome.
pub(crate) const CHUNK_COMPOSITION: &[u8; 4] = b"COMP";
/// The plots, with their geoms.
pub(crate) const CHUNK_PLOTS: &[u8; 4] = b"PLOT";

/// Embedded font files. Ancillary: a document names families, and a
/// reader with fonts of its own resolves them itself.
pub(crate) const CHUNK_FONTS: &[u8; 4] = b"font";
/// Marker shapes a registry holds beyond the built-ins. Ancillary: a
/// reader rebuilds the built-ins, so absence means no overrides.
pub(crate) const CHUNK_SHAPES: &[u8; 4] = b"shps";
/// Embedded raster images, PNG-encoded. Ancillary: pixels are payload,
/// and a reader without the `png` feature skips them by design.
pub(crate) const CHUNK_IMAGES: &[u8; 4] = b"imgs";

/// Whether an unknown chunk with this tag must be refused rather than
/// skipped. See the module docs.
#[cfg(feature = "document-read")]
pub(crate) fn is_critical(tag: &[u8; 4]) -> bool {
    tag[0].is_ascii_uppercase()
}

/// Every tag this build knows, for the unknown-critical-chunk check.
#[cfg(feature = "document-read")]
const KNOWN_TAGS: &[&[u8; 4]] = &[
    CHUNK_HEAD,
    CHUNK_STRINGS,
    CHUNK_GEOMETRY,
    CHUNK_SHEETS,
    CHUNK_THEME,
    CHUNK_SCALES,
    CHUNK_COMPOSITION,
    CHUNK_PLOTS,
    CHUNK_FONTS,
    CHUNK_SHAPES,
    CHUNK_IMAGES,
];

/// One chunk's tag and body, as found in a document.
#[cfg(feature = "document-read")]
#[derive(Debug)]
pub(crate) struct Chunk<'a> {
    /// The four-byte tag.
    pub(crate) tag: [u8; 4],
    /// The chunk's payload, excluding tag and length.
    pub(crate) body: &'a [u8],
}

/// Split a document into its chunks, having checked the header.
///
/// An **ancillary** chunk this build doesn't know is returned along with
/// the rest and ignored by the callers; that is what makes a minor
/// version additive. An unknown **critical** chunk is refused, since
/// skipping something load-bearing would rebuild a plot that silently
/// differs. A repeated tag is refused too: every tag the format defines
/// is written at most once.
#[cfg(feature = "document-read")]
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Chunk<'_>>, super::DocumentError> {
    use super::codec::Reader;
    use super::DocumentError;

    let mut r = Reader::new(bytes);
    if r.take(MAGIC.len()).ok() != Some(&MAGIC[..]) {
        return Err(DocumentError::BadMagic);
    }
    let major = r.u16_fixed()?;
    let _minor = r.u16_fixed()?;
    if major != VERSION_MAJOR {
        return Err(DocumentError::UnsupportedVersion {
            found: major,
            supported: VERSION_MAJOR,
        });
    }
    let flags = r.u16_fixed()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(DocumentError::UnsupportedFlags {
            bits: flags & !KNOWN_FLAGS,
        });
    }

    let mut out: Vec<Chunk<'_>> = Vec::new();
    while !r.is_empty() {
        let tag = r.take(4)?;
        let tag = [tag[0], tag[1], tag[2], tag[3]];
        let len = r.u32_fixed()? as usize;
        let body = r.take(len)?;

        if out.iter().any(|c| c.tag == tag) {
            return Err(DocumentError::DuplicateChunk {
                tag: tag_name(&tag),
            });
        }
        if is_critical(&tag) && !KNOWN_TAGS.iter().any(|k| **k == tag) {
            return Err(DocumentError::UnknownCriticalChunk {
                tag: tag_name(&tag),
            });
        }
        out.push(Chunk { tag, body });
    }
    Ok(out)
}

/// A tag rendered for an error message, with non-ASCII bytes escaped.
#[cfg(feature = "document-read")]
fn tag_name(tag: &[u8; 4]) -> String {
    tag.iter()
        .map(|b| {
            if b.is_ascii_graphic() {
                (*b as char).to_string()
            } else {
                format!("\\x{b:02x}")
            }
        })
        .collect()
}

/// The body of the chunk tagged `tag`, or `None`.
///
/// A tag appears at most once — [`parse`] refuses a duplicate — so this
/// is a lookup rather than a first-match.
#[cfg(feature = "document-read")]
pub(crate) fn chunk<'a>(chunks: &[Chunk<'a>], tag: &[u8; 4]) -> Option<&'a [u8]> {
    chunks.iter().find(|c| &c.tag == tag).map(|c| c.body)
}

/// Write the header and every chunk into `w`, in the order given.
///
/// Each chunk's length is reserved and backfilled once its body is in,
/// so a body never has to be measured twice or held anywhere but `w`.
#[cfg(feature = "document-write")]
pub(crate) fn assemble(w: &mut super::codec::Writer, chunks: &[(&[u8; 4], Vec<u8>)]) {
    w.raw(MAGIC);
    w.u16_fixed(VERSION_MAJOR);
    w.u16_fixed(VERSION_MINOR);
    w.u16_fixed(FLAGS);
    for (tag, body) in chunks {
        w.raw(*tag);
        let length_at = w.len();
        w.u32_fixed(0);
        w.raw(body);
        let written = (w.len() - length_at - 4) as u32;
        w.patch_u32_at(length_at, written);
    }
}
