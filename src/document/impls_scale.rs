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
//! A scale's label spec is plain data — a rename table plus a template
//! string — so it travels whole, and a reader renders the labels the
//! writer saw with nothing to resolve. [`Locale`] is a tag, so it
//! travels as the string it is.

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;
use crate::plot::scale::{
    BreaksSpec, LabelSpec, MinorBreaksSpec, Scale, ScaleRegistry, ScaleTypeKind,
};
use crate::scales::breaks::{CalendarUnit, TemporalInterval};
use crate::scales::direction::Direction;
use crate::scales::input::InputRange;
use crate::scales::locale::Locale;
use crate::scales::output::OutputRange;
use crate::scales::scale_type::TemporalUnit;
use crate::scales::transform::{Transform, TransformKind};
use crate::scales::value::Value;

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
        super::codec::write_record(w, |w| {
            self.scale_type_kind().encode(w);
            self.transform().encode(w);
            self.input_range().cloned().encode(w);
            self.output_range().cloned().encode(w);
            self.bins().map(<[f64]>::to_vec).encode(w);
            self.breaks_spec().cloned().encode(w);
            self.minor_breaks_spec().cloned().encode(w);
            self.color_space().encode(w);
            self.direction().encode(w);
            // The label spec, whole: rename entries as
            // `(value, Some(new label) | None = suppressed)`, then the
            // fallback template's source text.
            self.labels()
                .renames()
                .map(|(v, l)| (v.clone(), l.map(str::to_string)))
                .collect::<Vec<_>>()
                .encode(w);
            self.labels()
                .template()
                .map(|t| t.source().to_string())
                .encode(w);
            // Trailing: readers from before this field existed skip it.
            self.na_value().clone().encode(w);
            // `generation` and the break memo are not written: the counter
            // only keys a cache that starts empty, so a rebuilt scale
            // recomputes rather than trusting a number from the wire.
        });
    }
}

