//! The container: a magic number, a version, and a sequence of tagged
//! length-prefixed chunks.
//!
//! Chunks rather than one flat stream, for two reasons. A reader can
//! **skip a tag it doesn't know**, so a minor version can add a section
//! — animation frames, a pick-id-to-payload map — without invalidating
//! documents or readers already in the world. And chunks can be written
//! in the order a reader needs them while being *encoded* in the
//! opposite order: the string and geometry tables have to precede the
//! sections that index into them, but it's encoding those sections that
//! discovers what belongs in the tables.

/// Leading bytes of every plot document.
pub(crate) const MAGIC: &[u8; 8] = b"HEPHPLOT";

/// Incremented when a change would make an older reader misread a
/// document. A reader refuses a major it doesn't know.
pub(crate) const VERSION_MAJOR: u16 = 1;

/// Incremented for additive changes — a new chunk, a new enum variant
/// behind a tag an older reader will reject on its own.
///
/// Only the writer names it: a reader accepts any minor, which is what
/// makes such a change additive.
#[cfg(feature = "document-write")]
pub(crate) const VERSION_MINOR: u16 = 1;

/// Header: root composition id and the render hints a consumer can use
/// as defaults.
pub(crate) const CHUNK_HEAD: &[u8; 4] = b"HEAD";
/// Embedded font files.
pub(crate) const CHUNK_FONTS: &[u8; 4] = b"FONT";
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
/// Marker shapes a plot's registry holds beyond the built-ins.
pub(crate) const CHUNK_SHAPES: &[u8; 4] = b"SHPS";
/// Embedded raster images, PNG-encoded.
pub(crate) const CHUNK_IMAGES: &[u8; 4] = b"IMGS";

/// One chunk's tag and body, as found in a document.
#[cfg(feature = "document-read")]
#[derive(Debug)]
pub(crate) struct Chunk<'a> {
    /// The four-byte tag.
    pub(crate) tag: [u8; 4],
    /// The chunk's payload, excluding tag and length.
    pub(crate) body: &'a [u8],
}

/// Split a document into its header and chunks.
///
/// A chunk whose tag this build doesn't know is returned along with the
/// rest; the caller ignores it. That is what makes a minor version
/// additive.
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

    let mut out = Vec::new();
    while !r.is_empty() {
        let tag = r.take(4)?;
        let tag = [tag[0], tag[1], tag[2], tag[3]];
        let len = r.u32_fixed()? as usize;
        out.push(Chunk {
            tag,
            body: r.take(len)?,
        });
    }
    Ok(out)
}

/// The body of the first chunk tagged `tag`, or `None`.
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
    for (tag, body) in chunks {
        w.raw(*tag);
        let length_at = w.len();
        w.u32_fixed(0);
        w.raw(body);
        let written = (w.len() - length_at - 4) as u32;
        w.patch_u32_at(length_at, written);
    }
}
