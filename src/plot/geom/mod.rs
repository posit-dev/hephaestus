//! Geoms — the vectorised drawing primitives that consume per-channel
//! data + a [`Scale`](crate::plot::scale::Scale) registry and emit scene
//! calls.
//!
//! The trait surface lives here; concrete geoms (e.g. [`point::PointGeom`])
//! live in submodules.
//!
//! ### Channel resolution
//!
//! A geom doesn't store its scales directly. Instead it declares the
//! channel names it consumes ([`Geom::declared_channels`]) and, at draw
//! time, asks the [`GeomContext`] to resolve each channel name to a
//! [`Scale`]. The context plumbs through a [`ScaleResolver`] — in v1.5+
//! this is the orchestrator's binding map + scale registry; in tests it
//! can be a hand-built [`DirectScaleResolver`].
//!
//! ### Channel data
//!
//! Each channel is either [`Channel::Constant`] (one value applied to
//! every row) or [`Channel::Data`] (one value per row, typed via
//! [`DataColumn`]). The hot draw loop matches on the column variant
//! **once** at the top and reads typed slices in the inner per-row body,
//! so per-row code stays monomorphic.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::color::Color;
use crate::geometry::Rect;
use crate::plot::scale::Scale;
use crate::plot::value::{DataColumn, Date, DateTime, Duration, Time, Value};
use crate::scene::SceneBuilder;
use crate::shape::ShapeRegistry;

pub mod point;

pub use point::PointGeom;

// ─── Channel ─────────────────────────────────────────────────────────────────

/// A geom channel — either a single constant value applied to every row,
/// or a typed columnar data series with one value per row.
#[derive(Clone, Debug)]
pub enum Channel {
    Constant(Value),
    Data(DataColumn),
}

impl Channel {
    /// `true` if this channel is data-bound (one value per row).
    pub fn is_data(&self) -> bool {
        matches!(self, Channel::Data(_))
    }

    /// Length of the data column if this is a [`Channel::Data`]; `None`
    /// for [`Channel::Constant`].
    pub fn data_len(&self) -> Option<usize> {
        match self {
            Channel::Constant(_) => None,
            Channel::Data(c) => Some(c.len()),
        }
    }
}

// ── `Into<Channel>` blanket — coerce Vecs to Data, scalars to Constant ──
//
// The set of types accepted is finite: anything that converts to
// `DataColumn` (vector inputs) lands in `Channel::Data`; anything that
// converts to `Value` (scalar inputs) lands in `Channel::Constant`.
// Concrete impls per type avoid the coherence overlap that a blanket
// `impl<T: Into<DataColumn>> From<T> for Channel` would have with
// `impl<T: Into<Value>> From<T> for Channel`.

impl From<DataColumn> for Channel {
    fn from(col: DataColumn) -> Self {
        Channel::Data(col)
    }
}

impl From<Value> for Channel {
    fn from(v: Value) -> Self {
        Channel::Constant(v)
    }
}

macro_rules! impl_channel_from_vec {
    ($t:ty) => {
        impl From<Vec<$t>> for Channel {
            fn from(v: Vec<$t>) -> Self {
                Channel::Data(v.into())
            }
        }
    };
}

