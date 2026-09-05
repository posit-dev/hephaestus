//! Codec for the plot layer: projections, axis and legend specs, the
//! layout extents a composition's tracks are made of, the composition
//! template, and the plots themselves.
//!
//! Three shapes of impl appear here, in ascending order of care:
//!
//! - **Plain data with reachable fields** — projections, legend specs,
//!   layout extents. One [`impl_codec`] line each. `Legend`'s fields are
//!   `pub(crate)`, which is enough from inside the crate.
//! - **Encapsulated types** — [`PolarProjection`] and
//!   [`Axis`](crate::plot::Axis) keep their fields private, so they are
//!   read through accessors and rebuilt through builders, exactly as
//!   [`Scale`](crate::plot::Scale) is in [`super::impls_scale`].
//! - **Aggregates** — a `Plot` is reassembled by replaying the calls
//!   that built it. Its geoms come back through the kind tag and the
//!   factory table in [`super::ReadContext`].
//!
//! One deliberate lossiness: `GeomId` / `AxisId` / `LegendId` are not
//! carried. They are handles for later `update_geom`-style calls, not
//! anything drawing depends on — draw order is vector order, which is
//! preserved — and a handle can't outlive the process that issued it
//! anyway. Replaying through `add_geom` / `add_axis` / `add_legend`
//! renumbers from zero, which differs from the original only where the
//! original had gaps.

use std::collections::HashMap;

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;
use crate::layout::Axis as LayoutAxis;
use crate::layout::{Extent, Inset, Placement, Track};
use crate::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use crate::plot::chrome::legend::{
    AestheticSource, BinSpacing, ColorbarSpec, Legend, LegendBody, LegendKey, LegendKeySpec,
    StackBody,
};
use crate::plot::geom::Channel;
use crate::plot::projection::{
    ChromeStrategy, CustomProjection, PolarEdgeStyle, PolarProjection, Projection,
};
use crate::scales::chrome::{Anchor, AxisSide, LegendSide};

// ─── Layout extents ──────────────────────────────────────────────────────────

impl_codec! {
    enum LayoutAxis {
        0 => Width,
        1 => Height,
    }

    enum Track {
        0 => Fixed(extent),
        1 => Fr(share),
        2 => Auto,
    }

    record Inset { left, right, top, bottom, width, height }
    record Placement { row, col, row_span, col_span, inset }
}

// A cross-grid track reference is the one `Extent` a document cannot
// express. `grid` is a `CellId` the layout solver hands out per solve, so
// it names nothing outside the process that allocated it — which is why
// `write::check_template_tracks` refuses a template carrying one. The
// references that drive alignment are not affected: `composition/build.rs`
// generates those while lowering a template to a grid, so they are rebuilt
// on load and never travel.
//
// The wire form is kept, and kept *symbolic*: whatever makes a reference
// portable will name its target, the way a composition template already
// names itself, so a string is the shape that survives that design. A
// varint `CellId` would not, which is what made the previous form a
// promise it could not keep.
//
// Both directions are deliberately inert rather than lossy. Encoding
// writes an empty name, which the validation pass has already made
// unreachable; decoding refuses instead of fabricating a `CellId` from a
// number, since a fabricated one indexes an arbitrary other grid and
// misplaces every track that reads it.
#[cfg(feature = "document-write")]
impl Encode for Extent {
    fn encode(&self, w: &mut Writer) {
        match self {
            Extent::Sum {
                px,
                inches,
                percent,
            } => {
                w.varint(0);
                px.encode(w);
                inches.encode(w);
                percent.encode(w);
            }
            Extent::Min(a, b) => {
                w.varint(1);
                a.encode(w);
                b.encode(w);
            }
            Extent::Max(a, b) => {
                w.varint(2);
                a.encode(w);
                b.encode(w);
            }
            Extent::TrackOf {
                grid: _,
                axis,
                track,
                span,
            } => {
                w.varint(3);
                String::new().encode(w);
                axis.encode(w);
                track.encode(w);
                span.encode(w);
            }
        }
    }
}

