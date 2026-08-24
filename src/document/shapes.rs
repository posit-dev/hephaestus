//! Carrying the marker shapes a document's plots resolve names against.
//!
//! A geom's `"shape"` / `"*_marker"` channel holds a *name*, looked up in
//! the [`ShapeRegistry`] its plot carries. Every reader reconstructs the
//! built-ins for itself, so a document that only uses those needs to say
//! nothing — which is the common case, and why this section is usually
//! empty. What has to travel is the delta: an entry whose name is not a
//! built-in, or one that replaces a built-in with something else.
//!
//! Unlike fonts, this is cheap enough to be unconditional. A custom
//! shape is a handful of Bézier subpaths — tens of bytes each — so there
//! is no `embed_shapes` flag to weigh, and a document that customises
//! nothing pays two varints.
//!
//! # Glyph-backed shapes travel as text
//!
//! A [`Shape`] can be a single font glyph rather than subpaths. Live, it
//! holds a resolved face and glyph id, because the draw path must not
//! shape per frame. Neither survives the trip: a glyph id means nothing
//! outside its own face, and a family name resolves to different faces
//! on different machines, so an id written here would index some
//! arbitrary other glyph on the reader's side — the worst kind of
//! failure, one that renders something plausible.
//!
//! So a glyph shape travels the way all this crate's text does, as a
//! source string plus a style descriptor
//! ([`GlyphSource`](crate::shape::GlyphSource)), and the reader re-runs
//! [`try_glyph_marker`](crate::text::try_glyph_marker) on them. That
//! puts glyph shapes under the same condition as every label in the
//! document: they need the family present. Where it is missing, or where
//! the reader's font does not ligate a multi-codepoint sequence, the
//! entry is dropped and rows naming it draw nothing — the same
//! degradation an unresolved name has always produced.

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;

use crate::geometry::Point;
use crate::path::Path;
#[cfg(feature = "document-write")]
use crate::plot::PlotComposition;
use crate::shape::ShapeStyle;
#[cfg(feature = "document-write")]
use crate::shape::{Shape, ShapeKind, ShapeRegistry};
use crate::text::TextStyle;

impl_codec! {
    enum ShapeStyle {
        0 => Stroke,
        1 => Fill,
    }
}

/// A shape in the form the wire carries it.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WireShape {
    /// Subpaths and a style hint, verbatim. The bounding box is derived
    /// at construction, so it is not written.
    Paths {
        paths: Vec<Path>,
        style: ShapeStyle,
        anchor: Point,
    },
    /// The text and style a glyph was resolved from, re-resolved on
    /// load. The anchor is carried because a caller may have overridden
    /// the one `glyph_marker` chooses.
    Glyph {
        text: String,
        style: TextStyle,
        anchor: Point,
    },
}

impl_codec! {
    enum WireShape {
        0 => Paths { paths, style, anchor },
        1 => Glyph { text, style, anchor },
    }
}

/// One registry entry a document carries.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ShapeEntry {
    /// The name the shape is registered under.
    pub(crate) name: String,
    pub(crate) shape: WireShape,
}

impl_codec! {
    struct ShapeEntry { name, shape }
}

/// The shape deltas one plot carries, addressed the way the reader
/// rebuilds plots: patch id plus position within that patch.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlotShapes {
    pub(crate) patch: String,
    pub(crate) index: u32,
    pub(crate) entries: Vec<ShapeEntry>,
}

impl_codec! {
    struct PlotShapes { patch, index, entries }
}

/// Every shape delta in a document: the composition's own registry,
/// which backs composition-level legend keys, plus one list per plot.
#[cfg(any(feature = "document-read", feature = "document-write"))]
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct EmbeddedShapes {
    pub(crate) composition: Vec<ShapeEntry>,
    pub(crate) plots: Vec<PlotShapes>,
}

impl_codec! {
    struct EmbeddedShapes { composition, plots }
}

/// Reduce a live shape to its wire form, or `None` when it cannot be
/// expressed.
///
/// The one unexpressible case is a glyph shape built straight from a
/// resolved face through [`Shape::glyph`], which carries no source text
/// to re-resolve from. `glyph_marker` — the way marker glyphs are
/// actually made — always supplies one.
#[cfg(feature = "document-write")]
pub(crate) fn to_wire(shape: &Shape) -> Option<WireShape> {
    match shape.kind() {
        ShapeKind::Paths { paths, style } => Some(WireShape::Paths {
            paths: paths.to_vec(),
            style,
            anchor: shape.anchor(),
        }),
        ShapeKind::Glyph { .. } => {
            let source = shape.glyph_source()?;
            Some(WireShape::Glyph {
                text: source.text.clone(),
                style: source.style.clone(),
                anchor: shape.anchor(),
            })
        }
    }
}

