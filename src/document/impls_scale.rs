//! Codec for the scales layer.
//!
//! [`Scale`] is the first type here that encapsulates its fields, so it
//! shows the pattern the rest of the document follows for such types:
//! read through the public accessors, rebuild through the public
//! setters. Decoding therefore runs the same validation ordinary
//! construction does — a document with non-increasing bin edges is
//! rejected by [`Scale::try_set_bins`], not silently accepted into a
//! scale that would misplace rows.
//!
//! The one value a scale carries that isn't plain data gets a wire form
//! of its own: [`FormatSpec`] names the label formatter instead of
//! carrying it. [`Locale`] is a tag, so it travels as the string it is.

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;
use crate::plot::scale::{
    BreaksSpec, FormatSpec, MinorBreaksSpec, Scale, ScaleRegistry, ScaleTypeKind,
};
use crate::scales::breaks::{CalendarUnit, TemporalInterval};
use crate::scales::direction::Direction;
use crate::scales::input::InputRange;
use crate::scales::locale::Locale;
use crate::scales::output::OutputRange;
use crate::scales::scale_type::TemporalUnit;
use crate::scales::transform::{Transform, TransformKind};

impl_codec! {
    enum TemporalUnit {
        0 => Date,
        1 => DateTime,
        2 => Time,
        3 => Duration,
    }

    enum ScaleTypeKind {
        0 => Continuous,
        1 => Discrete,
        2 => Ordinal,
        3 => Binned,
        4 => Identity,
        5 => Temporal(unit),
    }

    enum TransformKind {
        0 => Identity,
        1 => Log10,
        2 => Log2,
        3 => Log,
        4 => Sqrt,
        5 => Square,
        6 => Exp10,
        7 => Exp2,
        8 => Exp,
        9 => Asinh,
        10 => PseudoLog,
        11 => PseudoLog2,
        12 => PseudoLog10,
    }

    // Written as a struct rather than a bare tag: `kind` is documented
    // as the place per-transform parameters will hang off, and a struct
    // leaves room to append them.
    struct Transform { kind }

    enum Direction {
        0 => Forward,
        1 => Reversed,
    }

    enum InputRange {
        0 => Continuous { min, max },
        1 => Discrete(values),
    }

    enum OutputRange {
        0 => Numbers(v),
        1 => Strings(v),
        2 => Colors(v),
        3 => Linetypes(v),
    }

    enum CalendarUnit {
        0 => Second,
        1 => Minute,
        2 => Hour,
        3 => Day,
        4 => Week,
        5 => Month,
        6 => Year,
    }

    struct TemporalInterval { count, unit }

    enum BreaksSpec {
        0 => Explicit(values),
        1 => Labeled { breaks, labels },
        2 => NumericInterval(step),
        3 => TemporalInterval(interval),
    }

    enum MinorBreaksSpec {
        0 => Explicit(values),
        1 => CountBetween(n),
        2 => NumericInterval(step),
        3 => TemporalInterval(interval),
    }

    enum FormatSpec {
        0 => Default,
        1 => Named(name),
        2 => Custom,
    }
}

// ─── Locale ──────────────────────────────────────────────────────────────────

#[cfg(feature = "document-write")]
impl Encode for Locale {
    fn encode(&self, w: &mut Writer) {
        self.tag().encode(w);
    }
}

#[cfg(feature = "document-read")]
impl Decode for Locale {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(Locale::from(String::decode(r)?))
    }
}

// ─── Scale ───────────────────────────────────────────────────────────────────

#[cfg(feature = "document-write")]
impl Encode for Scale {
    fn encode(&self, w: &mut Writer) {
        self.scale_type_kind().encode(w);
        self.transform().encode(w);
        self.input_range().cloned().encode(w);
        self.output_range().cloned().encode(w);
        self.bins().map(<[f64]>::to_vec).encode(w);
        self.breaks_spec().cloned().encode(w);
        self.minor_breaks_spec().cloned().encode(w);
        self.color_space().encode(w);
        self.direction().encode(w);
        self.format_spec().encode(w);
        // `generation` and the break memo are not written: the counter
        // only keys a cache that starts empty, so a rebuilt scale
        // recomputes rather than trusting a number from the wire.
    }
}

