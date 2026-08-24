//! Plot documents — capture a [`PlotComposition`] to a self-contained
//! byte string and rebuild it elsewhere.
//!
//! A plot exists only as live Rust objects, and
//! [`PlotComposition::render`] turns one into drawing operations for a
//! single pixel size. A document is the durable form: the plot's
//! *configuration* — layout tree, geoms and their columns, scales,
//! theme, shapes, fonts — with nothing size-dependent baked in. A
//! consumer reads it back into a live `PlotComposition` and calls
//! `render` itself, so resizing reflows exactly as it would have on the
//! machine that wrote it.
//!
//! Nothing shaped or measured is written. Shaped text runs, axis and
//! legend measures, solved layouts and every cache are functions of the
//! output size, so they are rebuilt on load; text travels as source
//! strings plus style descriptors.
//!
//! # Directions are separate features
//!
//! [`document-read`] and [`document-write`] gate the two halves
//! independently, because a consumer — a wasm build serving a website,
//! typically — only ever reads. `document` enables both.
//!
//! [`document-read`]: https://docs.rs/hephaestus
//! [`document-write`]: https://docs.rs/hephaestus
//! [`PlotComposition`]: crate::plot::PlotComposition
//! [`PlotComposition::render`]: crate::plot::PlotComposition::render

pub(crate) mod codec;
pub(crate) mod fonts;
pub(crate) mod images;
pub(crate) mod impls_core;
pub(crate) mod impls_plot;
pub(crate) mod impls_scale;
pub(crate) mod impls_theme;
pub(crate) mod intern;
#[cfg(feature = "document-read")]
mod read;
pub(crate) mod shapes;
pub(crate) mod wire;
#[cfg(feature = "document-write")]
mod write;

#[cfg(feature = "document-read")]
pub use read::{GeomFactory, ReadContext};
#[cfg(feature = "document-write")]
pub use write::{unsupported_items, unsupported_items_for, UnsupportedItem, WriteOptions};

/// Major version of the document format this build speaks.
///
/// A reader refuses a document whose major differs — the check is equality,
/// not a floor — so this is a hard compatibility boundary rather than a hint.
/// A consumer pinned to one build of this crate can only read documents
/// written at the same major, which is worth surfacing to whatever chooses
/// the two versions: a wasm client on a website and the process writing its
/// documents have to agree, and nothing at runtime can paper over a mismatch.
pub const FORMAT_VERSION_MAJOR: u16 = wire::VERSION_MAJOR;

/// Minor version this build writes. Readers accept any minor, which is what
/// makes an additive change additive.
#[cfg(feature = "document-write")]
pub const FORMAT_VERSION_MINOR: u16 = wire::VERSION_MINOR;

/// Why a document could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    /// The bytes don't start with the document magic.
    #[error("not a hephaestus plot document (bad magic)")]
    BadMagic,

    /// The document's major version postdates this build. Minor bumps
    /// read fine — unknown chunks are skipped — so only a major
    /// mismatch lands here.
    #[error(
        "plot document format version {found} is newer than this build reads (supports up to {supported})"
    )]
    UnsupportedVersion {
        /// Major version found in the document.
        found: u16,
        /// Highest major version this build reads.
        supported: u16,
    },

    /// A value ran past the end of the input.
    #[error("plot document ended mid-value at offset {offset}: wanted {wanted} more bytes, {available} left")]
    UnexpectedEof {
        /// Offset the read started at.
        offset: usize,
        /// Bytes the value needed.
        wanted: usize,
        /// Bytes actually remaining.
        available: usize,
    },

    /// An enum tag names no variant this build knows. Distinct from
    /// [`Self::UnsupportedVersion`]: the version was acceptable, so
    /// this is a corrupt or mislabelled document rather than a newer
    /// one.
    #[error("plot document has invalid {type_name} discriminant {tag} at offset {offset}")]
    BadDiscriminant {
        /// Name of the type whose tag failed to resolve.
        type_name: &'static str,
        /// The tag that was read.
        tag: u64,
        /// Offset the tag was read from.
        offset: usize,
    },

    /// A length-prefixed string wasn't valid UTF-8.
    #[error("plot document has invalid UTF-8 in a string at offset {offset}")]
    BadUtf8 {
        /// Offset the string started at.
        offset: usize,
    },

    /// A varint ran past its maximum encoded width.
    #[error("plot document has a malformed varint at offset {offset}")]
    BadVarint {
        /// Offset the varint started at.
        offset: usize,
    },

    /// The document names a geom kind this reader has no constructor
    /// for. Either it was written by a build with geoms this one lacks,
    /// or the host needs to register its own through
    /// [`ReadContext::with_geom`].
    #[error("plot document holds a geom of unknown kind {kind:?}")]
    UnknownGeom {
        /// The kind tag that couldn't be resolved.
        kind: String,
    },

    /// The document is missing a chunk the format requires.
    #[error("plot document is missing its {tag} chunk")]
    MissingChunk {
        /// Tag of the absent chunk.
        tag: &'static str,
    },

    /// The plot holds things a document can't carry, and the write
    /// wasn't asked to degrade them. Every problem is listed, not just
    /// the first.
    #[cfg(feature = "document-write")]
    #[error("plot cannot be written as a document: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Unsupported(Vec<UnsupportedItem>),

    /// A decoded value was well-formed on the wire but violates an
    /// invariant the live type enforces — non-increasing bin edges, a
    /// misaligned dash pattern, a channel column of the wrong length.
    #[error("plot document holds an invalid {what}: {why}")]
    Invalid {
        /// What was being rebuilt.
        what: &'static str,
        /// Why the live type rejected it.
        why: String,
    },
}

