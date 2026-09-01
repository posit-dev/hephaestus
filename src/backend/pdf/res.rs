//! Named resources: interning, deterministic name allocation, and the
//! `/Resources` dictionary every content stream references.
//!
//! Names are allocated in first-use order and emitted in name order,
//! never by iterating a hash map. Two encodes of one scene have to
//! produce byte-identical files, and hash iteration order is the usual
//! way that quietly stops being true.

use std::collections::HashMap;

use super::writer::{Objects, Ref};

/// What kind of resource a name refers to.
///
/// The letter a name starts with follows from this, which keeps the
/// kinds from colliding and makes an operator readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResKind {
    /// An `/ExtGState`: constant alpha, a blend mode, a soft mask.
    ExtGState,
    /// A shading `/Pattern`, which is how a gradient fills or strokes.
    Pattern,
    /// A `/Shading` painted directly with `sh`.
    Shading,
    /// An `/XObject` — an image, or a transparency-group form.
    XObject,
}

impl ResKind {
    /// Short tag distinguishing this kind's names from the others'.
    fn prefix(self) -> &'static str {
        match self {
            ResKind::ExtGState => "GS",
            ResKind::Pattern => "P",
            ResKind::Shading => "Sh",
            ResKind::XObject => "X",
        }
    }

    /// The `/Resources` sub-dictionary this kind lives in.
    fn category(self) -> &'static str {
        match self {
            ResKind::ExtGState => "ExtGState",
            ResKind::Pattern => "Pattern",
            ResKind::Shading => "Shading",
            ResKind::XObject => "XObject",
        }
    }
}

/// The token a resource body writes where the shared `/Resources`
/// object's reference belongs.
///
/// A transparency-group form must name a resource dictionary, and that
/// dictionary is the one this table produces — so the reference does
/// not exist when the form is interned. The object number is reserved
/// up front and substituted here.
pub(crate) const RES_REF: &str = "%RESREF%";

/// The token a resource body writes where its companion object's
/// reference belongs, which is how an image names its soft mask.
pub(crate) const SUB_REF: &str = "%SUBREF%";

/// The token a resource body writes where *another* resource's object
/// reference belongs, spelled `%REF:name%`.
///
/// Most references between resources go by name, through the
/// `/Resources` dictionary. A soft mask does not: `/SMask`'s `/G` entry
/// names a form XObject by object number, so the ExtGState that carries
/// it has to reach a number this table only assigns at write time.
/// Hence the token, and hence [`Resources::write`] allocating every
/// object before writing any of them.
const REF_PREFIX: &str = "%REF:";

/// The token naming `name`'s object reference, for a body that needs
/// one rather than a `/Resources` entry.
pub(crate) fn ref_token(name: &str) -> String {
    format!("{REF_PREFIX}{name}%")
}

/// One interned resource.
#[derive(Clone)]
struct Entry {
    kind: ResKind,
    name: String,
    /// The object's full body — a dictionary, or a stream's dictionary
    /// entries and payload.
    value: Value,
}

/// A resource is either a plain object or a stream.
#[derive(Clone)]
enum Value {
    /// A complete dictionary, `<<` … `>>` included.
    Direct(String),
    /// Stream dictionary entries (no enclosing `<<` `>>`), the payload,
    /// and an optional companion stream written first and referenced
    /// through [`SUB_REF`].
    Stream(String, Vec<u8>, Option<(String, Vec<u8>)>),
}

/// Accumulated named resources.
#[derive(Default, Clone)]
pub(crate) struct Resources {
    entries: Vec<Entry>,
    /// Serialized body to the name it was given. Keying on the
    /// serialization makes dedup exact by construction — two
    /// dictionaries that serialize identically render identically, and
    /// nothing else can — and it sidesteps the brush types being
    /// neither `Hash` nor `Eq`.
    seen: HashMap<String, String>,
    /// One counter shared across kinds, so an allocation-order bug is
    /// visible in the output rather than silent.
    next: u32,
}

impl Resources {
    /// Intern a dictionary and return the name that refers to it.
    pub(crate) fn intern(&mut self, kind: ResKind, body: &str) -> String {
        if let Some(name) = self.seen.get(body) {
            return name.clone();
        }
        let name = format!("{}{}", kind.prefix(), self.next);
        self.next += 1;
        self.seen.insert(body.to_string(), name.clone());
        self.entries.push(Entry {
            kind,
            name: name.clone(),
            value: Value::Direct(body.to_string()),
        });
        name
    }

    /// Intern a stream and return the name that refers to it.
    ///
    /// `key` stands in for the payload when deduplicating, so a caller
    /// with a cheap identity for a large blob — a `Blob`'s id, say —
    /// need not hash the blob. `sub` is a companion stream written
    /// ahead of this one and referenced through [`SUB_REF`].
    pub(crate) fn intern_stream(
        &mut self,
        kind: ResKind,
        key: &str,
        dict: &str,
        payload: Vec<u8>,
        sub: Option<(String, Vec<u8>)>,
    ) -> String {
        if let Some(name) = self.seen.get(key) {
            return name.clone();
        }
        let name = format!("{}{}", kind.prefix(), self.next);
        self.next += 1;
        self.seen.insert(key.to_string(), name.clone());
        self.entries.push(Entry {
            kind,
            name: name.clone(),
            value: Value::Stream(dict.to_string(), payload, sub),
        });
        name
    }