#[cfg(feature = "document-read")]
impl Decode for Extent {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let offset = r.pos();
        Ok(match r.varint()? {
            0 => Extent::Sum {
                px: f64::decode(r)?,
                inches: f64::decode(r)?,
                percent: f64::decode(r)?,
            },
            1 => Extent::Min(Box::decode(r)?, Box::decode(r)?),
            2 => Extent::Max(Box::decode(r)?, Box::decode(r)?),
            3 => {
                let name = String::decode(r)?;
                LayoutAxis::decode(r)?;
                u16::decode(r)?;
                u16::decode(r)?;
                return Err(DocumentError::Invalid {
                    what: "track reference",
                    why: format!(
                        "extent references tracks on grid {name:?}, which this build cannot \
                         resolve to a live grid"
                    ),
                });
            }
            other => {
                return Err(DocumentError::BadDiscriminant {
                    type_name: "Extent",
                    tag: other,
                    offset,
                })
            }
        })
    }
}

// ─── Projections ─────────────────────────────────────────────────────────────

impl_codec! {
    enum ChromeStrategy {
        0 => PatchSlots,
        1 => InsidePanel,
    }

    enum PolarEdgeStyle {
        0 => Geodesic,
        1 => Chord,
    }

    record CustomProjection { outline, x_major, x_minor, y_major, y_minor, x_channel, y_channel }

    enum Projection {
        0 => Cartesian,
        1 => Polar(p),
        2 => Custom(p),
    }
}

// `PolarProjection` keeps its fields private; every one has both an
// accessor and a builder, so it round-trips through the same calls that
// would have configured it by hand — including whatever clamping those
// builders apply.
#[cfg(feature = "document-write")]
impl Encode for PolarProjection {
    fn encode(&self, w: &mut Writer) {
        super::codec::write_record(w, |w| {
            self.angle_channel().encode(w);
            self.radius_channel().encode(w);
            self.theta_start().encode(w);
            self.theta_end().encode(w);
            self.inner_radius_frac().encode(w);
            self.outer_radius_frac().encode(w);
            self.edge_style().encode(w);
            self.theta_break_fracs().to_vec().encode(w);
            self.is_fit_to_bbox().encode(w);
        });
    }
}

#[cfg(feature = "document-read")]
impl Decode for PolarProjection {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        super::codec::read_record(r, "PolarProjection", |r| {
            let angle = String::decode(r)?;
            let radius = String::decode(r)?;
            let theta_start = f64::decode(r)?;
            let theta_end = f64::decode(r)?;
            let inner = f64::decode(r)?;
            let outer = f64::decode(r)?;
            let edges = PolarEdgeStyle::decode(r)?;
            let breaks = Vec::<f64>::decode(r)?;
            let fit = bool::decode(r)?;
            Ok(PolarProjection::full_circle()
                .channels(angle, radius)
                .theta_range(theta_start, theta_end)
                .inner_radius(inner)
                .outer_radius(outer)
                .edges(edges)
                .theta_breaks(breaks)
                .fit_to_bbox(fit))
        })
    }
}

// ─── Axis chrome ─────────────────────────────────────────────────────────────

impl_codec! {
    enum AxisSide {
        0 => Left,
        1 => Right,
        2 => Bottom,
        3 => Top,
    }

    enum PolarRing {
        0 => Outer,
        1 => Inner,
    }

    enum AxisPlacement {
        0 => Cartesian(side),
        1 => PolarRadius { theta_frac },
        2 => PolarAngular(ring),
    }
}

// An axis is either a rail or title-only, which is what the two
// constructors express; the private fields are reached through the
// matching accessors.
#[cfg(feature = "document-write")]
impl Encode for Axis {
    fn encode(&self, w: &mut Writer) {
        super::codec::write_record(w, |w| {
            self.scale_name().map(str::to_string).encode(w);
            self.placement().encode(w);
            self.title_ref().map(str::to_string).encode(w);
        });
    }
}

#[cfg(feature = "document-read")]
impl Decode for Axis {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        super::codec::read_record(r, "Axis", |r| {
            let scale_name = Option::<String>::decode(r)?;
            let placement = AxisPlacement::decode(r)?;
            let title = Option::<String>::decode(r)?;
            let axis = match (scale_name, &title) {
                (Some(name), _) => Axis::rail(name, placement),
                // A rail-less axis still reserves its title slot, and
                // `title_only` is the only way to build one.
                (None, Some(t)) => Axis::title_only(t.clone(), placement),
                (None, None) => Axis::title_only(String::new(), placement),
            };
            Ok(match title {
                Some(t) => axis.title(t),
                None => axis,
            })
        })
    }
}

// ─── Legends ─────────────────────────────────────────────────────────────────

