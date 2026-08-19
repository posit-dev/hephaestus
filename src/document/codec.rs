//! The byte-level codec: a writer, a reader, the [`Encode`] / [`Decode`]
//! traits every document type implements, and the [`impl_codec`] macro
//! that generates the mechanical impls.
//!
//! # Why traits rather than free functions
//!
//! Container types compose. One blanket impl each for `Option<T>`,
//! `Vec<T>`, `Box<T>`, `Arc<T>` and the arrays covers every nesting the
//! plot surface uses, which a family of `write_vec_of_theme_color`
//! functions could not.
//!
//! # Which types use the macro
//!
//! [`impl_codec`] handles types whose fields this module can name and
//! construct — `Theme` and most of its tree, the style vocabulary, the
//! plain enums. Types that encapsulate their fields (`Scale`,
//! `RichTextStyleSheet`, `Shape`, `Plot`, …) get hand-written impls that
//! go through their builders, so decoding runs the same validation as
//! ordinary construction.
//!
//! # Number widths
//!
//! `f64` is written whole. `f32` is written whole. Neither is
//! quantized — the savings aren't worth a lossy round-trip on values
//! like scale domains. Lengths, counts and enum tags are LEB128
//! varints, which is where the compaction actually comes from: almost
//! every count in a plot is under 128 and costs one byte.

use super::DocumentError;

/// Largest number of bytes a `u64` LEB128 varint can occupy.
const MAX_VARINT_BYTES: usize = 10;

// ─── Writer ──────────────────────────────────────────────────────────────────

/// Append-only byte sink. Infallible — writing to memory can't fail, so
/// the write half of the codec returns nothing to check.
#[cfg(feature = "document-write")]
#[derive(Debug, Default)]
pub(crate) struct Writer {
    buf: Vec<u8>,
    /// Values written once and referenced by index afterwards. Carried
    /// on the writer so any [`Encode`] impl can intern without the
    /// tables being threaded through every signature.
    tables: super::intern::WriteTables,
}

#[cfg(feature = "document-write")]
impl Writer {
    /// Start an empty writer.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mutable access to the intern tables.
    pub(crate) fn tables(&mut self) -> &mut super::intern::WriteTables {
        &mut self.tables
    }

    /// Encode into a detached buffer while keeping the intern tables.
    ///
    /// Chunks have to be *written* in the order a reader needs them —
    /// the geometry table before the plots that index into it — but
    /// *discovered* in the opposite order, since encoding the plots is
    /// what fills the table. Encoding a chunk body off to the side
    /// resolves that: the tables come back populated, ready to be
    /// written ahead of the body they describe.
    pub(crate) fn detached<F: FnOnce(&mut Writer)>(&mut self, f: F) -> Vec<u8> {
        let mut sub = Writer {
            buf: Vec::new(),
            tables: std::mem::take(&mut self.tables),
        };
        f(&mut sub);
        self.tables = sub.tables;
        sub.buf
    }

    /// Append already-encoded bytes verbatim, without a length prefix.
    pub(crate) fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes written so far.
    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    /// Take the accumulated bytes.
    pub(crate) fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// Write one byte.
    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Write a LEB128 varint. The encoding every length, count and enum
    /// tag goes through.
    pub(crate) fn varint(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// Write a signed integer, zigzag-mapped so small magnitudes stay
    /// one byte either side of zero.
    pub(crate) fn varint_signed(&mut self, v: i64) {
        self.varint(((v << 1) ^ (v >> 63)) as u64);
    }

    /// Write an `f32` little-endian.
    pub(crate) fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write an `f64` little-endian.
    pub(crate) fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write raw bytes with a varint length prefix.
    pub(crate) fn bytes(&mut self, v: &[u8]) {
        self.varint(v.len() as u64);
        self.buf.extend_from_slice(v);
    }

    /// Write a string with a varint byte-length prefix.
    pub(crate) fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }

