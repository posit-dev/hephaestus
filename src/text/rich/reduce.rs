//! Reduce a [`RichEvent`] stream to flat styled ranges.
//!
//! The reducer walks events, maintains a stack of active
//! [`StyleDelta`]s (top of stack = deepest span), and emits one
//! `(byte_range_in_output_text, resolved_delta)` tuple per contiguous
//! run of text sharing the same active style. Baseline shifts collect
//! into a parallel `Vec<(byte_range, em_shift)>` — parley has no
//! `StyleProperty::BaselineShift`, so we hold it separately and
//! apply it as a per-glyph-run y-offset when drawing.
//!
//! The reducer is **inline-only** in this pass: it flattens all block
//! containers (paragraph, blockquote, list, item, heading, code block,
//! div) into their content stream and inserts `\n\n` between
//! top-level paragraphs so parley line-breaks them cleanly. Block
//! layout (indent, margin, background, bullets, hr line) is the next
//! step — the reducer records block boundaries in [`BuiltRuns::blocks`]
//! for that pass to consume, but does not itself position anything.

use std::ops::Range;

use super::parser::{RichEvent, Selector};
use super::style::{css_color, RichTextStyleSheet, StyleDelta};
use crate::plot::theme::{Length, ThemeColor};

// ─── Output types ──────────────────────────────────────────────────────────

/// Product of [`reduce`]: the flattened text plus enough side-data to
/// build a parley `Layout` and render it.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltRuns {
    /// The concatenated text — what parley shapes.
    pub text: String,
    /// One entry per contiguous run of text sharing the same active
    /// style. The `range` is a byte range into [`Self::text`]; the
    /// `delta` is fully resolved (all active spans overlaid). Runs
    /// never overlap and cover [`Self::text`] end to end.
    pub inline: Vec<InlineRun>,
    /// One entry per baseline-shifted range (from `sup` / `sub` etc.).
    /// Non-empty and non-overlapping; the em shift is applied to the
    /// glyph y-position when drawing.
    pub baseline_shifts: Vec<BaselineRun>,
    /// Block boundaries recorded during reduction, in emission order.
    /// A layout pass consumes this to draw block backgrounds, indent
    /// content, place bullets, etc. Empty when the source is a single
    /// inline stream.
    pub blocks: Vec<Block>,
}

/// One styled inline run.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineRun {
    /// Byte range into [`BuiltRuns::text`].
    pub range: Range<usize>,
    /// Resolved style — every active span's delta already overlaid.
    pub delta: StyleDelta,
}

/// One baseline-shifted range.
#[derive(Debug, Clone, PartialEq)]
pub struct BaselineRun {
    /// Byte range into [`BuiltRuns::text`].
    pub range: Range<usize>,
    /// Shift in em (positive = up).
    pub shift_em: f32,
}

/// One block-level boundary recorded during reduction. The layout
/// pass uses these to draw backgrounds / borders / bullets and to
/// position the content vertically.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// Byte range in [`BuiltRuns::text`] that this block covers.
    pub range: Range<usize>,
    /// What kind of block.
    pub kind: BlockKind,
    /// Depth of the block's containing div stack at the time of
    /// emission — `0` at the top level. Used by the layout pass to
    /// resolve nested div boxes.
    pub depth: usize,
    /// Overlaid style delta for the block (from `paragraph`, `h1`,
    /// `block_quote`, custom div classes, etc.). The layout pass
    /// reads margin / padding / background / border / indent /
    /// hanging / bullet from here.
    pub delta: StyleDelta,
}

/// The kind of block a [`Block`] describes. Enough to route the
/// layout pass to the right drawing logic (bullets on list items,
/// per-level margins on headings, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum BlockKind {
    /// Plain paragraph.
    Paragraph,
    /// Heading at the given level (1..=6).
    Heading(u8),
    /// Blockquote container. Nested paragraphs land inside as
    /// separate `Paragraph` blocks with `depth` incremented.
    BlockQuote,
    /// Ordered or unordered list container.
    List {
        /// True for ordered lists.
        ordered: bool,
        /// First number for ordered lists; 1 for unordered.
        start: u64,
    },
    /// One item within a list. `ordinal` is the 1-based item index
    /// used to render numeric markers (`1.`, `2.`, …).
    ListItem {
        /// 1-based item index within its parent list.
        ordinal: u64,
    },
    /// Fenced code block. `lang` is the info string on the opening
    /// fence, `None` for indented or unlabelled blocks.
    CodeBlock {
        /// Info string on the opening fence.
        lang: Option<String>,
    },
    /// Horizontal rule (no content).
    Rule,
    /// Fenced div (`:::class`). `class` is the class name captured by
    /// the parser.
    Div {
        /// The class name from the opening `:::class` fence.
        class: String,
    },
}

