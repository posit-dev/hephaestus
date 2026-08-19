//! Codec for the foundational vocabulary: colours, geometry, paths,
//! strokes, the style vocabulary shared by theme / text / scales, and
//! the `Value` / `DataColumn` data model.
//!
//! Everything here is plain data. The one recurring subtlety is `Arc`
//! interning — `Arc<str>` and `Arc<[LinetypeStep]>` round-trip by value
//! and lose their sharing, which costs only memory; `Arc<Geometry>`
//! additionally carries *identity* (`Value::key_eq` compares it with
//! `Arc::ptr_eq`), so geometry columns are interned by the sections that
//! own them rather than written inline here.

use std::sync::Arc;

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{Encode, Writer};
use super::DocumentError;
use crate::color::{Color, ColorSpace};
use crate::geometry::{Point, Rect, Size, Vec2};
use crate::path::{Path, PathEl};
use crate::scales::geometry::{Geometry, Polygon};
use crate::scales::value::{DataColumn, Value};
use crate::stroke::{Cap, Join, Stroke};
use crate::style_vocab::{HAlign, Length, LinetypeStep, Margin, Palette, ThemeColor, VAlign};

// ─── Colour ──────────────────────────────────────────────────────────────────

// `Color` is a foreign type whose only field is its component array, so
// the four channels are the whole value.
#[cfg(feature = "document-write")]
impl Encode for Color {
    fn encode(&self, w: &mut Writer) {
        for c in self.components {
            w.f32(c);
        }
    }
}

#[cfg(feature = "document-read")]
impl Decode for Color {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(Color::new(<[f32; 4]>::decode(r)?))
    }
}

impl_codec! {
    enum ColorSpace {
        0 => Oklab,
        1 => Srgb,
    }

    struct Palette { paper, ink, accent }

    enum ThemeColor {
        0 => Fixed(color),
        1 => Paper,
        2 => Ink,
        3 => Accent,
        4 => Mix(a, b, t, space),
        5 => Alpha(inner, alpha),
    }
}

// ─── Geometry ────────────────────────────────────────────────────────────────

impl_codec! {
    struct Point { x, y }
    struct Vec2 { x, y }
    struct Size { width, height }
    struct Rect { x0, y0, x1, y1 }

    enum PathEl {
        0 => MoveTo(p),
        1 => LineTo(p),
        2 => QuadTo(p1, p2),
        3 => CurveTo(p1, p2, p3),
        4 => ClosePath,
    }
}

// `Path` wraps a `Vec<PathEl>` but exposes it only as a slice plus
// `push`, so it's rebuilt element by element.
#[cfg(feature = "document-write")]
impl Encode for Path {
    fn encode(&self, w: &mut Writer) {
        self.elements().encode(w);
    }
}

#[cfg(feature = "document-read")]
impl Decode for Path {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(Path::from_vec(Vec::<PathEl>::decode(r)?))
    }
}

impl_codec! {
    enum Geometry {
        0 => Empty,
        1 => Point(c),
        2 => MultiPoint(cs),
        3 => LineString(cs),
        4 => MultiLineString(css),
        5 => Polygon(p),
        6 => MultiPolygon(ps),
        7 => GeometryCollection(gs),
    }

    struct Polygon { exterior, interiors }
}

// ─── Strokes ─────────────────────────────────────────────────────────────────

impl_codec! {
    enum Cap {
        0 => Butt,
        1 => Square,
        2 => Round,
    }

    enum Join {
        0 => Bevel,
        1 => Miter,
        2 => Round,
    }
}

// `Stroke`'s dash pattern is a `SmallVec`, which the codec has no impl
// for; going through the slice keeps the spill threshold an
// implementation detail of kurbo.
#[cfg(feature = "document-write")]
impl Encode for Stroke {
    fn encode(&self, w: &mut Writer) {
        self.width.encode(w);
        self.join.encode(w);
        self.miter_limit.encode(w);
        self.start_cap.encode(w);
        self.end_cap.encode(w);
        self.dash_pattern.as_slice().encode(w);
        self.dash_offset.encode(w);
    }
}

#[cfg(feature = "document-read")]
impl Decode for Stroke {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let width = f64::decode(r)?;
        let join = Join::decode(r)?;
        let miter_limit = f64::decode(r)?;
        let start_cap = Cap::decode(r)?;
        let end_cap = Cap::decode(r)?;
        let dash_pattern = Vec::<f64>::decode(r)?;
        let dash_offset = f64::decode(r)?;
        Ok(Stroke {
            width,
            join,
            miter_limit,
            start_cap,
            end_cap,
            dash_pattern: dash_pattern.into_iter().collect(),
            dash_offset,
        })
    }
}