#[cfg(feature = "document-read")]
impl Decode for Scale {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let kind = ScaleTypeKind::decode(r)?;
        let transform = Transform::decode(r)?;
        let input = Option::<InputRange>::decode(r)?;
        let output = Option::<OutputRange>::decode(r)?;
        let bins = Option::<Vec<f64>>::decode(r)?;
        let breaks = Option::<BreaksSpec>::decode(r)?;
        let minor = Option::<MinorBreaksSpec>::decode(r)?;
        let color_space = super::codec::Decode::decode(r)?;
        let direction = Direction::decode(r)?;
        let format = FormatSpec::decode(r)?;

        let mut s = Scale::new(kind);
        s.set_transform(transform.kind);
        s.set_color_space(color_space);
        s.set_direction(direction);

        match input {
            Some(InputRange::Continuous { min, max }) => s.set_domain_continuous(min, max),
            Some(InputRange::Discrete(values)) => s.set_domain_discrete(values),
            None => {}
        }

        match output {
            Some(OutputRange::Numbers(v)) => s.set_range_numbers(v),
            Some(OutputRange::Strings(v)) => s.set_range_strings(v),
            Some(OutputRange::Colors(v)) => s.set_range_colors(v),
            Some(OutputRange::Linetypes(v)) => s.set_range_linetypes(v),
            None => {}
        }

        if let Some(edges) = bins {
            s.try_set_bins(edges).map_err(|e| DocumentError::Invalid {
                what: "scale bin edges",
                why: e.to_string(),
            })?;
        }

        match breaks {
            Some(BreaksSpec::Explicit(values)) => s.set_breaks(values),
            Some(BreaksSpec::Labeled { breaks, labels }) => {
                if breaks.len() != labels.len() {
                    return Err(DocumentError::Invalid {
                        what: "labeled scale breaks",
                        why: format!(
                            "{} break positions paired with {} labels",
                            breaks.len(),
                            labels.len()
                        ),
                    });
                }
                s.set_breaks_labeled(breaks.into_iter().zip(labels).collect());
            }
            Some(BreaksSpec::NumericInterval(step)) => s.set_interval(step),
            Some(BreaksSpec::TemporalInterval(i)) => s.set_temporal_interval(i),
            None => {}
        }

        match minor {
            Some(MinorBreaksSpec::Explicit(values)) => s.set_minor_breaks(values),
            Some(MinorBreaksSpec::CountBetween(n)) => s.set_minor_count(n),
            Some(MinorBreaksSpec::NumericInterval(step)) => s.set_minor_interval(step),
            Some(MinorBreaksSpec::TemporalInterval(i)) => s.set_minor_temporal_interval(i),
            None => {}
        }

        // Two ways to end up on default labels, both deliberate.
        // `Custom` reaches the wire only from a lossy write, and names
        // nothing a reader could resolve. A `Named` the reader hasn't
        // been taught is a gap in its registry — worth rendering plain
        // ticks for rather than refusing the whole plot over cosmetics.
        if let FormatSpec::Named(name) = &format {
            if let Some(f) = r.ctx().formatter(name) {
                s.set_named_format(name.clone(), move |v, loc| f(v, loc));
            }
        }

        Ok(s)
    }
}

#[cfg(feature = "document-write")]
impl Encode for ScaleRegistry {
    fn encode(&self, w: &mut Writer) {
        // Sorted, so the same registry always writes the same bytes.
        let mut entries: Vec<(&str, &Scale)> = self.iter().collect();
        entries.sort_unstable_by_key(|(name, _)| *name);
        w.varint(entries.len() as u64);
        for (name, scale) in entries {
            name.encode(w);
            scale.encode(w);
        }
    }
}