    /// The name `key` was interned under, if it was.
    ///
    /// Lets a caller skip building a payload it has already embedded.
    pub(crate) fn lookup(&self, key: &str) -> Option<&str> {
        self.seen.get(key).map(String::as_str)
    }

    /// Write every resource as an indirect object, returning the
    /// `/XObject` etc. sub-dictionary entries for a `/Resources`
    /// dictionary.
    ///
    /// `res_ref` is the object number reserved for that dictionary, and
    /// replaces [`RES_REF`] wherever a body wrote it. `extra` is
    /// appended verbatim, which is how the font registry's own `/Font`
    /// sub-dictionary joins this one.
    pub(crate) fn write(
        &self,
        objects: &mut Objects,
        compress: bool,
        res_ref: Ref,
        extra: &str,
    ) -> String {
        let res_ref = res_ref.to_ref_string();
        // Two passes, because a body may name another resource's object
        // number through `ref_token` and nothing can be written until
        // every number is known. `Objects::alloc` reserves without
        // writing, which is what makes the split cheap.
        let mut refs: Vec<(ResKind, &str, Ref)> = Vec::with_capacity(self.entries.len());
        let mut subs: Vec<Option<Ref>> = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let sub = match &entry.value {
                // A companion stream is written ahead of its owner, so
                // its number is reserved first.
                Value::Stream(_, _, Some(_)) => Some(objects.alloc()),
                _ => None,
            };
            subs.push(sub);
            refs.push((entry.kind, &entry.name, objects.alloc()));
        }
        let substitute = |text: &str, sub_ref: Option<Ref>| {
            let mut out = text.replace(RES_REF, &res_ref);
            if let Some(sr) = sub_ref {
                out = out.replace(SUB_REF, &sr.to_ref_string());
            }
            for (_, name, r) in &refs {
                let token = ref_token(name);
                if out.contains(&token) {
                    out = out.replace(&token, &r.to_ref_string());
                }
            }
            out
        };
        for ((entry, sub_ref), (_, _, r)) in self.entries.iter().zip(&subs).zip(&refs) {
            match &entry.value {
                Value::Direct(body) => objects.object(*r, &substitute(body, *sub_ref)),
                Value::Stream(dict, payload, sub) => {
                    if let (Some((sub_dict, sub_payload)), Some(sr)) = (sub, sub_ref) {
                        objects.stream(*sr, &substitute(sub_dict, None), sub_payload, compress);
                    }
                    objects.stream(*r, &substitute(dict, *sub_ref), payload, compress);
                }
            }
        }
        let mut out = String::new();
        for kind in [
            ResKind::ExtGState,
            ResKind::Pattern,
            ResKind::Shading,
            ResKind::XObject,
        ] {
            let mut any = false;
            for (k, name, r) in &refs {
                if *k != kind {
                    continue;
                }
                if !any {
                    out.push('/');
                    out.push_str(kind.category());
                    out.push_str(" << ");
                    any = true;
                }
                out.push('/');
                out.push_str(name);
                out.push(' ');
                out.push_str(&r.to_ref_string());
                out.push(' ');
            }
            if any {
                out.push_str(">> ");
            }
        }
        out.push_str(extra);
        out
    }

    /// Forget everything, for a new frame.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.seen.clear();
        self.next = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identical_body_is_defined_once_and_shares_its_name() {
        let mut r = Resources::default();
        let a = r.intern(ResKind::ExtGState, "<< /ca 0.5 >>");
        let b = r.intern(ResKind::ExtGState, "<< /ca 0.5 >>");
        assert_eq!(a, b);
        assert_eq!(r.entries.len(), 1);
    }

    #[test]
    fn names_are_allocated_in_first_use_order_across_kinds() {
        let mut r = Resources::default();
        assert_eq!(r.intern(ResKind::ExtGState, "<< /ca 0.5 >>"), "GS0");
        assert_eq!(r.intern(ResKind::Pattern, "<< /PatternType 2 >>"), "P1");
        assert_eq!(r.intern(ResKind::ExtGState, "<< /ca 0.25 >>"), "GS2");
    }

    #[test]
    fn the_resource_dictionary_groups_by_kind() {
        let mut r = Resources::default();
        r.intern(ResKind::ExtGState, "<< /ca 0.5 >>");
        r.intern(ResKind::Pattern, "<< /PatternType 2 >>");
        let mut objects = Objects::new();
        let res_ref = objects.alloc();
        let dict = r.write(&mut objects, false, res_ref, "");
        assert!(dict.contains("/ExtGState << /GS0 "), "{dict}");
        assert!(dict.contains("/Pattern << /P1 "), "{dict}");
    }

    #[test]
    fn clearing_restarts_name_allocation() {
        let mut r = Resources::default();
        r.intern(ResKind::ExtGState, "<< /ca 0.5 >>");
        r.clear();
        assert_eq!(r.intern(ResKind::Pattern, "<< /PatternType 2 >>"), "P0");
    }
}
