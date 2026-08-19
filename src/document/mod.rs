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
pub(crate) mod impls_core;
pub(crate) mod impls_scale;
pub(crate) mod impls_theme;
pub(crate) mod intern;
#[cfg(feature = "document-read")]
mod read;

#[cfg(feature = "document-read")]
pub use read::{GeomFactory, ReadContext};

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
