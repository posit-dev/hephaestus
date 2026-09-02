//! Carrying the raster images a document's plots resolve names against.
//!
//! An [`ImageGeom`](crate::plot::ImageGeom)'s `"image"` channel holds a
//! *name*, looked up in the [`ImageRegistry`](crate::plot::ImageRegistry)
//! its plot carries. A reader has no way to reconstruct pixels, so unlike
//! [marker shapes](super::shapes) there is nothing here a consumer could
//! rebuild for itself — either the document carries the image or the
//! consumer registers it.
//!
//! Which is why this section is **off by default**, like fonts and for
//! the same reason: it is payload rather than configuration. A page
//! embedding a plot usually already serves its own art, and telling it a
//! name is cheaper than shipping the pixels twice. See
//! [`WriteOptions::embed_images`](super::WriteOptions::embed_images).
//!
//! # Images travel as PNG
//!
//! A registry holds decoded RGBA8, so whatever the author originally
//! loaded is long gone by the time it is registered and the only option
//! is to re-encode. PNG is the choice: lossless, alpha-preserving, and
//! one decoder covers every document, where passing through the author's
//! original format would mean a reader needing all four. On rendered
//! plot content it is worth having — measured between 66x and 96x
//! smaller than the raw buffer — so an embedded image is kilobytes
//! rather than megabytes.
//!
//! Photographic content compresses far less well, which is the case
//! where naming the image and letting the page supply it stays the
//! better answer.
//!
//! # Both halves need the `png` feature
//!
//! `document-read` and `document-write` are otherwise dependency-free,
//! and building a renderer-free writer on the oldest supported rustc is
//! a configuration this crate promises. So the codec is not pulled in on
//! their behalf: without `png`, a writer asked to embed images reports
//! them through
//! [`UnsupportedItem::UnembeddableImage`](super::UnsupportedItem::UnembeddableImage)
//! and a reader skips the chunk, leaving rows that name an image drawing
//! nothing. Both are the same degradation a missing font produces.

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;

#[cfg(any(feature = "document-read", feature = "document-write"))]
use crate::plot::PlotComposition;

/// One image a document carries, as PNG bytes.
///
/// Dimensions are the PNG's own, so they are not written; the decoder
/// reports them.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageEntry {
    /// The name the image is registered under.
    pub(crate) name: String,
    /// The image, PNG-encoded.
    pub(crate) png: Vec<u8>,
}

// Hand-written rather than through `impl_codec!` for the reason
// `FontFace` is: the blanket `Vec<T>` impl varints every byte, and
// roughly half a compressed stream's bytes are >= 128, each of which
// would then cost two.
#[cfg(feature = "document-write")]
impl Encode for ImageEntry {
    fn encode(&self, w: &mut Writer) {
        super::codec::write_record(w, |w| {
            self.name.encode(w);
            w.bytes(&self.png);
        });
    }
}

#[cfg(feature = "document-read")]
impl Decode for ImageEntry {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        super::codec::read_record(r, "ImageEntry", |r| {
            Ok(ImageEntry {
                name: String::decode(r)?,
                png: r.bytes()?.to_vec(),
            })
        })
    }
}

/// The images one plot carries, addressed the way the reader rebuilds
/// plots: patch id plus position within that patch.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlotImages {
    pub(crate) patch: String,
    pub(crate) index: u32,
    pub(crate) entries: Vec<ImageEntry>,
}

impl_codec! {
    record PlotImages { patch, index, entries }
}

/// Every image a document carries: the composition's own registry,
/// which backs chrome that belongs to the composition rather than to a
/// plot, plus one list per plot.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EmbeddedImages {
    pub(crate) composition: Vec<ImageEntry>,
    pub(crate) plots: Vec<PlotImages>,
}

impl_codec! {
    record EmbeddedImages { composition, plots }
}