impl_channel_from_vec!(f64);
impl_channel_from_vec!(f32);
impl_channel_from_vec!(i32);
impl_channel_from_vec!(i64);
impl_channel_from_vec!(bool);
impl_channel_from_vec!(&'static str);
impl_channel_from_vec!(String);
impl_channel_from_vec!(Arc<str>);
impl_channel_from_vec!(Color);
impl_channel_from_vec!(Date);
impl_channel_from_vec!(DateTime);
impl_channel_from_vec!(Time);
impl_channel_from_vec!(Duration);

impl From<std::ops::Range<i64>> for Channel {
    fn from(r: std::ops::Range<i64>) -> Self {
        Channel::Data(r.into())
    }
}

macro_rules! impl_channel_from_scalar {
    ($t:ty) => {
        impl From<$t> for Channel {
            fn from(v: $t) -> Self {
                Channel::Constant(Value::from(v))
            }
        }
    };
}

impl_channel_from_scalar!(f64);
impl_channel_from_scalar!(f32);
impl_channel_from_scalar!(i32);
impl_channel_from_scalar!(i64);
impl_channel_from_scalar!(bool);
impl_channel_from_scalar!(&'static str);
impl_channel_from_scalar!(String);
impl_channel_from_scalar!(Arc<str>);
impl_channel_from_scalar!(Color);
impl_channel_from_scalar!(Date);
impl_channel_from_scalar!(DateTime);
impl_channel_from_scalar!(Time);
impl_channel_from_scalar!(Duration);

// ─── ChannelDecl ─────────────────────────────────────────────────────────────

/// What a geom declares about each channel it consumes — used by
/// `view.validate()` (Phase 7) to flag bindings whose scale output type
/// doesn't match the channel's expectation, and by Phase 6 to know which
/// channels are mandatory vs optional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelDecl {
    /// Channel name (e.g. `"x"`, `"fill"`, `"linewidth"`).
    pub name: &'static str,
    /// `true` if the geom holds a [`Channel::Data`] for this channel.
    /// `false` if it's a [`Channel::Constant`] (or unset entirely) — no
    /// scale needed in that case.
    pub data_bound: bool,
    /// What kind of output the geom expects the bound scale to produce.
    /// Used for validation only — not enforced at draw time.
    pub expected_output: ExpectedOutput,
}

/// Coarse output-type hint for [`ChannelDecl::expected_output`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedOutput {
    /// Position fraction (`[0, 1]`) or absolute pt value (size, linewidth).
    Numbers,
    /// Fill / stroke color.
    Colors,
    /// String channel (e.g. shape name, label text).
    Strings,
    /// No constraint.
    Any,
}

// ─── Scale resolution ────────────────────────────────────────────────────────

/// Resolves channel names to scales. Implementations: the Phase 6 Plot +
/// orchestrator combo (channel name → binding → scale registry), or
/// stand-alone test helpers like [`DirectScaleResolver`].
pub trait ScaleResolver {
    /// Return the scale bound to `channel`, or `None` if no scale is
    /// configured (e.g. the geom is using a constant for this channel,
    /// or this is a position channel with no transform).
    fn scale_for(&self, channel: &str) -> Option<&Scale>;
}

/// A direct channel-name → `&Scale` map. Used in Phase 5 stand-alone
/// tests; Phase 6 wires the orchestrator's binding-lookup path through
/// the same [`ScaleResolver`] trait.
pub struct DirectScaleResolver<'a> {
    scales: HashMap<&'static str, &'a Scale>,
}

impl<'a> DirectScaleResolver<'a> {
    pub fn new() -> Self {
        Self {
            scales: HashMap::new(),
        }
    }

    /// Bind `channel` to `scale`. Chainable.
    pub fn with(mut self, channel: &'static str, scale: &'a Scale) -> Self {
        self.scales.insert(channel, scale);
        self
    }
}

impl<'a> Default for DirectScaleResolver<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> ScaleResolver for DirectScaleResolver<'a> {
    fn scale_for(&self, channel: &str) -> Option<&Scale> {
        self.scales.get(channel).copied()
    }
}

// ─── GeomContext ─────────────────────────────────────────────────────────────

/// Per-draw context passed to [`Geom::draw`]. Carries the panel rect (in
/// pixels, output of the composition solver), the dpi, the shape
/// registry, and the channel→scale resolver.
///
/// `ticket_base` is the geom's starting ticket index into the
/// orchestrator-owned [`PickTable`](crate::plot::PickTable). When set,
/// geoms emit `PickId::Id(ticket_base + row + 1)` for each per-row draw
/// call (the `+ 1` keeps `PickId::Id(0)` reserved for
/// [`PickId::Block`](crate::pick::PickId::Block)). When `None`, geoms
/// emit `PickId::Skip` (default for stand-alone tests / non-pickable
/// renders).
///
/// The 24-bit `PickId` budget caps the table at ~16M tickets per
/// render. If `ticket_base + row + 1` exceeds that, `pick_id_for_row`
/// falls back to `Skip` (no panic, no overflow).
pub struct GeomContext<'a> {
    pub panel_rect: Rect,
    pub dpi: f64,
    pub shapes: &'a ShapeRegistry,
    pub scales: &'a dyn ScaleResolver,
    pub ticket_base: Option<u32>,
}