// ─── Entry points ────────────────────────────────────────────────────────────

/// Capture `comp` as a self-contained document.
///
/// Returns [`DocumentError::Unsupported`] listing everything the plot
/// carries that a document can't, unless
/// [`WriteOptions::lossy`] is set — see [`unsupported_items`] to check
/// ahead of time.
#[cfg(feature = "document-write")]
pub fn write_composition(
    comp: &crate::plot::PlotComposition,
    opts: &WriteOptions,
) -> Result<Vec<u8>, DocumentError> {
    use codec::{Encode, Writer};

    let problems = write::unsupported_items_for(comp, opts);
    if !problems.is_empty() && !opts.lossy {
        return Err(DocumentError::Unsupported(problems));
    }

    let mut w = Writer::new();

    // Encoded first, in dependency order, because encoding these is what
    // discovers what belongs in the tables the reader needs *before*
    // them. `detached` keeps the tables while producing a separate body.
    let theme = w.detached(|w| comp.theme.as_ref().encode(w));
    let scales = w.detached(|w| comp.scales.encode(w));
    let composition = w.detached(|w| {
        comp.template.encode(w);
        let mut ids: Vec<&String> = comp.chrome.keys().collect();
        ids.sort_unstable();
        w.varint(ids.len() as u64);
        for id in ids {
            id.encode(w);
            comp.chrome[id].encode(w);
        }
        comp.chrome_order.encode(w);
    });
    let plots = w.detached(|w| {
        w.varint(comp.plot_order.len() as u64);
        for patch in &comp.plot_order {
            patch.encode(w);
            let list = comp.plots.get(patch).map(Vec::as_slice).unwrap_or(&[]);
            w.varint(list.len() as u64);
            for plot in list {
                plot.encode(w);
            }
        }
    });

    // Sheets can add strings (a `StyleDelta` border pattern names a
    // marker shape), so they are encoded before the string table is
    // taken. Geometry adds neither, so it can follow.
    let sheets = w.detached(|w| {
        let table = w.tables().sheets().to_vec();
        w.varint(table.len() as u64);
        for sheet in &table {
            impls_theme::encode_sheet(sheet, w);
        }
    });
    let geometry = w.detached(|w| {
        let table = w.tables().geometries().to_vec();
        w.varint(table.len() as u64);
        for g in &table {
            g.as_ref().encode(w);
        }
    });
    // Marker shapes a caller registered or replaced. Encoded before the
    // string table because a glyph shape's style names font families.
    let shape_bytes = w.detached(|w| {
        shapes::collect(comp).encode(w);
    });
    // Raster images, PNG-encoded. Off by default and gated on a codec,
    // so the common shape of this chunk is an empty table.
    let image_bytes = w.detached(|w| {
        images::collect_if(comp, opts.embed_images).encode(w);
    });
    let strings = w.detached(|w| {
        let table = w.tables().strings().to_vec();
        w.varint(table.len() as u64);
        for s in &table {
            // Written inline: this *is* the table the interned
            // references resolve against.
            w.str(s);
        }
    });

    // Font files, so a consumer with no fonts of its own — a browser,
    // typically — resolves the same faces the writer shaped against.
    // Encoded before the string table, since a family name is a string.
    let font_bytes = w.detached(|w| {
        let embedded = if opts.embed_fonts {
            let (named, generics) = fonts::referenced_families(comp);
            fonts::collect(&named, &generics)
        } else {
            fonts::EmbeddedFonts::default()
        };
        embedded.encode(w);
    });

    let head = w.detached(|w| {
        comp.root_id.encode(w);
        opts.background.encode(w);
        opts.size_hint.encode(w);
        opts.dpi_hint.encode(w);
    });

    wire::assemble(
        &mut w,
        &[
            (wire::CHUNK_HEAD, head),
            (wire::CHUNK_FONTS, font_bytes),
            (wire::CHUNK_STRINGS, strings),
            (wire::CHUNK_GEOMETRY, geometry),
            (wire::CHUNK_SHEETS, sheets),
            (wire::CHUNK_THEME, theme),
            (wire::CHUNK_SCALES, scales),
            (wire::CHUNK_COMPOSITION, composition),
            (wire::CHUNK_PLOTS, plots),
            (wire::CHUNK_SHAPES, shape_bytes),
            (wire::CHUNK_IMAGES, image_bytes),
        ],
    );
    Ok(w.finish())
}

