//! Font specification — full modern font axis surface (weight, width,
//! style with oblique angle, OpenType features, variable-font
//! variations).
//!
//! Each field is `Option<>` so it cascades independently through the
//! element hierarchy: a child can override `weight` while inheriting
//! `family`, etc. List fields (`features`, `variations`) merge — child
//! entries replace parent entries with the same tag; other parent
//! entries carry through.

/// Sparse font spec — every field cascades independently. `None` walks
/// up the inheritance chain; a `Some` at any level wins.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FontSpec {
    /// Font family or fallback chain. Generic families (`Serif`,
    /// `SansSerif`, …) are resolved by the host shaper.
    pub family: Option<FontFamily>,
    /// Weight axis — numeric (`FontWeight(450)`) or named constant
    /// (`FontWeight::REGULAR`).
    pub weight: Option<FontWeight>,
    /// Width axis — semantic constants from `UltraCondensed` to
    /// `UltraExpanded`.
    pub width: Option<FontWidth>,
    /// Style axis — upright, italic, or oblique-with-angle.
    pub style: Option<FontStyle>,
    /// OpenType feature toggles (`liga`, `kern`, `tnum`, `ss01`, …).
    /// Merged with parent by tag.
    pub features: Vec<FontFeature>,
    /// Variable-font variation axes (`wght`, `wdth`, `slnt`, `opsz`).
    /// Merged with parent by tag.
    pub variations: Vec<FontVariation>,
}

impl FontSpec {
    /// Merge `over` into `self`: `Some` fields on `over` win; lists
    /// merge by tag (child replaces parent entries with the same
    /// tag).
    pub fn cascade(&self, over: &FontSpec) -> FontSpec {
        FontSpec {
            family: over.family.clone().or_else(|| self.family.clone()),
            weight: over.weight.or(self.weight),
            width: over.width.or(self.width),
            style: over.style.or(self.style),
            features: merge_by_tag(&self.features, &over.features, |f| f.tag),
            variations: merge_by_tag(&self.variations, &over.variations, |v| v.tag),
        }
    }
}

/// Font family or fallback chain.
#[derive(Debug, Clone, PartialEq)]
pub enum FontFamily {
    /// One or more named families, in fallback order.
    Named(Vec<String>),
    /// CSS-style generic family. The host shaper picks a concrete
    /// face that matches the category.
    Serif,
    /// Generic sans-serif family.
    SansSerif,
    /// Generic monospaced family.
    Mono,
    /// Generic cursive / script family.
    Cursive,
    /// Generic decorative family.
    Fantasy,
    /// The operating-system UI font.
    SystemUi,
}

/// CSS / OpenType weight axis: integer 1..=1000 (in practice 100..=900).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontWeight(pub u16);

impl FontWeight {
    /// Numeric weight 100 — "Thin" / "Hairline".
    pub const THIN: Self = Self(100);
    /// Numeric weight 200 — "Extra Light" / "Ultra Light".
    pub const EXTRA_LIGHT: Self = Self(200);
    /// Numeric weight 300 — "Light".
    pub const LIGHT: Self = Self(300);
    /// Numeric weight 400 — "Regular" / "Normal".
    pub const REGULAR: Self = Self(400);
    /// Numeric weight 500 — "Medium".
    pub const MEDIUM: Self = Self(500);
    /// Numeric weight 600 — "Semi Bold" / "Demi Bold".
    pub const SEMIBOLD: Self = Self(600);
    /// Numeric weight 700 — "Bold".
    pub const BOLD: Self = Self(700);
    /// Numeric weight 800 — "Extra Bold" / "Ultra Bold".
    pub const EXTRA_BOLD: Self = Self(800);
    /// Numeric weight 900 — "Black" / "Heavy".
    pub const BLACK: Self = Self(900);
}

impl Default for FontWeight {
    /// `REGULAR` (400).
    fn default() -> Self {
        Self::REGULAR
    }
}

/// Font width axis — semantic constants from condensed to expanded.
/// The named variants map onto the canonical CSS `font-stretch`
/// percentages (50% → UltraCondensed, … 200% → UltraExpanded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontWidth {
    /// 50% of normal width.
    UltraCondensed,
    /// 62.5%.
    ExtraCondensed,
    /// 75%.
    Condensed,
    /// 87.5%.
    SemiCondensed,
    /// 100% — the default.
    #[default]
    Normal,
    /// 112.5%.
    SemiExpanded,
    /// 125%.
    Expanded,
    /// 150%.
    ExtraExpanded,
    /// 200%.
    UltraExpanded,
}

/// Font style axis.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum FontStyle {
    /// Upright glyphs (the default).
    #[default]
    Normal,
    /// Italic — a separate set of glyphs designed for slanted use.
    Italic,
    /// Oblique — upright glyphs slanted by the given angle in degrees.
    /// Typical range 8°–20°.
    Oblique(f32),
}

/// OpenType feature toggle. `tag` is the 4-char feature tag (e.g.
/// `*b"liga"`); `value` is the feature parameter (0 = off, 1 = on for
/// most binary features; specific indices for stylistic-set
/// features like `ss01`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontFeature {
    /// OpenType 4-byte tag.
    pub tag: [u8; 4],
    /// Feature value.
    pub value: u32,
}

impl FontFeature {
    /// Construct a feature toggle from a 4-byte tag and value.
    #[inline]
    pub const fn new(tag: [u8; 4], value: u32) -> Self {
        Self { tag, value }
    }
}

/// Variable-font variation axis assignment. `tag` is the 4-char axis
/// tag (e.g. `*b"wght"`); `value` is the axis position (units are
/// axis-specific — `wght` is the weight numeric, `opsz` is optical
/// size in pt, etc.).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontVariation {
    /// Variable-font 4-byte axis tag.
    pub tag: [u8; 4],
    /// Axis position.
    pub value: f32,
}

impl FontVariation {
    /// Construct a variation from a 4-byte tag and value.
    #[inline]
    pub const fn new(tag: [u8; 4], value: f32) -> Self {
        Self { tag, value }
    }
}

fn merge_by_tag<T: Clone, F: Fn(&T) -> [u8; 4]>(parent: &[T], child: &[T], tag: F) -> Vec<T> {
    let mut out: Vec<T> = parent.to_vec();
    for c in child {
        let ct = tag(c);
        if let Some(slot) = out.iter_mut().find(|p| tag(p) == ct) {
            *slot = c.clone();
        } else {
            out.push(c.clone());
        }
    }
    out
}