/// Maximum valid pick ticket — capped by the 24-bit `PickId` budget.
const MAX_TICKET: u32 = 0xFFFFFF;

impl<'a> GeomContext<'a> {
    pub fn new(
        panel_rect: Rect,
        dpi: f64,
        shapes: &'a ShapeRegistry,
        scales: &'a dyn ScaleResolver,
    ) -> Self {
        Self {
            panel_rect,
            dpi,
            shapes,
            scales,
            ticket_base: None,
        }
    }

    pub fn scale_for(&self, channel: &str) -> Option<&Scale> {
        self.scales.scale_for(channel)
    }

    /// Build a `PickId` for the i-th row of this geom. Falls back to
    /// `PickId::Skip` when no `ticket_base` is set (stand-alone
    /// drawing) or when the resulting ticket exceeds the 24-bit budget.
    pub fn pick_id_for_row(&self, row: usize) -> crate::pick::PickId {
        let base = match self.ticket_base {
            None => return crate::pick::PickId::Skip,
            Some(b) => b,
        };
        let row_u32 = match u32::try_from(row) {
            Ok(r) => r,
            Err(_) => return crate::pick::PickId::Skip,
        };
        // ticket = base + row + 1; saturate-and-skip on overflow.
        let ticket = match base.checked_add(row_u32).and_then(|t| t.checked_add(1)) {
            Some(t) if t <= MAX_TICKET => t,
            _ => return crate::pick::PickId::Skip,
        };
        crate::pick::PickId::Id(ticket)
    }
}

// ─── Keys ────────────────────────────────────────────────────────────────────

/// The key column of a geom — used for identity-based diff matching
/// (D3-style enter / update / exit).
///
/// When the user supplies an explicit key column via [`GeomBuilder::keys`],
/// the geom stores it as [`Keys::Explicit`] and diffs through the columnar
/// hash path. When the user doesn't, the geom synthesises positional keys
/// — but only conceptually: [`Keys::Positional`] stores just the row count,
/// not an N-length `Vec<i64>` of `(0..N)`. The positional diff path
/// ([`diff_positional`](crate::plot::diff::diff_positional)) only needs
/// the length, so materialising the integer vector would be pure overhead.
///
/// `len()` returns `n` for `Positional(n)` and the underlying column's
/// length for `Explicit(col)`. `empty_like()` produces a length-zero
/// counterpart used to seed first-frame diff state (so the initial diff
/// produces all-enter).
#[derive(Clone, Debug)]
pub enum Keys {
    /// Conceptual `(0..n)` key column — stored as just the row count
    /// `n`. Zero allocation.
    Positional(usize),
    /// User-supplied key column. Carries identity for diff matching.
    Explicit(DataColumn),
}