#[cfg(feature = "document-read")]
impl Decode for ScaleRegistry {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let n = r.count()?;
        let mut out = ScaleRegistry::new();
        for _ in 0..n {
            let name = String::decode(r)?;
            out.insert(name, Scale::decode(r)?);
        }
        Ok(out)
    }
}

#[cfg(all(test, feature = "document-read", feature = "document-write"))]
mod tests {
    use super::super::codec::test_support::{assert_roundtrip, roundtrip, roundtrip_with_context};
    use super::*;
    use crate::color::{rgba, ColorSpace};
    use crate::plot::scale;
    use crate::scales::value::Value;

    /// Every value a `Scale` accessor can report, checked field by
    /// field. `Scale` has no `PartialEq`, so this is what equality
    /// means for it.
    fn assert_same_config(a: &Scale, b: &Scale) {
        assert_eq!(a.scale_type_kind(), b.scale_type_kind());
        assert_eq!(a.transform(), b.transform());
        assert_eq!(a.input_range(), b.input_range());
        assert_eq!(a.output_range(), b.output_range());
        assert_eq!(a.bins(), b.bins());
        assert_eq!(a.color_space(), b.color_space());
        assert_eq!(a.direction(), b.direction());
        assert_eq!(a.format_spec(), b.format_spec());
        assert_eq!(
            format!("{:?}", a.breaks_spec()),
            format!("{:?}", b.breaks_spec())
        );
        assert_eq!(
            format!("{:?}", a.minor_breaks_spec()),
            format!("{:?}", b.minor_breaks_spec())
        );
    }

    #[test]
    fn scale_type_kinds_round_trip() {
        for k in [
            ScaleTypeKind::Continuous,
            ScaleTypeKind::Discrete,
            ScaleTypeKind::Ordinal,
            ScaleTypeKind::Binned,
            ScaleTypeKind::Identity,
            ScaleTypeKind::Temporal(TemporalUnit::Date),
            ScaleTypeKind::Temporal(TemporalUnit::DateTime),
            ScaleTypeKind::Temporal(TemporalUnit::Time),
            ScaleTypeKind::Temporal(TemporalUnit::Duration),
        ] {
            assert_roundtrip(k);
        }
    }