// ─── Reducer ────────────────────────────────────────────────────────────────

/// Reduce `events` against `sheet` and produce a [`BuiltRuns`].
///
/// The reducer is deterministic and allocation-lean: each event pushes
/// / pops one stack frame and, for text nodes, appends to the output
/// string plus one `InlineRun` (coalesced when the resolved delta
/// hasn't changed since the previous run).
pub fn reduce(events: &[RichEvent], sheet: &RichTextStyleSheet) -> BuiltRuns {
    let mut r = Reducer {
        text: String::new(),
        inline: Vec::new(),
        baseline_shifts: Vec::new(),
        blocks: Vec::new(),
        inline_stack: Vec::new(),
        baseline_stack: Vec::new(),
        block_stack: Vec::new(),
        list_stack: Vec::new(),
    };
    for e in events {
        r.consume(e, sheet);
    }
    r.finish()
}

/// Reduction state — one instance per reduction. Public for
/// intra-crate reuse; not part of the module's external API.
struct Reducer {
    text: String,
    inline: Vec<InlineRun>,
    baseline_shifts: Vec<BaselineRun>,
    blocks: Vec<Block>,
    /// Stack of active inline deltas (deepest span at top).
    inline_stack: Vec<StyleDelta>,
    /// Stack of active baseline shifts (nested sup / sub etc.).
    baseline_stack: Vec<BaselineFrame>,
    /// Stack of open blocks (paragraph, heading, blockquote, item,
    /// code block, div). Each frame remembers the start offset and
    /// the delta at open-time.
    block_stack: Vec<BlockFrame>,
    /// Stack of active lists — records ordered/start plus the running
    /// item ordinal so nested lists number independently.
    list_stack: Vec<ListFrame>,
}

/// One frame on the block stack.
struct BlockFrame {
    /// Byte offset in the output text where this block begins.
    start: usize,
    kind: BlockKind,
    /// Depth of the block-container stack when this block opened
    /// (0 for top-level).
    depth: usize,
    /// Overlaid delta captured at open-time. Used verbatim on
    /// [`Block::delta`] so the layout pass reads the same style
    /// regardless of what child spans nested inside later.
    delta: StyleDelta,
}

/// One frame on the baseline-shift stack — carries the em shift for
/// the topmost sup/sub or explicit-baseline span.
struct BaselineFrame {
    /// Byte offset in the output text where the shift begins.
    start: usize,
    shift_em: f32,
}

/// One list container's running state.
struct ListFrame {
    /// The number of the next item to open. Ordered lists start from
    /// the parser's `start` value; unordered lists count from 1 so
    /// nested ordering is deterministic even though the ordinals are
    /// usually ignored (unordered items render bullets).
    next_ordinal: u64,
    /// `true` for ordered lists (`1. 2. 3.` numbering) — drives
    /// marker generation at [`Reducer::consume`]'s `ItemStart` arm.
    ordered: bool,
}