    /// Overwrite four bytes at `at` with `v` little-endian. Used to
    /// backfill a chunk length once the chunk's extent is known.
    pub(crate) fn patch_u32_at(&mut self, at: usize, v: u32) {
        self.buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Write a `u32` little-endian at a fixed width, so it can be
    /// backfilled later by [`Self::patch_u32_at`].
    pub(crate) fn u32_fixed(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
}

// ─── Reader ──────────────────────────────────────────────────────────────────

/// Cursor over a document's bytes. Every read is bounds-checked, so a
/// truncated or corrupt document reports where it gave up rather than
/// panicking.
#[cfg(feature = "document-read")]
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Registries that resolve the names a document can only refer to
    /// indirectly — formatters and geom kinds.
    ctx: &'a super::ReadContext,
    /// Values read from their own chunk and referenced by index
    /// afterwards. One `Arc` per entry, handed out to every reference,
    /// which is what restores the sharing the writer collapsed.
    tables: super::intern::ReadTables,
}

#[cfg(feature = "document-read")]
impl<'a> Reader<'a> {
    /// Start reading at the front of `buf` against the default context.
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self::with_context(buf, super::read::default_context())
    }

    /// Start reading at the front of `buf` against `ctx`.
    pub(crate) fn with_context(buf: &'a [u8], ctx: &'a super::ReadContext) -> Self {
        Self {
            buf,
            pos: 0,
            ctx,
            tables: super::intern::ReadTables::default(),
        }
    }

    /// The registries this document is being read against.
    pub(crate) fn ctx(&self) -> &'a super::ReadContext {
        self.ctx
    }

    /// Mutable access to the intern tables, for the chunk that fills
    /// them.
    pub(crate) fn tables_mut(&mut self) -> &mut super::intern::ReadTables {
        &mut self.tables
    }

    /// The intern tables, for the sections that index into them.
    pub(crate) fn tables(&self) -> &super::intern::ReadTables {
        &self.tables
    }

    /// Continue reading `body` with this reader's context and tables.
    ///
    /// A chunk's body is a self-contained byte range, so it's decoded by
    /// a reader of its own — but one that still resolves interned
    /// references and named factories through the document it belongs
    /// to.
    pub(crate) fn nested<'b>(&'b self, body: &'b [u8]) -> Reader<'b>
    where
        'a: 'b,
    {
        Reader {
            buf: body,
            pos: 0,
            ctx: self.ctx,
            tables: self.tables.clone(),
        }
    }

    /// Current offset, for error reporting.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Bytes not yet consumed.
    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// True when every byte has been consumed.
    pub(crate) fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Borrow the next `n` bytes and advance past them.
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], DocumentError> {
        if self.remaining() < n {
            return Err(DocumentError::UnexpectedEof {
                offset: self.pos,
                wanted: n,
                available: self.remaining(),
            });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Read one byte.
    pub(crate) fn u8(&mut self) -> Result<u8, DocumentError> {
        Ok(self.take(1)?[0])
    }

    /// Read a LEB128 varint.
    pub(crate) fn varint(&mut self) -> Result<u64, DocumentError> {
        let start = self.pos;
        let mut out: u64 = 0;
        for i in 0..MAX_VARINT_BYTES {
            let byte = self.u8()?;
            out |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(out);
            }
        }
        Err(DocumentError::BadVarint { offset: start })
    }

    /// Read a zigzag-mapped signed integer.
    pub(crate) fn varint_signed(&mut self) -> Result<i64, DocumentError> {
        let raw = self.varint()?;
        Ok(((raw >> 1) as i64) ^ -((raw & 1) as i64))
    }

    /// Read a varint that indexes or sizes something, as a `usize`.
    ///
    /// Rejects a length that couldn't possibly be backed by the
    /// remaining input, so a corrupt prefix fails here instead of
    /// asking for a multi-gigabyte allocation. The bound is deliberately
    /// loose — one byte per element — since the real check is the
    /// element reads that follow.
    pub(crate) fn count(&mut self) -> Result<usize, DocumentError> {
        let raw = self.varint()?;
        let n = usize::try_from(raw).map_err(|_| DocumentError::UnexpectedEof {
            offset: self.pos,
            wanted: usize::MAX,
            available: self.remaining(),
        })?;
        if n > self.remaining() {
            return Err(DocumentError::UnexpectedEof {
                offset: self.pos,
                wanted: n,
                available: self.remaining(),
            });
        }
        Ok(n)
    }

    /// Read an `f32` little-endian.
    pub(crate) fn f32(&mut self) -> Result<f32, DocumentError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read an `f64` little-endian.
    pub(crate) fn f64(&mut self) -> Result<f64, DocumentError> {
        let b = self.take(8)?;
        Ok(f64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a fixed-width `u32` little-endian, the counterpart to
    /// [`Writer::u32_fixed`].
    pub(crate) fn u32_fixed(&mut self) -> Result<u32, DocumentError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read length-prefixed raw bytes.
    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], DocumentError> {
        let n = self.count()?;
        self.take(n)
    }

    /// Read a length-prefixed UTF-8 string.
    pub(crate) fn str(&mut self) -> Result<&'a str, DocumentError> {
        let offset = self.pos;
        let raw = self.bytes()?;
        std::str::from_utf8(raw).map_err(|_| DocumentError::BadUtf8 { offset })
    }
}