impl_codec! {
    enum Anchor {
        0 => TopLeft,
        1 => TopCenter,
        2 => TopRight,
        3 => CenterLeft,
        4 => Center,
        5 => CenterRight,
        6 => BottomLeft,
        7 => BottomCenter,
        8 => BottomRight,
    }

    enum LegendSide {
        0 => Left,
        1 => Right,
        2 => Top,
        3 => Bottom,
        4 => InPanel { anchor, inset_pt },
    }

    enum LegendKey {
        0 => Point,
        1 => Line,
        2 => Rect,
        3 => Text,
    }

    enum AestheticSource {
        0 => Scaled(scale_name),
        1 => Fixed(value),
    }

    record LegendKeySpec { kind, bindings }
    record StackBody { keys, binned }
    record ColorbarSpec { samples, stepped, bindings }

    enum LegendBody {
        0 => Stack(body),
        1 => Colorbar(spec),
    }

    enum BinSpacing {
        0 => Proportional,
        1 => Equal,
    }

    record Legend {
        side,
        title,
        domain_scale,
        body,
        open_lower,
        open_upper,
        bin_spacing,
        theme_variant,
        merge,
    }
}

// ─── Geoms ───────────────────────────────────────────────────────────────────

impl_codec! {
    enum Channel {
        0 => Constant(v),
        1 => Data(col),
        2 => RawConstant(v),
        3 => RawData(col),
    }
}

/// A geom's whole serializable state: its kind tag plus the parts a
/// [`GeomBuilder`](crate::plot::GeomBuilder) would have been given.
///
/// Everything else a concrete geom holds — mark layouts, orientation,
/// the declared-channel list, the diff snapshot — is derived, and
/// `build_from` recomputes all of it.
#[cfg(any(feature = "document-read", feature = "document-write"))]
pub(crate) struct GeomParts {
    pub(crate) kind: String,
    pub(crate) keys: Option<crate::scales::value::DataColumn>,
    pub(crate) channels: HashMap<String, Channel>,
}

#[cfg(feature = "document-write")]
impl Encode for GeomParts {
    fn encode(&self, w: &mut Writer) {
        super::codec::write_record(w, |w| {
            self.kind.encode(w);
            self.keys.encode(w);
            self.channels.encode(w);
        });
    }
}

#[cfg(feature = "document-read")]
impl Decode for GeomParts {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        super::codec::read_record(r, "GeomParts", |r| {
            Ok(GeomParts {
                kind: String::decode(r)?,
                keys: Option::decode(r)?,
                channels: HashMap::decode(r)?,
            })
        })
    }
}

#[cfg(feature = "document-read")]
impl GeomParts {
    /// Rebuild the geom, resolving the kind tag through `r`'s context.
    pub(crate) fn build(self, r: &Reader<'_>) -> Result<Box<dyn crate::plot::Geom>, DocumentError> {
        let factory =
            r.ctx()
                .geom_factory(&self.kind)
                .ok_or_else(|| DocumentError::UnknownGeom {
                    kind: self.kind.clone(),
                })?;
        Ok(factory(self.keys, self.channels))
    }
}

/// Read a geom's parts off a live geom, or `None` when it can't be
/// named on the wire.
#[cfg(feature = "document-write")]
pub(crate) fn geom_parts(g: &dyn crate::plot::Geom) -> Option<GeomParts> {
    let kind = g.kind()?.to_string();
    let state = g.state();
    Some(GeomParts {
        kind,
        // The builder's view of keys: an explicit column, or `None` for
        // synthesised positional ones. This mirrors what each geom's
        // `update` carries forward, so replaying reproduces the same
        // state the live geom had.
        keys: match &state.keys {
            crate::plot::Keys::Explicit(col) => Some(col.clone()),
            crate::plot::Keys::Positional(_) => None,
        },
        channels: state.channels.clone(),
    })
}

// ─── Composition template ────────────────────────────────────────────────────

impl_codec! {
    struct Span { rows, cols }

    enum ElementTemplate {
        0 => NamedPatch(id),
        1 => Spacer,
        2 => Composition(nested),
    }

    record PlacementTemplate { row, col, span, element }

    record CompositionTemplate {
        id,
        rows,
        cols,
        widths,
        heights,
        aspect,
        margin,
        padding,
        placements,
    }

    record CompositionChrome {
        title,
        subtitle,
        caption,
        axis_titles,
        legends,
        next_legend_id,
    }
}

use crate::composition::Span;
use crate::plot::composition::{
    CompositionChrome, CompositionTemplate, ElementTemplate, PlacementTemplate,
};

// ─── Plot ────────────────────────────────────────────────────────────────────

