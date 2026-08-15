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
//! **Parsing never fails.** Any input is a valid document. A malformed
//! construct degrades to the literal characters that spell it: an
//! unrecognised selector head renders as `{`-plus-prose, an unclosed
//! span renders its head as text, a stray `:::` line is body text, and
//! an unclosed `:::` fence is closed at end of input. This mirrors
//! marquee, where markdown is a formatting convenience layered over
//! arbitrary user strings — a label that happens to contain a brace
//! must still render.
//!
//! **Underscore emphasis is underline.** `*x*` is italic and `_x_` is
//! underline, matching marquee. `**x**` and `__x__` are both bold.
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
//! **Literal braces.** Both `\{` and `{{` yield a literal `{`; `\}`
//! and `}}` yield a literal `}`.
//!
//! **Raw HTML passes through as text.** There is no HTML renderer
//! behind this pipeline, so `<b>` renders as the four characters that
//! spell it rather than vanishing.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// One selector on a `{selector body}` span head. The variants mirror
/// marquee's literal-value fallbacks.
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
    /// `#name` where `name` isn't a hex colour — a style-sheet lookup
    /// in the id namespace. The payload keeps the leading `#`, so id
    /// and class entries can share one map without colliding.
    HashName(String),
}

/// Enriched event yielded by [`parse`]. Combines pulldown-cmark's
/// standard events with our marquee-style `SpanStart` / `SpanEnd`
/// injections.
///
/// Block-level events (paragraph, heading, blockquote, list, item,
/// code block, rule) come in matched Start / End pairs. Inline styling
/// (emphasis, underline, strong, strikethrough, sup, sub, link, span)
/// also comes in Start / End pairs. Text, code, math, and breaks are
/// leaf events.
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
    /// Start of `*em*`.
    EmphasisStart,
    /// End of emphasis.
    EmphasisEnd,
    /// Start of `_underline_`.
    UnderlineStart,
    /// End of underline.
    UnderlineEnd,
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
    /// Inline `$math$`, passed through as its source text until an
    /// equation shaper lands.
    InlineMath(String),
    /// Display `$$math$$`, passed through as its source text.
    DisplayMath(String),
    /// A soft line break (single newline in the source).
    SoftBreak,
    /// A hard line break (trailing `\` or two spaces before newline).
    HardBreak,
}

/// Parse `source` as marquee-flavoured markdown and produce a
/// [`RichEvent`] stream ready for layout.
///
/// Infallible: every input is a document. See the module docs for how
/// malformed spans and fences degrade.
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
pub fn parse(source: &str) -> Vec<RichEvent> {
    let chunks = split_divs(source);

    let mut out: Vec<RichEvent> = Vec::new();
    for chunk in chunks {
        match chunk {
            DivChunk::DivStart(class) => out.push(RichEvent::DivStart { class }),
            DivChunk::DivEnd => out.push(RichEvent::DivEnd),
            DivChunk::Markdown(md) => translate_chunk(&md, &mut out),
        }
    }
    out
}

/// An inline span whose `{head` has been seen but whose `}` hasn't.
/// Carries what it takes to un-emit the span if the close never
/// arrives.
struct OpenSpan {
    /// Index into the output stream of the emitted `SpanStart`.
    event_idx: usize,
    /// The source characters the head consumed, replayed as literal
    /// text when the span turns out to be unclosed.
    head: String,
}

fn translate_chunk(md: &str, out: &mut Vec<RichEvent>) {
    if md.is_empty() {
        return;
    }
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_SUPERSCRIPT);
    opts.insert(Options::ENABLE_SUBSCRIPT);
    opts.insert(Options::ENABLE_MATH);
    let mut state = ChunkState {
        spans: Vec::new(),
        code_block_depth: 0,
        emphasis_underscore: Vec::new(),
    };
    for (event, range) in Parser::new_ext(md, opts).into_offset_iter() {
        translate_event(event, range, md, out, &mut state);
    }
    // A span left open at end of chunk was never a span — replay its
    // head as the literal text it spells.
    for open in state.spans {
        out[open.event_idx] = RichEvent::Text(open.head);
    }
}