/// Collect and encode every image the composition's plots hold.
///
/// Sorted by name, so the same registry writes the same bytes whatever
/// order it was filled in. An image the encoder refuses is skipped —
/// `unembeddable` is what reports it.
#[cfg(all(feature = "document-write", feature = "png"))]
pub(crate) fn collect(comp: &PlotComposition) -> EmbeddedImages {
    let mut plots = Vec::new();
    for (patch, index, registry) in plot_registries(comp) {
        let entries = entries_of(registry);
        if !entries.is_empty() {
            plots.push(PlotImages {
                patch,
                index,
                entries,
            });
        }
    }
    EmbeddedImages {
        composition: entries_of(comp.image_registry_ref()),
        plots,
    }
}

/// Encode every image one registry can hand out.
///
/// Sorted by name, so the same registry writes the same bytes whatever
/// order it was filled in. An image the encoder refuses is skipped —
/// `unembeddable` is what reports it.
#[cfg(all(feature = "document-write", feature = "png"))]
fn entries_of(registry: &crate::image_registry::ImageRegistry) -> Vec<ImageEntry> {
    let mut entries = Vec::new();
    for name in carried_names(registry) {
        let Some(image) = registry.resolve(&name) else {
            continue;
        };
        if let Some(png) = encode(&image) {
            entries.push(ImageEntry { name, png });
        }
    }
    entries
}

/// Whether this build can put `image` on the wire.
///
/// The predicate is the writer's own buffer contract, which a registry
/// image built through
/// [`from_rgba8`](crate::image::from_rgba8) already satisfies — so a
/// `false` here means the registry was filled by hand with something the
/// encoder would reject.
#[cfg(all(feature = "document-write", feature = "png"))]
fn embeddable(image: &crate::brush::Image) -> bool {
    image.format == crate::brush::ImageFormat::Rgba8
        && image.width > 0
        && image.height > 0
        && image.data.as_ref().len() as u128
            == u128::from(image.width) * u128::from(image.height) * 4
}

/// No codec, nothing embeddable.
#[cfg(all(feature = "document-write", not(feature = "png")))]
fn embeddable(_image: &crate::brush::Image) -> bool {
    false
}

/// Re-encode a registry image as PNG, or `None` if it cannot be.
///
/// Gated on [`embeddable`] rather than on the encoder's own error, so
/// the set of images this skips is exactly the set `unembeddable`
/// reports.
#[cfg(all(feature = "document-write", feature = "png"))]
fn encode(image: &crate::brush::Image) -> Option<Vec<u8>> {
    if !embeddable(image) {
        return None;
    }
    // No dpi: a payload the reader hands straight back as pixels.
    crate::image::encode_png(image.width, image.height, image.data.as_ref(), None).ok()
}

/// Every name a register can hand pixels for, sorted so the same
/// contents write the same bytes whatever order they arrived in.
///
/// Registered entries plus the locations [`ImageRegistry::resolve`]
/// has already read: a markdown `![](logo.png)` registers nothing, so
/// carrying only the registered names would leave a reader without a
/// filesystem — a page rebuilding the document — with a broken image
/// where the writer had a picture.
#[cfg(feature = "document-write")]
fn carried_names(registry: &crate::image_registry::ImageRegistry) -> Vec<String> {
    let mut names: Vec<String> = registry.names().map(str::to_string).collect();
    names.extend(registry.loaded_names());
    names.sort_unstable();
    names.dedup();
    names
}

/// Every plot's image registry, with the address the reader will use to
/// find that plot again: patch id plus position within the patch.
///
/// The composition's own registry is not here — it is addressed by a
/// field of its own on [`EmbeddedImages`] rather than by a plot address
/// it would have to fake.
#[cfg(feature = "document-write")]
fn plot_registries(
    comp: &PlotComposition,
) -> Vec<(String, u32, &crate::plot::image_registry::ImageRegistry)> {
    let mut patches: Vec<&str> = comp.plots().map(|(id, _)| id).collect();
    patches.sort_unstable();
    patches.dedup();
    let mut out = Vec::new();
    for patch in patches {
        for (index, plot) in comp.plots_in(patch).iter().enumerate() {
            let registry = plot.image_registry_ref();
            if is_empty(registry) {
                continue;
            }
            out.push((patch.to_string(), index as u32, registry));
        }
    }
    out
}

