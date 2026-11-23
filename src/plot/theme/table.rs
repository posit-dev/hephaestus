//! Table theming — a structural cascade in the spirit of the axis one.
//!
//! A table is a small set of structural roles — a heading block
//! (spanner rows, the column-label row, the stub head), body rows, a
//! stub column, and the rules between them — and each role is a sparse
//! sub-theme that cascades from a shared layer down to a specific one,
//! per field. This mirrors how [`PerAxis`](super::PerAxis) layers
//! `by_channel_side` over `by_channel` over `all` rather than giving
//! every chrome slot its own flat field.
//!
//! The cascade for a body cell walks: row parity → column parity →
//! `body.all` (both parities via [`Alternating`]) → concrete defaults,
//! with per-row and per-cell overrides from the table model on top.
//! A stub cell inserts the `stub` layer above its row's parity, so
//! stub text inherits the row's striping unless the stub overrides
//! it. Heading roles resolve `spanner` / `column_label` / `stub_head`
//! over `heading.all`; row-group headers and summary rows resolve
//! their roles over `body.all`.

use super::cascade::Alternating;
use super::element::{
    line_concrete_defaults, text_concrete_defaults, Element, LineElement, RectElement, TextElement,
};
use super::font::{FontSpec, FontWeight};
use super::length::{Length, Margin};
use super::palette::ThemeColor;

/// Sparse cell theme — the text and fill of one structural region of
/// cells: a heading role, a body row, a column, the stub, a row group,
/// or a single overridden cell. Every field falls through when
/// `Inherit`, exactly as on [`super::AxisTheme`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableCellTheme {
    /// Cell text.
    pub text: Element<TextElement>,
    /// Row background fill. Border fields are ignored — rules are
    /// themed separately on [`TableRules`].
    pub background: Element<RectElement>,
}

/// The heading block cascade — `all` styles every heading row, the
/// role layers override it per row kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableHeading {
    /// Applies to every heading row.
    pub all: TableCellTheme,
    /// Spanner rows — a label spanning several columns.
    pub spanner: TableCellTheme,
    /// The column-label row.
    pub column_label: TableCellTheme,
    /// The corner cell above the stub column.
    pub stub_head: TableCellTheme,
}

/// Which heading row a cell sits on, for
/// [`TableTheme::resolved_heading`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingRole {
    /// A spanner row.
    Spanner,
    /// The column-label row.
    ColumnLabel,
    /// The corner cell above the stub column.
    StubHead,
}

/// Rules — the lines separating a table's regions. One shared line
/// element with sparse per-position overrides, in the
/// [`super::PerChannel`] spirit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableRules {
    /// Every rule in the table.
    pub all: Element<LineElement>,
    /// Horizontal rules between body rows.
    pub horizontal: Element<LineElement>,
    /// Vertical rules between columns.
    pub vertical: Element<LineElement>,
    /// The rule separating the heading block from the body.
    pub header: Element<LineElement>,
    /// The rules along the table's outer top and bottom edges.
    pub outer: Element<LineElement>,
}

/// Which rule position to resolve, for [`TableTheme::resolved_rule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulePosition {
    /// The outer top or bottom edge.
    Outer,
    /// The heading/body separator.
    Header,
    /// Between two body rows.
    BetweenRows,
    /// Between two columns.
    BetweenColumns,
}

/// A fully cascaded cell-region style — the table renderer reads
/// this directly, so no theme walking lives in the draw path.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTableCells {
    /// Cascaded text style. `Length::Rel` fields stay unresolved; the
    /// renderer applies them against the same per-call parents as
    /// other chrome text. `None` when any layer said `Blank` — the
    /// cell's text is not drawn.
    pub text: Option<TextElement>,
    /// Cascaded background. `None` when any layer said `Blank` or no
    /// layer set a fill — nothing is drawn.
    pub background: Option<RectElement>,
}