/// Per-chunk translation state carried across events.
struct ChunkState {
    /// Spans opened by `{head` and awaiting their `}`.
    spans: Vec<OpenSpan>,
    /// Nesting depth of code blocks — inside one, `{`, `~`, `^` are
    /// all literal.
    code_block_depth: usize,
    /// One entry per open `Tag::Emphasis`, recording whether its
    /// delimiter was `_` (underline) rather than `*` (italic).
    emphasis_underscore: Vec<bool>,
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
/// Unbalanced fences degrade rather than fail: a bare `:::` with no
/// open div is body text, and any div still open at end of input is
/// closed there.
fn split_divs(source: &str) -> Vec<DivChunk> {
    let mut out: Vec<DivChunk> = Vec::new();
    let mut current = String::new();
    let mut depth: usize = 0;
    let mut line_start: usize = 0;
    let bytes = source.as_bytes();
    let mut i = 0;
    while i <= source.len() {
        let at_eol = i == source.len() || bytes[i] == b'\n';
        if at_eol {
            let line = &source[line_start..i];
            let mut classified = classify_line(line);
            if matches!(classified, DivLine::Close) && depth == 0 {
                classified = DivLine::Body;
            }
            match classified {
                DivLine::Open(class) => {
                    if !current.is_empty() {
                        out.push(DivChunk::Markdown(std::mem::take(&mut current)));
                    }
                    out.push(DivChunk::DivStart(class));
                    depth += 1;
                }
                DivLine::Close => {
                    depth -= 1;
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
    for _ in 0..depth {
        out.push(DivChunk::DivEnd);
    }
    out
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
    range: std::ops::Range<usize>,
    md: &str,
    out: &mut Vec<RichEvent>,
    state: &mut ChunkState,
) {
    match event {
        Event::Start(Tag::Emphasis) => {
            // CommonMark forbids intraword `_`, so the byte at the
            // tag's start is always the opening delimiter.
            let underscore = md.as_bytes().get(range.start) == Some(&b'_');
            state.emphasis_underscore.push(underscore);
            out.push(if underscore {
                RichEvent::UnderlineStart
            } else {
                RichEvent::EmphasisStart
            });
        }
        Event::End(TagEnd::Emphasis) => {
            let underscore = state.emphasis_underscore.pop().unwrap_or(false);
            out.push(if underscore {
                RichEvent::UnderlineEnd
            } else {
                RichEvent::EmphasisEnd
            });
        }
        Event::Start(tag) => {
            if matches!(tag, Tag::CodeBlock(_)) {
                state.code_block_depth += 1;
            }
            translate_start_tag(tag, out);
        }
        Event::End(tag) => {
            if matches!(tag, TagEnd::CodeBlock) {
                state.code_block_depth = state.code_block_depth.saturating_sub(1);
            }
            translate_end_tag(tag, out);
        }
        Event::Text(s) => {
            if state.code_block_depth > 0 {
                // Inside a code block: `{`, `~`, `^`, etc. are all
                // literal — no inline-span pre-pass.
                out.push(RichEvent::Text(s.into_string()));
                return;
            }
            // pulldown reports an escaped character as its own text
            // event starting *after* the backslash, so a preceding
            // `\` in the source marks the payload's first character
            // as escaped. That character bypasses span scanning, which
            // is what makes `\{` a literal brace.
            let escaped_head = range.start > 0 && md.as_bytes()[range.start - 1] == b'\\';
            if escaped_head && !s.is_empty() {
                let head_end = next_char_boundary(&s, 0);
                out.push(RichEvent::Text(s[..head_end].to_string()));
                scan_text_for_spans(&s[head_end..], out, &mut state.spans);
            } else {
                scan_text_for_spans(&s, out, &mut state.spans);
            }
        }
        Event::Code(s) => out.push(RichEvent::Code(s.into_string())),
        Event::InlineMath(s) => out.push(RichEvent::InlineMath(s.into_string())),
        Event::DisplayMath(s) => out.push(RichEvent::DisplayMath(s.into_string())),
        Event::SoftBreak => out.push(RichEvent::SoftBreak),
        Event::HardBreak => out.push(RichEvent::HardBreak),
        Event::Rule => out.push(RichEvent::Rule),
        // Raw HTML renders as the characters that spell it — there is
        // no HTML renderer behind this pipeline, and dropping the
        // markup would silently lose content.
        Event::Html(s) | Event::InlineHtml(s) => out.push(RichEvent::Text(s.into_string())),
        // Footnote references and task-list markers have no visual
        // vocabulary here yet.
        Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
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
        Tag::Strong => out.push(RichEvent::StrongStart),
        Tag::Strikethrough => out.push(RichEvent::StrikethroughStart),
        Tag::Superscript => out.push(RichEvent::SuperscriptStart),
        Tag::Subscript => out.push(RichEvent::SubscriptStart),
        Tag::Link { dest_url, .. } => out.push(RichEvent::LinkStart {
            dest: dest_url.into_string(),
        }),
        // Ignored / out-of-scope tags: HtmlBlock, FootnoteDefinition,
        // DefinitionList (+ friends), Table (+ friends), Image,
        // MetadataBlock. `Emphasis` is handled by the caller, which
        // needs the source range to tell `*` from `_`.
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
/// Walks `s` character-by-character. A `{` followed by a recognised
/// selector opens a span (emit `SpanStart`, push onto `spans`); a `{`
/// followed by anything else is literal. A `}` closes the innermost
/// open span (emit `SpanEnd`, pop) or, if no span is open, passes
/// through as literal text. Doubled `{{` / `}}` are literal braces.
///
/// Non-span text is coalesced into a single `Text` event per contiguous
/// run — one call to this function may emit any number of alternating
/// text-run and span-start events.
fn scan_text_for_spans(s: &str, out: &mut Vec<RichEvent>, spans: &mut Vec<OpenSpan>) {
    let bytes = s.as_bytes();
    let mut buf = String::new();
    let mut i = 0;
    while i < s.len() {
        let b = bytes[i];
        // Doubled braces escape to literal braces so authors can
        // include a `{` or `}` in prose without opening a span. Only
        // applied at depth 0 — inside a span, `}}` is close-plus-close
        // (matching the natural reading of nested spans like
        // `{.red {.17 x}}`), and `{{` opens a span whose selector is
        // itself a brace, which falls back to literal.
        if spans.is_empty() && b == b'{' && i + 1 < s.len() && bytes[i + 1] == b'{' {
            buf.push('{');
            i += 2;
            continue;
        }
        if spans.is_empty() && b == b'}' && i + 1 < s.len() && bytes[i + 1] == b'}' {
            buf.push('}');
            i += 2;
            continue;
        }
        if b == b'{' {
            match parse_selector_head(s, i + 1) {
                Some((selector, body_start)) => {
                    if !buf.is_empty() {
                        out.push(RichEvent::Text(std::mem::take(&mut buf)));
                    }
                    out.push(RichEvent::SpanStart { selector });
                    spans.push(OpenSpan {
                        event_idx: out.len() - 1,
                        head: s[i..body_start].to_string(),
                    });
                    i = body_start;
                }
                // No selector we recognise — the brace is prose.
                None => {
                    buf.push('{');
                    i += 1;
                }
            }
            continue;
        }
        if b == b'}' && !spans.is_empty() {
            if !buf.is_empty() {
                out.push(RichEvent::Text(std::mem::take(&mut buf)));
            }
            out.push(RichEvent::SpanEnd);
            spans.pop();
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
/// Returns `(selector, body_start_index)`, or `None` when the head
/// holds no recognisable selector.
fn parse_selector_head(s: &str, head_start: usize) -> Option<(Selector, usize)> {
    let bytes = s.as_bytes();
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
    let selector = classify_selector(&s[token_start..i])?;
    // Consume exactly one separating whitespace before the body, so
    // `{.red hello}` yields body `hello` rather than ` hello`. If the
    // very next char is `}`, the body is empty — leave `i` at the `}`.
    if i < s.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\t' {
            i += 1;
        }
    }
    Some((selector, i))
}

/// Classify a raw selector token as one of the [`Selector`] variants.
/// Returns `None` for unrecognised tokens, which the caller renders as
/// literal text.
fn classify_selector(token: &str) -> Option<Selector> {
    if let Some(rest) = token.strip_prefix('#') {
        if let Some(rgb) = parse_hex_color(rest) {
            return Some(Selector::HexColor(rgb));
        }
        if is_valid_class_name(rest) {
            return Some(Selector::HashName(token.to_string()));
        }
        return None;
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
        parse(s)
    }

    fn joined_text(ev: &[RichEvent]) -> String {
        ev.iter()
            .filter_map(|e| match e {
                RichEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
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
    fn empty_selector_renders_the_brace_literally() {
        for src in ["{}", "{  }"] {
            let ev = parse(src);
            assert!(
                !ev.iter().any(|e| matches!(e, RichEvent::SpanStart { .. })),
                "{src:?} opened a span"
            );
            assert!(joined_text(&ev).contains('{'), "{src:?} lost its brace");
        }
    }

    #[test]
    fn unrecognised_selector_renders_the_brace_literally() {
        let ev = parse("{gibberish body}");
        assert!(!ev.iter().any(|e| matches!(e, RichEvent::SpanStart { .. })));
        assert_eq!(joined_text(&ev), "{gibberish body}");
    }

    #[test]
    fn unclosed_span_replays_its_head_as_text() {
        let ev = parse("{.red never closes");
        assert!(!ev.iter().any(|e| matches!(e, RichEvent::SpanStart { .. })));
        assert_eq!(joined_text(&ev), "{.red never closes");
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
    fn unclosed_div_is_closed_at_end_of_input() {
        let ev = parse(":::note\nunclosed");
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, RichEvent::DivStart { .. }))
                .count(),
            1
        );
        assert_eq!(ev.last(), Some(&RichEvent::DivEnd));
    }

    #[test]
    fn stray_close_div_is_body_text() {
        let ev = parse("no open\n:::");
        assert!(!ev.iter().any(|e| matches!(e, RichEvent::DivEnd)));
        assert!(joined_text(&ev).contains(":::"));
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

    #[test]
    fn underscore_emphasis_is_underline_and_star_is_italic() {
        let ev = parse_ok("*italic* and _under_ and __bold__");
        assert!(ev.contains(&RichEvent::EmphasisStart));
        assert!(ev.contains(&RichEvent::EmphasisEnd));
        assert!(ev.contains(&RichEvent::UnderlineStart));
        assert!(ev.contains(&RichEvent::UnderlineEnd));
        assert!(ev.contains(&RichEvent::StrongStart));
    }

    #[test]
    fn backslash_escaped_braces_are_literal() {
        // Pins the pulldown behaviour the escape detection relies on:
        // an escaped character starts its own text event one byte
        // after the backslash.
        let ev = parse_ok(r"a \{.red b\} c");
        assert!(!ev.iter().any(|e| matches!(e, RichEvent::SpanStart { .. })));
        assert_eq!(joined_text(&ev), "a {.red b} c");
    }

    #[test]
    fn hash_name_selector_survives_as_a_lookup_key() {
        let ev = parse_ok("{#note body}");
        let span = ev
            .iter()
            .find(|e| matches!(e, RichEvent::SpanStart { .. }))
            .expect("span start");
        assert!(matches!(
            span,
            RichEvent::SpanStart {
                selector: Selector::HashName(n)
            } if n == "#note"
        ));
    }

    #[test]
    fn raw_html_renders_as_literal_text() {
        let ev = parse_ok("<b>hi</b>");
        assert_eq!(joined_text(&ev), "<b>hi</b>");
    }

    #[test]
    fn text_before_a_failed_span_is_not_lost() {
        let ev = parse_ok("before {nope after");
        assert_eq!(joined_text(&ev), "before {nope after");
    }
}