// ─── Traits ──────────────────────────────────────────────────────────────────

/// A type that can be written into a document.
#[cfg(feature = "document-write")]
pub(crate) trait Encode {
    /// Append this value's bytes to `w`.
    fn encode(&self, w: &mut Writer);
}

/// A type that can be read back out of a document.
#[cfg(feature = "document-read")]
pub(crate) trait Decode: Sized {
    /// Consume one value from `r`.
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError>;
}

// ─── Primitive impls ─────────────────────────────────────────────────────────

/// Expand to `$sub`, discarding `$tt`.
///
/// Lets a macro repetition driven by field *names* emit one
/// name-independent expression per field — the decode side of a tuple
/// variant, where the names exist only to count the fields.
macro_rules! replace_expr {
    ($_tt:tt, $sub:expr) => {
        $sub
    };
}

/// Generate `Encode` / `Decode` for a type that maps onto one writer
/// and one reader primitive.
macro_rules! impl_codec_scalar {
    ($ty:ty, $write:ident, $read:ident $(, $cast_out:ty)?) => {
        #[cfg(feature = "document-write")]
        impl Encode for $ty {
            fn encode(&self, w: &mut Writer) {
                w.$write((*self) $(as $cast_out)?);
            }
        }
        #[cfg(feature = "document-read")]
        impl Decode for $ty {
            fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
                #[allow(clippy::useless_conversion)]
                Ok(r.$read()?.try_into().map_err(|_| DocumentError::Invalid {
                    what: stringify!($ty),
                    why: "value out of range".to_string(),
                })?)
            }
        }
    };
}

impl_codec_scalar!(u8, varint, varint, u64);
impl_codec_scalar!(u16, varint, varint, u64);
impl_codec_scalar!(u32, varint, varint, u64);
impl_codec_scalar!(u64, varint, varint);
impl_codec_scalar!(usize, varint, varint, u64);
impl_codec_scalar!(i32, varint_signed, varint_signed, i64);
impl_codec_scalar!(i64, varint_signed, varint_signed);

#[cfg(feature = "document-write")]
impl Encode for f32 {
    fn encode(&self, w: &mut Writer) {
        w.f32(*self);
    }
}

#[cfg(feature = "document-read")]
impl Decode for f32 {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        r.f32()
    }
}

#[cfg(feature = "document-write")]
impl Encode for f64 {
    fn encode(&self, w: &mut Writer) {
        w.f64(*self);
    }
}

#[cfg(feature = "document-read")]
impl Decode for f64 {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        r.f64()
    }
}

#[cfg(feature = "document-write")]
impl Encode for bool {
    fn encode(&self, w: &mut Writer) {
        w.u8(u8::from(*self));
    }
}

#[cfg(feature = "document-read")]
impl Decode for bool {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        // Any non-zero byte reads as true rather than erroring: the
        // distinction carries no meaning a document could rely on.
        Ok(r.u8()? != 0)
    }
}

#[cfg(feature = "document-write")]
impl Encode for char {
    fn encode(&self, w: &mut Writer) {
        w.varint(u32::from(*self) as u64);
    }
}

#[cfg(feature = "document-read")]
impl Decode for char {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let raw = r.varint()?;
        u32::try_from(raw)
            .ok()
            .and_then(char::from_u32)
            .ok_or(DocumentError::Invalid {
                what: "char",
                why: format!("{raw} is not a Unicode scalar value"),
            })
    }
}

#[cfg(feature = "document-write")]
impl Encode for str {
    fn encode(&self, w: &mut Writer) {
        w.str(self);
    }
}

#[cfg(feature = "document-write")]
impl Encode for String {
    fn encode(&self, w: &mut Writer) {
        w.str(self);
    }
}

#[cfg(feature = "document-read")]
impl Decode for String {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(r.str()?.to_string())
    }
}

