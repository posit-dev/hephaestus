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

use super::length::pt;
use super::parser::{RichEvent, Selector};
use super::style::{css_color, ResolvedStyle, RichTextStyleSheet, StyleDelta};
use crate::style_vocab::ThemeColor;

// ─── Output types ──────────────────────────────────────────────────────────

/// Product of [`reduce`]: the flattened text plus enough side-data to
/// build a parley `Layout` and render it.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltRuns {
    /// The concatenated text — what parley shapes.
    pub text: String,
    /// One entry per contiguous run of text sharing the same active
    /// style. The `range` is a byte range into [`Self::text`]; the
    /// `style` has every active span already cascaded in. Runs never
    /// overlap and cover [`Self::text`] end to end.
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
    /// Resolved style — every active span's delta already cascaded in,
    /// lengths in points.
    pub style: ResolvedStyle,
}

/// One baseline-shifted range. Nested shifts overlap; the innermost
/// is emitted first, so a first-match lookup finds it.
#[derive(Debug, Clone, PartialEq)]
pub struct BaselineRun {
    /// Byte range into [`BuiltRuns::text`].
    pub range: Range<usize>,
    /// Accumulated shift in points (positive = up).
    pub shift_pt: f64,
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
    /// Resolved style for the block (from `paragraph`, `h1`,
    /// `block_quote`, custom div classes, etc.). The layout pass
    /// reads margin / padding / background / border / indent /
    /// hanging / bullet from here, all in points.
    pub style: ResolvedStyle,
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
    /// One item within a list.
    ListItem {
        /// 1-based item index within its parent list.
        ordinal: u64,
        /// The marker text (`•`, `1.`, …) the layout pass shapes and
        /// places in the list's start gutter. `None` when the sheet
        /// suppresses markers at this nesting depth.
        marker: Option<String>,
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
pub fn reduce(events: &[RichEvent], sheet: &RichTextStyleSheet, base: &ResolvedStyle) -> BuiltRuns {
    // The document root is the caller's base style with the sheet's
    // `base` selector applied, so a sheet can set run-wide line height
    // or tracking without every block repeating it.
    let root = match sheet.get("base") {
        Some(d) => base.apply(d, base, base.size_pt),
        None => base.clone(),
    };
    let mut r = Reducer {
        text: String::new(),
        inline: Vec::new(),
        baseline_shifts: Vec::new(),
        blocks: Vec::new(),
        base_size_pt: base.size_pt,
        style_stack: vec![StyleFrame {
            style: root,
            baseline_start: None,
        }],
        block_stack: Vec::new(),
        list_stack: Vec::new(),
        item_body_pending: false,
        synthetic_paragraph_open: false,
    };
    for e in events {
        r.consume(e, sheet);
    }
    r.finish()
}

/// Reduction state — one instance per reduction.
struct Reducer {
    text: String,
    inline: Vec<InlineRun>,
    baseline_shifts: Vec<BaselineRun>,
    blocks: Vec<Block>,
    /// The run's base font size — what `Rem` lengths measure against.
    base_size_pt: f64,
    /// Cascade stack, root at index 0 and the deepest span on top.
    style_stack: Vec<StyleFrame>,
    /// Stack of open blocks (paragraph, heading, blockquote, item,
    /// code block, div). Each frame remembers the start offset and
    /// the delta at open-time.
    block_stack: Vec<BlockFrame>,
    /// Stack of active lists — records ordered/start plus the running
    /// item ordinal so nested lists number independently.
    list_stack: Vec<ListFrame>,
    /// True between an `ItemStart` and the item's body opening.
    ///
    /// **Tight vs loose detection.** CommonMark distinguishes tight
    /// lists (items with no blank lines) from loose lists (any blank
    /// line inside an item). pulldown-cmark surfaces the distinction:
    /// loose items get an explicit `ParagraphStart` wrapping their
    /// content; tight items get bare `Text`. We wait to see the first
    /// content-carrying event after `ItemStart` — a `ParagraphStart`
    /// means loose (body styled as `paragraph`, with its
    /// `margin.bottom`), anything else means tight (body styled as
    /// `list_item_body`, no default margin).
    item_body_pending: bool,
    /// True while a synthetic Paragraph body (opened for a tight
    /// list item by [`Reducer::ensure_item_body_open`]) sits on the
    /// block stack. Cleared when a nested container start or
    /// `ItemEnd` closes it.
    synthetic_paragraph_open: bool,
}

/// One frame on the block stack.
struct BlockFrame {
    /// Byte offset in the output text where this block begins.
    start: usize,
    kind: BlockKind,
    /// Depth of the block-container stack when this block opened
    /// (0 for top-level).
    depth: usize,
    /// Resolved style captured at open-time. Used verbatim on
    /// [`Block::style`] so the layout pass reads the same style
    /// regardless of what child spans nested inside later.
    style: ResolvedStyle,
}

/// One frame on the cascade stack.
struct StyleFrame {
    /// The resolved style in effect while this frame is on the stack.
    style: ResolvedStyle,
    /// Byte offset where this frame's baseline shift began, when the
    /// frame changed the shift relative to its parent.
    baseline_start: Option<usize>,
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
            RichEvent::ParagraphStart => {
                // If we're inside a list item that hasn't opened its
                // body paragraph yet, pulldown-cmark just told us
                // the item is *loose* (CommonMark: only loose items
                // wrap their content in an explicit paragraph). Use
                // the `paragraph` class so this paragraph picks up
                // paragraph's default margin.bottom.
                self.item_body_pending = false;
                self.open_block(BlockKind::Paragraph, sheet, "paragraph");
            }
            RichEvent::ParagraphEnd => self.close_block(),
            RichEvent::HeadingStart { level } => {
                self.commit_pending_item_body(sheet, true);
                let key = heading_key(*level);
                self.open_block(BlockKind::Heading(*level), sheet, key);
            }
            RichEvent::HeadingEnd { .. } => self.close_block(),
            RichEvent::BlockQuoteStart => {
                self.commit_pending_item_body(sheet, true);
                self.open_block(BlockKind::BlockQuote, sheet, "block_quote");
            }
            RichEvent::BlockQuoteEnd => self.close_block(),
            RichEvent::ListStart { ordered, start } => {
                self.commit_pending_item_body(sheet, true);
                let nested = !self.list_stack.is_empty();
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
                // Nested lists sit inside a ListItem's content flow, so
                // the outer `list` / `list_ordered` top/bottom margin
                // (which separates lists from surrounding prose) would
                // introduce a spurious gap. Zero those two edges on the
                // just-opened frame; horizontal padding / indent still
                // apply.
                if nested {
                    if let Some(frame) = self.block_stack.last_mut() {
                        frame.style.margin_pt[0] = 0.0;
                        frame.style.margin_pt[2] = 0.0;
                    }
                }
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
                // Bullets cycle on consecutive *unordered* nesting:
                // an `ol` between two `ul`s restarts the bullet set,
                // matching marquee.
                let bullet_depth = self
                    .list_stack
                    .iter()
                    .rev()
                    .take_while(|f| !f.ordered)
                    .count()
                    .saturating_sub(1);
                // Open the ListItem as a *container*. Its body
                // paragraph opens later, only once we've seen enough
                // events to know whether the item is tight (bare
                // Text → `list_item_body` class, empty delta) or
                // loose (`ParagraphStart` from pulldown →
                // `paragraph` class with margin). This matches
                // CommonMark's tight/loose semantics without pre-
                // committing to a style.
                let marker = compute_marker(sheet, ordered, ordinal, bullet_depth);
                self.open_block(BlockKind::ListItem { ordinal, marker }, sheet, "list_item");
                self.item_body_pending = true;
            }
            RichEvent::ItemEnd => {
                // Empty item — no content-carrying event ever fired,
                // so commit a synthetic body now to preserve the
                // marker (may be empty) and give the item a leaf.
                self.commit_pending_item_body(sheet, true);
                self.close_block();
            }
            RichEvent::CodeBlockStart { lang } => {
                self.commit_pending_item_body(sheet, true);
                self.open_block(
                    BlockKind::CodeBlock { lang: lang.clone() },
                    sheet,
                    "code_block",
                );
            }
            RichEvent::CodeBlockEnd => {
                // pulldown-cmark emits the code-block content
                // verbatim including the trailing newline before the
                // closing fence. That trailing `\n` pushes parley to
                // shape an empty last line inside the block, which
                // reads visually as an extra blank line. Strip a
                // single trailing '\n' from the current block's
                // content — only if the block hasn't been made empty
                // by other logic.
                if let Some(frame) = self.block_stack.last() {
                    if self.text.len() > frame.start && self.text.ends_with('\n') {
                        self.text.pop();
                        // Trim any InlineRun that ended at the popped
                        // byte so parley doesn't see a range past the
                        // new text end.
                        let new_len = self.text.len();
                        if let Some(last) = self.inline.last_mut() {
                            if last.range.end > new_len {
                                last.range.end = new_len;
                                if last.range.is_empty() {
                                    self.inline.pop();
                                }
                            }
                        }
                    }
                }
                self.close_block();
            }
            RichEvent::Rule => {
                self.commit_pending_item_body(sheet, true);
                let start = self.text.len();
                let style = self.resolve_class("hr", sheet);
                self.blocks.push(Block {
                    range: start..start,
                    kind: BlockKind::Rule,
                    depth: self.container_depth(),
                    style,
                });
                self.push_paragraph_break();
            }
            RichEvent::DivStart { class } => {
                self.commit_pending_item_body(sheet, true);
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
            RichEvent::UnderlineStart => self.push_inline("underline", sheet),
            RichEvent::UnderlineEnd => self.pop_inline(),
            RichEvent::StrongStart => self.push_inline("strong", sheet),
            RichEvent::StrongEnd => self.pop_inline(),
            RichEvent::StrikethroughStart => self.push_inline("del", sheet),
            RichEvent::StrikethroughEnd => self.pop_inline(),
            RichEvent::SuperscriptStart => self.push_inline("sup", sheet),
            RichEvent::SuperscriptEnd => self.pop_inline(),
            RichEvent::SubscriptStart => self.push_inline("sub", sheet),
            RichEvent::SubscriptEnd => self.pop_inline(),
            RichEvent::LinkStart { .. } => self.push_inline("link", sheet),
            RichEvent::LinkEnd => self.pop_inline(),
            RichEvent::SpanStart { selector } => self.push_selector(selector, sheet),
            RichEvent::SpanEnd => self.pop_inline(),

            // ── Leaves ──
            RichEvent::Text(t) => {
                self.ensure_item_body_open(sheet);
                self.push_text(t);
            }
            RichEvent::Code(t) => {
                self.ensure_item_body_open(sheet);
                // Inline code = a span styled by the `code` selector.
                self.push_inline("code", sheet);
                self.push_text(t);
                self.pop_inline();
            }
            RichEvent::InlineMath(t) | RichEvent::DisplayMath(t) => {
                self.ensure_item_body_open(sheet);
                // v1 renders math as literal text — no shaper yet.
                self.push_text(t);
            }
            RichEvent::SoftBreak => {
                self.ensure_item_body_open(sheet);
                self.push_text(" ");
            }
            RichEvent::HardBreak => {
                self.ensure_item_body_open(sheet);
                self.push_text("\n");
            }
        }
    }

    /// Open a tight-item body paragraph if an item is waiting for one.
    /// Called by every content-carrying leaf event — the *first* such
    /// event after an `ItemStart` decides "this item is tight" and
    /// creates the `list_item_body` paragraph on the fly.
    fn ensure_item_body_open(&mut self, sheet: &RichTextStyleSheet) {
        if self.item_body_pending {
            self.item_body_pending = false;
            self.open_block(BlockKind::Paragraph, sheet, "list_item_body");
            self.synthetic_paragraph_open = true;
        }
    }

    /// Close whatever body-in-progress the current list item has
    /// open. An item whose first content is a nested container still
    /// needs an empty leaf so the layout pass has somewhere to anchor
    /// its marker. If a synthetic body is already open, close it.
    /// No-op when neither state applies.
    fn commit_pending_item_body(&mut self, sheet: &RichTextStyleSheet, close_synthetic: bool) {
        if self.item_body_pending {
            self.item_body_pending = false;
            self.open_block(BlockKind::Paragraph, sheet, "list_item_body");
            self.close_block();
        } else if close_synthetic && self.synthetic_paragraph_open {
            self.close_block();
            self.synthetic_paragraph_open = false;
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
        let style = self.resolve_class(class_key, sheet);
        let depth = self.container_depth();
        // The block paints its own box, so the cascade its content
        // inherits carries only the glyph-level fields — otherwise a
        // descendant span would repaint the background and border.
        self.push_style(style.for_inline());
        self.block_stack.push(BlockFrame {
            start,
            kind,
            depth,
            style,
        });
    }

    fn close_block(&mut self) {
        // Pop the cascade frame first (LIFO). block_stack and
        // style_stack were pushed in the same order, so they stay
        // balanced.
        self.pop_style();
        if let Some(frame) = self.block_stack.pop() {
            let end = self.text.len();
            self.blocks.push(Block {
                range: frame.start..end,
                kind: frame.kind,
                depth: frame.depth,
                style: frame.style,
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
        let style = self.resolve_class(key, sheet);
        self.push_style(style);
    }

    fn pop_inline(&mut self) {
        self.pop_style();
    }

    /// The style currently in effect — top of the cascade stack.
    fn top(&self) -> &ResolvedStyle {
        &self.style_stack.last().expect("cascade root").style
    }

    /// Cascade `delta` onto the current top of stack and push the
    /// result. The frame opens a baseline range whenever it moves the
    /// accumulated shift, so `sup` / `sub` and any span that sets
    /// `baseline` all produce draw-time offsets.
    fn push_resolved(&mut self, delta: &StyleDelta) {
        let n = self.style_stack.len();
        let parent = &self.style_stack[n - 1].style;
        let grandparent = &self.style_stack[n.saturating_sub(2)].style;
        let style = parent.apply(delta, grandparent, self.base_size_pt);
        self.push_style(style);
    }

    fn push_style(&mut self, style: ResolvedStyle) {
        let baseline_start = (style.baseline_pt != self.top().baseline_pt).then_some(self.text.len());
        self.style_stack.push(StyleFrame {
            style,
            baseline_start,
        });
    }

    fn pop_style(&mut self) {
        // Never pop the root frame — an unbalanced event stream would
        // otherwise leave the cascade empty.
        if self.style_stack.len() <= 1 {
            return;
        }
        let frame = self.style_stack.pop().expect("non-root frame");
        if let Some(start) = frame.baseline_start {
            let end = self.text.len();
            if end > start {
                self.baseline_shifts.push(BaselineRun {
                    range: start..end,
                    shift_pt: frame.style.baseline_pt,
                });
            }
        }
    }

    /// Resolve a sheet selector against the current cascade top.
    fn resolve_class(&self, key: &str, sheet: &RichTextStyleSheet) -> ResolvedStyle {
        let n = self.style_stack.len();
        let parent = &self.style_stack[n - 1].style;
        let grandparent = &self.style_stack[n.saturating_sub(2)].style;
        match sheet.get(key) {
            Some(d) => parent.apply(d, grandparent, self.base_size_pt),
            None => parent.clone(),
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
            Selector::Size(size_pt) => StyleDelta {
                size: Some(pt(*size_pt as f64)),
                ..StyleDelta::empty()
            },
            // An id the sheet doesn't define carries no style — the
            // span's content still renders, matching the unknown-class
            // fallback.
            Selector::HashName(name) => self
                .lookup_class(name, sheet)
                .unwrap_or_else(StyleDelta::empty),
        };
        self.push_resolved(&delta);
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
        let start = self.text.len();
        self.text.push_str(s);
        let end = self.text.len();
        // If the last run carries the same style and abuts `start`,
        // extend it rather than opening a second run.
        let style = self.top().clone();
        if let Some(last) = self.inline.last_mut() {
            if last.range.end == start && last.style == style {
                last.range.end = end;
                return;
            }
        }
        self.inline.push(InlineRun {
            range: start..end,
            style,
        });
    }

    fn finish(mut self) -> BuiltRuns {
        // If the caller finishes with an unclosed inline / block
        // frame we still return a coherent result — this can happen
        // in tests that feed hand-rolled streams. In production the
        // parser guarantees matched pairs.
        while self.block_stack.pop().is_some() {}
        while self.style_stack.len() > 1 {
            self.pop_style();
        }
        BuiltRuns {
            text: self.text,
            inline: self.inline,
            baseline_shifts: self.baseline_shifts,
            blocks: self.blocks,
        }
    }
}

/// The marker text for a list item at the given ordinal and bullet
/// nesting depth. `None` when the sheet's `list_item.bullet` vector
/// is empty (user opts out of markers) or when the depth's bullet is
/// an empty string.
fn compute_marker(
    sheet: &RichTextStyleSheet,
    ordered: bool,
    ordinal: u64,
    bullet_depth: usize,
) -> Option<String> {
    if ordered {
        return Some(format!("{ordinal}."));
    }
    match sheet.get("list_item").and_then(|d| d.bullet.as_ref()) {
        Some(v) if v.is_empty() => None,
        Some(v) => {
            let s = &v[bullet_depth % v.len()];
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        None => Some("•".to_string()),
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
    use crate::text::rich::length::{em, relative, RichMargin};
    use crate::text::rich::parser::parse;
    use crate::text::TextStyle;

    const BASE_PT: f64 = 10.0;

    fn base_style() -> ResolvedStyle {
        ResolvedStyle::from_base(&TextStyle::new(BASE_PT as f32))
    }

    fn reduce_with(src: &str, sheet: &RichTextStyleSheet) -> BuiltRuns {
        reduce(&parse(src), sheet, &base_style())
    }

    fn reduce_ok(src: &str) -> BuiltRuns {
        reduce_with(src, &RichTextStyleSheet::new())
    }

    fn run_for<'a>(r: &'a BuiltRuns, text: &str) -> &'a InlineRun {
        r.inline
            .iter()
            .find(|run| &r.text[run.range.clone()] == text)
            .unwrap_or_else(|| panic!("no run for {text:?} in {:?}", r.text))
    }

    fn items(r: &BuiltRuns) -> Vec<&Block> {
        r.blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ListItem { .. }))
            .collect()
    }

    fn marker_of(b: &Block) -> Option<&str> {
        match &b.kind {
            BlockKind::ListItem { marker, .. } => marker.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn plain_text_yields_one_run_at_the_base_style() {
        let r = reduce_ok("hello");
        assert_eq!(r.text, "hello");
        assert_eq!(r.inline.len(), 1);
        assert_eq!(r.inline[0].range, 0..5);
        let d = &r.inline[0].style;
        let base = base_style();
        assert_eq!(d.weight, base.weight);
        assert_eq!(d.italic, base.italic);
        assert!(d.family.is_none());
        assert_eq!(d.size_pt, BASE_PT);
        assert!(d.color.is_none());
        assert!(!d.underline);
        assert!(!d.strikethrough);
        assert_eq!(d.baseline_pt, 0.0);
        assert!(r.baseline_shifts.is_empty());
    }

    #[test]
    fn a_block_style_does_not_leak_its_box_onto_its_text() {
        // `code_block` paints a background; the text inside it must
        // not carry that background as an inline chip too.
        let r = reduce_ok("```\nlet x = 1;\n```");
        let body = r
            .inline
            .iter()
            .find(|run| r.text[run.range.clone()].contains("let"))
            .expect("code body run");
        assert!(body.style.background.is_none());
        assert_eq!(body.style.padding_pt, [0.0; 4]);
        assert_eq!(body.style.family.as_deref(), Some("monospace"));
    }

    #[test]
    fn bold_run_gets_strong_weight() {
        let r = reduce_ok("a **bold** c");
        assert_eq!(r.inline.len(), 3, "got {:?}", r.inline);
        assert_eq!(run_for(&r, "bold").style.weight, 700);
    }

    #[test]
    fn nested_strong_and_em_overlay() {
        let r = reduce_ok("***both***");
        let both = run_for(&r, "both");
        assert_eq!(both.style.weight, 700);
        assert!(both.style.italic);
    }

    #[test]
    fn underscore_emphasis_underlines() {
        let r = reduce_ok("a _u_ b");
        assert!(run_for(&r, "u").style.underline);
        assert!(!run_for(&r, "u").style.italic);
    }

    #[test]
    fn sup_produces_a_positive_baseline_shift() {
        // pulldown-cmark's `^…^` matcher wants word boundaries around
        // the caret pair, so we surround with spaces here.
        let r = reduce_ok("a ^2^ b");
        assert_eq!(r.baseline_shifts.len(), 1, "got {:?}", r.baseline_shifts);
        let bs = &r.baseline_shifts[0];
        assert_eq!(&r.text[bs.range.clone()], "2");
        assert!(bs.shift_pt > 0.0, "sup shift should be positive");
    }

    #[test]
    fn sub_baseline_shift_is_negative() {
        let r = reduce_ok("a ~2~ b");
        assert_eq!(r.baseline_shifts.len(), 1);
        assert!(r.baseline_shifts[0].shift_pt < 0.0);
    }

    #[test]
    fn sup_inside_sup_stops_shrinking_but_keeps_lifting() {
        let r = reduce_ok("a ^b ^c^^ d");
        let outer = run_for(&r, "b ");
        let inner = run_for(&r, "c");
        assert!((outer.style.size_pt - inner.style.size_pt).abs() < 1e-9);
        assert!(inner.style.baseline_pt > outer.style.baseline_pt);
    }

    #[test]
    fn hex_color_span_sets_color() {
        let r = reduce_ok("{#ff8800 warm}");
        assert!(matches!(
            run_for(&r, "warm").style.color,
            Some(ThemeColor::Fixed(_))
        ));
    }

    #[test]
    fn css_color_fallback_when_class_undefined() {
        let r = reduce_ok("{.steelblue hi}");
        assert!(
            matches!(run_for(&r, "hi").style.color, Some(ThemeColor::Fixed(_))),
            "expected CSS colour fallback"
        );
    }

    #[test]
    fn size_selector_sets_an_absolute_size() {
        let r = reduce_ok("{.17 x}");
        assert!((run_for(&r, "x").style.size_pt - 17.0).abs() < 1e-6);
    }

    #[test]
    fn nested_selectors_overlay() {
        let r = reduce_ok("{.red {.17 x}}");
        let x = run_for(&r, "x");
        assert!(matches!(x.style.color, Some(ThemeColor::Fixed(_))));
        assert!((x.style.size_pt - 17.0).abs() < 1e-6);
    }

    #[test]
    fn user_class_overrides_css_name() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "red",
            StyleDelta {
                weight: Some(900),
                ..StyleDelta::empty()
            },
        );
        let r = reduce_with("{.red word}", &sheet);
        let word = run_for(&r, "word");
        assert_eq!(word.style.weight, 900);
        // Colour comes from the CSS fallback ONLY when the class is
        // undefined — so redefining `red` clears the colour bit.
        assert!(word.style.color.is_none());
    }

    #[test]
    fn unknown_hash_selector_leaves_the_body_unstyled() {
        let r = reduce_ok("{#nosuchid word}");
        let word = run_for(&r, "word");
        assert!(word.style.color.is_none());
        assert_eq!(word.style.weight, base_style().weight);
    }

    #[test]
    fn two_paragraphs_separated_by_double_newline() {
        let r = reduce_ok("first\n\nsecond");
        assert!(r.text.contains("\n\n"), "got text = {:?}", r.text);
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
        let body_run = run_for(&r, "body");
        assert!(div.range.start <= body_run.range.start);
        assert!(div.range.end >= body_run.range.end);
    }

    #[test]
    fn heading_text_inherits_heading_style() {
        let r = reduce_ok("# Big");
        let big = run_for(&r, "Big");
        assert_eq!(big.style.weight, 700, "h1 weight should apply to text");
        assert!(
            big.style.size_pt > BASE_PT * 1.5,
            "h1 size should apply to text, got {}",
            big.style.size_pt
        );
    }

    #[test]
    fn heading_margins_measure_against_the_headings_own_size() {
        let r = reduce_ok("# Big");
        let heading = r
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::Heading(1)))
            .expect("h1 block");
        // `em(1)` of top margin at 2.25 × 10pt.
        assert!(
            (heading.style.margin_pt[0] - heading.style.size_pt).abs() < 1e-6,
            "got {:?} for a {}pt heading",
            heading.style.margin_pt,
            heading.style.size_pt
        );
    }

    #[test]
    fn sizes_compound_through_nested_divs() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "half",
            StyleDelta {
                size: Some(relative(0.5)),
                ..StyleDelta::empty()
            },
        );
        let r = reduce_with(":::half\n:::half\ndeep\n:::\n:::", &sheet);
        assert!((run_for(&r, "deep").style.size_pt - BASE_PT * 0.25).abs() < 1e-6);
    }

    #[test]
    fn nested_strong_inside_heading_composes() {
        let r = reduce_ok("# **bold** heading");
        let heading = r
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::Heading(1)))
            .expect("h1 block");
        for inline in &r.inline {
            if inline.range.start >= heading.range.end || inline.range.end <= heading.range.start {
                continue;
            }
            assert!(
                inline.style.size_pt > BASE_PT * 1.5,
                "run {:?} inside h1 should inherit the heading size, got {}",
                &r.text[inline.range.clone()],
                inline.style.size_pt
            );
            assert_eq!(
                inline.style.weight,
                700,
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
        assert_eq!(body.style.family.as_deref(), Some("monospace"));
    }

    #[test]
    fn list_items_carry_ordinal() {
        let r = reduce_ok("1. one\n2. two\n3. three");
        let ords: Vec<u64> = items(&r)
            .iter()
            .map(|b| match b.kind {
                BlockKind::ListItem { ordinal, .. } => ordinal,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(ords.len(), 3);
        assert!(ords.contains(&1) && ords.contains(&2) && ords.contains(&3));
    }

    #[test]
    fn markers_ride_on_the_item_not_in_its_text() {
        let r = reduce_ok("- alpha");
        let item = items(&r)[0];
        assert_eq!(marker_of(item), Some("•"));
        assert_eq!(
            r.text[item.range.clone()].trim(),
            "alpha",
            "the marker must not be injected into the item's text"
        );
    }

    #[test]
    fn ordered_markers_carry_the_ordinal() {
        let r = reduce_ok("1. one\n2. two");
        let it = items(&r);
        assert_eq!(it.len(), 2);
        let mut markers: Vec<&str> = it.iter().filter_map(|b| marker_of(b)).collect();
        markers.sort_unstable();
        assert_eq!(markers, vec!["1.", "2."]);
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
        let r = reduce_with("- one", &sheet);
        assert_eq!(marker_of(items(&r)[0]), Some("★"));
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
        let r = reduce_with("- naked", &sheet);
        assert_eq!(marker_of(items(&r)[0]), None);
    }

    #[test]
    fn empty_string_entry_suppresses_at_that_depth() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "list_item",
            StyleDelta {
                bullet: Some(vec!["•".to_string(), String::new()]),
                ..StyleDelta::empty()
            },
        );
        let r = reduce_with("- outer\n  - inner", &sheet);
        let it = items(&r);
        assert_eq!(it.len(), 2);
        // Blocks emit in close order — inner before outer.
        assert_eq!(marker_of(it[0]), None, "depth 1 is suppressed");
        assert_eq!(marker_of(it[1]), Some("•"));
    }

    #[test]
    fn bullet_cycles_through_vector_by_depth() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "list_item",
            StyleDelta {
                bullet: Some(vec!["•".to_string(), "◦".to_string()]),
                ..StyleDelta::empty()
            },
        );
        let r = reduce_with("- a\n  - b\n    - c", &sheet);
        let it = items(&r);
        assert_eq!(it.len(), 3);
        // Close order = deepest first.
        assert_eq!(marker_of(it[0]), Some("•"), "depth 2 cycles back");
        assert_eq!(marker_of(it[1]), Some("◦"));
        assert_eq!(marker_of(it[2]), Some("•"));
    }

    #[test]
    fn an_ordered_list_restarts_the_bullet_cycle() {
        // `ul > ol > ul`: the innermost unordered list is one level
        // of *unordered* nesting deep, so it returns to `•`.
        let r = reduce_ok("- a\n  1. b\n     - c");
        let inner = items(&r)
            .into_iter()
            .find(|b| r.text[b.range.clone()].contains('c'))
            .expect("innermost item");
        assert_eq!(marker_of(inner), Some("•"));
    }

    #[test]
    fn tight_list_items_use_list_item_body_class() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "list_item_body",
            StyleDelta {
                margin: Some(RichMargin::new(pt(0.0), pt(0.0), pt(1.0), pt(0.0))),
                ..StyleDelta::empty()
            },
        );
        let r = reduce_with("- a\n- b", &sheet);
        let bodies: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::Paragraph))
            .collect();
        assert!(bodies.len() >= 2, "expected two body paragraphs");
        for body in bodies {
            assert_eq!(
                body.style.margin_pt,
                [0.0, 0.0, 1.0, 0.0],
                "tight body should carry the list_item_body margin"
            );
        }
    }

    #[test]
    fn loose_list_items_use_paragraph_class() {
        let r = reduce_ok("- a\n\n- b");
        let bodies: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::Paragraph))
            .collect();
        assert!(bodies.len() >= 2, "expected two body paragraphs");
        for body in bodies {
            assert!(
                body.style.margin_pt[2] > 0.0,
                "loose body should carry paragraph's bottom margin, got {:?}",
                body.style.margin_pt
            );
        }
    }

    #[test]
    fn lists_indent_their_items_through_container_padding() {
        let r = reduce_ok("- a");
        let list = r
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::List { .. }))
            .expect("list container");
        // The default sheet gives lists `em(2)` of start padding.
        assert!((list.style.padding_pt[3] - BASE_PT * 2.0).abs() < 1e-6);
    }

    #[test]
    fn nested_lists_drop_the_container_margin() {
        let r = reduce_ok("- a\n  - b");
        let lists: Vec<&Block> = r
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::List { .. }))
            .collect();
        assert_eq!(lists.len(), 2);
        // Close order = inner first.
        assert_eq!(lists[0].style.margin_pt[0], 0.0);
        assert_eq!(lists[0].style.margin_pt[2], 0.0);
        assert!(
            lists[1].style.margin_pt[0] > 0.0,
            "outer list keeps its gap"
        );
    }

    #[test]
    fn nested_ordered_lists_number_independently() {
        let r = reduce_ok("1. first\n2. second\n   1. inner1\n   2. inner2\n3. third");
        let ords: Vec<u64> = items(&r)
            .iter()
            .map(|b| match b.kind {
                BlockKind::ListItem { ordinal, .. } => ordinal,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(ords.len(), 5);
        assert_eq!(
            ords.iter().filter(|&&n| n == 1).count(),
            2,
            "expected two `1`s (outer + nested first), got {ords:?}"
        );
    }

    #[test]
    fn base_selector_applies_run_wide() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "base",
            StyleDelta {
                tracking: Some(50.0),
                ..StyleDelta::empty()
            },
        );
        let r = reduce_with("plain", &sheet);
        assert_eq!(r.inline[0].style.tracking, 50.0);
    }

    #[test]
    fn em_lengths_inside_a_scaled_block_follow_that_block() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "big",
            StyleDelta {
                size: Some(relative(2.0)),
                padding: Some(RichMargin::all(em(1.0))),
                ..StyleDelta::empty()
            },
        );
        let r = reduce_with(":::big\nx\n:::", &sheet);
        let div = r
            .blocks
            .iter()
            .find(|b| matches!(&b.kind, BlockKind::Div { .. }))
            .expect("div");
        assert!((div.style.padding_pt[3] - BASE_PT * 2.0).abs() < 1e-6);
    }

    #[test]
    fn inline_runs_coalesce_across_soft_breaks() {
        let r = reduce_ok("first\nsecond");
        assert_eq!(r.text, "first second");
        assert_eq!(r.inline.len(), 1, "got {:?}", r.inline);
    }

    #[test]
    fn depth_increments_inside_nested_divs() {
        let r = reduce_ok(":::outer\n:::inner\nx\n:::\n:::");
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
