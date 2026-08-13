//! Markdown → [`RichEvent`] stream. Wraps `pulldown-cmark` and layers on
//! two extensions:
//!
//! - A **block pre-pass** that recognises Pandoc / Quarto / marquee
//!   style fenced divs (`:::class` … `:::`) and splits the source into
//!   independent markdown chunks separated by synthetic `DivStart` /
//!   `DivEnd` events.
//! - An **inline post-pass** on each `Event::Text` payload that
//!   recognises marquee-style `{selector body}` inline spans and
//!   injects `SpanStart` / `SpanEnd` events around the body.
//!
//! **Enabled pulldown-cmark options**: strikethrough, superscript,
//! subscript, math. Everything else is CommonMark.
//!
//! **Span head is one token.** Inside a `{…}` head, everything up to
//! the first whitespace is the selector; the rest is body text. So
//! `{.red .17 something}` is a red-classed span whose body is the
//! literal string `.17 something`. To combine styles: nest, e.g.
//! `{.red {.17 something}}`.
//!
//! **Div fences must sit on their own line.** `:::class` at the start
//! of a line (after optional leading whitespace) opens a div;
//! bare `:::` on its own line closes the innermost open div. A
//! paragraph or list cannot span a div boundary — each inter-fence
//! chunk is parsed as its own markdown block.
//!
//! **Literal braces.** Doubled braces escape: `{{` yields a literal
//! `{` and `}}` yields a literal `}`. Backslash-escapes (`\{`) do
//! **not** work here because pulldown-cmark strips backslashes off
//! ASCII punctuation before our post-pass sees the text.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// One selector on a `{selector body}` span head. The three variants
/// mirror marquee's literal-value fallbacks.
#[derive(Debug, Clone, PartialEq)]
pub enum Selector {
    /// `.name` — apply the style-sheet entry `name` if defined, else
    /// interpret `name` as a CSS colour keyword and apply as text
    /// colour.
    Class(String),
    /// `#RRGGBB` (or `#RGB` expanded to 8-bit channels) — hex colour
    /// literal, applied as text colour.
    HexColor([u8; 3]),
    /// `.<number>` (e.g. `.17`) — numeric size in points.
    Size(f32),
}

/// Enriched event yielded by [`parse`]. Combines pulldown-cmark's
/// standard events with our marquee-style `SpanStart` / `SpanEnd`
/// injections.
///
/// Block-level events (paragraph, heading, blockquote, list, item,
/// code block, rule) come in matched Start / End pairs. Inline styling
/// (emphasis, strong, strikethrough, sup, sub, link, span) also comes
/// in Start / End pairs. Text, code, math, and breaks are leaf events.
#[derive(Debug, Clone, PartialEq)]
pub enum RichEvent {
    // ── Block boundaries ──
    /// Start of a paragraph.
    ParagraphStart,
    /// End of a paragraph.
    ParagraphEnd,
    /// Start of a heading. `level` is 1..=6.
    HeadingStart {
        /// 1..=6.
        level: u8,
    },
    /// End of a heading. `level` is 1..=6.
    HeadingEnd {
        /// 1..=6.
        level: u8,
    },
    /// Start of a blockquote.
    BlockQuoteStart,
    /// End of a blockquote.
    BlockQuoteEnd,
    /// Start of a list. `ordered` is true for `1.`-style lists;
    /// `start` is the first-item number for ordered lists (1 for
    /// unordered).
    ListStart {
        /// True for ordered lists (`1.` / `1)`); false for unordered
        /// (`-` / `*` / `+`).
        ordered: bool,
        /// First item's number for ordered lists; `1` for unordered.
        start: u64,
    },
    /// End of a list.
    ListEnd,
    /// Start of a list item.
    ItemStart,
    /// End of a list item.
    ItemEnd,
    /// Start of a fenced or indented code block. `lang` is the info
    /// string for fenced blocks; `None` for indented / unlabelled.
    CodeBlockStart {
        /// Info string on the opening fence (e.g. `rust`). `None` for
        /// indented blocks or unlabelled fences.
        lang: Option<String>,
    },
    /// End of a code block.
    CodeBlockEnd,
    /// A horizontal rule (`---`, `***`, `___`).
    Rule,
    /// Start of a Quarto/Pandoc-style fenced div block. Opened by a
    /// line beginning with `:::` followed by a class name (e.g.
    /// `:::note`); closed by a bare `:::` line at the matching nesting
    /// level. The `class` payload is what follows the leading `:::` on
    /// the opening line, verbatim.
    DivStart {
        /// The class name on the opening `:::class` line.
        class: String,
    },
    /// End of a fenced div block.
    DivEnd,