/// Whether a register can hand out no pixels at all — nothing
/// registered and nothing read from a location.
#[cfg(feature = "document-write")]
fn is_empty(registry: &crate::image_registry::ImageRegistry) -> bool {
    registry.is_empty() && registry.loaded_names().is_empty()
}

/// The images a composition names but this build cannot carry, for
/// [`unsupported_items`](super::write::unsupported_items).
///
/// Only reachable with the `png` feature off — with it on, every image a
/// registry can legally hold encodes.
#[cfg(feature = "document-write")]
pub(crate) fn unembeddable(comp: &PlotComposition) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut report = |patch: &str, registry: &crate::image_registry::ImageRegistry| {
        for name in carried_names(registry) {
            let Some(image) = registry.resolve(&name) else {
                continue;
            };
            if embeddable(&image) {
                continue;
            }
            out.push((patch.to_string(), name));
        }
    };
    // Empty patch for the composition's own registry, matching how
    // `shapes::unnameable` reports one.
    report("", comp.image_registry_ref());
    for (patch, _, registry) in plot_registries(comp) {
        report(&patch, registry);
    }
    out
}

/// Decode every carried image into the registry that named it.
///
/// Each entry decodes once, so every plot naming it shares one blob —
/// which is what a backend's texture cache keys on, so sharing the
/// handle is one upload rather than several. An image that fails to
/// decode is skipped, leaving rows that name it drawing nothing.
#[cfg(all(feature = "document-read", feature = "png"))]
pub(crate) fn apply(embedded: &EmbeddedImages, comp: &mut PlotComposition) {
    for entry in &embedded.composition {
        if let Ok(image) = crate::image::decode_png(&entry.png) {
            comp.image_registry_mut().insert(entry.name.clone(), image);
        }
    }
    for plot in &embedded.plots {
        let decoded: Vec<(String, crate::brush::Image)> = plot
            .entries
            .iter()
            .filter_map(|e| {
                crate::image::decode_png(&e.png)
                    .ok()
                    .map(|img| (e.name.clone(), img))
            })
            .collect();
        if decoded.is_empty() {
            continue;
        }
        comp.update_plot_at(&plot.patch, plot.index as usize, |p| {
            for (name, image) in decoded {
                p.image_registry_mut().insert(name, image);
            }
        });
    }
}

/// [`collect`] when the caller asked for it, an empty table otherwise.
///
/// The chunk is written either way, so a document's byte layout does not
/// depend on which features built the writer — only its contents do.
#[cfg(all(feature = "document-write", feature = "png"))]
pub(crate) fn collect_if(comp: &PlotComposition, embed: bool) -> EmbeddedImages {
    if embed {
        collect(comp)
    } else {
        EmbeddedImages::default()
    }
}

/// Without a codec there is nothing to collect; `unembeddable` is what
/// tells the caller their images went nowhere.
#[cfg(all(feature = "document-write", not(feature = "png")))]
pub(crate) fn collect_if(_comp: &PlotComposition, _embed: bool) -> EmbeddedImages {
    EmbeddedImages::default()
}

/// [`apply`] when this build can decode.
#[cfg(all(feature = "document-read", feature = "png"))]
pub(crate) fn apply_if_supported(embedded: &EmbeddedImages, comp: &mut PlotComposition) {
    apply(embedded, comp);
}

/// Without a codec the section is skipped, leaving rows that name an
/// image drawing nothing — the same degradation a missing font gives.
#[cfg(all(feature = "document-read", not(feature = "png")))]
pub(crate) fn apply_if_supported(_embedded: &EmbeddedImages, _comp: &mut PlotComposition) {}