/// Rebuild a live shape from its wire form, or `None` when the reader
/// cannot resolve it.
#[cfg(feature = "document-read")]
pub(crate) fn from_wire(wire: &WireShape) -> Option<crate::shape::Shape> {
    match wire {
        WireShape::Paths {
            paths,
            style,
            anchor,
        } => Some(crate::shape::Shape::new(paths.clone(), *style, *anchor)),
        WireShape::Glyph {
            text,
            style,
            anchor,
        } => Some(crate::text::try_glyph_marker(text, style)?.with_anchor(*anchor)),
    }
}

/// The entries of `registry` that differ from what a reader rebuilds on
/// its own.
///
/// Compared against a fresh built-in registry rather than against the
/// name list, so replacing `"circle"` with something else travels while
/// leaving it alone does not. Sorted by name, so the same registry
/// writes the same bytes whatever order it was filled in.
#[cfg(feature = "document-write")]
fn delta(registry: &ShapeRegistry) -> Vec<ShapeEntry> {
    let builtins = ShapeRegistry::shared_builtins();
    let mut names: Vec<&str> = registry.names().collect();
    names.sort_unstable();
    let mut out = Vec::new();
    for name in names {
        let shape = match registry.get(name) {
            Some(s) => s,
            None => continue,
        };
        if builtins.get(name) == Some(shape) {
            continue;
        }
        if let Some(wire) = to_wire(shape) {
            out.push(ShapeEntry {
                name: name.to_string(),
                shape: wire,
            });
        }
    }
    out
}

/// Collect every shape delta a composition carries.
#[cfg(feature = "document-write")]
pub(crate) fn collect(comp: &PlotComposition) -> EmbeddedShapes {
    let mut plots = Vec::new();
    let mut patches: Vec<&str> = comp.plots().map(|(id, _)| id).collect();
    patches.sort_unstable();
    patches.dedup();
    for patch in patches {
        for (index, plot) in comp.plots_in(patch).iter().enumerate() {
            let entries = delta(plot.shape_registry_ref());
            if entries.is_empty() {
                continue;
            }
            plots.push(PlotShapes {
                patch: patch.to_string(),
                index: index as u32,
                entries,
            });
        }
    }
    EmbeddedShapes {
        composition: delta(comp.shape_registry_ref()),
        plots,
    }
}

/// A shape a document names but cannot carry, for
/// [`unsupported_items`](super::write::unsupported_items).
///
/// Only a glyph shape with no source text lands here; see [`to_wire`].
#[cfg(feature = "document-write")]
pub(crate) fn unnameable(comp: &PlotComposition) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut report = |patch: &str, registry: &ShapeRegistry| {
        let builtins = ShapeRegistry::shared_builtins();
        let mut names: Vec<&str> = registry.names().collect();
        names.sort_unstable();
        for name in names {
            let Some(shape) = registry.get(name) else {
                continue;
            };
            if builtins.get(name) == Some(shape) {
                continue;
            }
            if to_wire(shape).is_none() {
                out.push((patch.to_string(), name.to_string()));
            }
        }
    };
    report("", comp.shape_registry_ref());
    let mut patches: Vec<&str> = comp.plots().map(|(id, _)| id).collect();
    patches.sort_unstable();
    patches.dedup();
    for patch in patches {
        for plot in comp.plots_in(patch) {
            report(patch, plot.shape_registry_ref());
        }
    }
    out
}

/// Insert every carried shape into the registry that named it.
///
/// Inserts rather than replaces: each registry already holds the
/// built-ins the reader rebuilt, and a carried entry either adds a name
/// or overrides one of those. An entry the reader cannot resolve — a
/// glyph whose family is absent, or a sequence its fonts do not ligate —
/// is skipped, leaving rows that name it drawing nothing.
#[cfg(feature = "document-read")]
pub(crate) fn apply(embedded: &EmbeddedShapes, comp: &mut crate::plot::PlotComposition) {
    for entry in &embedded.composition {
        if let Some(shape) = from_wire(&entry.shape) {
            comp.shape_registry_mut().insert(entry.name.clone(), shape);
        }
    }
    for plot in &embedded.plots {
        let rebuilt: Vec<(String, crate::shape::Shape)> = plot
            .entries
            .iter()
            .filter_map(|e| from_wire(&e.shape).map(|s| (e.name.clone(), s)))
            .collect();
        if rebuilt.is_empty() {
            continue;
        }
        comp.update_plot_at(&plot.patch, plot.index as usize, |p| {
            for (name, shape) in rebuilt {
                p.shape_registry_mut().insert(name, shape);
            }
        });
    }
}
