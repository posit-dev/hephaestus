//! What a reader needs beyond the bytes: the registries that resolve
//! names a document can only refer to indirectly.
//!
//! Two things in a plot are code rather than data — a
//! [`Scale`](crate::plot::Scale)'s label formatter and a
//! [`Geom`](crate::plot::Geom)'s concrete type. A document names them;
//! the [`ReadContext`] is how a host says what those names mean. Both
//! default to what this crate ships, so a plot built only from builtins
//! reads with no setup.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::plot::geom::{Channel, GeomBuilder};
use crate::plot::scale::LabelFormatter;
use crate::plot::Geom;
use crate::scales::value::DataColumn;

/// Builds one geom from the parts a document stores for it.
///
/// The keys column and channel map are exactly what
/// [`GeomBuilder::into_parts`] yields, so a factory for a geom that
/// follows the usual pattern is a one-liner over
/// [`BuildableGeom::build_from`](crate::plot::BuildableGeom::build_from).
pub type GeomFactory = fn(Option<DataColumn>, HashMap<String, Channel>) -> Box<dyn Geom>;

/// Registries a document is read against.
///
/// [`Self::new`] knows every geom and no named formatters, which is
/// right for a plot that uses only this crate's geoms and no
/// [`Scale::with_named_format`](crate::plot::Scale::with_named_format).
/// [`Self::default`] registers nothing, so a document read against it
/// fails with [`DocumentError::UnknownGeom`](super::DocumentError::UnknownGeom)
/// on the first geom it holds; build up from it only when supplying every
/// factory by hand.
#[derive(Default)]
pub struct ReadContext {
    geoms: HashMap<String, GeomFactory>,
    formatters: HashMap<String, Arc<LabelFormatter>>,
}

impl std::fmt::Debug for ReadContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut geoms: Vec<&str> = self.geoms.keys().map(String::as_str).collect();
        geoms.sort_unstable();
        let mut formatters: Vec<&str> = self.formatters.keys().map(String::as_str).collect();
        formatters.sort_unstable();
        f.debug_struct("ReadContext")
            .field("geoms", &geoms)
            .field("formatters", &formatters)
            .finish()
    }
}

impl ReadContext {
    /// A context that knows this crate's geoms and no named formatters.
    pub fn new() -> Self {
        let mut out = Self::default();
        out.register_builtin_geoms();
        out
    }

    /// Teach the context a geom kind, so a document written by a host
    /// with its own geoms can be read back.
    ///
    /// `kind` must match what that geom's
    /// [`Geom::kind`](crate::plot::Geom::kind) returns. Replaces any
    /// factory already registered under the name.
    pub fn with_geom(mut self, kind: impl Into<String>, factory: GeomFactory) -> Self {
        self.geoms.insert(kind.into(), factory);
        self
    }

    /// Teach the context a named label formatter, matching a name a
    /// scale was given via
    /// [`Scale::with_named_format`](crate::plot::Scale::with_named_format).
    ///
    /// A document naming a formatter this context doesn't know reads
    /// with the scale's default labels rather than failing: the labels
    /// are cosmetic, and refusing the whole plot over them would be
    /// worse than rendering it with plain ones.
    pub fn with_formatter<F>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(&crate::scales::value::Value, &crate::scales::locale::Locale) -> String
            + Send
            + Sync
            + 'static,
    {
        self.formatters.insert(name.into(), Arc::new(f));
        self
    }

    /// The factory for `kind`, if the context knows it.
    pub(crate) fn geom_factory(&self, kind: &str) -> Option<GeomFactory> {
        self.geoms.get(kind).copied()
    }

    /// The formatter registered under `name`, if any.
    pub(crate) fn formatter(&self, name: &str) -> Option<Arc<LabelFormatter>> {
        self.formatters.get(name).cloned()
    }

    fn register_builtin_geoms(&mut self) {
        use crate::plot::geom::{
            BSplineGeom, BuildableGeom, EllipseGeom, GeometryGeom, LineGeom, PointGeom,
            PolygonGeom, RectGeom, RibbonBSplineGeom, RibbonGeom, SegmentGeom, TextFitGeom,
            TextGeom, TextPathGeom, WedgeGeom,
        };

        /// Rebuild one geom the way `GeomBuilder` would have.
        fn build<G: BuildableGeom>(
            keys: Option<DataColumn>,
            channels: HashMap<String, Channel>,
        ) -> Box<dyn Geom> {
            Box::new(G::build_from(GeomBuilder::from_parts(keys, channels)))
        }

        /// Pair each geom type with the tag its `Geom::kind` reports.
        ///
        /// The tags are repeated here rather than read off the impls,
        /// which needs an instance. `every_builtin_geom_is_registered`
        /// builds all fourteen and looks each one up, so a tag that
        /// drifts from its impl fails there.
        macro_rules! register {
            ($($tag:literal => $ty:ty),+ $(,)?) => {
                $( self.geoms.insert($tag.to_string(), build::<$ty> as GeomFactory); )+
            };
        }

        register! {
            "point" => PointGeom,
            "line" => LineGeom,
            "bspline" => BSplineGeom,
            "segment" => SegmentGeom,
            "rect" => RectGeom,
            "ellipse" => EllipseGeom,
            "polygon" => PolygonGeom,
            "ribbon" => RibbonGeom,
            "ribbon-bspline" => RibbonBSplineGeom,
            "wedge" => WedgeGeom,
            "geometry" => GeometryGeom,
            "text" => TextGeom,
            "text-fit" => TextFitGeom,
            "text-path" => TextPathGeom,
        }
    }
}

/// The context used when a caller doesn't supply one.
pub(crate) fn default_context() -> &'static ReadContext {
    static CTX: OnceLock<ReadContext> = OnceLock::new();
    CTX.get_or_init(ReadContext::new)
}