// ─── Style vocabulary ────────────────────────────────────────────────────────

impl_codec! {
    enum Length {
        0 => Abs(pt),
        1 => Rel(factor),
    }

    struct Margin { top, right, bottom, left }

    enum LinetypeStep {
        0 => Dash(pt),
        1 => Marker(name),
        2 => Gap(pt),
    }

    enum HAlign {
        0 => Start,
        1 => Center,
        2 => End,
        3 => Justify,
    }

    enum VAlign {
        0 => Top,
        1 => Middle,
        2 => Baseline,
        3 => Bottom,
    }
}

// ─── Data model ──────────────────────────────────────────────────────────────

impl_codec! {
    enum Value {
        0 => Null,
        1 => Number(n),
        2 => String(s),
        3 => Bool(b),
        4 => Color(c),
        5 => Date(days),
        6 => DateTime(micros),
        7 => Time(nanos),
        8 => Duration(micros),
        9 => Linetype(steps),
        10 => Geometry(g),
    }

    enum DataColumn {
        0 => F64(v),
        1 => F32(v),
        2 => I32(v),
        3 => I64(v),
        4 => Bool(v),
        5 => String(v),
        6 => Color(v),
        7 => Date(v),
        8 => DateTime(v),
        9 => Time(v),
        10 => Duration(v),
        11 => Linetype(v),
        12 => Geometry(v),
    }
}

// ─── Shared geometry ─────────────────────────────────────────────────────────

// There is deliberately no blanket `impl Encode for Arc<T>`. Whether a
// shared value should be written inline or interned is a decision per
// type, not a property of `Arc`, and the two differ in kind:
// `Arc<str>` and `Arc<[LinetypeStep]>` are values whose sharing saves
// only memory, while `Arc<Geometry>` and `Arc<RichTextStyleSheet>`
// carry *identity* that live code compares by pointer. Leaving the
// blanket impl out forces each new shared type to say which it is.

#[cfg(feature = "document-write")]
impl Encode for Arc<Geometry> {
    fn encode(&self, w: &mut Writer) {
        let index = w.tables().geometry(self);
        w.varint(u64::from(index));
    }
}

#[cfg(feature = "document-read")]
impl Decode for Arc<Geometry> {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let index = u32::decode(r)?;
        r.tables().geometry(index).ok_or(DocumentError::Invalid {
            what: "geometry reference",
            why: format!("index {index} is past the end of the geometry table"),
        })
    }
}

#[cfg(all(test, feature = "document-read", feature = "document-write"))]
mod tests {
    use super::super::codec::test_support::{assert_roundtrip, roundtrip, roundtrip_interned};
    use super::*;
    use crate::color::rgba;

    #[test]
    fn colors_round_trip_with_every_channel_intact() {
        let c = rgba(0.1, 0.25, 0.75, 0.5);
        assert_eq!(roundtrip(&c).components, c.components);
    }

    #[test]
    fn color_spaces_round_trip() {
        assert_roundtrip(ColorSpace::Oklab);
        assert_roundtrip(ColorSpace::Srgb);
    }

    #[test]
    fn palettes_round_trip() {
        assert_roundtrip(Palette {
            paper: rgba(1.0, 1.0, 1.0, 1.0),
            ink: rgba(0.0, 0.0, 0.0, 1.0),
            accent: rgba(0.2, 0.4, 0.8, 1.0),
        });
    }

    /// `ThemeColor` nests through `Box`, so a deep tree is the case that
    /// proves the recursion rather than just the leaves.
    #[test]
    fn nested_theme_colors_round_trip() {
        assert_roundtrip(ThemeColor::Alpha(
            Box::new(ThemeColor::Mix(
                Box::new(ThemeColor::Paper),
                Box::new(ThemeColor::Fixed(rgba(1.0, 0.0, 0.0, 1.0))),
                0.25,
                ColorSpace::Srgb,
            )),
            0.5,
        ));
        for c in [ThemeColor::Paper, ThemeColor::Ink, ThemeColor::Accent] {
            assert_roundtrip(c);
        }
    }