    // ── Inline style boundaries ──
    /// Start of `*em*` / `_em_`.
    EmphasisStart,
    /// End of emphasis.
    EmphasisEnd,
    /// Start of `**strong**` / `__strong__`.
    StrongStart,
    /// End of strong.
    StrongEnd,
    /// Start of `~~strikethrough~~`.
    StrikethroughStart,
    /// End of strikethrough.
    StrikethroughEnd,
    /// Start of `^superscript^`.
    SuperscriptStart,
    /// End of superscript.
    SuperscriptEnd,
    /// Start of `~subscript~`.
    SubscriptStart,
    /// End of subscript.
    SubscriptEnd,
    /// Start of a link. `dest` is the resolved destination URL; used
    /// for the `link` style-sheet selector (colour + underline). Links
    /// are not interactive in the renderer.
    LinkStart {
        /// The destination URL as parsed by pulldown-cmark.
        dest: String,
    },
    /// End of a link.
    LinkEnd,
    /// Start of a marquee-style `{selector body}` span.
    SpanStart {
        /// The single selector token from the span head.
        selector: Selector,
    },
    /// End of a marquee-style span.
    SpanEnd,

    // ── Leaf inline events ──
    /// A run of literal text.
    Text(String),
    /// Inline `` `code` ``.
    Code(String),
    /// Inline `$math$`. Deferred for v1 — layers below may pass it
    /// through as literal text until an equation shaper lands.
    InlineMath(String),
    /// Display `$$math$$`. Deferred for v1.
    DisplayMath(String),
    /// A soft line break (single newline in the source).
    SoftBreak,
    /// A hard line break (trailing `\` or two spaces before newline).
    HardBreak,
}

/// Error returned by [`parse`]. Currently only the malformed-span
/// varieties survive parsing — unmatched braces are a legitimate
/// authoring mistake worth flagging up front.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// A `{` opened a span head that never closed with a matching
    /// `}` before end-of-input.
    UnclosedSpan {
        /// Byte offset into the original source where the opening
        /// `{` sits.
        opened_at: usize,
    },
    /// A `{…}` head contained no selector (e.g. `{ body}` or `{}`).
    EmptySelector {
        /// Byte offset of the opening `{`.
        opened_at: usize,
    },
    /// A `{` head selector didn't match any recognised token form.
    /// The selector text is captured for diagnostic use.
    UnrecognisedSelector {
        /// Byte offset of the opening `{`.
        opened_at: usize,
        /// The offending selector token.
        token: String,
    },
}

/// Parse `source` as marquee-flavoured markdown and produce a
/// [`RichEvent`] stream ready for layout. Errors indicate malformed
/// `{selector body}` spans or `:::class` / `:::` fenced-div markers;
/// unmatched `}` characters outside any span pass through as literal
/// text (no error).
///
/// The parser runs in two passes:
///
/// 1. **Div pre-pass** — scan lines for `:::class` open fences and
///    bare `:::` close fences. Split the source into a sequence of
///    text chunks separated by synthetic `DivStart` / `DivEnd`
///    events. Each text chunk is a self-contained markdown block; a
///    paragraph or list cannot span a div boundary.
/// 2. **Chunk pass** — feed each text chunk through pulldown-cmark
///    (with strikethrough, sup, sub, and math extensions enabled) and
///    layer on the `{selector body}` inline-span post-pass.
pub fn parse(source: &str) -> Result<Vec<RichEvent>, ParseError> {
    let chunks = split_divs(source)?;

    let mut out: Vec<RichEvent> = Vec::new();
    for chunk in chunks {
        match chunk {
            DivChunk::DivStart(class) => out.push(RichEvent::DivStart { class }),
            DivChunk::DivEnd => out.push(RichEvent::DivEnd),
            DivChunk::Markdown(md) => translate_chunk(&md, &mut out)?,
        }
    }
    Ok(out)
}