#[cfg(feature = "document-write")]
impl Encode for std::sync::Arc<str> {
    fn encode(&self, w: &mut Writer) {
        w.str(self);
    }
}

#[cfg(feature = "document-read")]
impl Decode for std::sync::Arc<str> {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(std::sync::Arc::from(r.str()?))
    }
}

#[cfg(feature = "document-write")]
impl<T: Encode> Encode for Option<T> {
    fn encode(&self, w: &mut Writer) {
        match self {
            None => w.u8(0),
            Some(v) => {
                w.u8(1);
                v.encode(w);
            }
        }
    }
}

#[cfg(feature = "document-read")]
impl<T: Decode> Decode for Option<T> {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        match r.u8()? {
            0 => Ok(None),
            _ => Ok(Some(T::decode(r)?)),
        }
    }
}

#[cfg(feature = "document-write")]
impl<T: Encode> Encode for Vec<T> {
    fn encode(&self, w: &mut Writer) {
        w.varint(self.len() as u64);
        for v in self {
            v.encode(w);
        }
    }
}

#[cfg(feature = "document-read")]
impl<T: Decode> Decode for Vec<T> {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let n = r.count()?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(T::decode(r)?);
        }
        Ok(out)
    }
}

#[cfg(feature = "document-write")]
impl<T: Encode> Encode for [T] {
    fn encode(&self, w: &mut Writer) {
        w.varint(self.len() as u64);
        for v in self {
            v.encode(w);
        }
    }
}

#[cfg(feature = "document-write")]
impl<T: Encode> Encode for std::sync::Arc<[T]> {
    fn encode(&self, w: &mut Writer) {
        w.varint(self.len() as u64);
        for v in self.iter() {
            v.encode(w);
        }
    }
}

#[cfg(feature = "document-read")]
impl<T: Decode> Decode for std::sync::Arc<[T]> {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(std::sync::Arc::from(Vec::<T>::decode(r)?))
    }
}

#[cfg(feature = "document-write")]
impl<T: Encode> Encode for Box<T> {
    fn encode(&self, w: &mut Writer) {
        (**self).encode(w);
    }
}

#[cfg(feature = "document-read")]
impl<T: Decode> Decode for Box<T> {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(Box::new(T::decode(r)?))
    }
}

/// Fixed-length arrays are written without a count — the length is in
/// the type, so writing it would let a corrupt document disagree with
/// what the reader will build.
#[cfg(feature = "document-write")]
impl<T: Encode, const N: usize> Encode for [T; N] {
    fn encode(&self, w: &mut Writer) {
        for v in self {
            v.encode(w);
        }
    }
}

#[cfg(feature = "document-read")]
impl<T: Decode, const N: usize> Decode for [T; N] {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let mut out = Vec::with_capacity(N);
        for _ in 0..N {
            out.push(T::decode(r)?);
        }
        // `N` pushes produced `N` elements, so the conversion holds.
        out.try_into().map_err(|_| DocumentError::Invalid {
            what: "fixed-length array",
            why: format!("expected {N} elements"),
        })
    }
}

#[cfg(feature = "document-write")]
impl<A: Encode, B: Encode> Encode for (A, B) {
    fn encode(&self, w: &mut Writer) {
        self.0.encode(w);
        self.1.encode(w);
    }
}

#[cfg(feature = "document-read")]
impl<A: Decode, B: Decode> Decode for (A, B) {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok((A::decode(r)?, B::decode(r)?))
    }
}

/// Maps are written with their keys sorted, so the same plot always
/// produces the same bytes — a document can be diffed and cached by
/// hash.
#[cfg(feature = "document-write")]
impl<V: Encode> Encode for std::collections::HashMap<String, V> {
    fn encode(&self, w: &mut Writer) {
        let mut keys: Vec<&String> = self.keys().collect();
        keys.sort_unstable();
        w.varint(keys.len() as u64);
        for k in keys {
            k.encode(w);
            self[k].encode(w);
        }
    }
}

#[cfg(feature = "document-read")]
impl<V: Decode> Decode for std::collections::HashMap<String, V> {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let n = r.count()?;
        let mut out = std::collections::HashMap::with_capacity(n);
        for _ in 0..n {
            let k = String::decode(r)?;
            out.insert(k, V::decode(r)?);
        }
        Ok(out)
    }
}