    #[test]
    fn geometry_primitives_round_trip() {
        assert_roundtrip(Point::new(1.5, -2.5));
        assert_roundtrip(Vec2::new(3.0, 4.0));
        assert_roundtrip(Size::new(10.0, 20.0));
        assert_roundtrip(Rect::new(0.0, 1.0, 2.0, 3.0));
    }

    /// Every `PathEl` variant, in one path, so the element tags are
    /// checked together rather than one at a time.
    #[test]
    fn paths_round_trip_every_element_kind() {
        let mut p = Path::new();
        p.move_to(Point::new(0.0, 0.0));
        p.line_to(Point::new(1.0, 0.0));
        p.quad_to(Point::new(2.0, 1.0), Point::new(3.0, 0.0));
        p.curve_to(
            Point::new(4.0, 1.0),
            Point::new(5.0, -1.0),
            Point::new(6.0, 0.0),
        );
        p.close_path();
        assert_eq!(roundtrip(&p).elements(), p.elements());
    }

    #[test]
    fn an_empty_path_round_trips() {
        let p = Path::new();
        assert!(roundtrip(&p).elements().is_empty());
    }

    #[test]
    fn spatial_geometry_round_trips_through_every_variant() {
        let poly = Polygon {
            exterior: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
            interiors: vec![vec![(0.2, 0.2), (0.4, 0.2), (0.4, 0.4)]],
        };
        for g in [
            Geometry::Empty,
            Geometry::Point((1.0, 2.0)),
            Geometry::MultiPoint(vec![(0.0, 0.0), (1.0, 1.0)]),
            Geometry::LineString(vec![(0.0, 0.0), (1.0, 1.0)]),
            Geometry::MultiLineString(vec![vec![(0.0, 0.0)], vec![(1.0, 1.0)]]),
            Geometry::Polygon(poly.clone()),
            Geometry::MultiPolygon(vec![poly.clone()]),
            Geometry::GeometryCollection(vec![
                Geometry::Point((3.0, 4.0)),
                Geometry::Polygon(poly),
            ]),
        ] {
            assert_roundtrip(g);
        }
    }

    #[test]
    fn strokes_round_trip_including_their_dash_pattern() {
        let s = Stroke::new(2.5)
            .with_caps(Cap::Square)
            .with_join(Join::Bevel)
            .with_dashes(1.5, [4.0, 2.0, 1.0, 2.0]);
        let out = roundtrip(&s);
        assert_eq!(out.width, s.width);
        assert_eq!(out.join, s.join);
        assert_eq!(out.miter_limit, s.miter_limit);
        assert_eq!(out.start_cap, s.start_cap);
        assert_eq!(out.end_cap, s.end_cap);
        assert_eq!(out.dash_offset, s.dash_offset);
        assert_eq!(out.dash_pattern.as_slice(), [4.0, 2.0, 1.0, 2.0].as_slice());
    }

    /// A dash pattern longer than the `SmallVec` inline capacity forces
    /// the heap-spilled path, which is the one a plain `Vec` round-trip
    /// could get wrong.
    #[test]
    fn a_long_dash_pattern_round_trips() {
        let dashes: Vec<f64> = (1..=9).map(f64::from).collect();
        let s = Stroke::new(1.0).with_dashes(0.0, dashes.clone());
        assert_eq!(roundtrip(&s).dash_pattern.as_slice(), dashes.as_slice());
    }

    #[test]
    fn style_vocabulary_round_trips() {
        assert_roundtrip(Length::Abs(12.0));
        assert_roundtrip(Length::Rel(1.5));
        assert_roundtrip(Margin {
            top: Length::Abs(1.0),
            right: Length::Rel(2.0),
            bottom: Length::Abs(3.0),
            left: Length::Rel(4.0),
        });
        assert_roundtrip(LinetypeStep::Dash(4.0));
        assert_roundtrip(LinetypeStep::Gap(2.0));
        assert_roundtrip(LinetypeStep::Marker(Arc::from("circle")));
        for h in [HAlign::Start, HAlign::Center, HAlign::End, HAlign::Justify] {
            assert_roundtrip(h);
        }
        for v in [
            VAlign::Top,
            VAlign::Middle,
            VAlign::Baseline,
            VAlign::Bottom,
        ] {
            assert_roundtrip(v);
        }
    }

    #[test]
    fn linetype_patterns_round_trip_as_shared_slices() {
        let p: Arc<[LinetypeStep]> = crate::linetype::dashdot();
        assert_eq!(roundtrip(&p).as_ref(), p.as_ref());
    }