fn translate_chunk(md: &str, out: &mut Vec<RichEvent>) -> Result<(), ParseError> {
    if md.is_empty() {
        return Ok(());
    }
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_SUPERSCRIPT);
    opts.insert(Options::ENABLE_SUBSCRIPT);
    opts.insert(Options::ENABLE_MATH);
    let parser = Parser::new_ext(md, opts);
    let mut span_depth: usize = 0;
    for event in parser {
        translate_event(event, out, &mut span_depth)?;
    }
    if span_depth > 0 {
        return Err(ParseError::UnclosedSpan { opened_at: 0 });
    }
    Ok(())
}

/// One piece of the source, produced by [`split_divs`].
enum DivChunk {
    /// Markdown to be parsed by pulldown-cmark. May be empty (skipped
    /// downstream).
    Markdown(String),
    /// Opening fence — emit a synthetic `DivStart`.
    DivStart(String),
    /// Closing fence — emit a synthetic `DivEnd`.
    DivEnd,
}

/// Scan `source` line-by-line for `:::class` / `:::` fenced-div
/// markers. Returns an interleaved sequence of markdown chunks and
/// div boundaries.
///
/// Recognition rules (matching Pandoc / Quarto / marquee):
/// - A line whose first non-whitespace content is `:::` followed by a
///   class name (`:::note`, `:::warning`) opens a div. Nested opens
///   push onto a stack.
/// - A line whose first non-whitespace content is exactly `:::` (no
///   class after it) closes the innermost open div.
/// - Any line that isn't a fence is body markdown, appended to the
///   current chunk.
/// - Fences must appear on their own line (no trailing content after
///   the class name is honoured — anything after `:::class` up to the
///   line end is captured as part of the class token verbatim, so
///   `:::note-bold` is a class name `note-bold`).
///
/// Errors: an unmatched close (bare `:::` with no open on the stack)
/// or an unclosed div at end-of-input both surface as
/// [`ParseError::UnclosedSpan`] with a byte offset pointing at the
/// offending fence. We reuse the span-error variant rather than
/// growing the enum for what is effectively the same authoring
/// mistake — an unbalanced structural marker.
fn split_divs(source: &str) -> Result<Vec<DivChunk>, ParseError> {
    let mut out: Vec<DivChunk> = Vec::new();
    let mut current = String::new();
    // Stack of byte offsets of open fences, used for the unclosed-at-
    // end-of-input error message.
    let mut depth: Vec<usize> = Vec::new();
    let mut line_start: usize = 0;
    let bytes = source.as_bytes();
    let mut i = 0;
    while i <= source.len() {
        let at_eol = i == source.len() || bytes[i] == b'\n';
        if at_eol {
            let line = &source[line_start..i];
            match classify_line(line) {
                DivLine::Open(class) => {
                    if !current.is_empty() {
                        out.push(DivChunk::Markdown(std::mem::take(&mut current)));
                    }
                    out.push(DivChunk::DivStart(class));
                    depth.push(line_start);
                }
                DivLine::Close => {
                    if depth.pop().is_none() {
                        return Err(ParseError::UnclosedSpan {
                            opened_at: line_start,
                        });
                    }
                    if !current.is_empty() {
                        out.push(DivChunk::Markdown(std::mem::take(&mut current)));
                    }
                    out.push(DivChunk::DivEnd);
                }
                DivLine::Body => {
                    current.push_str(line);
                    if i < source.len() {
                        current.push('\n');
                    }
                }
            }
            line_start = i + 1;
        }
        i += 1;
    }
    if !current.is_empty() {
        out.push(DivChunk::Markdown(current));
    }
    if let Some(opened_at) = depth.pop() {
        return Err(ParseError::UnclosedSpan { opened_at });
    }
    Ok(out)
}

/// Classify one raw source line as a div-open, div-close, or body.
enum DivLine {
    /// `:::class` opening a fenced div. Payload is the class name.
    Open(String),
    /// Bare `:::` closing the innermost div.
    Close,
    /// Regular markdown line.
    Body,
}

fn classify_line(line: &str) -> DivLine {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix(":::") {
        let rest = rest.trim();
        if rest.is_empty() {
            return DivLine::Close;
        }
        // Some editors / authors write more than three colons for
        // outer nesting (`::::note`); we treat any run of ≥3 leading
        // colons the same as `:::`. The class name is whatever is
        // left after all leading colons.
        let class = rest.trim_start_matches(':').trim();
        if class.is_empty() {
            return DivLine::Close;
        }
        return DivLine::Open(class.to_string());
    }
    DivLine::Body
}