impl ResolvedTableCells {
    /// Merge a sparse per-row or per-cell override over this resolved
    /// row, per field. `Inherit` leaves the resolved value untouched;
    /// `Blank` hides (no text, or no fill); `Set` merges per field.
    /// This is the outermost layer of the cascade, above every
    /// structural role.
    pub fn with_override(&self, o: &TableCellTheme) -> ResolvedTableCells {
        let text = match (&o.text, &self.text) {
            (Element::Blank, _) => None,
            (Element::Set(v), base) => {
                let base = base
                    .clone()
                    .unwrap_or_else(crate::plot::theme::text_concrete_defaults);
                Some(v.cascade(&base))
            }
            (Element::Inherit, base) => base.clone(),
        };
        let background = match (&o.background, &self.background) {
            (Element::Blank, _) => None,
            (Element::Set(v), base) => {
                let base = base
                    .clone()
                    .unwrap_or_else(crate::plot::theme::rect_concrete_defaults);
                Some(v.cascade(&base))
            }
            (Element::Inherit, base) => base.clone(),
        };
        ResolvedTableCells { text, background }
    }
}

/// Theme for a complete table — heading block, body rows, stub
/// column, row groups, summary rows, rules, and the table's own
/// title / subtitle / caption / footnotes.
#[derive(Debug, Clone, PartialEq)]
pub struct TableTheme {
    /// Table title, above the heading block.
    pub title: Element<TextElement>,
    /// Table subtitle, below the title.
    pub subtitle: Element<TextElement>,
    /// Table caption, below the body.
    pub caption: Element<TextElement>,
    /// Footnotes, below the caption — one full-width row each.
    pub footnote: Element<TextElement>,
    /// Heading-block cascade: `all` → spanner / column-label /
    /// stub-head.
    pub heading: TableHeading,
    /// Body-row cascade with odd/even parity layers. Striping is
    /// `body.even.background = Set(..)`; nothing else changes.
    pub body: Alternating<TableCellTheme>,
    /// Body-column cascade, the second parity axis. A body cell
    /// resolves its row parity above its column parity above
    /// `body.all`, so column striping shows through wherever the row
    /// layers set nothing.
    pub columns: Alternating<TableCellTheme>,
    /// Stub-column cells, resolved above the cell's body-row parity
    /// layer so a stub inherits its row's striping unless it sets a
    /// background of its own.
    pub stub: TableCellTheme,
    /// Row-group header rows, resolved over `body.all` — group labels
    /// read as part of the body unless styled otherwise.
    pub row_group: TableCellTheme,
    /// Summary rows, resolved over `body.all` for the same reason.
    pub summary: TableCellTheme,
    /// The rules between regions.
    pub rules: TableRules,
    /// Inner padding of every cell.
    pub cell_padding: Margin,
    /// Outer margin around the whole table.
    pub margin: Margin,
}