    /// `Value` has no `PartialEq`, so each variant is checked through
    /// the key-equality the diff path actually uses.
    #[test]
    fn values_round_trip_through_every_variant() {
        let cases = [
            Value::Null,
            Value::Number(1.5),
            Value::String(Arc::from("abc")),
            Value::Bool(true),
            Value::Color(rgba(0.1, 0.2, 0.3, 0.4)),
            Value::Date(19_000),
            Value::DateTime(1_700_000_000_000_000),
            Value::Time(3_600_000_000_000),
            Value::Duration(-5_000_000),
        ];
        for v in cases {
            assert!(
                roundtrip(&v).key_eq(&v),
                "{v:?} did not survive the round trip"
            );
        }
    }

    /// Geometry is interned, so the sharing a plot had survives: two
    /// values built from one `Arc` come back sharing one `Arc`, and
    /// therefore still compare equal under the pointer-based
    /// `Value::key_eq` the diff path uses.
    #[test]
    fn shared_geometry_is_still_shared_after_a_round_trip() {
        let shared = Arc::new(Geometry::Point((1.0, 2.0)));
        let pair = vec![
            Value::Geometry(shared.clone()),
            Value::Geometry(shared.clone()),
        ];
        let out = roundtrip_interned(&pair);
        assert!(
            out[0].key_eq(&out[1]),
            "interning should give both references one allocation"
        );
        match &out[0] {
            Value::Geometry(g) => assert_eq!(g.as_ref(), shared.as_ref()),
            other => panic!("expected a geometry value, got {other:?}"),
        }
    }

    /// Distinct geometries stay distinct — interning keys on the `Arc`,
    /// so it must not fold two separate allocations together even when
    /// their coordinates match.
    #[test]
    fn separately_allocated_geometries_stay_distinct() {
        let a = Arc::new(Geometry::Point((1.0, 2.0)));
        let b = Arc::new(Geometry::Point((1.0, 2.0)));
        let pair = vec![Value::Geometry(a), Value::Geometry(b)];
        let out = roundtrip_interned(&pair);
        assert!(!out[0].key_eq(&out[1]));
    }

    /// An index the table can't satisfy is a corrupt document, not a
    /// silently missing geometry.
    #[test]
    fn a_geometry_index_past_the_table_is_rejected() {
        let mut w = Writer::new();
        w.varint(7);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert!(matches!(
            Arc::<Geometry>::decode(&mut r),
            Err(DocumentError::Invalid {
                what: "geometry reference",
                ..
            })
        ));
    }

    #[test]
    fn data_columns_round_trip_with_their_width_preserved() {
        let cases = [
            DataColumn::F64(vec![1.0, 2.0]),
            DataColumn::F32(vec![1.0, 2.0]),
            DataColumn::I32(vec![-1, 2]),
            DataColumn::I64(vec![-1, 2]),
            DataColumn::Bool(vec![true, false]),
            DataColumn::String(vec![Arc::from("a"), Arc::from("b")]),
            DataColumn::Color(vec![rgba(0.0, 0.0, 0.0, 1.0)]),
            DataColumn::Date(vec![1, 2]),
            DataColumn::DateTime(vec![1, 2]),
            DataColumn::Time(vec![1, 2]),
            DataColumn::Duration(vec![1, -2]),
            DataColumn::Linetype(vec![crate::linetype::dashed()]),
            DataColumn::Geometry(vec![Arc::new(Geometry::Point((0.0, 0.0)))]),
        ];
        for col in cases {
            let out = roundtrip_interned(&col);
            assert_eq!(
                std::mem::discriminant(&out),
                std::mem::discriminant(&col),
                "column width changed for {col:?}"
            );
            assert_eq!(out.len(), col.len());
        }
    }

    /// A tag no variant claims is a corrupt document, not a newer one,
    /// and the error says which type failed to resolve it.
    #[test]
    fn an_unknown_discriminant_names_the_type_that_rejected_it() {
        let mut w = Writer::new();
        w.varint(200);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        match ThemeColor::decode(&mut r) {
            Err(DocumentError::BadDiscriminant {
                type_name,
                tag,
                offset,
            }) => {
                assert_eq!(type_name, "ThemeColor");
                assert_eq!(tag, 200);
                assert_eq!(offset, 0);
            }
            other => panic!("expected a discriminant error, got {other:?}"),
        }
    }
}