// ─── The generating macro ────────────────────────────────────────────────────

/// Generate [`Encode`] / [`Decode`] for plain-data types.
///
/// Three forms — a struct with named fields, a newtype, and an enum with
/// explicit discriminants:
///
/// ```ignore
/// impl_codec! {
///     struct Palette { paper, ink, accent }
///
///     newtype FontWeight;
///
///     enum ThemeColor {
///         0 => Fixed(color),
///         1 => Paper,
///         4 => Mix(a, b, t, space),
///         6 => Sided { all, by_side },
///     }
/// }
/// ```
///
/// Fields and variant payloads are named, never typed: the type comes
/// from the definition by inference, so the invocation can't drift from
/// it without failing to compile.
///
/// Discriminants are written explicitly and are part of the file format.
/// Adding a variant means taking the next free number; renumbering an
/// existing one silently reinterprets every document already written.
macro_rules! impl_codec {
    () => {};

    // ── generic struct with named fields ──
    //
    // Listed before the non-generic forms so `Sided<T> { … }` isn't
    // first offered to a rule that expects `{` right after the name.
    (
        struct $ty:ident < $($gen:ident),+ $(,)? > { $($field:ident),+ $(,)? }
        $($rest:tt)*
    ) => {
        #[cfg(feature = "document-write")]
        impl<$($gen: Encode),+> Encode for $ty<$($gen),+> {
            fn encode(&self, w: &mut Writer) {
                $( self.$field.encode(w); )+
            }
        }

        #[cfg(feature = "document-read")]
        impl<$($gen: Decode),+> Decode for $ty<$($gen),+> {
            fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
                Ok($ty { $( $field: Decode::decode(r)?, )+ })
            }
        }

        impl_codec! { $($rest)* }
    };

    // ── generic enum with explicit discriminants ──
    (
        enum $ty:ident < $($gen:ident),+ $(,)? > {
            $(
                $tag:literal => $variant:ident
                    $( ( $($bind:ident),+ $(,)? ) )?
                    $( { $($vfield:ident),+ $(,)? } )?
            ),+ $(,)?
        }
        $($rest:tt)*
    ) => {
        #[cfg(feature = "document-write")]
        impl<$($gen: Encode),+> Encode for $ty<$($gen),+> {
            fn encode(&self, w: &mut Writer) {
                match self {
                    $(
                        $ty::$variant
                            $( ( $($bind),+ ) )?
                            $( { $($vfield),+ } )?
                        => {
                            w.varint($tag);
                            $( $( $bind.encode(w); )+ )?
                            $( $( $vfield.encode(w); )+ )?
                        }
                    )+
                }
            }
        }

        #[cfg(feature = "document-read")]
        impl<$($gen: Decode),+> Decode for $ty<$($gen),+> {
            fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
                let offset = r.pos();
                let tag = r.varint()?;
                Ok(match tag {
                    $(
                        $tag => $ty::$variant
                            $( ( $( $crate::document::codec::replace_expr!(
                                $bind,
                                Decode::decode(r)?
                            ) ),+ ) )?
                            $( { $( $vfield: Decode::decode(r)? ),+ } )?,
                    )+
                    other => {
                        return Err(DocumentError::BadDiscriminant {
                            type_name: stringify!($ty),
                            tag: other,
                            offset,
                        })
                    }
                })
            }
        }

        impl_codec! { $($rest)* }
    };

    // ── struct with named fields ──
    (
        struct $ty:ident { $($field:ident),+ $(,)? }
        $($rest:tt)*
    ) => {
        #[cfg(feature = "document-write")]
        impl Encode for $ty {
            fn encode(&self, w: &mut Writer) {
                $( self.$field.encode(w); )+
            }
        }

        #[cfg(feature = "document-read")]
        impl Decode for $ty {
            fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
                Ok($ty { $( $field: Decode::decode(r)?, )+ })
            }
        }

        impl_codec! { $($rest)* }
    };

    // ── newtype over a single encodable field ──
    (
        newtype $ty:ident;
        $($rest:tt)*
    ) => {
        #[cfg(feature = "document-write")]
        impl Encode for $ty {
            fn encode(&self, w: &mut Writer) {
                self.0.encode(w);
            }
        }

        #[cfg(feature = "document-read")]
        impl Decode for $ty {
            fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
                Ok($ty(Decode::decode(r)?))
            }
        }

        impl_codec! { $($rest)* }
    };

    // ── enum with explicit discriminants ──
    (
        enum $ty:ident {
            $(
                $tag:literal => $variant:ident
                    $( ( $($bind:ident),+ $(,)? ) )?
                    $( { $($vfield:ident),+ $(,)? } )?
            ),+ $(,)?
        }
        $($rest:tt)*
    ) => {
        #[cfg(feature = "document-write")]
        impl Encode for $ty {
            fn encode(&self, w: &mut Writer) {
                match self {
                    $(
                        $ty::$variant
                            $( ( $($bind),+ ) )?
                            $( { $($vfield),+ } )?
                        => {
                            w.varint($tag);
                            $( $( $bind.encode(w); )+ )?
                            $( $( $vfield.encode(w); )+ )?
                        }
                    )+
                }
            }
        }

        #[cfg(feature = "document-read")]
        impl Decode for $ty {
            fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
                let offset = r.pos();
                let tag = r.varint()?;
                Ok(match tag {
                    $(
                        $tag => $ty::$variant
                            $( ( $( $crate::document::codec::replace_expr!(
                                $bind,
                                Decode::decode(r)?
                            ) ),+ ) )?
                            $( { $( $vfield: Decode::decode(r)? ),+ } )?,
                    )+
                    other => {
                        return Err(DocumentError::BadDiscriminant {
                            type_name: stringify!($ty),
                            tag: other,
                            offset,
                        })
                    }
                })
            }
        }

        impl_codec! { $($rest)* }
    };
}

