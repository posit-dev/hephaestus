//! Spatial geometry — points, lines, polygons, and their multi-part
//! variants — modelled on OGC Simple Features and GeoJSON.
//!
//! [`Geometry`] is the per-row value type consumed by
//! `crate::plot::geom::GeometryGeom`. One row of a typical spatial dataset
//! (an sf data frame, a geopandas GeoDataFrame, a PostGIS query result) is
//! one self-contained feature whose shape may be a Point, LineString,
//! Polygon, or any of the Multi* variants. `Geometry` carries that whole
//! feature in one [`Value::Geometry`](crate::scales::Value::Geometry)
//! variant; the geom dispatches per row to the right primitive at draw
//! time.
//!
//! Coordinates are stored as bare `(f64, f64)` tuples — no kurbo or peniko
//! dependency, so the scales module stays a clean leaf. Conversion to
//! `kurbo::Point` happens at the geom boundary.
//!
//! Coordinate ordering is `(x, y)` throughout — geographic input should
//! be `(longitude, latitude)`. CRS is not interpreted; user data is
//! whatever the user already has.
//!
//! ## Parsers
//!
//! Three optional, feature-gated constructors parse from common spatial
//! interchange formats:
//!
//! - [`Geometry::from_wkt`] (feature `geom-wkt`) — OGC Well-Known Text.
//! - [`Geometry::from_wkb`] (feature `geom-wkb`) — OGC Well-Known Binary.
//! - [`Geometry::from_geojson`] (feature `geom-geojson`) — GeoJSON
//!   geometry objects.
//!
//! All three are hand-rolled, dependency-free, and accept the seven OGC
//! simple-feature types plus `GeometryCollection`. Z/M coordinates are
//! dropped — only the XY pair is retained.

use std::fmt;

#[cfg(feature = "geom-geojson")]
mod geojson;
#[cfg(feature = "geom-wkb")]
mod wkb;
#[cfg(feature = "geom-wkt")]
mod wkt;

/// A 2D coordinate in data space: `(x, y)`.
pub type Coord = (f64, f64);

/// A polygon — one exterior ring plus zero or more interior rings (holes).
///
/// Ring orientation is not normalised by the parsers; downstream rendering
/// treats all rings uniformly under the even-odd fill rule, so winding
/// order does not affect the visual outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct Polygon {
    /// Outer boundary, as a closed ring of coordinates. The first and
    /// last coordinate are conventionally equal; parsers do not enforce
    /// this and renderers do not require it.
    pub exterior: Vec<Coord>,
    /// Inner boundaries (holes). Each is a closed ring under the same
    /// conventions as `exterior`.
    pub interiors: Vec<Vec<Coord>>,
}

impl Polygon {
    /// Construct from an exterior ring with no holes.
    pub fn new(exterior: Vec<Coord>) -> Self {
        Polygon {
            exterior,
            interiors: Vec::new(),
        }
    }

    /// Attach a hole to this polygon. Returns the modified polygon for
    /// builder-style chaining.
    pub fn with_hole(mut self, hole: Vec<Coord>) -> Self {
        self.interiors.push(hole);
        self
    }
}

/// A spatial feature: one of the seven OGC simple-feature types plus
/// `GeometryCollection` and an explicit `Empty` sentinel.
///
/// `Geometry` is the per-row payload of
/// [`Value::Geometry`](crate::scales::Value::Geometry). It is opaque to
/// scales — geometries do not enter continuous or discrete domains and
/// cannot be mapped directly. The consuming geom walks the structure and
/// maps each coordinate through bound `x`/`y` scales at draw time.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Geometry {
    /// A single 2D point.
    Point(Coord),
    /// An unordered collection of points sharing one row's styling.
    MultiPoint(Vec<Coord>),
    /// An open polyline of two or more coordinates.
    LineString(Vec<Coord>),
    /// A collection of independent polylines.
    MultiLineString(Vec<Vec<Coord>>),
    /// A polygon with an exterior ring and optional holes.
    Polygon(Polygon),
    /// A collection of independent polygons.
    MultiPolygon(Vec<Polygon>),
    /// A heterogeneous collection — children may be any geometry variant.
    GeometryCollection(Vec<Geometry>),
    /// The empty geometry. Renders as nothing; equivalent to a row with
    /// `Value::Null` for the `geometry` channel.
    #[default]
    Empty,
}

