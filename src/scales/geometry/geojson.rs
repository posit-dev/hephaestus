//! GeoJSON geometry parser.
//!
//! A tiny purpose-built JSON walker — no `serde_json` dependency. Accepts
//! the eight geometry-object `"type"` values defined by RFC 7946:
//! `Point`, `MultiPoint`, `LineString`, `MultiLineString`, `Polygon`,
//! `MultiPolygon`, and `GeometryCollection`. `Feature` and
//! `FeatureCollection` are rejected — callers should pass their
//! `"geometry"` field directly.
//!
//! Z (a third coordinate component) is consumed and discarded; M is not
//! defined by GeoJSON. Unknown object members are skipped silently.

use super::{Coord, Geometry, ParseError, Polygon};

/// Parse a GeoJSON geometry-object string into a [`Geometry`].
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
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
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

    /// Read a JSON string. Handles the escapes that matter for GeoJSON
    /// type names (`\"`, `\\`, `\/`) and leaves the other escapes as raw
    /// bytes — type comparisons are ASCII-only so the result is exact for
    /// the values we care about.
    fn string(&mut self) -> Result<String, ParseError> {
        self.skip_ws();
        if self.peek() != Some(b'"') {
            return Err(ParseError::Syntax(format!(
                "expected string at byte {}",
                self.pos
            )));
        }
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None => return Err(ParseError::UnexpectedEnd),
                Some(b'"') => {
                    self.pos += 1;
                    return String::from_utf8(out)
                        .map_err(|_| ParseError::Syntax("non-utf8 bytes in string".to_string()));
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"') => out.push(b'"'),
                        Some(b'\\') => out.push(b'\\'),
                        Some(b'/') => out.push(b'/'),
                        Some(b'n') => out.push(b'\n'),
                        Some(b't') => out.push(b'\t'),
                        Some(b'r') => out.push(b'\r'),
                        Some(b'b') => out.push(0x08),
                        Some(b'f') => out.push(0x0c),
                        // \uXXXX — not used by GeoJSON type names; consume
                        // the four hex digits without interpretation.
                        Some(b'u') => {
                            self.pos += 1;
                            for _ in 0..4 {
                                if !matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) {
                                    return Err(ParseError::Syntax(
                                        "invalid \\u escape".to_string(),
                                    ));
                                }
                                self.pos += 1;
                            }
                            out.push(b'?');
                            continue;
                        }
                        _ => return Err(ParseError::Syntax("invalid escape".to_string())),
                    }
                    self.pos += 1;
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Result<f64, ParseError> {
        self.skip_ws();
        let start = self.pos;
        if matches!(self.peek(), Some(b'-') | Some(b'+')) {
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
            .map_err(|_| ParseError::Syntax("invalid number".to_string()))
    }

    /// Skip any JSON value, recursively. Used to ignore irrelevant object
    /// members (`bbox`, `crs`, etc.) without parsing them.
    fn skip_value(&mut self) -> Result<(), ParseError> {
        self.skip_ws();
        match self.peek() {
            None => Err(ParseError::UnexpectedEnd),
            Some(b'"') => {
                let _ = self.string()?;
                Ok(())
            }
            Some(b'{') => {
                self.pos += 1;
                self.skip_ws();
                if self.try_consume(b'}') {
                    return Ok(());
                }
                loop {
                    let _ = self.string()?;
                    self.expect(b':')?;
                    self.skip_value()?;
                    if !self.try_consume(b',') {
                        break;
                    }
                }
                self.expect(b'}')
            }
            Some(b'[') => {
                self.pos += 1;
                self.skip_ws();
                if self.try_consume(b']') {
                    return Ok(());
                }
                loop {
                    self.skip_value()?;
                    if !self.try_consume(b',') {
                        break;
                    }
                }
                self.expect(b']')
            }
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(_) => {
                let _ = self.number()?;
                Ok(())
            }
        }
    }

    fn literal(&mut self, lit: &[u8]) -> Result<(), ParseError> {
        if self.pos + lit.len() > self.src.len() || &self.src[self.pos..self.pos + lit.len()] != lit
        {
            return Err(ParseError::Syntax(format!(
                "expected {:?} at byte {}",
                std::str::from_utf8(lit).unwrap(),
                self.pos
            )));
        }
        self.pos += lit.len();
        Ok(())
    }

    fn coord(&mut self) -> Result<Coord, ParseError> {
        self.expect(b'[')?;
        let x = self.number()?;
        self.expect(b',')?;
        let y = self.number()?;
        // Drop Z (and any further) components.
        while self.try_consume(b',') {
            let _ = self.number()?;
        }
        self.expect(b']')?;
        Ok((x, y))
    }

    fn coord_list(&mut self) -> Result<Vec<Coord>, ParseError> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        if self.try_consume(b']') {
            return Ok(out);
        }
        out.push(self.coord()?);
        while self.try_consume(b',') {
            out.push(self.coord()?);
        }
        self.expect(b']')?;
        Ok(out)
    }

    fn ring_list(&mut self) -> Result<Vec<Vec<Coord>>, ParseError> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        if self.try_consume(b']') {
            return Ok(out);
        }
        out.push(self.coord_list()?);
        while self.try_consume(b',') {
            out.push(self.coord_list()?);
        }
        self.expect(b']')?;
        Ok(out)
    }

    fn polygon_list(&mut self) -> Result<Vec<Vec<Vec<Coord>>>, ParseError> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        if self.try_consume(b']') {
            return Ok(out);
        }
        out.push(self.ring_list()?);
        while self.try_consume(b',') {
            out.push(self.ring_list()?);
        }
        self.expect(b']')?;
        Ok(out)
    }

    /// Top-level: read one geometry object. Members may appear in any
    /// order; only `"type"`, `"coordinates"`, and `"geometries"` are
    /// inspected — everything else is skipped silently.
    fn geometry(&mut self) -> Result<Geometry, ParseError> {
        self.expect(b'{')?;
        let mut ty: Option<String> = None;
        let mut coords_pos: Option<usize> = None;
        let mut geoms_pos: Option<usize> = None;
        self.skip_ws();
        if !self.try_consume(b'}') {
            loop {
                let key = self.string()?;
                self.expect(b':')?;
                match key.as_str() {
                    "type" => {
                        ty = Some(self.string()?);
                    }
                    "coordinates" => {
                        coords_pos = Some(self.pos);
                        self.skip_value()?;
                    }
                    "geometries" => {
                        geoms_pos = Some(self.pos);
                        self.skip_value()?;
                    }
                    _ => {
                        self.skip_value()?;
                    }
                }
                if !self.try_consume(b',') {
                    break;
                }
            }
            self.expect(b'}')?;
        }
        let ty = ty.ok_or_else(|| ParseError::Syntax("missing \"type\"".to_string()))?;
        match ty.as_str() {
            "Point" => {
                let pos = coords_pos
                    .ok_or_else(|| ParseError::Syntax("Point missing coordinates".to_string()))?;
                let mut sub = self.scoped(pos);
                if sub.peek_is_empty_array() {
                    return Ok(Geometry::Empty);
                }
                Ok(Geometry::Point(sub.coord()?))
            }
            "MultiPoint" => {
                let pos = coords_pos.ok_or_else(|| {
                    ParseError::Syntax("MultiPoint missing coordinates".to_string())
                })?;
                let mut sub = self.scoped(pos);
                Ok(Geometry::MultiPoint(sub.coord_list()?))
            }
            "LineString" => {
                let pos = coords_pos.ok_or_else(|| {
                    ParseError::Syntax("LineString missing coordinates".to_string())
                })?;
                let mut sub = self.scoped(pos);
                Ok(Geometry::LineString(sub.coord_list()?))
            }
            "MultiLineString" => {
                let pos = coords_pos.ok_or_else(|| {
                    ParseError::Syntax("MultiLineString missing coordinates".to_string())
                })?;
                let mut sub = self.scoped(pos);
                Ok(Geometry::MultiLineString(sub.ring_list()?))
            }
            "Polygon" => {
                let pos = coords_pos
                    .ok_or_else(|| ParseError::Syntax("Polygon missing coordinates".to_string()))?;
                let mut sub = self.scoped(pos);
                let rings = sub.ring_list()?;
                let (exterior, interiors) = split_rings(rings);
                Ok(Geometry::Polygon(Polygon {
                    exterior,
                    interiors,
                }))
            }
            "MultiPolygon" => {
                let pos = coords_pos.ok_or_else(|| {
                    ParseError::Syntax("MultiPolygon missing coordinates".to_string())
                })?;
                let mut sub = self.scoped(pos);
                let polys = sub
                    .polygon_list()?
                    .into_iter()
                    .map(|rings| {
                        let (exterior, interiors) = split_rings(rings);
                        Polygon {
                            exterior,
                            interiors,
                        }
                    })
                    .collect();
                Ok(Geometry::MultiPolygon(polys))
            }
            "GeometryCollection" => {
                let pos = geoms_pos.ok_or_else(|| {
                    ParseError::Syntax("GeometryCollection missing geometries".to_string())
                })?;
                let mut sub = self.scoped(pos);
                sub.expect(b'[')?;
                let mut children = Vec::new();
                if !sub.try_consume(b']') {
                    children.push(sub.geometry()?);
                    while sub.try_consume(b',') {
                        children.push(sub.geometry()?);
                    }
                    sub.expect(b']')?;
                }
                Ok(Geometry::GeometryCollection(children))
            }
            other => Err(ParseError::UnknownType(other.to_string())),
        }
    }

    /// Build a sibling parser scoped at the saved byte offset, sharing the
    /// same source buffer. Used to defer parsing of `"coordinates"` /
    /// `"geometries"` until after `"type"` is known (members may appear in
    /// any order in JSON).
    fn scoped(&self, pos: usize) -> Parser<'a> {
        Parser { src: self.src, pos }
    }

    fn peek_is_empty_array(&mut self) -> bool {
        self.skip_ws();
        if self.peek() != Some(b'[') {
            return false;
        }
        let mut i = self.pos + 1;
        while i < self.src.len() && matches!(self.src[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        i < self.src.len() && self.src[i] == b']'
    }
}

fn split_rings(rings: Vec<Vec<Coord>>) -> (Vec<Coord>, Vec<Vec<Coord>>) {
    if rings.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut it = rings.into_iter();
    let exterior = it.next().unwrap();
    let interiors = it.collect();
    (exterior, interiors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Geometry {
        super::parse(s).expect(s)
    }

    #[test]
    fn point() {
        assert_eq!(
            p(r#"{"type":"Point","coordinates":[1.5,2.5]}"#),
            Geometry::Point((1.5, 2.5))
        );
    }

    #[test]
    fn point_z_dropped() {
        assert_eq!(
            p(r#"{"type":"Point","coordinates":[1,2,99]}"#),
            Geometry::Point((1.0, 2.0))
        );
    }

    #[test]
    fn point_empty_when_coords_are_empty() {
        assert_eq!(p(r#"{"type":"Point","coordinates":[]}"#), Geometry::Empty);
    }

    #[test]
    fn linestring() {
        assert_eq!(
            p(r#"{"type":"LineString","coordinates":[[1,2],[3,4]]}"#),
            Geometry::LineString(vec![(1.0, 2.0), (3.0, 4.0)])
        );
    }

    #[test]
    fn polygon_with_hole() {
        let g = p(r#"{
            "type":"Polygon",
            "coordinates":[
              [[0,0],[10,0],[10,10],[0,10],[0,0]],
              [[2,2],[8,2],[8,8],[2,8],[2,2]]
            ]
        }"#);
        match g {
            Geometry::Polygon(p) => {
                assert_eq!(p.exterior.len(), 5);
                assert_eq!(p.interiors.len(), 1);
            }
            _ => panic!("expected polygon"),
        }
    }

    #[test]
    fn multipoint() {
        assert_eq!(
            p(r#"{"type":"MultiPoint","coordinates":[[1,2],[3,4]]}"#),
            Geometry::MultiPoint(vec![(1.0, 2.0), (3.0, 4.0)])
        );
    }

    #[test]
    fn multilinestring() {
        match p(r#"{"type":"MultiLineString","coordinates":[[[1,2],[3,4]],[[5,6],[7,8]]]}"#) {
            Geometry::MultiLineString(ls) => assert_eq!(ls.len(), 2),
            _ => panic!("expected multilinestring"),
        }
    }

    #[test]
    fn multipolygon() {
        match p(
            r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[1,0],[1,1],[0,0]]],[[[2,2],[3,2],[3,3],[2,2]]]]}"#,
        ) {
            Geometry::MultiPolygon(ps) => assert_eq!(ps.len(), 2),
            _ => panic!("expected multipolygon"),
        }
    }

    #[test]
    fn geometry_collection() {
        let g = p(r#"{
            "type":"GeometryCollection",
            "geometries":[
              {"type":"Point","coordinates":[1,2]},
              {"type":"LineString","coordinates":[[3,4],[5,6]]}
            ]
        }"#);
        match g {
            Geometry::GeometryCollection(cs) => assert_eq!(cs.len(), 2),
            _ => panic!("expected collection"),
        }
    }

    #[test]
    fn members_in_any_order() {
        // "coordinates" before "type".
        assert_eq!(
            p(r#"{"coordinates":[1,2],"type":"Point"}"#),
            Geometry::Point((1.0, 2.0))
        );
    }

    #[test]
    fn extra_members_ignored() {
        assert_eq!(
            p(r#"{"type":"Point","coordinates":[1,2],"bbox":[0,0,1,1],"crs":{"foo":1}}"#),
            Geometry::Point((1.0, 2.0))
        );
    }

    #[test]
    fn feature_rejected() {
        assert!(matches!(
            super::parse(r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]}}"#),
            Err(ParseError::UnknownType(_))
        ));
    }
}
