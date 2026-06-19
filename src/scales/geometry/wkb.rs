//! Well-Known Binary geometry parser.
//!
//! Implements OGC WKB plus the common ISO and EWKB extensions: Z / M / ZM
//! coordinate dimensions and the EWKB SRID flag. Both little- and
//! big-endian byte orders are accepted, and a single payload may mix them
//! across sub-records (each child geometry carries its own byte-order
//! byte). Z and M coordinates are consumed and discarded.

use super::{Coord, Geometry, ParseError, Polygon};

const TYPE_POINT: u32 = 1;
const TYPE_LINESTRING: u32 = 2;
const TYPE_POLYGON: u32 = 3;
const TYPE_MULTIPOINT: u32 = 4;
const TYPE_MULTILINESTRING: u32 = 5;
const TYPE_MULTIPOLYGON: u32 = 6;
const TYPE_GEOMETRYCOLLECTION: u32 = 7;

/// EWKB Z flag — adds a Z component to every coordinate.
const EWKB_FLAG_Z: u32 = 0x80000000;
/// EWKB M flag — adds an M component to every coordinate.
const EWKB_FLAG_M: u32 = 0x40000000;
/// EWKB SRID flag — prefixes the geometry with a u32 SRID.
const EWKB_FLAG_SRID: u32 = 0x20000000;

/// Parse a WKB byte slice into a [`Geometry`]. The slice must contain a
/// single geometry; trailing bytes are rejected.
pub(super) fn parse(bytes: &[u8]) -> Result<Geometry, ParseError> {
    let mut p = Parser { buf: bytes, pos: 0 };
    let g = p.geometry()?;
    if p.pos != p.buf.len() {
        return Err(ParseError::Syntax(format!(
            "trailing {} byte(s) after geometry",
            p.buf.len() - p.pos
        )));
    }
    Ok(g)
}

struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

/// Resolved type tag plus a flag indicating whether each coordinate has a
/// trailing Z and/or M component to be skipped.
struct TypeTag {
    base: u32,
    has_z: bool,
    has_m: bool,
}