impl Geometry {
    /// `true` if the geometry contains no drawable coordinates. Includes
    /// the [`Geometry::Empty`] sentinel and any empty `Multi*` /
    /// `LineString` / `GeometryCollection` payloads.
    pub fn is_empty(&self) -> bool {
        match self {
            Geometry::Empty => true,
            Geometry::Point(_) => false,
            Geometry::MultiPoint(pts) => pts.is_empty(),
            Geometry::LineString(pts) => pts.is_empty(),
            Geometry::MultiLineString(ls) => ls.iter().all(|l| l.is_empty()),
            Geometry::Polygon(p) => p.exterior.is_empty(),
            Geometry::MultiPolygon(ps) => ps.iter().all(|p| p.exterior.is_empty()),
            Geometry::GeometryCollection(cs) => cs.iter().all(|c| c.is_empty()),
        }
    }

    /// Axis-aligned bounding box `(xmin, ymin, xmax, ymax)`, or `None` if
    /// the geometry contains no coordinates. NaN coordinates are skipped.
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut acc: Option<(f64, f64, f64, f64)> = None;
        self.for_each_coord(&mut |(x, y)| {
            if !x.is_finite() || !y.is_finite() {
                return;
            }
            match &mut acc {
                Some((xmin, ymin, xmax, ymax)) => {
                    if x < *xmin {
                        *xmin = x;
                    }
                    if x > *xmax {
                        *xmax = x;
                    }
                    if y < *ymin {
                        *ymin = y;
                    }
                    if y > *ymax {
                        *ymax = y;
                    }
                }
                None => acc = Some((x, y, x, y)),
            }
        });
        acc
    }

    /// Visit every coordinate in the geometry in left-to-right traversal
    /// order. Recurses into `GeometryCollection` children.
    fn for_each_coord(&self, f: &mut impl FnMut(Coord)) {
        match self {
            Geometry::Empty => {}
            Geometry::Point(c) => f(*c),
            Geometry::MultiPoint(cs) | Geometry::LineString(cs) => {
                for c in cs {
                    f(*c);
                }
            }
            Geometry::MultiLineString(ls) => {
                for line in ls {
                    for c in line {
                        f(*c);
                    }
                }
            }
            Geometry::Polygon(p) => {
                for c in &p.exterior {
                    f(*c);
                }
                for ring in &p.interiors {
                    for c in ring {
                        f(*c);
                    }
                }
            }
            Geometry::MultiPolygon(ps) => {
                for p in ps {
                    for c in &p.exterior {
                        f(*c);
                    }
                    for ring in &p.interiors {
                        for c in ring {
                            f(*c);
                        }
                    }
                }
            }
            Geometry::GeometryCollection(cs) => {
                for c in cs {
                    c.for_each_coord(f);
                }
            }
        }
    }

    /// Parse a [Well-Known Text] geometry string.
    ///
    /// Supports `POINT`, `MULTIPOINT`, `LINESTRING`, `MULTILINESTRING`,
    /// `POLYGON`, `MULTIPOLYGON`, and `GEOMETRYCOLLECTION`. Both the
    /// "outer parens" `MULTIPOINT ((1 2), (3 4))` and "bare" `MULTIPOINT
    /// (1 2, 3 4)` forms are accepted. Z, M, and ZM coordinate variants
    /// are accepted but the trailing dimensions are discarded.
    ///
    /// [Well-Known Text]: https://en.wikipedia.org/wiki/Well-known_text_representation_of_geometry
    #[cfg(feature = "geom-wkt")]
    pub fn from_wkt(s: &str) -> Result<Self, ParseError> {
        wkt::parse(s)
    }

    /// Parse a [Well-Known Binary] geometry payload.
    ///
    /// Accepts both little- and big-endian encodings. The EWKB SRID flag
    /// is honoured insofar as the SRID bytes are consumed and discarded
    /// (we do not interpret CRS). Z, M, and ZM coordinate variants are
    /// accepted but the trailing dimensions are discarded.
    ///
    /// [Well-Known Binary]: https://libgeos.org/specifications/wkb/
    #[cfg(feature = "geom-wkb")]
    pub fn from_wkb(bytes: &[u8]) -> Result<Self, ParseError> {
        wkb::parse(bytes)
    }

    /// Parse a [GeoJSON] geometry object.
    ///
    /// Accepts the eight `"type"` values: `Point`, `MultiPoint`,
    /// `LineString`, `MultiLineString`, `Polygon`, `MultiPolygon`, and
    /// `GeometryCollection`. Higher-level GeoJSON objects (`Feature`,
    /// `FeatureCollection`) are not accepted — pass their `"geometry"`
    /// field directly.
    ///
    /// [GeoJSON]: https://datatracker.ietf.org/doc/html/rfc7946
    #[cfg(feature = "geom-geojson")]
    pub fn from_geojson(s: &str) -> Result<Self, ParseError> {
        geojson::parse(s)
    }
}

