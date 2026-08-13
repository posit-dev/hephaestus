//! Rich-text markdown support. Gated behind `text`; layered on top of
//! [`crate::text::TextRun`].
//!
//! **Marquee parity.** The parser recognises standard CommonMark plus
//! the marquee extensions: `~~strike~~`, `^sup^`, `~sub~`, `$math$`,
//! `$$display math$$`, and marquee-style `{selector body}` inline
//! spans. `{selector body}` heads carry **one** selector token; combine
//! styles by nesting: `{.red {.17 x}}`. Selector tokens are `.name`
//! (class or CSS colour fallback), `#RRGGBB` / `#RGB` (hex colour), or
//! `.<number>` (size in pt).
//!
//! See `src/text/rich/CLAUDE.md` for the full architectural note.

pub mod parser;

pub use parser::{parse, ParseError, RichEvent, Selector};