impl Keys {
    pub fn len(&self) -> usize {
        match self {
            Keys::Positional(n) => *n,
            Keys::Explicit(col) => col.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `true` if the user supplied an explicit key column. Diff matching
    /// uses identity for explicit keys and position for positional keys.
    pub fn is_explicit(&self) -> bool {
        matches!(self, Keys::Explicit(_))
    }

    /// Length-zero counterpart of the same variant. Used to seed the
    /// first-frame `prev_keys` snapshot so diff produces all-enter on
    /// the first draw.
    pub fn empty_like(&self) -> Keys {
        match self {
            Keys::Positional(_) => Keys::Positional(0),
            Keys::Explicit(col) => Keys::Explicit(empty_datacolumn_like(col)),
        }
    }
}

/// Same DataColumn variant as `col`, but length 0. Shared between
/// [`Keys::empty_like`] and geom-internal channel rotation.
pub(crate) fn empty_datacolumn_like(col: &DataColumn) -> DataColumn {
    match col {
        DataColumn::F64(_) => DataColumn::F64(Vec::new()),
        DataColumn::F32(_) => DataColumn::F32(Vec::new()),
        DataColumn::I32(_) => DataColumn::I32(Vec::new()),
        DataColumn::I64(_) => DataColumn::I64(Vec::new()),
        DataColumn::Bool(_) => DataColumn::Bool(Vec::new()),
        DataColumn::String(_) => DataColumn::String(Vec::new()),
        DataColumn::Color(_) => DataColumn::Color(Vec::new()),
        DataColumn::Date(_) => DataColumn::Date(Vec::new()),
        DataColumn::DateTime(_) => DataColumn::DateTime(Vec::new()),
        DataColumn::Time(_) => DataColumn::Time(Vec::new()),
        DataColumn::Duration(_) => DataColumn::Duration(Vec::new()),
    }
}

// ─── GeomBuilder + BuildableGeom ─────────────────────────────────────────────

/// Generic, geom-agnostic builder. Holds the union of state every
/// buildable geom needs (an optional key column + a `HashMap` of named
/// channels) and exposes the canonical `keys` / `set` / `build` methods.
/// Geom-specific validation, defaults, and field derivation live entirely
/// inside the [`BuildableGeom::build_from`] impl on each concrete geom —
/// there's no per-geom builder type to define.
///
/// Methods take `&mut self` and return `&mut Self` so they're equally
/// fluent in two contexts:
///
/// ```ignore
/// // Initial construction (auto-ref the rvalue):
/// let g = PointGeom::builder()
///     .set("x", xs)
///     .set("y", ys)
///     .build();
///
/// // Inside an `update` closure (the builder is already borrowed):
/// g.update(|b| {
///     b.set("x", new_xs);
///     b.set("y", new_ys);
/// });
/// ```
pub struct GeomBuilder<G: BuildableGeom> {
    keys: Option<DataColumn>,
    channels: HashMap<String, Channel>,
    _phantom: PhantomData<fn() -> G>,
}

impl<G: BuildableGeom> Default for GeomBuilder<G> {
    fn default() -> Self {
        Self {
            keys: None,
            channels: HashMap::new(),
            _phantom: PhantomData,
        }
    }
}

impl<G: BuildableGeom> GeomBuilder<G> {
    /// Empty builder. Typically callers go through `Geom::builder()`
    /// (e.g. [`PointGeom::builder`]) to keep the type parameter inferred.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from existing parts — used by [`PointGeom::update`] to
    /// pre-populate the builder with the current state before running
    /// the user's closure.
    pub(crate) fn from_parts(keys: Option<DataColumn>, channels: HashMap<String, Channel>) -> Self {
        Self {
            keys,
            channels,
            _phantom: PhantomData,
        }
    }

    /// Supply an explicit key column. Optional — geoms without keys
    /// synthesise positional indices `(0..n)`.
    pub fn keys(&mut self, keys: impl Into<DataColumn>) -> &mut Self {
        self.keys = Some(keys.into());
        self
    }

    /// Bind a channel. The Data-vs-Constant variant is inferred from the
    /// value's type via [`From<T> for Channel`].
    pub fn set(&mut self, channel: impl Into<String>, value: impl Into<Channel>) -> &mut Self {
        self.channels.insert(channel.into(), value.into());
        self
    }

    /// Finalise. Delegates to the geom's [`BuildableGeom::build_from`]
    /// impl, which is where geom-specific validation and defaults live.
    /// Consumes the builder via `mem::take` so it can be called through
    /// `&mut self` (which keeps chaining off rvalues working).
    pub fn build(&mut self) -> G {
        G::build_from(std::mem::take(self))
    }

    /// Destructure into `(keys, channels)`. Implementations of
    /// [`BuildableGeom::build_from`] use this to take ownership of the
    /// builder's state in a single move.
    pub fn into_parts(self) -> (Option<DataColumn>, HashMap<String, Channel>) {
        (self.keys, self.channels)
    }
}

/// A geom that can be constructed from a [`GeomBuilder`]. Geom-specific
/// validation, default-injection, and channel-declaration computation
/// live entirely in [`BuildableGeom::build_from`] — no per-geom builder
/// type to define.
pub trait BuildableGeom: Geom + Sized {
    /// Validate the builder's state and construct a concrete geom.
    /// Panics on structural errors (missing required channels, length
    /// mismatch, wrong column variants) — the builder is the validation
    /// gate, not the runtime geom.
    fn build_from(builder: GeomBuilder<Self>) -> Self;
}

// ─── Geom trait ──────────────────────────────────────────────────────────────

/// What every geom implements. Three responsibilities:
///
/// 1. **Declare** the channels it consumes ([`Geom::declared_channels`]),
///    so the orchestrator can validate bindings and the plot anatomy
///    knows where to put axis / legend chrome.
/// 2. **Rebuild diff state** when its data has changed
///    ([`Geom::rebuild_diff_against_previous`]) — runs lazily before
///    [`Geom::draw`] when the geom is marked dirty.
/// 3. **Draw** the scene primitives for the current frame
///    ([`Geom::draw`]).
pub trait Geom: 'static {
    fn draw(&self, scene: &mut dyn SceneBuilder, ctx: &GeomContext<'_>);
    fn declared_channels(&self) -> &[ChannelDecl];
    fn rebuild_diff_against_previous(&mut self);

    /// Number of pickable rows this geom will draw. Used by
    /// [`Plot::draw_panel_into`](crate::plot::Plot::draw_panel_into) to
    /// reserve a contiguous range of pick tickets per geom before
    /// drawing. Geoms with no picking surface return 0.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Upcast to `&mut dyn Any` so the Plot orchestrator can downcast
    /// to a concrete geom type in [`Plot::update_geom`](crate::plot::Plot::update_geom).
    /// Each impl is a one-liner: `fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }`.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::scale;

    #[test]
    fn direct_resolver_returns_bound_scale() {
        let s = scale::continuous(0.0..=1.0);
        let resolver = DirectScaleResolver::new().with("x", &s);
        assert!(resolver.scale_for("x").is_some());
        assert!(resolver.scale_for("y").is_none());
    }

    #[test]
    fn channel_constant_is_not_data() {
        let c = Channel::Constant(Value::Number(1.0));
        assert!(!c.is_data());
        assert!(c.data_len().is_none());
    }

    #[test]
    fn channel_data_len() {
        let c = Channel::Data(vec![1.0_f64, 2.0, 3.0].into());
        assert!(c.is_data());
        assert_eq!(c.data_len(), Some(3));
    }

    // ── Channel inference via Into<Channel> ──

    #[test]
    fn vec_f64_into_channel_is_data() {
        let c: Channel = vec![1.0_f64, 2.0].into();
        assert!(matches!(c, Channel::Data(_)));
    }

    #[test]
    fn scalar_into_channel_is_constant() {
        let c: Channel = 5.0_f64.into();
        assert!(matches!(c, Channel::Constant(_)));
    }

    #[test]
    fn static_str_into_channel_is_constant() {
        let c: Channel = "circle".into();
        assert!(matches!(c, Channel::Constant(_)));
    }

    #[test]
    fn vec_str_into_channel_is_data() {
        let c: Channel = vec!["a", "b"].into();
        assert!(matches!(c, Channel::Data(_)));
    }

    #[test]
    fn color_into_channel_is_constant() {
        let c: Channel = Color::new([1.0, 0.0, 0.0, 1.0]).into();
        assert!(matches!(c, Channel::Constant(_)));
    }

    #[test]
    fn vec_color_into_channel_is_data() {
        let c: Channel = vec![Color::new([1.0, 0.0, 0.0, 1.0])].into();
        assert!(matches!(c, Channel::Data(_)));
    }
}
