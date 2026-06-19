//! Well-Known Text geometry parser.
//!
//! Hand-rolled recursive-descent over a tiny tokeniser. Supports the
//! seven OGC simple-feature types plus `GEOMETRYCOLLECTION`. The Z, M,
//! and ZM coordinate tags are accepted but the trailing dimensions are
//! discarded — `Geometry` is strictly 2D. The literal `EMPTY` follows
//! any type tag and produces [`Geometry::Empty`].

use super::{Coord, Geometry, ParseError, Polygon};

/// Parse a WKT string into a [`Geometry`]. The string must contain a
/// single geometry; trailing non-whitespace is rejected.
pub(super) fn parse(s: &str) -> Result<Geometry, ParseError> {
    let mut p = Parser::new(s);
    let g = p.geometry()?;
    p.skip_ws();
    if !p.eof() {
        return Err(ParseError::Syntax(format!(
            "trailing input at byte {}",
            p.pos
        )));
    }
    Ok(g)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser {
            src: s.as_bytes(),
            pos: 0,
        }
    }

    fn eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), ParseError> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(ParseError::Syntax(format!(
                "expected '{}' at byte {}",
                c as char, self.pos
            )))
        }
    }

    fn try_consume(&mut self, c: u8) -> bool {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Read an ASCII identifier of length >= 1 (letters only). Returns the
    /// upper-cased bytes so case-insensitive matching is one allocation per
    /// keyword instead of per byte.
    fn keyword(&mut self) -> Result<String, ParseError> {
        self.skip_ws();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphabetic() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err(ParseError::Syntax(format!(
                "expected keyword at byte {}",
                self.pos
            )));
        }
        Ok(std::str::from_utf8(&self.src[start..self.pos])
            .unwrap()
            .to_ascii_uppercase())
    }

    /// Read one floating-point number. Accepts the standard f64 grammar:
    /// optional sign, integer part, optional fraction, optional exponent.
    fn number(&mut self) -> Result<f64, ParseError> {
        self.skip_ws();
        let start = self.pos;
        if matches!(self.peek(), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if start == self.pos {
            return Err(ParseError::Syntax(format!(
                "expected number at byte {}",
                self.pos
            )));
        }
        std::str::from_utf8(&self.src[start..self.pos])
            .unwrap()
            .parse::<f64>()
            .map_err(|_| {
                ParseError::Syntax(format!(
                    "invalid number {:?}",
                    std::str::from_utf8(&self.src[start..self.pos]).unwrap()
                ))
            })
    }

    /// Read one coordinate. WKT permits 2, 3, or 4 numbers per coordinate
    /// depending on the dimension tag; trailing numbers (Z, M) are
    /// consumed and discarded so the caller does not need to know the tag.
    fn coord(&mut self) -> Result<Coord, ParseError> {
        let x = self.number()?;
        let y = self.number()?;
        // Drop any further coordinate components (Z, M).
        loop {
            let save = self.pos;
            self.skip_ws();
            if matches!(self.peek(), Some(c) if c == b'-' || c == b'+' || c.is_ascii_digit() || c == b'.')
            {
                let _ = self.number()?;
            } else {
                self.pos = save;
                break;
            }
        }
        Ok((x, y))
    }

    fn coord_list(&mut self) -> Result<Vec<Coord>, ParseError> {
        let mut out = Vec::new();
        out.push(self.coord()?);
        while self.try_consume(b',') {
            out.push(self.coord()?);
        }
        Ok(out)
    }

    fn ring(&mut self) -> Result<Vec<Coord>, ParseError> {
        self.expect(b'(')?;
        let cs = self.coord_list()?;
        self.expect(b')')?;
        Ok(cs)
    }

    fn polygon_body(&mut self) -> Result<Polygon, ParseError> {
        self.expect(b'(')?;
        let exterior = self.ring()?;
        let mut interiors = Vec::new();
        while self.try_consume(b',') {
            interiors.push(self.ring()?);
        }
        self.expect(b')')?;
        Ok(Polygon {
            exterior,
            interiors,
        })
    }

    /// Skip the optional Z / M / ZM dimension tag after a type keyword and
    /// detect a following `EMPTY` literal. Returns `true` if the geometry
    /// is empty (caller short-circuits to `Geometry::Empty`).
    fn dim_tag_or_empty(&mut self) -> Result<bool, ParseError> {
        self.skip_ws();
        // Optional Z / M / ZM tag — purely lexical; we already discard the
        // extra coordinate components in `coord()`.
        let save = self.pos;
        if let Some(c) = self.peek() {
            if c.is_ascii_alphabetic() {
                let kw = self.keyword()?;
                match kw.as_str() {
                    "Z" | "M" | "ZM" => {}
                    "EMPTY" => return Ok(true),
                    _ => {
                        // Not a tag — rewind so `geometry()` can re-read
                        // the keyword as a nested type name.
                        self.pos = save;
                    }
                }
            }
        }
        // The tag (if any) may be followed by EMPTY.
        self.skip_ws();
        let save = self.pos;
        if let Some(c) = self.peek() {
            if c.is_ascii_alphabetic() {
                let kw = self.keyword()?;
                if kw == "EMPTY" {
                    return Ok(true);
                }
                self.pos = save;
            }
        }
        Ok(false)
    }

    fn geometry(&mut self) -> Result<Geometry, ParseError> {
        let kw = self.keyword()?;
        let empty = self.dim_tag_or_empty()?;
        if empty {
            return Ok(Geometry::Empty);
        }
        match kw.as_str() {
            "POINT" => {
                self.expect(b'(')?;
                let c = self.coord()?;
                self.expect(b')')?;
                Ok(Geometry::Point(c))
            }
            "LINESTRING" => {
                self.expect(b'(')?;
                let cs = self.coord_list()?;
                self.expect(b')')?;
                Ok(Geometry::LineString(cs))
            }
            "POLYGON" => {
                let poly = self.polygon_body()?;
                Ok(Geometry::Polygon(poly))
            }
            "MULTIPOINT" => {
                self.expect(b'(')?;
                let mut pts = Vec::new();
                // Two accepted forms: `((x y), (x y), ...)` or
                // `(x y, x y, ...)`. Peek to decide.
                self.skip_ws();
                if self.peek() == Some(b'(') {
                    pts.push(self.ring_single_point()?);
                    while self.try_consume(b',') {
                        pts.push(self.ring_single_point()?);
                    }
                } else {
                    pts.push(self.coord()?);
                    while self.try_consume(b',') {
                        pts.push(self.coord()?);
                    }
                }
                self.expect(b')')?;
                Ok(Geometry::MultiPoint(pts))
            }
            "MULTILINESTRING" => {
                self.expect(b'(')?;
                let mut lines = Vec::new();
                lines.push(self.ring()?);
                while self.try_consume(b',') {
                    lines.push(self.ring()?);
                }
                self.expect(b')')?;
                Ok(Geometry::MultiLineString(lines))
            }
            "MULTIPOLYGON" => {
                self.expect(b'(')?;
                let mut polys = Vec::new();
                polys.push(self.polygon_body()?);
                while self.try_consume(b',') {
                    polys.push(self.polygon_body()?);
                }
                self.expect(b')')?;
                Ok(Geometry::MultiPolygon(polys))
            }
            "GEOMETRYCOLLECTION" => {
                self.expect(b'(')?;
                let mut children = Vec::new();
                children.push(self.geometry()?);
                while self.try_consume(b',') {
                    children.push(self.geometry()?);
                }
                self.expect(b')')?;
                Ok(Geometry::GeometryCollection(children))
            }
            other => Err(ParseError::UnknownType(other.to_string())),
        }
    }

    /// `( x y )` — a parenthesised single coordinate used by the
    /// "outer-parens" `MULTIPOINT ((1 2), (3 4))` form.
    fn ring_single_point(&mut self) -> Result<Coord, ParseError> {
        self.expect(b'(')?;
        let c = self.coord()?;
        self.expect(b')')?;
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Geometry {
        super::parse(s).expect(s)
    }

    #[test]
    fn point_basic() {
        assert_eq!(p("POINT (1 2)"), Geometry::Point((1.0, 2.0)));
    }

    #[test]
    fn point_z_dropped() {
        assert_eq!(p("POINT Z (1 2 3)"), Geometry::Point((1.0, 2.0)));
        assert_eq!(p("POINT ZM (1 2 3 4)"), Geometry::Point((1.0, 2.0)));
    }

    #[test]
    fn point_empty() {
        assert_eq!(p("POINT EMPTY"), Geometry::Empty);
        assert_eq!(p("MULTIPOLYGON EMPTY"), Geometry::Empty);
        assert_eq!(p("MULTIPOLYGON Z EMPTY"), Geometry::Empty);
    }

    #[test]
    fn linestring() {
        assert_eq!(
            p("LINESTRING (1 2, 3 4, 5 6)"),
            Geometry::LineString(vec![(1.0, 2.0), (3.0, 4.0), (5.0, 6.0)])
        );
    }

    #[test]
    fn polygon_with_hole() {
        let g = p("POLYGON ((0 0, 10 0, 10 10, 0 10, 0 0), (2 2, 8 2, 8 8, 2 8, 2 2))");
        match g {
            Geometry::Polygon(p) => {
                assert_eq!(p.exterior.len(), 5);
                assert_eq!(p.interiors.len(), 1);
                assert_eq!(p.interiors[0].len(), 5);
            }
            _ => panic!("expected polygon"),
        }
    }

    #[test]
    fn multipoint_bare() {
        assert_eq!(
            p("MULTIPOINT (1 2, 3 4)"),
            Geometry::MultiPoint(vec![(1.0, 2.0), (3.0, 4.0)])
        );
    }

    #[test]
    fn multipoint_parens() {
        assert_eq!(
            p("MULTIPOINT ((1 2), (3 4))"),
            Geometry::MultiPoint(vec![(1.0, 2.0), (3.0, 4.0)])
        );
    }

    #[test]
    fn multilinestring() {
        let g = p("MULTILINESTRING ((1 2, 3 4), (5 6, 7 8))");
        match g {
            Geometry::MultiLineString(ls) => {
                assert_eq!(ls.len(), 2);
                assert_eq!(ls[0], vec![(1.0, 2.0), (3.0, 4.0)]);
                assert_eq!(ls[1], vec![(5.0, 6.0), (7.0, 8.0)]);
            }
            _ => panic!("expected multilinestring"),
        }
    }

    #[test]
    fn multipolygon() {
        let g = p("MULTIPOLYGON (((0 0, 1 0, 1 1, 0 0)), ((2 2, 3 2, 3 3, 2 2)))");
        match g {
            Geometry::MultiPolygon(ps) => assert_eq!(ps.len(), 2),
            _ => panic!("expected multipolygon"),
        }
    }

    #[test]
    fn geometry_collection() {
        let g = p("GEOMETRYCOLLECTION (POINT (1 2), LINESTRING (3 4, 5 6))");
        match g {
            Geometry::GeometryCollection(cs) => assert_eq!(cs.len(), 2),
            _ => panic!("expected collection"),
        }
    }

    #[test]
    fn negative_and_scientific() {
        assert_eq!(p("POINT (-1.5e2 +3.0)"), Geometry::Point((-150.0, 3.0)));
    }

    #[test]
    fn case_insensitive_keywords() {
        assert_eq!(p("point (1 2)"), Geometry::Point((1.0, 2.0)));
        assert_eq!(p("Point (1 2)"), Geometry::Point((1.0, 2.0)));
    }

    #[test]
    fn unknown_type() {
        assert!(matches!(
            super::parse("CURVE (1 2)"),
            Err(ParseError::UnknownType(_))
        ));
    }

    #[test]
    fn trailing_input_rejected() {
        assert!(matches!(
            super::parse("POINT (1 2) garbage"),
            Err(ParseError::Syntax(_))
        ));
    }
}