impl<'a> Parser<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        if self.pos + n > self.buf.len() {
            return Err(ParseError::UnexpectedEnd);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn byte(&mut self) -> Result<u8, ParseError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self, le: bool) -> Result<u32, ParseError> {
        let s = self.take(4)?;
        let bytes = [s[0], s[1], s[2], s[3]];
        Ok(if le {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    fn f64(&mut self, le: bool) -> Result<f64, ParseError> {
        let s = self.take(8)?;
        let bytes = [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]];
        Ok(if le {
            f64::from_le_bytes(bytes)
        } else {
            f64::from_be_bytes(bytes)
        })
    }

    /// Read one geometry record — byte order, type tag, SRID (if flagged),
    /// then the body.
    fn geometry(&mut self) -> Result<Geometry, ParseError> {
        let bo = self.byte()?;
        let le = match bo {
            0 => false,
            1 => true,
            other => {
                return Err(ParseError::Syntax(format!("invalid byte order {other}")));
            }
        };
        let raw_type = self.u32(le)?;
        let tag = resolve_type(raw_type)?;
        if raw_type & EWKB_FLAG_SRID != 0 {
            let _srid = self.u32(le)?;
        }
        self.body(le, &tag)
    }

    fn body(&mut self, le: bool, tag: &TypeTag) -> Result<Geometry, ParseError> {
        match tag.base {
            TYPE_POINT => {
                let c = self.coord(le, tag)?;
                // OGC WKB has no native "empty point", but ISO recommends
                // encoding it as NaN-coords. Surface that as Empty.
                if c.0.is_nan() && c.1.is_nan() {
                    Ok(Geometry::Empty)
                } else {
                    Ok(Geometry::Point(c))
                }
            }
            TYPE_LINESTRING => {
                let n = self.u32(le)? as usize;
                let mut cs = Vec::with_capacity(n);
                for _ in 0..n {
                    cs.push(self.coord(le, tag)?);
                }
                Ok(Geometry::LineString(cs))
            }
            TYPE_POLYGON => Ok(Geometry::Polygon(self.polygon_body(le, tag)?)),
            TYPE_MULTIPOINT => {
                let n = self.u32(le)? as usize;
                let mut pts = Vec::with_capacity(n);
                for _ in 0..n {
                    match self.geometry()? {
                        Geometry::Point(c) => pts.push(c),
                        Geometry::Empty => {}
                        other => {
                            return Err(ParseError::Syntax(format!(
                                "MultiPoint child was {other:?}, expected Point",
                            )));
                        }
                    }
                }
                Ok(Geometry::MultiPoint(pts))
            }
            TYPE_MULTILINESTRING => {
                let n = self.u32(le)? as usize;
                let mut lines = Vec::with_capacity(n);
                for _ in 0..n {
                    match self.geometry()? {
                        Geometry::LineString(cs) => lines.push(cs),
                        other => {
                            return Err(ParseError::Syntax(format!(
                                "MultiLineString child was {other:?}, expected LineString",
                            )));
                        }
                    }
                }
                Ok(Geometry::MultiLineString(lines))
            }
            TYPE_MULTIPOLYGON => {
                let n = self.u32(le)? as usize;
                let mut polys = Vec::with_capacity(n);
                for _ in 0..n {
                    match self.geometry()? {
                        Geometry::Polygon(p) => polys.push(p),
                        other => {
                            return Err(ParseError::Syntax(format!(
                                "MultiPolygon child was {other:?}, expected Polygon",
                            )));
                        }
                    }
                }
                Ok(Geometry::MultiPolygon(polys))
            }
            TYPE_GEOMETRYCOLLECTION => {
                let n = self.u32(le)? as usize;
                let mut children = Vec::with_capacity(n);
                for _ in 0..n {
                    children.push(self.geometry()?);
                }
                Ok(Geometry::GeometryCollection(children))
            }
            other => Err(ParseError::UnknownType(format!("type code {other}"))),
        }
    }

    fn polygon_body(&mut self, le: bool, tag: &TypeTag) -> Result<Polygon, ParseError> {
        let nrings = self.u32(le)? as usize;
        if nrings == 0 {
            return Ok(Polygon {
                exterior: Vec::new(),
                interiors: Vec::new(),
            });
        }
        let mut rings: Vec<Vec<Coord>> = Vec::with_capacity(nrings);
        for _ in 0..nrings {
            let npts = self.u32(le)? as usize;
            let mut ring = Vec::with_capacity(npts);
            for _ in 0..npts {
                ring.push(self.coord(le, tag)?);
            }
            rings.push(ring);
        }
        let mut it = rings.into_iter();
        let exterior = it.next().unwrap();
        let interiors = it.collect();
        Ok(Polygon {
            exterior,
            interiors,
        })
    }

    fn coord(&mut self, le: bool, tag: &TypeTag) -> Result<Coord, ParseError> {
        let x = self.f64(le)?;
        let y = self.f64(le)?;
        if tag.has_z {
            let _ = self.f64(le)?;
        }
        if tag.has_m {
            let _ = self.f64(le)?;
        }
        Ok((x, y))
    }
}

/// Strip the EWKB SRID flag, separate Z/M markers, and project the
/// remaining type code into the canonical 1..=7 OGC range. Accepts both
/// EWKB high-bit flags and the ISO `+1000`/`+2000`/`+3000` offsets.
fn resolve_type(raw: u32) -> Result<TypeTag, ParseError> {
    let mut t = raw & !EWKB_FLAG_SRID;
    let has_z_ewkb = t & EWKB_FLAG_Z != 0;
    let has_m_ewkb = t & EWKB_FLAG_M != 0;
    t &= !(EWKB_FLAG_Z | EWKB_FLAG_M);
    let (base, has_z_iso, has_m_iso) = match t {
        1..=7 => (t, false, false),
        1001..=1007 => (t - 1000, true, false),
        2001..=2007 => (t - 2000, false, true),
        3001..=3007 => (t - 3000, true, true),
        other => return Err(ParseError::UnknownType(format!("type code {other}"))),
    };
    Ok(TypeTag {
        base,
        has_z: has_z_ewkb || has_z_iso,
        has_m: has_m_ewkb || has_m_iso,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roll a WKB Point with the given byte order + coord. Helper for
    /// keeping tests self-contained — they describe the wire format byte
    /// by byte instead of relying on a roundtrip via an external encoder.
    fn point_le(x: f64, y: f64) -> Vec<u8> {
        let mut v = Vec::new();
        v.push(0x01); // little-endian
        v.extend_from_slice(&TYPE_POINT.to_le_bytes());
        v.extend_from_slice(&x.to_le_bytes());
        v.extend_from_slice(&y.to_le_bytes());
        v
    }

    #[test]
    fn point_round_trip_le() {
        assert_eq!(
            parse(&point_le(1.5, 2.5)).unwrap(),
            Geometry::Point((1.5, 2.5))
        );
    }

    #[test]
    fn point_round_trip_be() {
        let mut v = Vec::new();
        v.push(0x00); // big-endian
        v.extend_from_slice(&TYPE_POINT.to_be_bytes());
        v.extend_from_slice(&1.5f64.to_be_bytes());
        v.extend_from_slice(&2.5f64.to_be_bytes());
        assert_eq!(parse(&v).unwrap(), Geometry::Point((1.5, 2.5)));
    }

    #[test]
    fn point_z_drops_third_coord() {
        let mut v = Vec::new();
        v.push(0x01);
        v.extend_from_slice(&(TYPE_POINT | EWKB_FLAG_Z).to_le_bytes());
        v.extend_from_slice(&1.0f64.to_le_bytes());
        v.extend_from_slice(&2.0f64.to_le_bytes());
        v.extend_from_slice(&99.0f64.to_le_bytes());
        assert_eq!(parse(&v).unwrap(), Geometry::Point((1.0, 2.0)));
    }

    #[test]
    fn point_iso_z_drops_third_coord() {
        let mut v = Vec::new();
        v.push(0x01);
        v.extend_from_slice(&1001u32.to_le_bytes());
        v.extend_from_slice(&1.0f64.to_le_bytes());
        v.extend_from_slice(&2.0f64.to_le_bytes());
        v.extend_from_slice(&99.0f64.to_le_bytes());
        assert_eq!(parse(&v).unwrap(), Geometry::Point((1.0, 2.0)));
    }

    #[test]
    fn linestring_round_trip() {
        let mut v = Vec::new();
        v.push(0x01);
        v.extend_from_slice(&TYPE_LINESTRING.to_le_bytes());
        v.extend_from_slice(&2u32.to_le_bytes());
        for n in [1.0f64, 2.0, 3.0, 4.0] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        assert_eq!(
            parse(&v).unwrap(),
            Geometry::LineString(vec![(1.0, 2.0), (3.0, 4.0)])
        );
    }

    #[test]
    fn polygon_with_hole() {
        let mut v = Vec::new();
        v.push(0x01);
        v.extend_from_slice(&TYPE_POLYGON.to_le_bytes());
        v.extend_from_slice(&2u32.to_le_bytes()); // 2 rings
                                                  // exterior
        v.extend_from_slice(&4u32.to_le_bytes());
        for n in [0.0f64, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 0.0] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        // hole
        v.extend_from_slice(&4u32.to_le_bytes());
        for n in [1.0f64, 1.0, 2.0, 1.0, 2.0, 2.0, 1.0, 1.0] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        match parse(&v).unwrap() {
            Geometry::Polygon(p) => {
                assert_eq!(p.exterior.len(), 4);
                assert_eq!(p.interiors.len(), 1);
                assert_eq!(p.interiors[0].len(), 4);
            }
            _ => panic!("expected polygon"),
        }
    }

    #[test]
    fn multipoint_with_two_points() {
        let mut v = Vec::new();
        v.push(0x01);
        v.extend_from_slice(&TYPE_MULTIPOINT.to_le_bytes());
        v.extend_from_slice(&2u32.to_le_bytes());
        v.extend_from_slice(&point_le(1.0, 2.0));
        v.extend_from_slice(&point_le(3.0, 4.0));
        assert_eq!(
            parse(&v).unwrap(),
            Geometry::MultiPoint(vec![(1.0, 2.0), (3.0, 4.0)])
        );
    }

    #[test]
    fn srid_consumed_silently() {
        let mut v = Vec::new();
        v.push(0x01);
        v.extend_from_slice(&(TYPE_POINT | EWKB_FLAG_SRID).to_le_bytes());
        v.extend_from_slice(&4326u32.to_le_bytes());
        v.extend_from_slice(&1.0f64.to_le_bytes());
        v.extend_from_slice(&2.0f64.to_le_bytes());
        assert_eq!(parse(&v).unwrap(), Geometry::Point((1.0, 2.0)));
    }

    #[test]
    fn nan_point_becomes_empty() {
        let mut v = Vec::new();
        v.push(0x01);
        v.extend_from_slice(&TYPE_POINT.to_le_bytes());
        v.extend_from_slice(&f64::NAN.to_le_bytes());
        v.extend_from_slice(&f64::NAN.to_le_bytes());
        assert_eq!(parse(&v).unwrap(), Geometry::Empty);
    }

    #[test]
    fn truncated_input_errors() {
        let bytes = &point_le(1.0, 2.0)[..10];
        assert!(matches!(parse(bytes), Err(ParseError::UnexpectedEnd)));
    }
}