impl Default for TableTheme {
    /// Defaults follow the gt look: no fills anywhere, a bold column
    /// label row at `rel(0.9)`, an ink rule under the heading block
    /// and along the outer edges, and no interior rules. Title /
    /// subtitle / caption mirror the plot chrome's sizing and
    /// alignment; body cells are start-aligned, headings centered.
    fn default() -> Self {
        Self {
            title: Element::Set(TextElement {
                size_pt: Some(Length::Rel(1.2)),
                font: FontSpec {
                    weight: Some(FontWeight::BOLD),
                    ..FontSpec::default()
                },
                align: Some(super::element::HAlign::Start),
                ..TextElement::default()
            }),
            subtitle: Element::Set(TextElement {
                align: Some(super::element::HAlign::Start),
                ..TextElement::default()
            }),
            caption: Element::Set(TextElement {
                size_pt: Some(Length::Rel(0.8)),
                align: Some(super::element::HAlign::End),
                ..TextElement::default()
            }),
            footnote: Element::Set(TextElement {
                size_pt: Some(Length::Rel(0.8)),
                align: Some(super::element::HAlign::Start),
                ..TextElement::default()
            }),
            heading: TableHeading {
                all: TableCellTheme::default(),
                spanner: TableCellTheme::default(),
                column_label: TableCellTheme {
                    text: Element::Set(TextElement {
                        size_pt: Some(Length::Rel(0.9)),
                        font: FontSpec {
                            weight: Some(FontWeight::BOLD),
                            ..FontSpec::default()
                        },
                        ..TextElement::default()
                    }),
                    background: Element::Inherit,
                },
                stub_head: TableCellTheme::default(),
            },
            body: Alternating::new(TableCellTheme {
                text: Element::Set(TextElement {
                    align: Some(super::element::HAlign::Start),
                    ..TextElement::default()
                }),
                background: Element::Inherit,
            }),
            columns: Alternating::default(),
            stub: TableCellTheme::default(),
            row_group: TableCellTheme {
                text: Element::Set(TextElement {
                    font: FontSpec {
                        weight: Some(FontWeight::BOLD),
                        ..FontSpec::default()
                    },
                    align: Some(super::element::HAlign::Start),
                    ..TextElement::default()
                }),
                background: Element::Inherit,
            },
            summary: TableCellTheme {
                text: Element::Set(TextElement {
                    font: FontSpec {
                        weight: Some(FontWeight::BOLD),
                        ..FontSpec::default()
                    },
                    align: Some(super::element::HAlign::Start),
                    ..TextElement::default()
                }),
                background: Element::Inherit,
            },
            rules: TableRules {
                all: Element::Inherit,
                horizontal: Element::Blank,
                vertical: Element::Blank,
                header: Element::Set(LineElement {
                    color: Some(ThemeColor::Ink),
                    linewidth_pt: Some(Length::Abs(1.0)),
                    ..LineElement::default()
                }),
                outer: Element::Set(LineElement {
                    color: Some(ThemeColor::Ink),
                    linewidth_pt: Some(Length::Abs(1.0)),
                    ..LineElement::default()
                }),
            },
            cell_padding: Margin::new(
                Length::Abs(4.0),
                Length::Abs(8.0),
                Length::Abs(4.0),
                Length::Abs(8.0),
            ),
            margin: Margin::ZERO,
        }
    }
}

/// Cascade a chain of sparse `Element<T>` layers, given least
/// specific first, merging per field where several layers `Set`.
/// `Blank` at any layer short-circuits to `None` — it never falls
/// through.
fn cascade_layers<'a, T>(layers: impl IntoIterator<Item = &'a Element<T>>) -> Option<T>
where
    T: Clone + MergeOver + 'a,
{
    let mut acc: Option<T> = None;
    // Walk most specific first: the accumulated value merges *over*
    // each less specific parent.
    for layer in layers.into_iter().collect::<Vec<_>>().into_iter().rev() {
        match layer {
            Element::Set(v) => {
                acc = Some(match acc {
                    Some(child) => child.merge_over(&v.clone()),
                    None => v.clone(),
                });
            }
            Element::Blank => return None,
            Element::Inherit => {}
        }
    }
    acc
}

/// Per-field merge — `self` over `parent`. Implemented by the three
/// element types; equivalent to each type's own `cascade` method,
/// named for the layer walk so [`cascade_layers`] can stay generic.
pub(crate) trait MergeOver: Sized {
    /// Merge `self` over `parent`: `self`'s set fields win, unset
    /// fields fall through.
    fn merge_over(&self, parent: &Self) -> Self;
}

impl MergeOver for TextElement {
    fn merge_over(&self, parent: &Self) -> Self {
        self.cascade(parent)
    }
}

impl MergeOver for RectElement {
    fn merge_over(&self, parent: &Self) -> Self {
        self.cascade(parent)
    }
}

impl MergeOver for LineElement {
    fn merge_over(&self, parent: &Self) -> Self {
        self.cascade(parent)
    }
}

impl TableCellTheme {
    /// Cascade this cell theme over a parent, per element.
    /// `Blank` on either side hides; two `Inherit`s stay `Inherit` —
    /// collapsing them to `Blank` would turn "no opinion anywhere"
    /// into "hidden", the opposite of inheritance.
    fn over(&self, parent: &TableCellTheme) -> TableCellTheme {
        fn layer<T: Clone + MergeOver>(child: &Element<T>, parent: &Element<T>) -> Element<T> {
            if matches!(child, Element::Blank) || matches!(parent, Element::Blank) {
                return Element::Blank;
            }
            match cascade_layers([parent, child]) {
                Some(v) => Element::Set(v),
                None => Element::Inherit,
            }
        }
        TableCellTheme {
            text: layer(&self.text, &parent.text),
            background: layer(&self.background, &parent.background),
        }
    }
}