impl Reducer {
    fn consume(&mut self, event: &RichEvent, sheet: &RichTextStyleSheet) {
        match event {
            // ── Block boundaries ──
            RichEvent::ParagraphStart => self.open_block(BlockKind::Paragraph, sheet, "paragraph"),
            RichEvent::ParagraphEnd => self.close_block(),
            RichEvent::HeadingStart { level } => {
                let key = heading_key(*level);
                self.open_block(BlockKind::Heading(*level), sheet, key);
            }
            RichEvent::HeadingEnd { .. } => self.close_block(),
            RichEvent::BlockQuoteStart => {
                self.open_block(BlockKind::BlockQuote, sheet, "block_quote")
            }
            RichEvent::BlockQuoteEnd => self.close_block(),
            RichEvent::ListStart { ordered, start } => {
                self.list_stack.push(ListFrame {
                    next_ordinal: *start,
                    ordered: *ordered,
                });
                self.open_block(
                    BlockKind::List {
                        ordered: *ordered,
                        start: *start,
                    },
                    sheet,
                    if *ordered { "list_ordered" } else { "list" },
                );
            }
            RichEvent::ListEnd => {
                self.close_block();
                self.list_stack.pop();
            }
            RichEvent::ItemStart => {
                let (ordinal, ordered) = self
                    .list_stack
                    .last_mut()
                    .map(|f| {
                        let n = f.next_ordinal;
                        f.next_ordinal += 1;
                        (n, f.ordered)
                    })
                    .unwrap_or((1, false));
                // 0-based nesting depth of the current list — the
                // enclosing `ListStart` already pushed its frame, so
                // `list_stack.len() - 1` is the depth. Used to index
                // the marker vector for unordered lists.
                let list_depth = self.list_stack.len().saturating_sub(1);
                self.open_block(BlockKind::ListItem { ordinal }, sheet, "list_item");
                // Prepend the item marker as literal text, styled the
                // same as the item body (the `list_item` delta is on
                // top of the inline stack at this point). Ordered
                // lists print `n. `; unordered print the sheet's
                // `bullet[depth % len]` + a space (defaults to `•` if
                // the sheet has no entry or the vector is empty). An
                // explicit empty-string entry at this depth suppresses
                // the marker.
                let marker = if ordered {
                    Some(format!("{ordinal}. "))
                } else {
                    match sheet.get("list_item").and_then(|d| d.bullet.as_ref()) {
                        Some(v) if v.is_empty() => None,
                        Some(v) => {
                            let s = &v[list_depth % v.len()];
                            if s.is_empty() {
                                None
                            } else {
                                Some(format!("{s} "))
                            }
                        }
                        None => Some("• ".to_string()),
                    }
                };
                if let Some(m) = marker {
                    self.push_text(&m);
                }
            }
            RichEvent::ItemEnd => self.close_block(),
            RichEvent::CodeBlockStart { lang } => {
                self.open_block(
                    BlockKind::CodeBlock { lang: lang.clone() },
                    sheet,
                    "code_block",
                );
            }
            RichEvent::CodeBlockEnd => self.close_block(),
            RichEvent::Rule => {
                // A rule has no content, so open + immediately close
                // at the same offset. The layout pass sees a zero-
                // length block and draws the line.
                let start = self.text.len();
                let delta = self
                    .lookup_class("hr", sheet)
                    .unwrap_or_else(StyleDelta::empty);
                self.blocks.push(Block {
                    range: start..start,
                    kind: BlockKind::Rule,
                    depth: self.container_depth(),
                    delta,
                });
                // Insert a paragraph break so the surrounding text
                // laid out by parley doesn't run its lines through
                // the rule's vertical space.
                self.push_paragraph_break();
            }
            RichEvent::DivStart { class } => {
                self.open_block(
                    BlockKind::Div {
                        class: class.clone(),
                    },
                    sheet,
                    class,
                );
            }
            RichEvent::DivEnd => self.close_block(),

            // ── Inline style boundaries ──
            RichEvent::EmphasisStart => self.push_inline("em", sheet),
            RichEvent::EmphasisEnd => self.pop_inline(),
            RichEvent::StrongStart => self.push_inline("strong", sheet),
            RichEvent::StrongEnd => self.pop_inline(),
            RichEvent::StrikethroughStart => self.push_inline("del", sheet),
            RichEvent::StrikethroughEnd => self.pop_inline(),
            RichEvent::SuperscriptStart => {
                self.push_inline("sup", sheet);
                self.push_baseline("sup", sheet);
            }
            RichEvent::SuperscriptEnd => {
                self.pop_baseline();
                self.pop_inline();
            }
            RichEvent::SubscriptStart => {
                self.push_inline("sub", sheet);
                self.push_baseline("sub", sheet);
            }
            RichEvent::SubscriptEnd => {
                self.pop_baseline();
                self.pop_inline();
            }
            RichEvent::LinkStart { .. } => self.push_inline("link", sheet),
            RichEvent::LinkEnd => self.pop_inline(),
            RichEvent::SpanStart { selector } => self.push_selector(selector, sheet),
            RichEvent::SpanEnd => self.pop_inline(),

            // ── Leaves ──
            RichEvent::Text(t) => self.push_text(t),
            RichEvent::Code(t) => {
                // Inline code = a span styled by the `code` selector.
                self.push_inline("code", sheet);
                self.push_text(t);
                self.pop_inline();
            }
            RichEvent::InlineMath(t) | RichEvent::DisplayMath(t) => {
                // v1 renders math as literal text — no shaper yet.
                self.push_text(t);
            }
            RichEvent::SoftBreak => self.push_text(" "),
            RichEvent::HardBreak => self.push_text("\n"),
        }
    }