fn translate_event(
    event: Event<'_>,
    out: &mut Vec<RichEvent>,
    span_depth: &mut usize,
) -> Result<(), ParseError> {
    match event {
        Event::Start(tag) => {
            translate_start_tag(tag, out);
            Ok(())
        }
        Event::End(tag) => {
            translate_end_tag(tag, out);
            Ok(())
        }
        Event::Text(s) => scan_text_for_spans(&s, out, span_depth),
        Event::Code(s) => {
            out.push(RichEvent::Code(s.into_string()));
            Ok(())
        }
        Event::InlineMath(s) => {
            out.push(RichEvent::InlineMath(s.into_string()));
            Ok(())
        }
        Event::DisplayMath(s) => {
            out.push(RichEvent::DisplayMath(s.into_string()));
            Ok(())
        }
        Event::SoftBreak => {
            out.push(RichEvent::SoftBreak);
            Ok(())
        }
        Event::HardBreak => {
            out.push(RichEvent::HardBreak);
            Ok(())
        }
        Event::Rule => {
            out.push(RichEvent::Rule);
            Ok(())
        }
        // HTML / footnote / task-list events are ignored for v1; images
        // are out of scope. Task-list markers ride inside Item bodies
        // and would surface as text; the FootnoteReference / HTML
        // events pass through silently.
        Event::Html(_)
        | Event::InlineHtml(_)
        | Event::FootnoteReference(_)
        | Event::TaskListMarker(_) => Ok(()),
    }
}

fn translate_start_tag(tag: Tag<'_>, out: &mut Vec<RichEvent>) {
    match tag {
        Tag::Paragraph => out.push(RichEvent::ParagraphStart),
        Tag::Heading { level, .. } => out.push(RichEvent::HeadingStart {
            level: heading_level_to_u8(level),
        }),
        Tag::BlockQuote(_) => out.push(RichEvent::BlockQuoteStart),
        Tag::List(start) => out.push(RichEvent::ListStart {
            ordered: start.is_some(),
            start: start.unwrap_or(1),
        }),
        Tag::Item => out.push(RichEvent::ItemStart),
        Tag::CodeBlock(kind) => out.push(RichEvent::CodeBlockStart {
            lang: code_block_lang(kind),
        }),
        Tag::Emphasis => out.push(RichEvent::EmphasisStart),
        Tag::Strong => out.push(RichEvent::StrongStart),
        Tag::Strikethrough => out.push(RichEvent::StrikethroughStart),
        Tag::Superscript => out.push(RichEvent::SuperscriptStart),
        Tag::Subscript => out.push(RichEvent::SubscriptStart),
        Tag::Link { dest_url, .. } => out.push(RichEvent::LinkStart {
            dest: dest_url.into_string(),
        }),
        // Ignored / out-of-scope tags: HtmlBlock, FootnoteDefinition,
        // DefinitionList (+ friends), Table (+ friends), Image,
        // MetadataBlock.
        _ => {}
    }
}