#[cfg(feature = "document-read")]
impl Decode for Scale {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        // Hand-rolled record framing rather than `read_record`: the
        // trailing `na_value` predates nobody, so the reader needs the
        // record's end to tell "absent from an older document" apart
        // from "present" — a blind read would consume the next record's
        // bytes instead of failing.
        let len = r.count()?;
        let end = r.pos() + len;
        let out = (|r: &mut Reader<'_>| {
            let kind = ScaleTypeKind::decode(r)?;
            let transform = Transform::decode(r)?;
            let input = Option::<InputRange>::decode(r)?;
            let output = Option::<OutputRange>::decode(r)?;
            let bins = Option::<Vec<f64>>::decode(r)?;
            let breaks = Option::<BreaksSpec>::decode(r)?;
            let minor = Option::<MinorBreaksSpec>::decode(r)?;
            let color_space = super::codec::Decode::decode(r)?;
            let direction = Direction::decode(r)?;
            let renames = Vec::<(Value, Option<String>)>::decode(r)?;
            let template = Option::<String>::decode(r)?;
            // A document written before `na_value` existed ends the
            // record here; missing data mapped to `Value::Null` then.
            let na_value = if r.pos() < end {
                Value::decode(r)?
            } else {
                Value::Null
            };

            let mut labels = LabelSpec::new();
            for (v, label) in renames {
                match label {
                    Some(l) => {
                        labels.rename(v, l);
                    }
                    None => {
                        labels.suppress(v);
                    }
                }
            }
            if let Some(t) = &template {
                labels.set_template(t);
            }

            let mut s = Scale::new(kind);
            s.set_na_value(na_value);
            s.set_labels(labels);
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

            Ok(s)
        })(r)?;
        r.skip_to(end, "Scale")?;
        Ok(out)
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
    use super::super::codec::test_support::{assert_roundtrip, roundtrip};
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
        assert!(a.na_value().key_eq(b.na_value()));
        // `LabelSpec` has no `PartialEq`; the wire form is the
        // comparison — same rename pairs, same template source.
        let renames_of = |s: &Scale| {
            s.labels()
                .renames()
                .map(|(v, l)| (format!("{v:?}"), l.map(str::to_string)))
                .collect::<Vec<_>>()
        };
        assert_eq!(renames_of(a), renames_of(b));
        assert_eq!(
            a.labels().template().map(|t| t.source()),
            b.labels().template().map(|t| t.source())
        );
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
    fn a_scale_round_trips_its_na_value() {
        let s = scale::continuous(0.0..=1.0)
            .range_colors(vec![rgba(0.0, 0.0, 0.0, 1.0), rgba(1.0, 1.0, 1.0, 1.0)])
            .with_na_value(Value::Color(rgba(0.5, 0.5, 0.5, 1.0)));
        let out = roundtrip(&s);
        assert_same_config(&out, &s);
        assert!(out
            .na_value()
            .key_eq(&Value::Color(rgba(0.5, 0.5, 0.5, 1.0))));
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
            out.format(&Value::Number(0.0)),
            "low",
            "explicit labels should outrank the label spec"
        );
        assert_eq!(out.format(&Value::Number(2.0)), "high");
    }

    /// Break positions and labels are index-aligned by construction, so
    /// a document where they disagree is corrupt and must be refused
    /// rather than indexed past the end.
    #[test]
    fn mismatched_break_labels_are_rejected() {
        let mut w = super::Writer::new();
        crate::document::codec::write_record(&mut w, |w| {
            ScaleTypeKind::Continuous.encode(w);
            Transform {
                kind: TransformKind::Identity,
            }
            .encode(w);
            Option::<InputRange>::None.encode(w);
            Option::<OutputRange>::None.encode(w);
            Option::<Vec<f64>>::None.encode(w);
            Some(BreaksSpec::Labeled {
                breaks: vec![Value::Number(1.0), Value::Number(2.0)],
                labels: vec!["only one".to_string()],
            })
            .encode(w);
            Option::<MinorBreaksSpec>::None.encode(w);
            ColorSpace::Oklab.encode(w);
            Direction::Forward.encode(w);
            Vec::<(Value, Option<String>)>::new().encode(w);
            Option::<String>::None.encode(w);
        });

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
        crate::document::codec::write_record(&mut w, |w| {
            ScaleTypeKind::Binned.encode(w);
            Transform {
                kind: TransformKind::Identity,
            }
            .encode(w);
            Option::<InputRange>::None.encode(w);
            Option::<OutputRange>::None.encode(w);
            Some(vec![5.0, 1.0]).encode(w);
            Option::<BreaksSpec>::None.encode(w);
            Option::<MinorBreaksSpec>::None.encode(w);
            ColorSpace::Oklab.encode(w);
            Direction::Forward.encode(w);
            Vec::<(Value, Option<String>)>::new().encode(w);
            Option::<String>::None.encode(w);
        });

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

    // ── Label specs ──

    #[test]
    fn an_empty_label_spec_round_trips() {
        let s = scale::continuous(0.0..=1.0);
        let out = roundtrip(&s);
        assert!(out.labels().rename_is_empty());
        assert!(out.labels().template().is_none());
    }

    /// The spec is plain data, so it travels whole: the reader renders
    /// the labels the writer saw, with nothing to resolve.
    #[test]
    fn a_label_spec_round_trips_renames_suppressions_and_template() {
        let mut spec = LabelSpec::new();
        spec.rename(Value::Number(0.0), "zero")
            .suppress(Value::Number(50.0))
            .set_template("{:num %.1f}");
        let s = scale::continuous(0.0..=100.0).with_labels(spec);

        let out = roundtrip(&s);
        assert_same_config(&out, &s);
        assert_eq!(out.format(&Value::Number(0.0)), "zero");
        assert_eq!(out.format(&Value::Number(50.0)), "");
        assert_eq!(out.format(&Value::Number(25.0)), "25.0");
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