    fn open_block(&mut self, kind: BlockKind, sheet: &RichTextStyleSheet, class_key: &str) {
        // Between two top-level blocks that both carry text, insert
        // a paragraph break so parley's line breaker treats them as
        // separate paragraphs. We only insert between blocks that
        // have already emitted text — no leading blank line on the
        // first block.
        if matches!(
            kind,
            BlockKind::Paragraph
                | BlockKind::Heading(_)
                | BlockKind::ListItem { .. }
                | BlockKind::CodeBlock { .. }
        ) {
            self.push_paragraph_break();
        }
        let start = self.text.len();
        let delta = self
            .lookup_class(class_key, sheet)
            .unwrap_or_else(StyleDelta::empty);
        let depth = self.container_depth();
        // Push the block's delta onto the inline stack so its
        // glyph-level fields (size / weight / family / colour) cascade
        // into the block's content. Block-only fields (margin,
        // padding, background, border, indent, hanging, bullet) ride
        // along too, but `run.rs`'s `apply_delta_range` ignores them
        // — they're consumed by the block-layout pass reading the
        // `Block` list. This is what makes `# Big` actually render at
        // 2× size and bold, and `` `code` `` at code_block's
        // monospace family.
        self.inline_stack.push(delta.clone());
        self.block_stack.push(BlockFrame {
            start,
            kind,
            depth,
            delta,
        });
    }

    fn close_block(&mut self) {
        // Pop inline first (LIFO). block_stack and inline_stack were
        // pushed in the same order, so they stay balanced.
        self.inline_stack.pop();
        if let Some(frame) = self.block_stack.pop() {
            let end = self.text.len();
            self.blocks.push(Block {
                range: frame.start..end,
                kind: frame.kind,
                depth: frame.depth,
                delta: frame.delta,
            });
        }
    }

    /// Container-depth = how many BlockQuote / Div / List /
    /// ListItem frames are currently open. Used so layout can push
    /// the right indent for nested containers.
    fn container_depth(&self) -> usize {
        self.block_stack
            .iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    BlockKind::BlockQuote
                        | BlockKind::List { .. }
                        | BlockKind::ListItem { .. }
                        | BlockKind::Div { .. }
                )
            })
            .count()
    }

    fn push_paragraph_break(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with("\n\n") {
            if self.text.ends_with('\n') {
                self.text.push('\n');
            } else {
                self.text.push_str("\n\n");
            }
        }
    }

    fn push_inline(&mut self, key: &str, sheet: &RichTextStyleSheet) {
        let delta = self
            .lookup_class(key, sheet)
            .unwrap_or_else(StyleDelta::empty);
        self.inline_stack.push(delta);
    }

    fn pop_inline(&mut self) {
        self.inline_stack.pop();
    }

    fn push_baseline(&mut self, key: &str, sheet: &RichTextStyleSheet) {
        // Read the sheet's baseline_em on the way in so a user-tuned
        // `sup` / `sub` overrides the default.
        let shift = sheet
            .get(key)
            .and_then(|d| d.baseline_em)
            .unwrap_or_default();
        self.baseline_stack.push(BaselineFrame {
            start: self.text.len(),
            shift_em: shift,
        });
    }

    fn pop_baseline(&mut self) {
        if let Some(frame) = self.baseline_stack.pop() {
            let end = self.text.len();
            if end > frame.start && frame.shift_em != 0.0 {
                self.baseline_shifts.push(BaselineRun {
                    range: frame.start..end,
                    shift_em: frame.shift_em,
                });
            }
        }
    }

    fn push_selector(&mut self, sel: &Selector, sheet: &RichTextStyleSheet) {
        let delta = match sel {
            Selector::Class(name) => self.lookup_class(name, sheet).unwrap_or_else(|| {
                // Fall back to CSS colour keyword.
                css_color(name).map_or_else(StyleDelta::empty, |[r, g, b]| StyleDelta {
                    color: Some(ThemeColor::Fixed(rgb8_to_color(r, g, b))),
                    ..StyleDelta::empty()
                })
            }),
            Selector::HexColor([r, g, b]) => StyleDelta {
                color: Some(ThemeColor::Fixed(rgb8_to_color(*r, *g, *b))),
                ..StyleDelta::empty()
            },
            Selector::Size(pt) => StyleDelta {
                size: Some(Length::Abs(*pt as f64)),
                ..StyleDelta::empty()
            },
        };
        self.inline_stack.push(delta);
    }

    fn lookup_class(&self, name: &str, sheet: &RichTextStyleSheet) -> Option<StyleDelta> {
        sheet.get(name).cloned()
    }

    /// Append a run of literal text at the current styled state.
    /// Coalesces with the previous run when the resolved delta hasn't
    /// changed since it opened.
    fn push_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let resolved = self.resolved_inline();
        let start = self.text.len();
        self.text.push_str(s);
        let end = self.text.len();
        // If the last run has identical delta and abuts `start`, merge.
        if let Some(last) = self.inline.last_mut() {
            if last.range.end == start && last.delta == resolved {
                last.range.end = end;
                return;
            }
        }
        self.inline.push(InlineRun {
            range: start..end,
            delta: resolved,
        });
    }

    fn resolved_inline(&self) -> StyleDelta {
        let mut d = StyleDelta::empty();
        for frame in &self.inline_stack {
            d = d.overlay(frame);
        }
        d
    }

    fn finish(mut self) -> BuiltRuns {
        // If the caller finishes with an unclosed inline / block
        // frame we still return a coherent result — this can happen
        // in tests that feed hand-rolled streams. In production the
        // parser guarantees matched pairs.
        while self.block_stack.pop().is_some() {}
        while self.baseline_stack.pop().is_some() {}
        while self.inline_stack.pop().is_some() {}
        BuiltRuns {
            text: self.text,
            inline: self.inline,
            baseline_shifts: self.baseline_shifts,
            blocks: self.blocks,
        }
    }
}

