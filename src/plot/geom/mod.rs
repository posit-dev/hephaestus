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
//! [`Scale`]. The context plumbs through a [`ScaleResolver`]: in the
//! orchestrator this is the binding map + scale registry; in tests it
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

pub mod ellipse;
pub mod line;
pub(crate) mod marks;
pub mod point;
pub mod polygon;
pub mod rect;
pub mod resolve;
pub mod ribbon;
pub mod segment;
pub mod state;
#[cfg(feature = "text")]
pub mod text;
#[cfg(feature = "text")]
pub mod text_fit;
#[cfg(feature = "text")]
pub mod text_path;
pub mod wedge;

pub use ellipse::EllipseGeom;
pub use line::LineGeom;
pub use point::PointGeom;
pub use polygon::PolygonGeom;
pub use rect::RectGeom;
pub use ribbon::RibbonGeom;
pub use segment::SegmentGeom;
pub use state::{GeomState, KeysStrategy};
#[cfg(feature = "text")]
pub use text::TextGeom;
#[cfg(feature = "text")]
pub use text_fit::TextFitGeom;
#[cfg(feature = "text")]
pub use text_path::TextPathGeom;
pub use wedge::WedgeGeom;

// ─── Channel ─────────────────────────────────────────────────────────────────

/// A geom channel — a single constant value applied to every row, or a
/// typed columnar data series with one value per row. The `Raw*`
/// variants bypass any [`Scale`] bound to the channel name, letting
/// callers supply values that are already in the output type the geom
/// expects (panel fraction for positions, [`Color`] for colors, pt for
/// sizes, dash-pattern for linetype, etc.).
///
/// The default scaled variants are produced by the `Into<Channel>`
/// blanket on scalars and `Vec<T>`. The unscaled variants are produced
/// by wrapping the value in [`Raw`]: `Raw(0.5_f64)` →
/// `Channel::RawConstant`, `Raw(vec![0.1, 0.5, 0.9])` →
/// `Channel::RawData`.
#[derive(Clone, Debug)]
pub enum Channel {
    Constant(Value),
    Data(DataColumn),
    /// A constant value, **bypassing** any scale bound to the channel
    /// name. Used as-is at draw time — must already be in the output
    /// type the geom expects (e.g. a panel fraction in `[0, 1]` for a
    /// position channel, or a [`Color`] for a color channel). Values
    /// outside the usual range are accepted; positions outside `[0, 1]`
    /// produce drawing outside the panel that the panel clip handles.
    RawConstant(Value),
    /// A per-row column, **bypassing** any scale bound to the channel
    /// name. Each row's value is used as-is at draw time. Same
    /// output-type contract as [`Channel::RawConstant`].
    RawData(DataColumn),
}

impl Channel {
    /// `true` if this channel carries per-row data (scaled or raw).
    pub fn is_data(&self) -> bool {
        matches!(self, Channel::Data(_) | Channel::RawData(_))
    }

    /// Length of the data column if this is a per-row variant; `None`
    /// for the constant variants.
    pub fn data_len(&self) -> Option<usize> {
        match self {
            Channel::Constant(_) | Channel::RawConstant(_) => None,
            Channel::Data(c) | Channel::RawData(c) => Some(c.len()),
        }
    }
}

/// Marker wrapper that turns the inner value into a scale-bypassing
/// [`Channel::RawConstant`] / [`Channel::RawData`] via `Into<Channel>`.
///
/// ```ignore
/// PointGeom::builder()
///     .set("x", xs)                      // scaled through "x" binding
///     .set("y", Raw(prescaled_y_fracs))  // bypass the "y" binding
///     .set("fill", Raw(rgb8(220, 60, 60)))
///     .build();
/// ```
///
/// `Raw(vec![...])` produces a [`Channel::RawData`]; `Raw(scalar)`
/// produces a [`Channel::RawConstant`].
#[derive(Clone, Debug)]
pub struct Raw<T>(pub T);

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

// ── Raw<T> → Channel ──────────────────────────────────────────────────
//
// Mirrors the scaled `From` impls above but produces the `Raw*`
// variants. Same coherence pattern — separate impls per concrete vec /
// scalar type avoids the blanket overlap.

impl From<Raw<DataColumn>> for Channel {
    fn from(r: Raw<DataColumn>) -> Self {
        Channel::RawData(r.0)
    }
}

impl From<Raw<Value>> for Channel {
    fn from(r: Raw<Value>) -> Self {
        Channel::RawConstant(r.0)
    }
}

