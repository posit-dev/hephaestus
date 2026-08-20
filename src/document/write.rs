//! The write half: options, the validation pass, and assembly.
//!
//! Encoding is mechanical and infallible; deciding whether a plot *can*
//! be written is a separate pass that runs first. Keeping them apart is
//! what lets [`Encode`](super::codec::Encode) return nothing to check,
//! and it means a refusal names every problem at once instead of
//! stopping at the first.

use crate::color::Color;
use crate::layout::{Extent, Inset, Track};
use crate::plot::{FormatSpec, PlotComposition};

/// Something in a plot that a document can't carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedItem {
    /// A scale carries an anonymous formatter closure. Give it a name
    /// with
    /// [`Scale::with_named_format`](crate::plot::Scale::with_named_format)
    /// so a reader can resolve it, or accept the default labels.
    CustomFormatter {
        /// Name the scale is registered under.
        scale: String,
    },

    /// A geom returns `None` from [`Geom::kind`](crate::plot::Geom::kind),
    /// so nothing identifies which constructor should rebuild it.
    UnnameableGeom {
        /// Patch the plot is bound to.
        patch: String,
        /// Position in the plot's draw order.
        index: usize,
    },

    /// A track or inset is sized relative to another grid's track. The
    /// reference is a `CellId` the layout solver hands out per solve, so
    /// it means nothing in another process.
    TrackReference {
        /// Where the reference was found.
        location: String,
    },
}

impl std::fmt::Display for UnsupportedItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CustomFormatter { scale } => write!(
                f,
                "scale {scale:?} has an anonymous label formatter; name it with \
                 `with_named_format` so a reader can resolve it"
            ),
            Self::UnnameableGeom { patch, index } => write!(
                f,
                "geom {index} on patch {patch:?} does not implement `Geom::kind`, \
                 so nothing identifies how to rebuild it"
            ),
            Self::TrackReference { location } => write!(
                f,
                "{location} is sized relative to another grid's track, which is \
                 identified by a per-solve id and cannot be carried"
            ),
        }
    }
}

/// How a document is written.
#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// Degrade what can't be carried instead of refusing.
    ///
    /// An anonymous formatter is dropped to default labels, an
    /// unnameable geom is omitted, a track reference is replaced with an
    /// `Auto` track. Off by default: silently changing a plot is worse
    /// than saying what's wrong.
    pub lossy: bool,

    /// Background colour a consumer should render behind the plot.
    ///
    /// Not part of a `PlotComposition` — it's an argument to
    /// [`Renderer::render_to_buffer`](crate::Renderer::render_to_buffer)
    /// — so it travels as a hint.
    pub background: Option<Color>,

    /// Canvas size a consumer should use when it has no size of its own.
    ///
    /// A hint only. The whole point of the format is that any size
    /// works.
    pub size_hint: Option<(f64, f64)>,

    /// Dots per inch a consumer should render at when it has no display
    /// to ask.
    pub dpi_hint: Option<f64>,

    /// Embed the font files the plot's text needs. **Off** by default.
    ///
    /// The reader re-shapes rather than replaying glyphs, so it has to
    /// resolve the same faces — and a browser's font context starts
    /// empty, which is the case embedding exists for. It is off by
    /// default anyway, because a system family is not small: the
    /// four-panel plot in `tests/document_roundtrip.rs` is about 10 kB,
    /// and embedding the one family it names takes it past 2 MB, since
    /// macOS resolves `sans-serif` to a 2.4 MB Helvetica collection.
    ///
    /// A consumer that needs fonts is usually better served registering
    /// them itself with [`register_font_bytes`] and
    /// [`set_generic_family`] — a website already serves a subsetted web
    /// font far smaller than any system family. Turn this on when
    /// self-containment matters more than size, or when the plot names a
    /// family that is genuinely small.
    ///
    /// [`register_font_bytes`]: crate::text::register_font_bytes
    /// [`set_generic_family`]: crate::text::set_generic_family
    pub embed_fonts: bool,
}

impl WriteOptions {
    /// Defaults: strict, with no render hints.
    pub fn new() -> Self {
        Self::default()
    }

    /// Degrade unsupported items instead of refusing. See
    /// [`Self::lossy`].
    pub fn lossy(mut self, lossy: bool) -> Self {
        self.lossy = lossy;
        self
    }

    /// Record the background colour a consumer should use.
    pub fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    /// Record the canvas size a consumer should default to.
    pub fn size_hint(mut self, width: f64, height: f64) -> Self {
        self.size_hint = Some((width, height));
        self
    }

    /// Record the dpi a consumer should default to.
    pub fn dpi_hint(mut self, dpi: f64) -> Self {
        self.dpi_hint = Some(dpi);
        self
    }

    /// Whether to embed the plot's font files. See
    /// [`Self::embed_fonts`].
    pub fn embed_fonts(mut self, embed: bool) -> Self {
        self.embed_fonts = embed;
        self
    }
}

/// Everything about `comp` that a document can't carry.
///
/// Empty means the plot writes losslessly.
pub fn unsupported_items(comp: &PlotComposition) -> Vec<UnsupportedItem> {
    let mut out = Vec::new();

    for (name, scale) in comp.scales.iter() {
        if scale.format_spec() == FormatSpec::Custom {
            out.push(UnsupportedItem::CustomFormatter {
                scale: name.to_string(),
            });
        }
    }

    for patch in &comp.plot_order {
        for plot in comp.plots.get(patch).into_iter().flatten() {
            for (index, (_, geom)) in plot.geoms().enumerate() {
                if geom.kind().is_none() {
                    out.push(UnsupportedItem::UnnameableGeom {
                        patch: patch.clone(),
                        index,
                    });
                }
            }
        }
    }

    check_template_tracks(&comp.template, &mut out);
    out
}

/// Walk a template's tracks and insets for cross-grid references.
fn check_template_tracks(
    template: &crate::plot::composition::CompositionTemplate,
    out: &mut Vec<UnsupportedItem>,
) {
    let id = template.id.as_deref().unwrap_or("<unnamed>");
    for (axis, tracks) in [("width", &template.widths), ("height", &template.heights)] {
        for (i, track) in tracks.iter().enumerate() {
            if let Track::Fixed(e) = track {
                if extent_has_track_ref(e) {
                    out.push(UnsupportedItem::TrackReference {
                        location: format!("composition {id:?} {axis} track {i}"),
                    });
                }
            }
        }
    }
    for (what, inset) in [("margin", &template.margin), ("padding", &template.padding)] {
        if inset_has_track_ref(inset) {
            out.push(UnsupportedItem::TrackReference {
                location: format!("composition {id:?} {what}"),
            });
        }
    }
    for placement in &template.placements {
        if let crate::plot::composition::ElementTemplate::Composition(nested) = &placement.element {
            check_template_tracks(nested, out);
        }
    }
}

fn extent_has_track_ref(e: &Extent) -> bool {
    match e {
        Extent::TrackOf { .. } => true,
        Extent::Min(a, b) | Extent::Max(a, b) => extent_has_track_ref(a) || extent_has_track_ref(b),
        Extent::Sum { .. } => false,
    }
}

fn inset_has_track_ref(i: &Inset) -> bool {
    [&i.left, &i.right, &i.top, &i.bottom, &i.width, &i.height]
        .into_iter()
        .flatten()
        .any(extent_has_track_ref)
}