/// The render hints a document carries.
///
/// Advisory: a document describes a plot, not a frame, so a consumer is free
/// to render it at any size. These say what the writer had in mind, which is
/// what a consumer needs when it has to choose a size before it has laid
/// anything out — an aspect ratio for a container, or a background to clear to.
#[cfg(feature = "document-read")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocumentHints {
    /// Color the writer expected the scene to be rasterised over.
    pub background: Option<crate::color::Color>,
    /// Width and height, in points, the writer rendered at.
    pub size: Option<(f64, f64)>,
    /// Dots per inch the writer rendered at.
    pub dpi: Option<f64>,
}

/// Read just the hints a document carries, without rebuilding it.
///
/// Decodes only the head, so this is cheap enough to call before deciding
/// what size to render at. [`read_composition`] is what builds the plot.
#[cfg(feature = "document-read")]
pub fn read_hints(bytes: &[u8]) -> Result<DocumentHints, DocumentError> {
    let chunks = wire::parse(bytes)?;
    let body = wire::chunk(&chunks, wire::CHUNK_HEAD)
        .ok_or(DocumentError::MissingChunk { tag: "HEAD" })?;
    decode_head(body, read::default_context()).map(|(_, hints)| hints)
}

/// Decode the head chunk: the root composition's id, plus the hints.
#[cfg(feature = "document-read")]
fn decode_head(body: &[u8], ctx: &ReadContext) -> Result<(String, DocumentHints), DocumentError> {
    use codec::{Decode, Reader};

    let mut r = Reader::with_context(body, ctx);
    let root_id = String::decode(&mut r)?;
    let hints = DocumentHints {
        background: Option::<crate::color::Color>::decode(&mut r)?,
        size: Option::<(f64, f64)>::decode(&mut r)?,
        dpi: Option::<f64>::decode(&mut r)?,
    };
    Ok((root_id, hints))
}