fn translate_end_tag(tag: TagEnd, out: &mut Vec<RichEvent>) {
    match tag {
        TagEnd::Paragraph => out.push(RichEvent::ParagraphEnd),
        TagEnd::Heading(level) => out.push(RichEvent::HeadingEnd {
            level: heading_level_to_u8(level),
        }),
        TagEnd::BlockQuote(_) => out.push(RichEvent::BlockQuoteEnd),
        TagEnd::List(_) => out.push(RichEvent::ListEnd),
        TagEnd::Item => out.push(RichEvent::ItemEnd),
        TagEnd::CodeBlock => out.push(RichEvent::CodeBlockEnd),
        TagEnd::Emphasis => out.push(RichEvent::EmphasisEnd),
        TagEnd::Strong => out.push(RichEvent::StrongEnd),
        TagEnd::Strikethrough => out.push(RichEvent::StrikethroughEnd),
        TagEnd::Superscript => out.push(RichEvent::SuperscriptEnd),
        TagEnd::Subscript => out.push(RichEvent::SubscriptEnd),
        TagEnd::Link => out.push(RichEvent::LinkEnd),
        _ => {}
    }
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn code_block_lang(kind: pulldown_cmark::CodeBlockKind<'_>) -> Option<String> {
    use pulldown_cmark::CodeBlockKind;
    match kind {
        CodeBlockKind::Fenced(info) => {
            let s = info.into_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        CodeBlockKind::Indented => None,
    }
}

// ─── Span pre-pass ──────────────────────────────────────────────────────────

/// Tokenize a Text payload for `{selector body}` markers.
///
/// Walks `s` character-by-character. An unescaped `{` opens a span
/// (parse the selector, emit `SpanStart`, increment depth). An
/// unescaped `}` closes the innermost open span (emit `SpanEnd`,
/// decrement depth) or, if no span is open, passes through as literal
/// text. Backslash-escapes `\{` and `\}` produce literal braces.
///
/// Non-span text is coalesced into a single `Text` event per contiguous
/// run — one call to this function may emit any number of alternating
/// text-run and span-start events.
fn scan_text_for_spans(
    s: &str,
    out: &mut Vec<RichEvent>,
    span_depth: &mut usize,
) -> Result<(), ParseError> {
    let bytes = s.as_bytes();
    let mut buf = String::new();
    let mut i = 0;
    while i < s.len() {
        let b = bytes[i];
        // Doubled braces escape to literal braces so authors can
        // include a `{` or `}` in prose without opening a span. Only
        // applied at depth 0 — inside a span, `}}` is close-plus-close
        // (matching the natural reading of nested spans like
        // `{.red {.17 x}}`), and `{{` is open-a-new-span-plus-error
        // (the empty selector case).
        if *span_depth == 0 && b == b'{' && i + 1 < s.len() && bytes[i + 1] == b'{' {
            buf.push('{');
            i += 2;
            continue;
        }
        if *span_depth == 0 && b == b'}' && i + 1 < s.len() && bytes[i + 1] == b'}' {
            buf.push('}');
            i += 2;
            continue;
        }
        if b == b'{' {
            // Flush accumulated text.
            if !buf.is_empty() {
                out.push(RichEvent::Text(std::mem::take(&mut buf)));
            }
            // Parse selector head from immediately after `{`.
            let head_start = i + 1;
            let (selector, body_start) = parse_selector_head(s, head_start)?;
            out.push(RichEvent::SpanStart { selector });
            *span_depth += 1;
            i = body_start;
            continue;
        }
        if b == b'}' && *span_depth > 0 {
            if !buf.is_empty() {
                out.push(RichEvent::Text(std::mem::take(&mut buf)));
            }
            out.push(RichEvent::SpanEnd);
            *span_depth -= 1;
            i += 1;
            continue;
        }
        // A stray `}` outside any span falls through to the literal
        // text-append branch below.
        // Multi-byte UTF-8 char — copy the whole codepoint into buf.
        let ch_end = next_char_boundary(s, i);
        buf.push_str(&s[i..ch_end]);
        i = ch_end;
    }
    if !buf.is_empty() {
        out.push(RichEvent::Text(buf));
    }
    Ok(())
}

/// Step to the next UTF-8 char boundary at or after `start`. Handles
/// the multi-byte case correctly (str indices always align).
fn next_char_boundary(s: &str, start: usize) -> usize {
    let mut i = start + 1;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Read the selector token from `s[head_start..]`, stopping at the
/// first whitespace (which begins the body) or at `}` (empty body).
/// Returns `(selector, body_start_index)`.
fn parse_selector_head(s: &str, head_start: usize) -> Result<(Selector, usize), ParseError> {
    let bytes = s.as_bytes();
    let opened_at = head_start.saturating_sub(1);
    let mut i = head_start;
    // Skip leading whitespace inside the head — a courtesy so
    // `{ .red x }` behaves identically to `{.red x}`.
    while i < s.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let token_start = i;
    while i < s.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\t' || b == b'}' {
            break;
        }
        i += 1;
    }
    let token = &s[token_start..i];
    if token.is_empty() {
        return Err(ParseError::EmptySelector { opened_at });
    }
    let selector = classify_selector(token).ok_or_else(|| ParseError::UnrecognisedSelector {
        opened_at,
        token: token.to_string(),
    })?;
    // Consume exactly one separating whitespace before the body, so
    // `{.red hello}` yields body `hello` rather than ` hello`. If the
    // very next char is `}`, the body is empty — leave `i` at the `}`.
    if i < s.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\t' {
            i += 1;
        }
    }
    Ok((selector, i))
}