impl TableTheme {
    /// Resolve a heading row's style for the given role, cascading
    /// role → `heading.all` → the theme's root text → concrete
    /// defaults.
    pub fn resolved_heading(
        &self,
        role: HeadingRole,
        root_text: &TextElement,
    ) -> ResolvedTableCells {
        let role_layer = match role {
            HeadingRole::Spanner => &self.heading.spanner,
            HeadingRole::ColumnLabel => &self.heading.column_label,
            HeadingRole::StubHead => &self.heading.stub_head,
        };
        self.resolve_row(&role_layer.over(&self.heading.all), root_text)
    }

    /// Resolve a body cell's style by 0-based row and column index,
    /// cascading row parity → column parity → `body.all` → the
    /// theme's root text → concrete defaults.
    pub fn resolved_body(
        &self,
        row: usize,
        col: usize,
        root_text: &TextElement,
    ) -> ResolvedTableCells {
        let [all, row_parity] = self.body.layers(row);
        let [_, col_parity] = self.columns.layers(col);
        self.resolve_row(&row_parity.over(&col_parity.over(all)), root_text)
    }

    /// Resolve a row-group header row's style, cascading
    /// `row_group` → `body.all` → root text → concrete defaults.
    pub fn resolved_row_group(&self, root_text: &TextElement) -> ResolvedTableCells {
        self.resolve_row(&self.row_group.over(&self.body.all), root_text)
    }

    /// Resolve a summary row's style, cascading `summary` →
    /// `body.all` → root text → concrete defaults.
    pub fn resolved_summary(&self, root_text: &TextElement) -> ResolvedTableCells {
        self.resolve_row(&self.summary.over(&self.body.all), root_text)
    }

    /// Resolve a stub cell's style by 0-based body row index. The
    /// `stub` layer sits above the row's parity layer, so striping
    /// shows through unless the stub sets a background.
    pub fn resolved_stub(&self, row: usize, root_text: &TextElement) -> ResolvedTableCells {
        let [all, parity] = self.body.layers(row);
        self.resolve_row(&self.stub.over(&parity.over(all)), root_text)
    }

    /// Resolve a rule at the given position. `None` means no rule is
    /// drawn there.
    /// The header and outer positions cascade through `all` only —
    /// they are siblings of the interior positions, not
    /// specializations of them, so hiding interior rules never hides
    /// the frame.
    pub fn resolved_rule(&self, position: RulePosition) -> Option<LineElement> {
        let line = match position {
            RulePosition::Header => cascade_layers([&self.rules.all, &self.rules.header]),
            RulePosition::BetweenRows => cascade_layers([&self.rules.all, &self.rules.horizontal]),
            RulePosition::BetweenColumns => cascade_layers([&self.rules.all, &self.rules.vertical]),
            RulePosition::Outer => cascade_layers([&self.rules.all, &self.rules.outer]),
        };
        line.map(|l| l.cascade(&line_concrete_defaults()))
    }

    /// Resolve one of the table's own text slots (title / subtitle /
    /// caption) against the theme's root text and the concrete text
    /// defaults. `None` when the slot is `Blank`.
    pub fn resolved_text_slot(
        slot: &Element<TextElement>,
        root_text: &TextElement,
    ) -> Option<TextElement> {
        let merged = match slot {
            Element::Set(v) => v.cascade(root_text),
            Element::Blank => return None,
            Element::Inherit => root_text.clone(),
        };
        Some(merged.cascade(&text_concrete_defaults()))
    }

    /// Shared tail of the resolvers: cascade the region's elements
    /// through the root text and the concrete defaults.
    fn resolve_row(&self, row: &TableCellTheme, root_text: &TextElement) -> ResolvedTableCells {
        let text = match &row.text {
            Element::Blank => None,
            Element::Set(v) => Some(v.cascade(root_text).cascade(&text_concrete_defaults())),
            Element::Inherit => Some(root_text.clone().cascade(&text_concrete_defaults())),
        };
        let background = match &row.background {
            Element::Set(r) => Some(r.clone()),
            Element::Blank | Element::Inherit => None,
        };
        ResolvedTableCells { text, background }
    }
}