pub(crate) use {impl_codec, replace_expr};

// ─── Test support ────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "document-read", feature = "document-write"))]
pub(crate) mod test_support {
    use super::*;

    /// Encode `v`, decode it back, and assert the reader consumed
    /// exactly what the writer produced.
    ///
    /// Leftover bytes mean the two halves disagree about a value's
    /// extent — a bug that a value-equality check alone would miss,
    /// because the *next* value in a real document is what would be
    /// misread.
    pub(crate) fn roundtrip<T: Encode + Decode>(v: &T) -> T {
        let mut w = Writer::new();
        v.encode(&mut w);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        let out = T::decode(&mut r).expect("decoding what we just encoded");
        assert!(
            r.is_empty(),
            "decode left {} of {} bytes unread",
            r.remaining(),
            bytes.len()
        );
        out
    }

    /// [`roundtrip`] plus an equality check, for types with `PartialEq`.
    pub(crate) fn assert_roundtrip<T: Encode + Decode + PartialEq + std::fmt::Debug>(v: T) {
        let out = roundtrip(&v);
        assert_eq!(out, v);
    }

    /// [`roundtrip`] for values that reference the intern tables.
    ///
    /// Carries the tables the writer filled over to the reader, which is
    /// what the chunk container does with the table chunks — a bare
    /// `roundtrip` has nothing for an interned index to resolve against.
    pub(crate) fn roundtrip_interned<T: Encode + Decode>(v: &T) -> T {
        let mut w = Writer::new();
        v.encode(&mut w);
        let geometries = w.tables().geometries().to_vec();
        let sheets = w.tables().sheets().to_vec();
        let bytes = w.finish();

        let mut r = Reader::new(&bytes);
        r.tables_mut().set_geometries(geometries);
        r.tables_mut().set_sheets(sheets);
        let out = T::decode(&mut r).expect("decoding what we just encoded");
        assert!(
            r.is_empty(),
            "decode left {} of {} bytes unread",
            r.remaining(),
            bytes.len()
        );
        out
    }
}

#[cfg(all(test, feature = "document-read", feature = "document-write"))]
mod tests {
    use super::test_support::assert_roundtrip;
    use super::*;

    #[test]
    fn varints_round_trip_across_their_whole_range() {
        for v in [
            0u64,
            1,
            127,
            128,
            300,
            u64::from(u32::MAX),
            u64::MAX - 1,
            u64::MAX,
        ] {
            let mut w = Writer::new();
            w.varint(v);
            let bytes = w.finish();
            let mut r = Reader::new(&bytes);
            assert_eq!(r.varint().expect("well-formed varint"), v);
            assert!(r.is_empty());
        }
    }