/// Classify a raw selector token as one of the three [`Selector`]
/// variants. Returns `None` for unrecognised tokens (upstream turns
/// this into `ParseError::UnrecognisedSelector`).
fn classify_selector(token: &str) -> Option<Selector> {
    if let Some(rest) = token.strip_prefix('#') {
        return parse_hex_color(rest).map(Selector::HexColor);
    }
    if let Some(rest) = token.strip_prefix('.') {
        // `.17` → numeric size; `.red` → class / colour name.
        // A dotted numeric literal has all-ASCII-digit characters.
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
            if let Ok(v) = rest.parse::<f32>() {
                return Some(Selector::Size(v));
            }
        }
        if is_valid_class_name(rest) {
            return Some(Selector::Class(rest.to_string()));
        }
    }
    None
}

fn parse_hex_color(hex: &str) -> Option<[u8; 3]> {
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            Some([r * 17, g * 17, b * 17])
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some([r, g, b])
        }
        _ => None,
    }
}

fn is_valid_class_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // ASCII letter, digit, `-`, `_`. Matches CSS identifier rules
    // loosely — enough for the class / colour-name lookups we care
    // about.
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(s: &str) -> Vec<RichEvent> {
        parse(s).expect("parse failed")
    }

    #[test]
    fn plain_text_produces_paragraph_with_text() {
        let ev = parse_ok("hello");
        assert_eq!(
            ev,
            vec![
                RichEvent::ParagraphStart,
                RichEvent::Text("hello".to_string()),
                RichEvent::ParagraphEnd,
            ]
        );
    }

    #[test]
    fn bold_italic_and_strikethrough_native_events() {
        let ev = parse_ok("**bold** *em* ~~strike~~");
        // Only checking key structural events — text nodes in between
        // are asserted to exist.
        assert!(ev.contains(&RichEvent::StrongStart));
        assert!(ev.contains(&RichEvent::StrongEnd));
        assert!(ev.contains(&RichEvent::EmphasisStart));
        assert!(ev.contains(&RichEvent::EmphasisEnd));
        assert!(ev.contains(&RichEvent::StrikethroughStart));
        assert!(ev.contains(&RichEvent::StrikethroughEnd));
    }

    #[test]
    fn sup_and_sub_are_native_events() {
        let ev = parse_ok("A ^sup^ and ~sub~ here");
        assert!(ev.contains(&RichEvent::SubscriptStart));
        assert!(ev.contains(&RichEvent::SubscriptEnd));
        assert!(ev.contains(&RichEvent::SuperscriptStart));
        assert!(ev.contains(&RichEvent::SuperscriptEnd));
    }

    #[test]
    fn math_events_pass_through() {
        let ev = parse_ok("Inline $x^2$ and block:\n\n$$\\int f$$\n");
        assert!(matches!(
            ev.iter().find(|e| matches!(e, RichEvent::InlineMath(_))),
            Some(RichEvent::InlineMath(s)) if s == "x^2"
        ));
        assert!(matches!(
            ev.iter().find(|e| matches!(e, RichEvent::DisplayMath(_))),
            Some(RichEvent::DisplayMath(s)) if s.trim() == "\\int f"
        ));
    }

    #[test]
    fn class_span_wraps_body_text() {
        let ev = parse_ok("{.red hello}");
        assert_eq!(
            ev,
            vec![
                RichEvent::ParagraphStart,
                RichEvent::SpanStart {
                    selector: Selector::Class("red".to_string())
                },
                RichEvent::Text("hello".to_string()),
                RichEvent::SpanEnd,
                RichEvent::ParagraphEnd,
            ]
        );
    }

    #[test]
    fn hex_color_selector() {
        let ev = parse_ok("{#ae5013 tint}");
        let span = ev
            .iter()
            .find(|e| matches!(e, RichEvent::SpanStart { .. }))
            .expect("span start");
        match span {
            RichEvent::SpanStart {
                selector: Selector::HexColor([r, g, b]),
            } => {
                assert_eq!(*r, 0xae);
                assert_eq!(*g, 0x50);
                assert_eq!(*b, 0x13);
            }
            _ => panic!("expected hex-colour selector, got {span:?}"),
        }
    }

    #[test]
    fn size_selector_numeric() {
        let ev = parse_ok("{.17 huge}");
        let span = ev
            .iter()
            .find(|e| matches!(e, RichEvent::SpanStart { .. }))
            .expect("span start");
        assert!(matches!(
            span,
            RichEvent::SpanStart {
                selector: Selector::Size(v)
            } if (*v - 17.0).abs() < 1e-6
        ));
    }

    #[test]
    fn nested_spans_open_and_close_in_order() {
        let ev = parse_ok("{.red {.17 something}}");
        // Expect: SpanStart(red), SpanStart(17), Text("something"),
        // SpanEnd, SpanEnd.
        let inner: Vec<&RichEvent> = ev
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    RichEvent::SpanStart { .. } | RichEvent::SpanEnd | RichEvent::Text(_)
                )
            })
            .collect();
        assert_eq!(inner.len(), 5, "got {inner:?}");
        assert!(matches!(
            inner[0],
            RichEvent::SpanStart {
                selector: Selector::Class(c)
            } if c == "red"
        ));
        assert!(matches!(
            inner[1],
            RichEvent::SpanStart {
                selector: Selector::Size(v)
            } if (v - 17.0).abs() < 1e-6
        ));
        assert!(matches!(inner[2], RichEvent::Text(t) if t == "something"));
        assert!(matches!(inner[3], RichEvent::SpanEnd));
        assert!(matches!(inner[4], RichEvent::SpanEnd));
    }

    #[test]
    fn ambiguous_body_treats_extra_dots_as_text() {
        // `{.red .17 something}` — selector is `.red`, body is
        // `.17 something` (no second selector).
        let ev = parse_ok("{.red .17 something}");
        let text_after_span = ev
            .iter()
            .find_map(|e| match e {
                RichEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .expect("text");
        assert_eq!(text_after_span, ".17 something");
    }

    #[test]
    fn doubled_braces_are_literal() {
        let ev = parse_ok("{{not a span}}");
        let text = ev
            .iter()
            .find_map(|e| match e {
                RichEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .expect("text");
        assert_eq!(text, "{not a span}");
        assert!(!ev.iter().any(|e| matches!(e, RichEvent::SpanStart { .. })));
    }

    #[test]
    fn stray_close_brace_outside_span_is_literal() {
        let ev = parse_ok("just a } brace");
        let text = ev
            .iter()
            .find_map(|e| match e {
                RichEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .expect("text");
        assert_eq!(text, "just a } brace");
    }

    #[test]
    fn empty_selector_errors() {
        let err = parse("{}").unwrap_err();
        assert!(
            matches!(err, ParseError::EmptySelector { .. }),
            "got {err:?}"
        );
        let err = parse("{  }").unwrap_err();
        assert!(
            matches!(err, ParseError::EmptySelector { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn unrecognised_selector_errors() {
        let err = parse("{gibberish body}").unwrap_err();
        assert!(matches!(err, ParseError::UnrecognisedSelector { .. }));
    }

    #[test]
    fn unclosed_span_errors() {
        let err = parse("{.red never closes").unwrap_err();
        assert!(matches!(err, ParseError::UnclosedSpan { .. }));
    }

    #[test]
    fn span_may_contain_markdown_inline_elements() {
        // Span brackets a **bold** run — pulldown-cmark tokenises it,
        // and our pre-pass wraps the whole sub-tree in SpanStart /
        // SpanEnd because `}` sits at depth 1 relative to the pre-pass.
        let ev = parse_ok("{.red **bold**}");
        let span_starts = ev
            .iter()
            .filter(|e| matches!(e, RichEvent::SpanStart { .. }))
            .count();
        let strong_starts = ev
            .iter()
            .filter(|e| matches!(e, RichEvent::StrongStart))
            .count();
        assert_eq!(span_starts, 1);
        assert_eq!(strong_starts, 1);
        // SpanEnd must be before ParagraphEnd — well-nested.
        let span_end_idx = ev.iter().position(|e| *e == RichEvent::SpanEnd).unwrap();
        let strong_end_idx = ev.iter().position(|e| *e == RichEvent::StrongEnd).unwrap();
        let paragraph_end_idx = ev
            .iter()
            .position(|e| *e == RichEvent::ParagraphEnd)
            .unwrap();
        assert!(strong_end_idx < span_end_idx);
        assert!(span_end_idx < paragraph_end_idx);
    }

    #[test]
    fn headings_report_correct_level() {
        let ev = parse_ok("# h1\n\n## h2\n\n###### h6");
        let levels: Vec<u8> = ev
            .iter()
            .filter_map(|e| match e {
                RichEvent::HeadingStart { level } => Some(*level),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![1, 2, 6]);
    }

    #[test]
    fn ordered_and_unordered_lists() {
        let ev = parse_ok("- a\n- b\n\n1. one\n2. two");
        let starts: Vec<bool> = ev
            .iter()
            .filter_map(|e| match e {
                RichEvent::ListStart { ordered, .. } => Some(*ordered),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![false, true]);
        let start_num = ev
            .iter()
            .find_map(|e| match e {
                RichEvent::ListStart {
                    ordered: true,
                    start,
                } => Some(*start),
                _ => None,
            })
            .unwrap();
        assert_eq!(start_num, 1);
    }

    #[test]
    fn horizontal_rule() {
        let ev = parse_ok("---");
        assert!(ev.contains(&RichEvent::Rule));
    }

    #[test]
    fn code_block_reports_language() {
        let ev = parse_ok("```rust\nlet x = 1;\n```");
        let lang = ev
            .iter()
            .find_map(|e| match e {
                RichEvent::CodeBlockStart { lang } => Some(lang.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(lang, Some("rust".to_string()));
    }

    #[test]
    fn div_open_and_close_emit_events() {
        let ev = parse_ok(":::note\nbody\n:::");
        // Expect: DivStart(note), ParagraphStart, Text("body"),
        // ParagraphEnd, DivEnd.
        assert_eq!(
            ev,
            vec![
                RichEvent::DivStart {
                    class: "note".to_string()
                },
                RichEvent::ParagraphStart,
                RichEvent::Text("body".to_string()),
                RichEvent::ParagraphEnd,
                RichEvent::DivEnd,
            ]
        );
    }

    #[test]
    fn nested_divs_stack_lifo() {
        let ev = parse_ok(":::outer\n:::inner\nhi\n:::\n:::");
        let boundaries: Vec<&RichEvent> = ev
            .iter()
            .filter(|e| matches!(e, RichEvent::DivStart { .. } | RichEvent::DivEnd))
            .collect();
        assert_eq!(boundaries.len(), 4);
        assert!(matches!(
            boundaries[0],
            RichEvent::DivStart { class } if class == "outer"
        ));
        assert!(matches!(
            boundaries[1],
            RichEvent::DivStart { class } if class == "inner"
        ));
        assert!(matches!(boundaries[2], RichEvent::DivEnd));
        assert!(matches!(boundaries[3], RichEvent::DivEnd));
    }

    #[test]
    fn div_body_is_parsed_as_markdown() {
        let ev = parse_ok(":::note\n**bold** in a div\n:::");
        assert!(ev.contains(&RichEvent::StrongStart));
        assert!(ev.contains(&RichEvent::StrongEnd));
        let div_start_idx = ev
            .iter()
            .position(|e| matches!(e, RichEvent::DivStart { .. }))
            .unwrap();
        let div_end_idx = ev.iter().position(|e| *e == RichEvent::DivEnd).unwrap();
        let strong_idx = ev
            .iter()
            .position(|e| *e == RichEvent::StrongStart)
            .unwrap();
        assert!(
            div_start_idx < strong_idx && strong_idx < div_end_idx,
            "strong span must sit between DivStart and DivEnd"
        );
    }

    #[test]
    fn unclosed_div_errors() {
        let err = parse(":::note\nunclosed").unwrap_err();
        assert!(matches!(err, ParseError::UnclosedSpan { .. }));
    }

    #[test]
    fn stray_close_div_errors() {
        let err = parse("no open\n:::").unwrap_err();
        assert!(matches!(err, ParseError::UnclosedSpan { .. }));
    }

    #[test]
    fn div_with_multi_paragraph_body() {
        let ev = parse_ok(":::note\nfirst paragraph\n\nsecond paragraph\n:::");
        let paragraph_starts = ev
            .iter()
            .filter(|e| matches!(e, RichEvent::ParagraphStart))
            .count();
        assert_eq!(paragraph_starts, 2, "expected two paragraphs, got {ev:?}");
    }

    #[test]
    fn hex_short_form_expands_to_full_bytes() {
        let ev = parse_ok("{#f00 red}");
        let span = ev
            .iter()
            .find(|e| matches!(e, RichEvent::SpanStart { .. }))
            .unwrap();
        assert!(matches!(
            span,
            RichEvent::SpanStart {
                selector: Selector::HexColor([255, 0, 0])
            }
        ));
    }
}