fn heading_key(level: u8) -> &'static str {
    match level {
        1 => "h1",
        2 => "h2",
        3 => "h3",
        4 => "h4",
        5 => "h5",
        _ => "h6",
    }
}

fn rgb8_to_color(r: u8, g: u8, b: u8) -> crate::color::Color {
    crate::color::Color::from_rgba8(r, g, b, 255)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::rich::parser::parse;

    fn reduce_ok(src: &str) -> BuiltRuns {
        let events = parse(src).expect("parse");
        reduce(&events, &RichTextStyleSheet::new())
    }

    #[test]
    fn plain_text_yields_one_run() {
        let r = reduce_ok("hello");
        assert_eq!(r.text, "hello");
        assert_eq!(r.inline.len(), 1);
        assert_eq!(r.inline[0].range, 0..5);
        // Glyph-level fields must all be unset — the paragraph
        // delta on top of the stack may carry `margin` (a block-only
        // field), which is fine because it's ignored by parley
        // shaping.
        let d = &r.inline[0].delta;
        assert!(d.weight.is_none());
        assert!(d.italic.is_none());
        assert!(d.family.is_none());
        assert!(d.size.is_none());
        assert!(d.color.is_none());
        assert!(d.underline.is_none());
        assert!(d.strikethrough.is_none());
        assert!(d.baseline_em.is_none());
        assert!(r.baseline_shifts.is_empty());
    }

    #[test]
    fn bold_run_gets_strong_weight() {
        let r = reduce_ok("a **bold** c");
        // Runs: "a ", "bold", " c" — three runs.
        assert_eq!(r.inline.len(), 3, "got {:?}", r.inline);
        let bold_run = &r.inline[1];
        let bold_text = &r.text[bold_run.range.clone()];
        assert_eq!(bold_text, "bold");
        assert_eq!(bold_run.delta.weight, Some(700));
    }

    #[test]
    fn nested_strong_and_em_overlay() {
        let r = reduce_ok("***both***");
        // The bolded-italic span should have both weight and italic set.
        let both = r
            .inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == "both")
            .expect("run for 'both'");
        assert_eq!(both.delta.weight, Some(700));
        assert_eq!(both.delta.italic, Some(true));
    }

    #[test]
    fn sup_produces_baseline_shift() {
        // pulldown-cmark's `^…^` matcher wants word boundaries around
        // the caret pair, so we surround with spaces here.
        let r = reduce_ok("a ^2^ b");
        assert_eq!(r.baseline_shifts.len(), 1, "got {:?}", r.baseline_shifts);
        let bs = &r.baseline_shifts[0];
        let shifted = &r.text[bs.range.clone()];
        assert_eq!(shifted, "2");
        assert!(bs.shift_em > 0.0, "sup shift should be positive");
    }

    #[test]
    fn sub_baseline_shift_is_negative() {
        let r = reduce_ok("a ~2~ b");
        assert_eq!(r.baseline_shifts.len(), 1);
        assert!(r.baseline_shifts[0].shift_em < 0.0);
    }

    #[test]
    fn hex_color_span_sets_color() {
        let r = reduce_ok("{#ff8800 warm}");
        let warm = r
            .inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == "warm")
            .expect("run for 'warm'");
        match &warm.delta.color {
            Some(ThemeColor::Fixed(_)) => {}
            other => panic!("expected Fixed colour, got {other:?}"),
        }
    }

    #[test]
    fn css_color_fallback_when_class_undefined() {
        // No `steelblue` class in the sheet → CSS-name fallback.
        let r = reduce_ok("{.steelblue hi}");
        let hi = r
            .inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == "hi")
            .expect("run for 'hi'");
        assert!(
            matches!(&hi.delta.color, Some(ThemeColor::Fixed(_))),
            "expected CSS colour fallback"
        );
    }

    #[test]
    fn size_selector_sets_size() {
        let r = reduce_ok("{.17 x}");
        let x = r
            .inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == "x")
            .expect("run for 'x'");
        assert!(matches!(x.delta.size, Some(Length::Abs(v)) if (v - 17.0).abs() < 1e-6));
    }

    #[test]
    fn nested_selectors_overlay() {
        // Outer red + inner size=17 → the interior text has both.
        let r = reduce_ok("{.red {.17 x}}");
        let x = r
            .inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == "x")
            .expect("run for 'x'");
        assert!(matches!(x.delta.color, Some(ThemeColor::Fixed(_))));
        assert!(matches!(x.delta.size, Some(Length::Abs(_))));
    }

    #[test]
    fn user_class_overrides_css_name() {
        // Define a `red` class in the sheet — the sheet entry wins
        // over the CSS colour fallback.
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "red",
            StyleDelta {
                weight: Some(900),
                ..StyleDelta::empty()
            },
        );
        let events = parse("{.red word}").unwrap();
        let r = reduce(&events, &sheet);
        let word = r
            .inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == "word")
            .unwrap();
        assert_eq!(word.delta.weight, Some(900));
        // Colour comes from the CSS fallback ONLY when the class is
        // undefined — so redefining `red` clears the colour bit.
        assert!(word.delta.color.is_none());
    }

    #[test]
    fn two_paragraphs_separated_by_double_newline() {
        let r = reduce_ok("first\n\nsecond");
        assert!(r.text.contains("\n\n"), "got text = {:?}", r.text);
        // Two block frames of kind Paragraph.
        let paragraph_count = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::Paragraph))
            .count();
        assert_eq!(paragraph_count, 2);
    }

    #[test]
    fn div_block_recorded_with_class() {
        let r = reduce_ok(":::warning\nbody\n:::");
        let div = r
            .blocks
            .iter()
            .find(|b| matches!(&b.kind, BlockKind::Div { class } if class == "warning"))
            .expect("div block");
        // The div's range must cover the paragraph inside it.
        let body_run = r
            .inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == "body")
            .unwrap();
        assert!(div.range.start <= body_run.range.start);
        assert!(div.range.end >= body_run.range.end);
    }

    #[test]
    fn heading_text_inherits_heading_delta() {
        // # Big — the "Big" text run should carry weight=700 and a
        // Rel-size delta from the h1 sheet entry.
        let r = reduce_ok("# Big");
        let big = r
            .inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == "Big")
            .expect("run for 'Big'");
        assert_eq!(
            big.delta.weight,
            Some(700),
            "h1 weight should apply to text"
        );
        assert!(
            matches!(big.delta.size, Some(Length::Rel(m)) if m > 1.5),
            "h1 size delta should apply to text, got {:?}",
            big.delta.size
        );
    }

    #[test]
    fn nested_strong_inside_heading_composes() {
        // # **bold** heading — every run in the heading should carry
        // h1's size delta AND weight=700 (h1 sets it; strong also sets
        // it on the "bold" sub-range, matching value).
        let r = reduce_ok("# **bold** heading");
        // The heading block boundary tells us where the header text
        // lives — everything inline that overlaps it should inherit
        // h1's delta.
        let heading = r
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::Heading(1)))
            .expect("h1 block");
        for inline in &r.inline {
            // Only inline runs that overlap the heading's range.
            if inline.range.start >= heading.range.end || inline.range.end <= heading.range.start {
                continue;
            }
            assert!(
                matches!(inline.delta.size, Some(Length::Rel(m)) if m > 1.5),
                "run {:?} inside h1 should inherit Rel size, got {:?}",
                &r.text[inline.range.clone()],
                inline.delta.size
            );
            assert_eq!(
                inline.delta.weight,
                Some(700),
                "run {:?} inside h1 should be bold",
                &r.text[inline.range.clone()]
            );
        }
    }

    #[test]
    fn code_block_body_gets_monospace_family() {
        let r = reduce_ok("```\nlet x = 1;\n```");
        let body = r
            .inline
            .iter()
            .find(|run| r.text[run.range.clone()].contains("let"))
            .expect("code body run");
        assert_eq!(body.delta.family.as_deref(), Some("monospace"));
    }

    #[test]
    fn heading_block_carries_heading_delta() {
        let r = reduce_ok("# Big\n\nSmall");
        let heading = r
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::Heading(1)))
            .expect("h1 block");
        assert!(
            matches!(heading.delta.size, Some(Length::Rel(m)) if m > 1.5),
            "h1 size should be >= 1.5× base, got {:?}",
            heading.delta.size
        );
    }

    #[test]
    fn list_items_carry_ordinal() {
        let r = reduce_ok("1. one\n2. two\n3. three");
        let items: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 3);
        let ords: Vec<u64> = items
            .iter()
            .map(|b| match b.kind {
                BlockKind::ListItem { ordinal } => ordinal,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(ords, vec![1, 2, 3]);
    }

    #[test]
    fn unordered_item_prepends_bullet_marker() {
        let r = reduce_ok("- alpha");
        // The item's byte range must cover both "• " and "alpha".
        let item = r
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .expect("list item");
        let body = &r.text[item.range.clone()];
        assert!(
            body.starts_with("• "),
            "expected bullet marker at start; body = {body:?}"
        );
        assert!(body.contains("alpha"));
    }

    #[test]
    fn ordered_item_prepends_number_marker() {
        let r = reduce_ok("1. one\n2. two");
        let items: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 2);
        let body0 = &r.text[items[0].range.clone()];
        let body1 = &r.text[items[1].range.clone()];
        assert!(
            body0.starts_with("1. "),
            "expected '1. ' marker; body = {body0:?}"
        );
        assert!(
            body1.starts_with("2. "),
            "expected '2. ' marker; body = {body1:?}"
        );
    }

    #[test]
    fn custom_bullet_replaces_default() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "list_item",
            StyleDelta {
                bullet: Some(vec!["★".to_string()]),
                ..StyleDelta::empty()
            },
        );
        let events = parse("- one").unwrap();
        let r = reduce(&events, &sheet);
        let item = r
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .unwrap();
        let body = &r.text[item.range.clone()];
        assert!(
            body.starts_with("★ "),
            "expected custom marker; body = {body:?}"
        );
    }

    #[test]
    fn empty_bullet_vec_suppresses_marker() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "list_item",
            StyleDelta {
                bullet: Some(Vec::new()),
                ..StyleDelta::empty()
            },
        );
        let events = parse("- naked").unwrap();
        let r = reduce(&events, &sheet);
        let item = r
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .unwrap();
        let body = &r.text[item.range.clone()];
        assert!(
            body.starts_with("naked"),
            "empty bullet vec should suppress marker; body = {body:?}"
        );
    }

    #[test]
    fn empty_string_entry_suppresses_at_that_depth() {
        // Two-entry vector, second entry is empty — the outer list
        // gets `•`, the nested list gets no marker.
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "list_item",
            StyleDelta {
                bullet: Some(vec!["•".to_string(), String::new()]),
                ..StyleDelta::empty()
            },
        );
        let events = parse("- outer\n  - inner").unwrap();
        let r = reduce(&events, &sheet);
        let items: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 2);
        // Blocks emit in close order — inner before outer.
        let inner_body = &r.text[items[0].range.clone()];
        let outer_body = &r.text[items[1].range.clone()];
        assert!(
            inner_body.starts_with("inner"),
            "inner (depth-1) should have no marker; body = {inner_body:?}"
        );
        assert!(
            outer_body.starts_with("• "),
            "outer (depth-0) should keep `•`; body = {outer_body:?}"
        );
    }

    #[test]
    fn bullet_cycles_through_vector_by_depth() {
        // Two-entry vector: `•` at even depths, `◦` at odd. A
        // three-level nested list cycles: `•`, `◦`, `•`.
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "list_item",
            StyleDelta {
                bullet: Some(vec!["•".to_string(), "◦".to_string()]),
                ..StyleDelta::empty()
            },
        );
        let events = parse("- a\n  - b\n    - c").unwrap();
        let r = reduce(&events, &sheet);
        let items: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 3);
        // Close order = deepest first — items[0] is depth 2, [1] is
        // depth 1, [2] is depth 0.
        assert!(&r.text[items[0].range.clone()].starts_with("• c")); // depth 2 → cycles to `•`
        assert!(&r.text[items[1].range.clone()].starts_with("◦ b")); // depth 1 → `◦`
        assert!(&r.text[items[2].range.clone()].starts_with("• a")); // depth 0 → `•`
    }

    #[test]
    fn default_sheet_uses_three_marker_cycle() {
        // The built-in `list_item` entry ships `• ◦ ▪` — verify the
        // second-nesting bullet is `◦`, not `•`.
        let events = parse("- outer\n  - inner").unwrap();
        let r = reduce(&events, &RichTextStyleSheet::new());
        let items: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 2);
        // items[0] = inner (depth 1), items[1] = outer (depth 0).
        assert!(
            r.text[items[0].range.clone()].starts_with("◦ "),
            "inner should use second entry `◦`; body = {:?}",
            &r.text[items[0].range.clone()]
        );
        assert!(
            r.text[items[1].range.clone()].starts_with("• "),
            "outer should use first entry `•`; body = {:?}",
            &r.text[items[1].range.clone()]
        );
    }

    #[test]
    fn nested_ordered_lists_number_independently() {
        // Outer list has three items; the middle item contains a
        // nested ordered list starting from 1.
        let r = reduce_ok("1. first\n2. second\n   1. inner1\n   2. inner2\n3. third");
        let items: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .collect();
        // Five items total: 3 outer + 2 inner. The nested ordinals
        // reset at inner1 because each `ListStart` pushes a fresh
        // frame.
        assert_eq!(items.len(), 5);
        let ords: Vec<u64> = items
            .iter()
            .map(|b| match b.kind {
                BlockKind::ListItem { ordinal } => ordinal,
                _ => unreachable!(),
            })
            .collect();
        // Emission order is block-close order (children before parent),
        // so inner1 and inner2 come out before the outer item that
        // contains them closes.
        assert!(
            ords.contains(&1) && ords.contains(&2) && ords.contains(&3),
            "expected outer ordinals 1..=3, got {ords:?}"
        );
        // Two `1`s: one for the outer's first item, one for the
        // nested list's first item.
        assert_eq!(
            ords.iter().filter(|&&n| n == 1).count(),
            2,
            "expected two `1`s (outer + nested first), got {ords:?}"
        );
    }

    #[test]
    fn inline_runs_coalesce_across_soft_breaks() {
        // A soft break is a SoftBreak event that becomes a space in
        // the output text — the two adjacent plain-text pieces plus
        // the space should collapse into one run at the paragraph's
        // resolved delta (all glyph-level fields still None, only
        // the block-only `margin` set from the `paragraph` sheet
        // entry).
        let r = reduce_ok("first\nsecond");
        assert_eq!(r.text, "first second");
        assert_eq!(r.inline.len(), 1, "got {:?}", r.inline);
        let d = &r.inline[0].delta;
        assert!(d.weight.is_none());
        assert!(d.italic.is_none());
        assert!(d.family.is_none());
        assert!(d.size.is_none());
        assert!(d.color.is_none());
    }

    #[test]
    fn depth_increments_inside_nested_divs() {
        let r = reduce_ok(":::outer\n:::inner\nx\n:::\n:::");
        // The inner div should have depth = 1 (outer is 0).
        let inner = r
            .blocks
            .iter()
            .find(|b| matches!(&b.kind, BlockKind::Div { class } if class == "inner"))
            .unwrap();
        let outer = r
            .blocks
            .iter()
            .find(|b| matches!(&b.kind, BlockKind::Div { class } if class == "outer"))
            .unwrap();
        assert_eq!(outer.depth, 0);
        assert_eq!(inner.depth, 1);
    }
}