#[cfg(feature = "document-write")]
impl Encode for Plot {
    fn encode(&self, w: &mut Writer) {
        super::codec::write_record(w, |w| {
            self.patch_id().to_string().encode(w);

            let mut bindings: Vec<(&str, &str)> = self.bindings().collect();
            bindings.sort_unstable_by_key(|(channel, _)| *channel);
            w.varint(bindings.len() as u64);
            for (channel, scale) in bindings {
                channel.encode(w);
                scale.encode(w);
            }

            self.title_ref().map(str::to_string).encode(w);
            self.subtitle_ref().map(str::to_string).encode(w);
            self.caption_ref().map(str::to_string).encode(w);

            // Strips are indexed by side, so the array position carries the
            // meaning and no side tag is needed.
            let strips: [Option<String>; 4] = [
                AxisSide::Left,
                AxisSide::Right,
                AxisSide::Bottom,
                AxisSide::Top,
            ]
            .map(|side| self.strip_at(side).map(str::to_string));
            strips.encode(w);

            self.axes().to_vec().encode(w);
            self.legends().to_vec().encode(w);
            self.projection_ref().encode(w);
            self.is_clipped().encode(w);
            self.tracks_identity().encode(w);
            self.aspect_ratio_ref().encode(w);
            self.aspect_mode_ref().encode(w);
            self.theme_override_ref().cloned().encode(w);

            // Only the geoms that can name themselves. A geom returning
            // `None` from `kind` has already been reported by the write-side
            // validation pass, so skipping it here can't silently drop one.
            let parts: Vec<GeomParts> = self.geoms().filter_map(|(_, g)| geom_parts(g)).collect();
            parts.encode(w);
        });
    }
}

/// Rebuild a plot bound to `patch_id`, which the caller has already
/// checked against the composition.
///
/// Not a [`Decode`] impl: a `Plot` can only be constructed against a
/// `Composition` that contains its patch, so the composition has to be
/// built first and passed in.
#[cfg(feature = "document-read")]
pub(crate) fn decode_plot(
    r: &mut Reader<'_>,
    composition: &crate::composition::Composition,
) -> Result<Plot, DocumentError> {
    super::codec::read_record(r, "Plot", |r| {
        let patch_id = String::decode(r)?;
        let mut plot =
            Plot::try_new(composition, &patch_id).map_err(|e| DocumentError::Invalid {
                what: "plot patch binding",
                why: e.to_string(),
            })?;

        let bindings = r.count()?;
        for _ in 0..bindings {
            let channel = String::decode(r)?;
            let scale = String::decode(r)?;
            plot.set_binding(channel, scale);
        }

        if let Some(t) = Option::<String>::decode(r)? {
            plot.set_title(t);
        }
        if let Some(t) = Option::<String>::decode(r)? {
            plot = plot.subtitle(t);
        }
        if let Some(t) = Option::<String>::decode(r)? {
            plot = plot.caption(t);
        }

        let strips = <[Option<String>; 4]>::decode(r)?;
        for (side, text) in [
            AxisSide::Left,
            AxisSide::Right,
            AxisSide::Bottom,
            AxisSide::Top,
        ]
        .into_iter()
        .zip(strips)
        {
            plot.set_strip(side, text);
        }

        // An axis is validated against the projection, which follows
        // it on the wire, so all three are read before any is applied.
        let axes = Vec::<Axis>::decode(r)?;
        let legends = Vec::<Legend>::decode(r)?;
        plot = plot.projection(Projection::decode(r)?);

        for axis in axes {
            plot.try_add_axis(axis)
                .map_err(|e| DocumentError::Invalid {
                    what: "axis placement",
                    why: e.to_string(),
                })?;
        }
        for legend in legends {
            plot.add_legend(legend);
        }

        plot = plot.clip(bool::decode(r)?);
        plot = plot.track_identity(bool::decode(r)?);
        if let Some(ratio) = Option::<f64>::decode(r)? {
            plot = plot.aspect_ratio(ratio);
        }
        plot = plot.aspect_mode(crate::plot::AspectMode::decode(r)?);
        plot.set_theme_override(Option::decode(r)?);

        let parts = Vec::<GeomParts>::decode(r)?;
        let geoms: Vec<Box<dyn crate::plot::Geom>> = parts
            .into_iter()
            .map(|p| p.build(r))
            .collect::<Result<_, _>>()?;
        for geom in geoms {
            plot.add_boxed_geom(geom);
        }

        Ok(plot)
    })
}

impl_codec! {
    enum AspectMode {
        0 => Panel,
        1 => Range,
    }
}

use crate::plot::{AspectMode, Plot};
