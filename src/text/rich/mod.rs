//! Rich-text markdown support. Gated behind `text`; layered on top of
//! [`crate::text::TextRun`].
//!
//! **Marquee parity.** The parser recognises standard CommonMark plus
//! the marquee extensions: `~~strike~~`, `^sup^`, `~sub~`, `$math$`,
//! `$$display math$$`, `:::class …:::` fenced divs, and marquee-style
//! `{selector body}` inline spans. `{selector body}` heads carry
//! **one** selector token; combine styles by nesting:
//! `{.red {.17 x}}`. Selector tokens are `.name` (class or CSS colour
//! fallback), `#RRGGBB` / `#RGB` (hex colour), or `.<number>` (size
//! in pt).
//!
//! **Drawing surface.** [`draw_rich_text`] mirrors marquee's
//! `marquee_grob` positioning vocabulary: `(x, y)` plus a
//! [`RichAnchor`] that specifies which point on the laid-out box
//! coincides with `(x, y)` ([`HAnchor::Left`] / `LeftInk` / `Center`
//! / `CenterInk` / `RightInk` / `Right` / `Fraction(f)` and the
//! analogous [`VAnchor`] adding `TopInk` / `FirstLine` / `LastLine`
//! / `BottomInk`). The `transform` argument composes **around the
//! anchor** so a rotation implicitly pivots at `(x, y)`.
//!
//! **Wrap width.** [`RichTextRun::new`] shapes at natural width (no
//! wrap). Use [`RichTextRun::new_with_width`] or
//! [`RichTextRun::set_max_width`] to force wrap at a specific
//! pixel width, mirroring marquee's `marquee_grob(width = ...)`.
//! When the run is used in a composition (via [`Measure::height_at`])
//! the wrap width comes from the layout solver, matching marquee's
//! `width = NULL` "parent container width" semantics.
//!
//! See `src/text/rich/CLAUDE.md` for the full architectural note.

pub mod anchor;
pub mod parser;
pub mod reduce;
pub mod run;
pub mod style;

pub use anchor::{AnchorOffsets, HAnchor, LayoutBounds, RichAnchor, VAnchor};
pub use parser::{parse, ParseError, RichEvent, Selector};
pub use reduce::{reduce, BaselineRun, Block, BlockKind, BuiltRuns, InlineRun};
pub use run::{draw_rich_text, RichBrush, RichTextRun, RichTextWidth};
pub use style::{css_color, RichTextStyleSheet, StyleDelta};