    /// All thirteen transforms, so no tag is mapped twice.
    #[test]
    fn every_transform_kind_round_trips() {
        let kinds = [
            TransformKind::Identity,
            TransformKind::Log10,
            TransformKind::Log2,
            TransformKind::Log,
            TransformKind::Sqrt,
            TransformKind::Square,
            TransformKind::Exp10,
            TransformKind::Exp2,
            TransformKind::Exp,
            TransformKind::Asinh,
            TransformKind::PseudoLog,
            TransformKind::PseudoLog2,
            TransformKind::PseudoLog10,
        ];
        for k in kinds {
            assert_roundtrip(Transform { kind: k });
        }
        let mut seen: Vec<Vec<u8>> = kinds
            .iter()
            .map(|k| {
                let mut w = super::Writer::new();
                Transform { kind: *k }.encode(&mut w);
                w.finish()
            })
            .collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), kinds.len(), "two transforms share a tag");
    }

    #[test]
    fn input_and_output_ranges_round_trip() {
        assert_roundtrip(InputRange::Continuous {
            min: -1.5,
            max: 10.0,
        });
        assert_roundtrip(InputRange::Discrete(vec![
            Value::from("a"),
            Value::Number(2.0),
        ]));
        assert_roundtrip(OutputRange::Numbers(vec![1.0, 5.0]));
        assert_roundtrip(OutputRange::Strings(vec![
            std::sync::Arc::from("x"),
            std::sync::Arc::from("y"),
        ]));
        assert_roundtrip(OutputRange::Colors(vec![rgba(1.0, 0.0, 0.0, 1.0)]));
        assert_roundtrip(OutputRange::Linetypes(vec![crate::linetype::dashed()]));
    }

    #[test]
    fn temporal_intervals_round_trip_through_every_unit() {
        for unit in [
            CalendarUnit::Second,
            CalendarUnit::Minute,
            CalendarUnit::Hour,
            CalendarUnit::Day,
            CalendarUnit::Week,
            CalendarUnit::Month,
            CalendarUnit::Year,
        ] {
            assert_roundtrip(TemporalInterval { count: 3, unit });
        }
    }

    #[test]
    fn a_bare_continuous_scale_round_trips() {
        let s = scale::continuous(0.0..=100.0);
        assert_same_config(&roundtrip(&s), &s);
    }

    /// A scale with every knob set at once — the case where a codec that
    /// wrote fields in one order and read them in another would show up.
    #[test]
    fn a_fully_configured_scale_round_trips() {
        let s = scale::continuous(1.0..=1000.0)
            .with_transform(TransformKind::Log10)
            .range_colors(vec![rgba(0.0, 0.0, 0.0, 1.0), rgba(1.0, 1.0, 1.0, 1.0)])
            .with_color_space(ColorSpace::Srgb)
            .with_direction(Direction::Reversed)
            .with_breaks(vec![Value::Number(1.0), Value::Number(10.0)])
            .with_minor_count(4);
        assert_same_config(&roundtrip(&s), &s);
    }

    #[test]
    fn a_binned_scale_round_trips_its_edges() {
        let s = scale::binned(0.0..=30.0, vec![0.0, 10.0, 20.0, 30.0]);
        let out = roundtrip(&s);
        assert_eq!(out.bins(), Some(&[0.0, 10.0, 20.0, 30.0][..]));
        assert_same_config(&out, &s);
    }

    #[test]
    fn a_discrete_scale_round_trips_its_domain_order() {
        let s = scale::discrete(["b", "a", "c"].map(Value::from));
        let out = roundtrip(&s);
        match (out.input_range(), s.input_range()) {
            (Some(InputRange::Discrete(a)), Some(InputRange::Discrete(b))) => {
                assert_eq!(a.len(), b.len());
                assert!(a.iter().zip(b).all(|(x, y)| x.key_eq(y)));
            }
            other => panic!("expected two discrete domains, got {other:?}"),
        }
    }

    #[test]
    fn labeled_breaks_round_trip_paired_with_their_labels() {
        let s = scale::continuous(0.0..=2.0).with_breaks_labeled(vec![
            (Value::Number(0.0), "low".to_string()),
            (Value::Number(2.0), "high".to_string()),
        ]);
        let out = roundtrip(&s);
        assert_eq!(
            out.format(&Value::Number(0.0), &Locale::EN_US),
            "low",
            "explicit labels should outrank the default formatter"
        );
        assert_eq!(out.format(&Value::Number(2.0), &Locale::EN_US), "high");
    }

    /// Break positions and labels are index-aligned by construction, so
    /// a document where they disagree is corrupt and must be refused
    /// rather than indexed past the end.
    #[test]
    fn mismatched_break_labels_are_rejected() {
        let mut w = super::Writer::new();
        ScaleTypeKind::Continuous.encode(&mut w);
        Transform {
            kind: TransformKind::Identity,
        }
        .encode(&mut w);
        Option::<InputRange>::None.encode(&mut w);
        Option::<OutputRange>::None.encode(&mut w);
        Option::<Vec<f64>>::None.encode(&mut w);
        Some(BreaksSpec::Labeled {
            breaks: vec![Value::Number(1.0), Value::Number(2.0)],
            labels: vec!["only one".to_string()],
        })
        .encode(&mut w);
        Option::<MinorBreaksSpec>::None.encode(&mut w);
        ColorSpace::Oklab.encode(&mut w);
        Direction::Forward.encode(&mut w);
        FormatSpec::Default.encode(&mut w);

        let bytes = w.finish();
        let mut r = super::Reader::new(&bytes);
        assert!(matches!(
            Scale::decode(&mut r),
            Err(DocumentError::Invalid {
                what: "labeled scale breaks",
                ..
            })
        ));
    }

    /// Bin edges go through the validating setter, so a corrupt ladder
    /// is an error rather than a scale that misplaces every row.
    #[test]
    fn non_increasing_bin_edges_are_rejected() {
        let mut w = super::Writer::new();
        ScaleTypeKind::Binned.encode(&mut w);
        Transform {
            kind: TransformKind::Identity,
        }
        .encode(&mut w);
        Option::<InputRange>::None.encode(&mut w);
        Option::<OutputRange>::None.encode(&mut w);
        Some(vec![5.0, 1.0]).encode(&mut w);
        Option::<BreaksSpec>::None.encode(&mut w);
        Option::<MinorBreaksSpec>::None.encode(&mut w);
        ColorSpace::Oklab.encode(&mut w);
        Direction::Forward.encode(&mut w);
        FormatSpec::Default.encode(&mut w);

        let bytes = w.finish();
        let mut r = super::Reader::new(&bytes);
        assert!(matches!(
            Scale::decode(&mut r),
            Err(DocumentError::Invalid {
                what: "scale bin edges",
                ..
            })
        ));
    }

    // ── Formatters ──

    #[test]
    fn a_default_formatter_round_trips_as_default() {
        let s = scale::continuous(0.0..=1.0);
        assert_eq!(roundtrip(&s).format_spec(), FormatSpec::Default);
    }

    /// The name survives, and the closure comes back from the context
    /// rather than from the document.
    #[test]
    fn a_named_formatter_is_restored_from_the_read_context() {
        let s = scale::continuous(0.0..=100.0)
            .with_named_format("pct", |v, _| format!("{}%", v.as_number().unwrap_or(0.0)));

        let ctx = super::super::ReadContext::new()
            .with_formatter("pct", |v, _| format!("{}%", v.as_number().unwrap_or(0.0)));
        let out = roundtrip_with_context(&s, &ctx);

        assert_eq!(out.format_spec(), FormatSpec::Named("pct".into()));
        assert_eq!(out.format(&Value::Number(50.0), &Locale::EN_US), "50%");
    }

    /// A formatter the reader has never heard of leaves default labels
    /// rather than failing the whole document.
    #[test]
    fn an_unknown_formatter_name_falls_back_to_default_labels() {
        let s = scale::continuous(0.0..=100.0)
            .with_named_format("nobody-knows", |_, _| "custom".to_string());
        let out = roundtrip(&s);
        assert_eq!(out.format_spec(), FormatSpec::Default);
        assert_eq!(out.format(&Value::Number(50.0), &Locale::EN_US), "50");
    }

    // ── Locale ──

    #[test]
    fn built_in_locales_round_trip() {
        for l in [Locale::EN_US, Locale::DE_DE, Locale::FR_FR] {
            assert_roundtrip(l);
        }
    }

    #[test]
    fn any_tag_round_trips() {
        // A tag is just a string, so there is no set of locales a
        // document can and cannot carry.
        for tag in ["ar-EG", "hi-IN", "zh-Hans-CN", "ar_EG.UTF-8", "x-private"] {
            assert_roundtrip(Locale::from(tag));
        }
    }

    #[test]
    fn a_decoded_tag_is_the_one_that_was_written() {
        let back = super::super::codec::test_support::roundtrip(&Locale::from("ar-EG"));
        assert_eq!(back.tag(), "ar-EG");
    }

    #[test]
    fn a_scale_registry_round_trips_every_entry() {
        let mut reg = ScaleRegistry::new();
        reg.insert("x", scale::continuous(0.0..=1.0));
        reg.insert(
            "y",
            scale::continuous(0.0..=10.0).with_direction(Direction::Reversed),
        );
        reg.insert("fill", scale::discrete(["a", "b"].map(Value::from)));

        let out = roundtrip(&reg);
        let mut names: Vec<&str> = out.iter().map(|(n, _)| n).collect();
        names.sort_unstable();
        assert_eq!(names, ["fill", "x", "y"]);
        for (name, scale) in reg.iter() {
            assert_same_config(out.get(name).expect("every scale survives"), scale);
        }
    }

    #[test]
    fn an_empty_registry_round_trips() {
        assert_eq!(roundtrip(&ScaleRegistry::new()).len(), 0);
    }
}