    #[test]
    fn small_varints_cost_one_byte() {
        let mut w = Writer::new();
        w.varint(127);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn signed_varints_round_trip_either_side_of_zero() {
        for v in [0i64, -1, 1, -64, 63, i64::MIN, i64::MAX] {
            let mut w = Writer::new();
            w.varint_signed(v);
            let bytes = w.finish();
            let mut r = Reader::new(&bytes);
            assert_eq!(r.varint_signed().expect("well-formed varint"), v);
        }
    }

    #[test]
    fn small_negative_varints_stay_narrow() {
        let mut w = Writer::new();
        w.varint_signed(-1);
        assert_eq!(w.len(), 1, "zigzag should keep -1 to a single byte");
    }

    #[test]
    fn a_varint_that_never_terminates_is_rejected() {
        let bytes = vec![0x80u8; MAX_VARINT_BYTES + 2];
        let mut r = Reader::new(&bytes);
        assert!(matches!(
            r.varint(),
            Err(DocumentError::BadVarint { offset: 0 })
        ));
    }

    #[test]
    fn reading_past_the_end_reports_the_offset() {
        let bytes = [1u8, 2];
        let mut r = Reader::new(&bytes);
        assert!(matches!(
            r.take(5),
            Err(DocumentError::UnexpectedEof {
                offset: 0,
                wanted: 5,
                available: 2
            })
        ));
    }

    /// A corrupt length prefix must not become an allocation request.
    #[test]
    fn a_count_larger_than_the_input_is_rejected_before_allocating() {
        let mut w = Writer::new();
        w.varint(1_000_000);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert!(matches!(
            r.count(),
            Err(DocumentError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn invalid_utf8_in_a_string_reports_where_it_started() {
        let mut w = Writer::new();
        w.bytes(&[0xff, 0xfe]);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert!(matches!(r.str(), Err(DocumentError::BadUtf8 { offset: 0 })));
    }

    #[test]
    fn scalars_and_containers_round_trip() {
        assert_roundtrip(0u8);
        assert_roundtrip(u16::MAX);
        assert_roundtrip(u32::MAX);
        assert_roundtrip(i32::MIN);
        assert_roundtrip(1.5f32);
        assert_roundtrip(-0.25f64);
        assert_roundtrip(true);
        assert_roundtrip(false);
        assert_roundtrip('é');
        assert_roundtrip("hello".to_string());
        assert_roundtrip(std::sync::Arc::<str>::from("shared"));
        assert_roundtrip(Some(3u32));
        assert_roundtrip(Option::<u32>::None);
        assert_roundtrip(vec![1u32, 2, 3]);
        assert_roundtrip(Vec::<u32>::new());
        assert_roundtrip(Box::new(7u32));
        assert_roundtrip([1u32, 2, 3, 4]);
        assert_roundtrip((1u32, "two".to_string()));
    }

    /// Non-finite floats reach the wire unchanged — a NaN position is
    /// how the geom layer already spells "skip this row".
    #[test]
    fn non_finite_floats_survive() {
        let out = super::test_support::roundtrip(&f64::NAN);
        assert!(out.is_nan());
        assert_roundtrip(f64::INFINITY);
        assert_roundtrip(f64::NEG_INFINITY);
        assert_roundtrip(-0.0f64);
    }

    #[test]
    fn maps_round_trip_and_write_the_same_bytes_whatever_the_insertion_order() {
        use std::collections::HashMap;

        let mut a: HashMap<String, u32> = HashMap::new();
        a.insert("one".into(), 1);
        a.insert("two".into(), 2);
        a.insert("three".into(), 3);

        let mut b: HashMap<String, u32> = HashMap::new();
        b.insert("three".into(), 3);
        b.insert("one".into(), 1);
        b.insert("two".into(), 2);

        let bytes_of = |m: &HashMap<String, u32>| {
            let mut w = Writer::new();
            m.encode(&mut w);
            w.finish()
        };
        assert_eq!(bytes_of(&a), bytes_of(&b));
        assert_eq!(super::test_support::roundtrip(&a), a);
    }

    #[test]
    fn a_chunk_length_can_be_backfilled_once_its_extent_is_known() {
        let mut w = Writer::new();
        let at = w.len();
        w.u32_fixed(0);
        w.varint(1);
        w.varint(2);
        let body = (w.len() - at - 4) as u32;
        w.patch_u32_at(at, body);

        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.u32_fixed().expect("length prefix"), 2);
    }
}
