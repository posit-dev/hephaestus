//! Theming system — configurable visuals for every chrome surface and
//! geom default in the plot layer.
//!
//! See `src/plot/theme/CLAUDE.md` for the module's role and the design
//! choices behind it. Briefly:
//!
//! - The top-level [`Theme`] is carried by `PlotComposition` (as an
//!   `Arc<Theme>`) and applied to every attached `Plot`. Plots can
//!   carry an optional `ThemePart` override that's merged on top at
//!   render.
//! - Element colors are [`ThemeColor`] references into the theme's
//!   [`Palette`] (`paper` / `ink` / `accent`). Swap `paper` ↔ `ink`
//!   with `Theme::invert()` to flip a light theme into a dark theme.
//! - Numeric measurements are [`Length`] — either absolute pt or
//!   `Rel(n)` against the inherited parent (ggplot2's `rel()`).
//! - Three reusable element types — [`TextElement`], [`LineElement`],
//!   [`RectElement`]. Every chrome slot is an [`Element<T>`] =
//!   `Inherit | Blank | Set`.
//! - Axis-like chrome (plot axes + legends' tick component) shares
//!   one [`AxisTheme`] struct, with a three-layer cascade for plot
//!   axes ([`PerAxis`]).
//! - Legends factor into a [`LegendTheme`] with [`KeyTheme`] /
//!   [`BarTheme`] sub-structs for symmetry.

pub mod axis;
pub mod builtin;
pub mod cascade;
pub mod element;
pub mod font;
pub mod geom;
pub mod legend;
pub mod length;
pub mod palette;
#[allow(clippy::module_inception)]
pub mod theme;

#[cfg(test)]
mod tests;

pub use axis::{axis_concrete_defaults, AxisTheme, PerAxis, ResolvedAxis, TitleLocation};
pub use cascade::{PerChannel, Sided};
pub use element::{
    line_concrete_defaults, rect_concrete_defaults, text_concrete_defaults, AlignTo, Element,
    HAlign, LineElement, RectElement, Rotation, TextElement, VAlign,
};
pub use font::{
    FontFamily, FontFeature, FontSpec, FontStyle, FontVariation, FontWeight, FontWidth,
};
pub use geom::{
    GeomTheme, LineDefaults, PointDefaults, ShapeDefaults, TextDefaults, TextFitDefaults,
};
pub use legend::{BarTheme, Direction, KeyTheme, LegendTheme, ResolvedDirection};
pub use length::{pt, rel, Length, Margin};
pub use palette::{Palette, ThemeColor};
pub use theme::{SharedTheme, Theme, ThemePart};