macro_rules! impl_channel_from_raw_vec {
    ($t:ty) => {
        impl From<Raw<Vec<$t>>> for Channel {
            fn from(r: Raw<Vec<$t>>) -> Self {
                Channel::RawData(r.0.into())
            }
        }
    };
}

impl_channel_from_raw_vec!(f64);
impl_channel_from_raw_vec!(f32);
impl_channel_from_raw_vec!(i32);
impl_channel_from_raw_vec!(i64);
impl_channel_from_raw_vec!(bool);
impl_channel_from_raw_vec!(&'static str);
impl_channel_from_raw_vec!(String);
impl_channel_from_raw_vec!(Arc<str>);
impl_channel_from_raw_vec!(Color);
impl_channel_from_raw_vec!(Date);
impl_channel_from_raw_vec!(DateTime);
impl_channel_from_raw_vec!(Time);
impl_channel_from_raw_vec!(Duration);

impl From<Raw<std::ops::Range<i64>>> for Channel {
    fn from(r: Raw<std::ops::Range<i64>>) -> Self {
        Channel::RawData(r.0.into())
    }
}

macro_rules! impl_channel_from_raw_scalar {
    ($t:ty) => {
        impl From<Raw<$t>> for Channel {
            fn from(r: Raw<$t>) -> Self {
                Channel::RawConstant(Value::from(r.0))
            }
        }
    };
}

impl_channel_from_raw_scalar!(f64);
impl_channel_from_raw_scalar!(f32);
impl_channel_from_raw_scalar!(i32);
impl_channel_from_raw_scalar!(i64);
impl_channel_from_raw_scalar!(bool);
impl_channel_from_raw_scalar!(&'static str);
impl_channel_from_raw_scalar!(String);
impl_channel_from_raw_scalar!(Arc<str>);
impl_channel_from_raw_scalar!(Color);
impl_channel_from_raw_scalar!(Date);
impl_channel_from_raw_scalar!(DateTime);
impl_channel_from_raw_scalar!(Time);
impl_channel_from_raw_scalar!(Duration);

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

pub mod linetype;

/// Coarse output-type hint for [`ChannelDecl::expected_output`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedOutput {
    /// Position fraction (`[0, 1]`) or absolute pt value (size, linewidth).
    Numbers,
    /// Fill / stroke color.
    Colors,
    /// String channel (e.g. shape name, label text).
    Strings,
    /// Dash-pattern channel (linetype).
    Linetypes,
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

/// A direct channel-name → `&Scale` map. Used in stand-alone tests; the
/// orchestrator's binding-lookup path also implements [`ScaleResolver`].
pub struct DirectScaleResolver<'a> {
    scales: HashMap<&'static str, &'a Scale>,
}

impl<'a> DirectScaleResolver<'a> {
    /// Empty resolver — no channels bound. Build it up with
    /// [`Self::with`].
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
/// Picking is driven entirely by each geom's `"pick_id"` channel: the
/// resolved value is the 24-bit id reported by
/// [`pick_at`](crate::backend::vello::VelloRenderer::pick_at). Unset
/// channel → `PickId::Skip` for the whole geom (no participation in
/// the hitmap); resolved value `0` → `PickId::Block` (occlude without
/// reporting); otherwise → `PickId::Id(value)`. The context does not
/// allocate or track ids — the user owns the namespace.
pub struct GeomContext<'a> {
    pub panel_rect: Rect,
    pub dpi: f64,
    pub shapes: &'a ShapeRegistry,
    pub scales: &'a dyn ScaleResolver,
    /// Coordinate projection. Geoms route their final fraction→pixel
    /// conversion through [`Projection::project_to_panel_px`] so
    /// non-Cartesian variants (polar, future ternary) drop in without
    /// touching geom code. Defaults to `Projection::Cartesian` for
    /// callers that construct a context via [`Self::new`].
    pub projection: &'a crate::plot::projection::Projection,
}

impl<'a> GeomContext<'a> {
    /// Construct a per-draw context with the default Cartesian
    /// projection. Use [`Self::with_projection`] to override.
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
            projection: &crate::plot::projection::Projection::Cartesian,
        }
    }

    /// Construct a per-draw context with an explicit projection. Used
    /// by the orchestrator (`Plot::draw_panel_into`) so the plot's
    /// configured projection threads to every geom.
    pub fn with_projection(
        panel_rect: Rect,
        dpi: f64,
        shapes: &'a ShapeRegistry,
        scales: &'a dyn ScaleResolver,
        projection: &'a crate::plot::projection::Projection,
    ) -> Self {
        Self {
            panel_rect,
            dpi,
            shapes,
            scales,
            projection,
        }
    }

    /// Resolve a channel name to a scale, if one is bound.
    pub fn scale_for(&self, channel: &str) -> Option<&Scale> {
        self.scales.scale_for(channel)
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
    /// Number of rows the keys cover. `n` for `Positional(n)`; the
    /// underlying column's length for `Explicit`.
    pub fn len(&self) -> usize {
        match self {
            Keys::Positional(n) => *n,
            Keys::Explicit(col) => col.len(),
        }
    }

    /// True when no rows are present.
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
        DataColumn::Linetype(_) => DataColumn::Linetype(Vec::new()),
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
    /// Borrow the shared geom state. Drives the default impls below.
    fn state(&self) -> &GeomState;

    /// Mutably borrow the shared geom state. Drives the default impls below.
    fn state_mut(&mut self) -> &mut GeomState;

    /// Emit scene primitives for the current frame. The only mandatory
    /// geom-specific behaviour.
    fn draw(&self, scene: &mut dyn SceneBuilder, ctx: &GeomContext<'_>);

    /// Upcast to `&mut dyn Any` so the Plot orchestrator can downcast
    /// to a concrete geom type in [`Plot::update_geom`](crate::plot::Plot::update_geom).
    /// Each impl is a one-liner: `fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }`.
    /// Not a default impl because trait-default `Any` upcasts erase the
    /// concrete type.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// Channels this geom recognises. Defaults to the catalog-filtered
    /// list built at construction time and cached in [`GeomState`].
    fn declared_channels(&self) -> &[ChannelDecl] {
        &self.state().declared
    }

    /// Number of pickable rows this geom holds. By default the row count.
    fn len(&self) -> usize {
        self.state().len()
    }

    fn is_empty(&self) -> bool {
        self.state().is_empty()
    }

    /// Number of pickable **marks** this geom will draw — one ticket per
    /// mark in the pick table. For "one mark per row" geoms (PointGeom)
    /// this is the same as [`Self::len`] and the default impl suffices.
    /// Multi-row-per-mark geoms (LineGeom, future AreaGeom, ...)
    /// override this to return their mark count.
    fn mark_count(&self) -> usize {
        self.len()
    }

    /// Rebuild the enter / update / exit diff sets against the previous
    /// frame's snapshot. The default forwards to
    /// [`GeomState::rebuild_diff_against_previous`]; multi-row-per-mark
    /// geoms (LineGeom) override to also rebuild their per-mark cache.
    fn rebuild_diff_against_previous(&mut self) {
        self.state_mut().rebuild_diff_against_previous();
    }

    /// Drop any per-geom caches whose validity depends on the current
    /// state. Called from the inherent `update` method (generated by the
    /// `impl_geom_inherents!` / `impl_geom_inherents_grouped!` macros)
    /// after the state has been replaced. Per-row geoms have no caches
    /// and inherit the empty default; multi-row-per-mark geoms
    /// (LineGeom, PolygonGeom) override to clear their mark-layout
    /// caches.
    fn invalidate_caches(&mut self) {}
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

    // ── Raw<T> → Channel ──

    #[test]
    fn raw_scalar_into_channel_is_raw_constant() {
        let c: Channel = Raw(5.0_f64).into();
        assert!(matches!(c, Channel::RawConstant(_)));
        assert!(!c.is_data());
        assert!(c.data_len().is_none());
    }

    #[test]
    fn raw_vec_into_channel_is_raw_data() {
        let c: Channel = Raw(vec![0.1_f64, 0.5, 0.9]).into();
        assert!(matches!(c, Channel::RawData(_)));
        assert!(c.is_data());
        assert_eq!(c.data_len(), Some(3));
    }

    #[test]
    fn raw_color_into_channel_is_raw_constant() {
        let c: Channel = Raw(Color::new([1.0, 0.0, 0.0, 1.0])).into();
        assert!(matches!(c, Channel::RawConstant(_)));
    }

    #[test]
    fn raw_str_vec_into_channel_is_raw_data() {
        let c: Channel = Raw(vec!["a", "b"]).into();
        assert!(matches!(c, Channel::RawData(_)));
        assert_eq!(c.data_len(), Some(2));
    }
}