// ─── ParseError ──────────────────────────────────────────────────────────────

/// Error returned by [`Geometry::from_wkt`] / [`Geometry::from_wkb`] /
/// [`Geometry::from_geojson`].
///
/// The variants stay coarse on purpose: hephaestus uses these constructors
/// to bring already-validated upstream data into a render pipeline, not to
/// drive an interactive editor where fine-grained error recovery matters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The input did not match the expected format.
    Syntax(String),
    /// The input named a geometry type the parser does not recognise.
    UnknownType(String),
    /// The byte stream ended before the geometry was fully consumed.
    UnexpectedEnd,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Syntax(msg) => write!(f, "geometry parse error: {msg}"),
            ParseError::UnknownType(t) => write!(f, "geometry parse error: unknown type {t:?}"),
            ParseError::UnexpectedEnd => write!(f, "geometry parse error: unexpected end of input"),
        }
    }
}

impl std::error::Error for ParseError {}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_empty() {
        assert!(Geometry::Empty.is_empty());
        assert!(Geometry::default().is_empty());
        assert!(Geometry::MultiPoint(vec![]).is_empty());
        assert!(Geometry::GeometryCollection(vec![Geometry::Empty]).is_empty());
    }

    #[test]
    fn point_is_not_empty() {
        assert!(!Geometry::Point((1.0, 2.0)).is_empty());
    }

    #[test]
    fn bounds_of_empty_is_none() {
        assert_eq!(Geometry::Empty.bounds(), None);
        assert_eq!(Geometry::MultiPoint(vec![]).bounds(), None);
    }

    #[test]
    fn bounds_of_point() {
        assert_eq!(
            Geometry::Point((1.5, 2.5)).bounds(),
            Some((1.5, 2.5, 1.5, 2.5))
        );
    }

    #[test]
    fn bounds_of_polygon_with_hole() {
        let poly = Polygon::new(vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ])
        .with_hole(vec![
            (2.0, 2.0),
            (8.0, 2.0),
            (8.0, 8.0),
            (2.0, 8.0),
            (2.0, 2.0),
        ]);
        assert_eq!(
            Geometry::Polygon(poly).bounds(),
            Some((0.0, 0.0, 10.0, 10.0))
        );
    }

    #[test]
    fn bounds_of_collection() {
        let g = Geometry::GeometryCollection(vec![
            Geometry::Point((1.0, 2.0)),
            Geometry::LineString(vec![(5.0, -1.0), (3.0, 4.0)]),
        ]);
        assert_eq!(g.bounds(), Some((1.0, -1.0, 5.0, 4.0)));
    }

    #[test]
    fn bounds_skips_nan() {
        let g = Geometry::MultiPoint(vec![(f64::NAN, 0.0), (1.0, 2.0), (3.0, f64::INFINITY)]);
        assert_eq!(g.bounds(), Some((1.0, 2.0, 1.0, 2.0)));
    }
}
