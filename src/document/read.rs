//! What a reader needs beyond the bytes: the registry that resolves
//! names a document can only refer to indirectly.
//!
//! One thing in a plot is code rather than data — a
//! [`Geom`](crate::plot::Geom)'s concrete type. A document names it;
//! the [`ReadContext`] is how a host says what the name means. It
//! defaults to what this crate ships, so a plot built only from
//! builtins reads with no setup.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::plot::geom::{Channel, GeomBuilder};
use crate::plot::Geom;
use crate::scales::value::DataColumn;

/// Builds one geom from the parts a document stores for it.
///
/// The keys column and channel map are exactly what
/// [`GeomBuilder::into_parts`] yields, so a factory for a geom that
/// follows the usual pattern is a one-liner over
/// [`BuildableGeom::build_from`](crate::plot::BuildableGeom::build_from).
pub type GeomFactory = fn(Option<DataColumn>, HashMap<String, Channel>) -> Box<dyn Geom>;

/// Registry a document is read against.
///
/// [`Self::new`] knows every geom, which is right for a plot that uses
/// only this crate's geoms. [`Self::default`] registers nothing, so a
/// document read against it fails with
/// [`DocumentError::UnknownGeom`](super::DocumentError::UnknownGeom)
/// on the first geom it holds; build up from it only when supplying every
/// factory by hand.
#[derive(Default)]
pub struct ReadContext {
    geoms: HashMap<String, GeomFactory>,
}

impl std::fmt::Debug for ReadContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut geoms: Vec<&str> = self.geoms.keys().map(String::as_str).collect();
        geoms.sort_unstable();
        f.debug_struct("ReadContext")
            .field("geoms", &geoms)
            .finish()
    }
}

impl ReadContext {
    /// The shared context that knows this crate's geoms.
    ///
    /// [`Self::new`] builds the same thing, but it builds a factory table per
    /// call; a consumer reading document after document — a page swapping
    /// plots, a renderer in a loop — wants this one. Reach for `new` only to
    /// add a geom of your own.
    pub fn builtin() -> &'static ReadContext {
        default_context()
    }

    /// A context that knows this crate's geoms.
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

    /// The factory for `kind`, if the context knows it.
    pub(crate) fn geom_factory(&self, kind: &str) -> Option<GeomFactory> {
        self.geoms.get(kind).copied()
    }

    fn register_builtin_geoms(&mut self) {
        use crate::plot::geom::{
            BSplineGeom, BuildableGeom, EllipseGeom, GeometryGeom, ImageGeom, LineGeom, PointGeom,
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
        /// builds every one and looks it up here, so a tag that drifts
        /// from its impl fails there.
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
            "image" => ImageGeom,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::geom::Geom;

    /// Every geom this crate ships has a factory registered under the
    /// exact tag its `Geom::kind` reports. The tag list in
    /// `register_builtin_geoms` is written by hand, so nothing but this
    /// test stops one from drifting away from the impl it names — and a
    /// drifted tag writes a document no reader can open.
    #[test]
    fn every_builtin_geom_is_registered() {
        use crate::plot::geom::{
            BSplineGeom, EllipseGeom, GeometryGeom, ImageGeom, LineGeom, PointGeom, PolygonGeom,
            RectGeom, RibbonBSplineGeom, RibbonGeom, SegmentGeom, TextFitGeom, TextGeom,
            TextPathGeom, WedgeGeom,
        };
        use crate::scales::geometry::Geometry;

        /// Build a geom from the minimum channels its `build_from`
        /// demands, erased for uniform iteration.
        macro_rules! built {
            ($ty:ty $(, $ch:expr => $v:expr)* $(,)?) => {{
                let mut b = <$ty>::builder();
                $( b.set($ch, $v); )*
                Box::new(b.build()) as Box<dyn Geom>
            }};
        }

        let xs = || vec![0.0, 1.0];
        let ys = || vec![0.0, 1.0];
        let x2s = || vec![1.0, 2.0];
        let y2s = || vec![1.0, 2.0];
        let text = || vec!["a", "b"];

        let geoms: Vec<Box<dyn Geom>> = vec![
            built!(PointGeom, "x" => xs(), "y" => ys()),
            built!(LineGeom, "x" => xs(), "y" => ys()),
            built!(BSplineGeom, "x" => xs(), "y" => ys()),
            built!(SegmentGeom, "x" => xs(), "y" => ys(), "x2" => x2s(), "y2" => y2s()),
            built!(RectGeom, "x" => xs(), "y" => ys(), "x2" => x2s(), "y2" => y2s()),
            built!(EllipseGeom, "x" => xs(), "y" => ys(), "x2" => x2s(), "y2" => y2s()),
            built!(PolygonGeom, "x" => xs(), "y" => ys()),
            built!(RibbonGeom, "x" => xs(), "y" => ys(), "y2" => y2s()),
            built!(RibbonBSplineGeom, "x" => xs(), "y" => ys(), "y2" => y2s()),
            built!(WedgeGeom, "x" => xs(), "y" => ys()),
            built!(GeometryGeom, "geometry" => vec![
                Geometry::Point((0.0, 0.0)),
                Geometry::Point((1.0, 1.0)),
            ]),
            built!(ImageGeom, "x" => xs(), "y" => ys(), "image" => text()),
            built!(TextGeom, "x" => xs(), "y" => ys(), "text" => text()),
            built!(TextFitGeom, "x" => xs(), "y" => ys(), "x2" => x2s(), "y2" => y2s(), "text" => text()),
            built!(TextPathGeom, "x" => xs(), "y" => ys(), "text" => text()),
        ];

        let ctx = ReadContext::new();
        for g in &geoms {
            let tag = g.kind().expect("every builtin geom names itself");
            assert!(
                ctx.geom_factory(tag).is_some(),
                "geom kind {tag:?} has no factory registered in `register_builtin_geoms`"
            );
        }

        assert_eq!(
            ctx.geoms.len(),
            geoms.len(),
            "the factory table and the builtin geom list disagree on how many geoms exist"
        );
    }
}