/// Rebuild the composition a document holds.
///
/// The result is a live [`PlotComposition`](crate::plot::PlotComposition):
/// call `render` on it at whatever size the output happens to be, and it
/// solves its layout and shapes its text for that size, exactly as the
/// composition that was captured would have.
#[cfg(feature = "document-read")]
pub fn read_composition(
    bytes: &[u8],
    ctx: &ReadContext,
) -> Result<crate::plot::PlotComposition, DocumentError> {
    use codec::{Decode, Reader};

    let chunks = wire::parse(bytes)?;
    let mut tables = intern::ReadTables::default();

    /// A chunk the format requires. Absence is a corrupt document, not
    /// an older one — every required chunk has existed since version 1.
    fn required<'a>(
        chunks: &[wire::Chunk<'a>],
        tag: &'static [u8; 4],
    ) -> Result<&'a [u8], DocumentError> {
        wire::chunk(chunks, tag).ok_or(DocumentError::MissingChunk {
            tag: std::str::from_utf8(tag).unwrap_or("????"),
        })
    }

    // Fonts first of all: registration has to happen before anything
    // shapes, and `Theme` decoding is the first thing that could. A
    // document written with `embed_fonts` off simply has none.
    if let Some(body) = wire::chunk(&chunks, wire::CHUNK_FONTS) {
        let mut r = Reader::with_context(body, ctx);
        fonts::register(&fonts::EmbeddedFonts::decode(&mut r)?);
    }

    // Tables next: everything after them may hold references.
    {
        let body = required(&chunks, wire::CHUNK_STRINGS)?;
        let mut r = Reader::with_context(body, ctx);
        let n = r.count()?;
        let mut strings = Vec::with_capacity(n);
        for _ in 0..n {
            strings.push(std::sync::Arc::from(r.str()?));
        }
        tables.set_strings(strings);
    }
    {
        let body = required(&chunks, wire::CHUNK_GEOMETRY)?;
        let mut r = Reader::with_tables(body, ctx, tables.clone());
        let n = r.count()?;
        let mut geometries = Vec::with_capacity(n);
        for _ in 0..n {
            geometries.push(std::sync::Arc::new(
                crate::scales::geometry::Geometry::decode(&mut r)?,
            ));
        }
        tables.set_geometries(geometries);
    }
    {
        let body = required(&chunks, wire::CHUNK_SHEETS)?;
        let mut r = Reader::with_tables(body, ctx, tables.clone());
        let n = r.count()?;
        let mut sheets = Vec::with_capacity(n);
        for _ in 0..n {
            sheets.push(std::sync::Arc::new(impls_theme::decode_sheet(&mut r)?));
        }
        tables.set_sheets(sheets);
    }

    // Hints are advisory here; `read_hints` is how a caller asks for them.
    let (root_id, _hints) = decode_head(required(&chunks, wire::CHUNK_HEAD)?, ctx)?;

    let theme = {
        let body = required(&chunks, wire::CHUNK_THEME)?;
        let mut r = Reader::with_tables(body, ctx, tables.clone());
        crate::plot::theme::Theme::decode(&mut r)?
    };
    let scales = {
        let body = required(&chunks, wire::CHUNK_SCALES)?;
        let mut r = Reader::with_tables(body, ctx, tables.clone());
        crate::plot::ScaleRegistry::decode(&mut r)?
    };

    let (template, chrome, chrome_order) = {
        let body = required(&chunks, wire::CHUNK_COMPOSITION)?;
        let mut r = Reader::with_tables(body, ctx, tables.clone());
        let template = crate::plot::composition::CompositionTemplate::decode(&mut r)?;
        let n = r.count()?;
        let mut chrome = std::collections::HashMap::with_capacity(n);
        for _ in 0..n {
            let id = String::decode(&mut r)?;
            chrome.insert(
                id,
                crate::plot::composition::CompositionChrome::decode(&mut r)?,
            );
        }
        let order = Vec::<String>::decode(&mut r)?;
        (template, chrome, order)
    };

    // The bare composition exists only so each plot's patch id can be
    // validated the way `Plot::new` would.
    let bare = template.bare();
    let (plots, plot_order) = {
        let body = required(&chunks, wire::CHUNK_PLOTS)?;
        let mut r = Reader::with_tables(body, ctx, tables.clone());
        let n = r.count()?;
        let mut plots: std::collections::HashMap<String, Vec<crate::plot::Plot>> =
            std::collections::HashMap::with_capacity(n);
        let mut order = Vec::with_capacity(n);
        for _ in 0..n {
            let patch = String::decode(&mut r)?;
            let count = r.count()?;
            let mut list = Vec::with_capacity(count);
            for _ in 0..count {
                list.push(impls_plot::decode_plot(&mut r, &bare)?);
            }
            order.push(patch.clone());
            plots.insert(patch, list);
        }
        (plots, order)
    };

    let mut composition = crate::plot::PlotComposition::from_document(
        template,
        root_id,
        scales,
        std::sync::Arc::new(theme),
        plots,
        plot_order,
        chrome,
        chrome_order,
    );

    // Shapes come last: every registry they add to already holds the
    // built-ins, so this inserts rather than replaces. An older document
    // has no such chunk, which reads as no customisation.
    if let Some(body) = wire::chunk(&chunks, wire::CHUNK_SHAPES) {
        let mut r = Reader::with_tables(body, ctx, tables.clone());
        let embedded = shapes::EmbeddedShapes::decode(&mut r)?;
        shapes::apply(&embedded, &mut composition);
    }
    if let Some(body) = wire::chunk(&chunks, wire::CHUNK_IMAGES) {
        let mut r = Reader::with_tables(body, ctx, tables.clone());
        let embedded = images::EmbeddedImages::decode(&mut r)?;
        images::apply_if_supported(&embedded, &mut composition);
    }

    Ok(composition)
}
