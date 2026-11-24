//! The high-level table API: a [`Table`] occupies a named patch in a
//! [`Composition`] the same way a [`Plot`](crate::plot::Plot) does, so
//! tables and plots mix freely in one layout and travel together in a
//! plot document.
//!
//! A table is a **grid of positioned [`TableCell`]s the caller
//! arranges**: every cell carries its grid position, its spans, and a
//! [`TableCellKind`] saying what it is structurally. Hephaestus owns no
//! arrangement — stub-to-front reordering, spanner runs, row-group
//! interleaving are the producer's job (ggsql's table layout solves
//! exactly this and hands over a solved grid, the way its HTML writer
//! iterates one). Number formatting is likewise the producer's job:
//! cells hold plain strings. Styling comes entirely from
//! [`TableTheme`](crate::plot::theme::TableTheme), driven by cell
//! *kinds* — a column label is a `ColumnLabel` cell, not "whatever is
//! in row 0" — and the draw path reads only resolved theme values.
//!
//! Cell aesthetics are set per cell as [`CellAesthetic`] values —
//! **fixed** (`Raw`, an output-space value that bypasses scales, like a
//! geom's `Raw(...)` channel) or **scaled** (a value plus a scale name,
//! mapped through the composition's `ScaleRegistry` exactly as a geom
//! channel is). The value is typically the cell's own, but any cell can
//! carry any value, so one cell can style an entire row's cells. Cell
//! aesthetics sit above the structural theme; both kinds travel in plot
//! documents.
//!
//! Columns and rows can additionally be *blessed* with structural
//! metadata ([`TableColumn::role`], [`TableRow::role`]). This is inert
//! at render time — styling stays kind-driven — and exists so the
//! producer's structural claims travel with the table (documents
//! included) for consumers that introspect structure, and for future
//! behavior like sticky rows in scrolling tables.
//!
//! Internally a table is one measured cell in the patch's
//! [`Slot::Panel`]. Its own geometry is solved by a private inner
//! [`crate::layout::Grid`] with `Auto` / `Fixed` tracks, the same
//! technique the legend's discrete stack uses. With `Auto` composition
//! tracks the table sizes itself to its content; with `Fr` tracks it
//! keeps its natural size and stays anchored to the panel's top-left.

use std::sync::Arc;

use crate::composition::{Composition, Patch, Slot};
use crate::geometry::{Affine, Rect, Shape as _, Size};
use crate::layout::{Cell, CellId, Extent, Grid, Measure, Placement, Track, WidthHint};
use crate::path::FillRule;
use crate::pick::PickId;
use crate::plot::chrome::root_text_pt;
use crate::plot::chrome::text::{rich_chrome_for, text_outline_from, text_style_from, ChromeRun};
use crate::plot::pick::{item_scope, part_scope, table_scope, PlotPart};
use crate::plot::plot::PlotError;
use crate::plot::theme::{
    rect_concrete_defaults, text_concrete_defaults, Element, FontFamily, FontStyle, FontWeight,
    FontWidth, HAlign, HeadingRole, Length, RectElement, ResolvedTableCells, RulePosition,
    TableCellTheme, TableTheme, TextElement, Theme, ThemeColor, ThemePart,
};
use crate::scales::Value;
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;

/// The structural blessing of one grid column. Structural metadata,
/// not styling: `role` is the producer's claim about the column's
/// place in the table's structure, carried (documents included) for
/// consumers that introspect structure and for future behavior like
/// sticky columns — the renderer's styling stays kind-driven and never
/// reads it. `width` is the one render-visible field, and it is
/// layout, not styling.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableColumn {
    /// Column width. `None` sizes to content; `Some` pins the track —
    /// an [`Extent`] with a `percent` component resolves against the
    /// panel's width.
    pub width: Option<Extent>,
    /// The column's structural role. Inert at render time.
    pub role: TableColumnRole,
}

/// The structural role of a grid column — the column-level parallel
/// of [`TableRowRole`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TableColumnRole {
    /// An ordinary data column.
    #[default]
    Body,
    /// A stub column, holding row labels.
    Stub,
}

/// The structural blessing of one grid row. Inert metadata: nothing at
/// render time reads the role — styling stays kind-driven, and the
/// header rule derives from header-cell kinds. The claim travels with
/// the table so a consumer never has to infer row structure by
/// inspecting cells (sticky headers / footers in scrolling tables).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TableRow {
    /// The row's structural role.
    pub role: TableRowRole,
}

/// The structural role of a grid row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TableRowRole {
    /// An ordinary body row.
    #[default]
    Body,
    /// A header row (title block, spanners, column labels).
    Header,
    /// A footer row (summaries, caption, footnotes).
    Footer,
}

/// A table bound to a named patch: a grid of positioned [`TableCell`]s
/// the caller arranges. See the module docs for the model.
#[derive(Debug, Clone)]
pub struct Table {
    patch_id: Arc<str>,
    cells: Vec<TableCell>,
    /// Grid-column-indexed structural blessings. May be shorter than
    /// the derived column count; missing entries use the default.
    columns: Vec<TableColumn>,
    /// Grid-row-indexed structural blessings, same padding rule.
    rows: Vec<TableRow>,
    theme_override: Option<ThemePart>,
    theme_variant: Option<String>,
}

impl Table {
    /// Bind a table to `patch_id`. Panics when the composition has no
    /// patch with that id — see [`Self::try_new`].
    pub fn new(composition: &Composition, patch_id: impl Into<String>) -> Self {
        match Self::try_new(composition, patch_id) {
            Ok(table) => table,
            Err(e) => panic!("{e}"),
        }
    }

    /// [`Self::new`] but reports an unknown patch id instead of
    /// panicking — for ids that come from configuration or user input
    /// rather than a literal in the calling code.
    pub fn try_new(
        composition: &Composition,
        patch_id: impl Into<String>,
    ) -> Result<Self, PlotError> {
        let patch_id: String = patch_id.into();
        if !composition.contains_patch_id(&patch_id) {
            return Err(PlotError::UnknownPatch(patch_id));
        }
        Ok(Self {
            patch_id: Arc::from(patch_id),
            cells: Vec::new(),
            columns: Vec::new(),
            rows: Vec::new(),
            theme_override: None,
            theme_variant: None,
        })
    }

    /// Build a table from already-decoded parts, running the same
    /// grid validation as [`Table::validate`]. This is the document
    /// reader's reconstruction path; callers assembling a table by
    /// hand want the builder methods.
    #[cfg(feature = "document-read")]
    pub(crate) fn from_cells(
        composition: &Composition,
        patch_id: String,
        cells: Vec<TableCell>,
        columns: Vec<TableColumn>,
        rows: Vec<TableRow>,
        theme_override: Option<ThemePart>,
        theme_variant: Option<String>,
    ) -> Result<Self, PlotError> {
        let mut table = Self::try_new(composition, patch_id)?;
        table.cells = cells;
        table.columns = columns;
        table.rows = rows;
        table.theme_override = theme_override;
        table.theme_variant = theme_variant;
        table.validate()?;
        Ok(table)
    }

    /// The patch this table is bound to.
    pub fn patch_id(&self) -> &str {
        &self.patch_id
    }

    /// Add a positioned cell. The caller arranges the grid; the table
    /// validates spans and overlaps at wire time.
    pub fn cell(mut self, cell: TableCell) -> Self {
        self.cells.push(cell);
        self
    }

    /// Add several positioned cells at once.
    pub fn cells(mut self, cells: impl IntoIterator<Item = TableCell>) -> Self {
        self.cells.extend(cells);
        self
    }

    /// Append a grid column's structural blessing. Columns are
    /// grid-indexed: the first `column` call describes grid column 0.
    pub fn column(mut self, column: TableColumn) -> Self {
        self.columns.push(column);
        self
    }

    /// Pin grid column `col`'s width, growing the blessing vec with
    /// defaults as needed. `None` sizes to content.
    pub fn column_width(mut self, col: usize, width: Extent) -> Self {
        self.columns
            .resize_with(self.columns.len().max(col + 1), TableColumn::default);
        self.columns[col].width = Some(width);
        self
    }

    /// Bless grid column `col` with a structural role — inert metadata
    /// carried for consumers, never a styling input.
    pub fn column_role(mut self, col: usize, role: TableColumnRole) -> Self {
        self.columns
            .resize_with(self.columns.len().max(col + 1), TableColumn::default);
        self.columns[col].role = role;
        self
    }

    /// Bless grid column `col` as a stub. Builder form of
    /// [`Table::column_role`] with [`TableColumnRole::Stub`].
    pub fn stub_column(self, col: usize) -> Self {
        self.column_role(col, TableColumnRole::Stub)
    }

    /// Bless grid row `row` with a structural role — inert metadata
    /// carried for consumers, never a styling input.
    pub fn row_role(mut self, row: usize, role: TableRowRole) -> Self {
        self.rows
            .resize_with(self.rows.len().max(row + 1), TableRow::default);
        self.rows[row].role = role;
        self
    }

    /// Bless grid row `row` as a header row. Builder form of
    /// [`Table::row_role`] with [`TableRowRole::Header`].
    pub fn header_row(self, row: usize) -> Self {
        self.row_role(row, TableRowRole::Header)
    }

    /// Bless grid row `row` as a footer row. Builder form of
    /// [`Table::row_role`] with [`TableRowRole::Footer`].
    pub fn footer_row(self, row: usize) -> Self {
        self.row_role(row, TableRowRole::Footer)
    }

    /// Install a sparse theme override merged over the composition's
    /// theme for this table only.
    pub fn theme_override(mut self, part: ThemePart) -> Self {
        self.theme_override = Some(part);
        self
    }

    /// Opt into a named [`TableTheme`] variant registered on the theme
    /// via [`Theme::with_table_variant`].
    pub fn theme_variant(mut self, name: impl Into<String>) -> Self {
        self.theme_variant = Some(name.into());
        self
    }

    // ── Accessors (the document codec reads through these) ───────────

    /// The positioned cells, in insertion order.
    pub fn cells_ref(&self) -> &[TableCell] {
        &self.cells
    }

    /// The grid-column structural blessings (may be shorter than the
    /// derived column count).
    pub fn columns(&self) -> &[TableColumn] {
        &self.columns
    }

    /// The grid-row structural blessings (may be shorter than the
    /// derived row count).
    pub fn rows(&self) -> &[TableRow] {
        &self.rows
    }

    /// The table's sparse theme override, if set.
    pub fn theme_override_ref(&self) -> Option<&ThemePart> {
        self.theme_override.as_ref()
    }

    /// The named theme variant the table opted into, if any.
    pub fn theme_variant_ref(&self) -> Option<&str> {
        self.theme_variant.as_deref()
    }

    /// The grid dimensions the cells imply: `(rows, columns)`.
    fn grid_shape(&self) -> (u16, u16) {
        let nrow = self
            .cells
            .iter()
            .map(|c| c.row + c.row_span)
            .max()
            .unwrap_or(0);
        let ncol = self
            .cells
            .iter()
            .map(|c| c.col + c.col_span)
            .max()
            .unwrap_or(0);
        (nrow, ncol)
    }

    /// Check the grid: spans at least 1, no two cells overlapping, and
    /// the blessing vecs no longer than the grid they describe.
    /// `wire_panel` panics on a violation (a malformed grid is a
    /// programming error); the document reader returns it.
    pub(crate) fn validate(&self) -> Result<(), PlotError> {
        let patch = self.patch_id.to_string();
        if let Some(c) = self
            .cells
            .iter()
            .find(|c| c.row_span == 0 || c.col_span == 0)
        {
            return Err(PlotError::TableCellSpan {
                patch,
                row: c.row,
                col: c.col,
            });
        }
        let (nrow, ncol) = self.grid_shape();
        if self.columns.len() > ncol as usize || self.rows.len() > nrow as usize {
            return Err(PlotError::TableSpecOutOfRange { patch });
        }
        let mut occupied = vec![false; nrow as usize * ncol as usize];
        for c in &self.cells {
            for r in c.row..c.row + c.row_span {
                for col in c.col..c.col + c.col_span {
                    let i = r as usize * ncol as usize + col as usize;
                    if occupied[i] {
                        return Err(PlotError::TableCellOverlap { patch, row: r, col });
                    }
                    occupied[i] = true;
                }
            }
        }
        Ok(())
    }

    // ── Layout and drawing ───────────────────────────────────────────

    /// Attach the table's measured panel cell to `patch`. The outer
    /// margin band is the shared `theme.margin` plus the table
    /// theme's own `margin`.
    pub(crate) fn wire_panel(
        &self,
        patch: Patch,
        theme: &Theme,
        registry: &crate::plot::scale::ScaleRegistry,
        dpi: f64,
        images: &Arc<crate::image_registry::ImageRegistry>,
    ) -> Patch {
        let table_theme = theme.table_for(self.theme_variant.as_deref());
        let root_pt = root_text_pt(theme);
        // `apply_plot_chrome` has already set the patch margin from
        // `theme.margin`; `Patch::margin` replaces rather than
        // accumulates, so the combined band is rebuilt here from both.
        let (st, sr, sb, sl) = theme.margin.resolve(root_pt);
        let (mt, mr, mb, ml) = table_theme.margin.resolve(root_pt);
        let pt_to_px = dpi / 72.0;
        let mut patch = patch;
        let inset = crate::layout::Inset::default()
            .top(Extent::px((st + mt) * pt_to_px))
            .right(Extent::px((sr + mr) * pt_to_px))
            .bottom(Extent::px((sb + mb) * pt_to_px))
            .left(Extent::px((sl + ml) * pt_to_px));
        patch = patch.margin(inset);
        if let Err(e) = self.validate() {
            panic!("{e}");
        }
        let measure = TableMeasure {
            inner: self.build_grid(theme, registry, dpi, images),
        };
        patch.slot(Slot::Panel, Cell::measured(measure))
    }

    /// Draw the table into its patch's panel rect. Reads only resolved
    /// theme values; `theme` is the already-merged effective theme.
    pub fn draw_into(
        &self,
        scene: &mut dyn SceneBuilder,
        layout: &crate::composition::CompositionLayout,
        theme: &Theme,
        registry: &crate::plot::scale::ScaleRegistry,
        dpi: f64,
        images: &Arc<crate::image_registry::ImageRegistry>,
    ) {
        let Some(panel) = layout.get(&self.patch_id, Slot::Panel) else {
            return;
        };
        if panel.x1 <= panel.x0 || panel.y1 <= panel.y0 {
            return;
        }
        scene.push_pick_scope(&table_scope(&self.patch_id));

        // The patch background, exactly as a plot would paint it —
        // phase 1 of the render loop only walks plots, so a table
        // paints its own.
        crate::plot::plot::paint_patch_background(&self.patch_id, scene, layout, theme, dpi);

        scene.push_pick_scope(&crate::plot::pick::region_scope(Slot::Panel));

        let inner = self.build_grid(theme, registry, dpi, images);
        let solved = inner.solve_at(panel.width(), dpi);

        // Normalize: the solver anchors the content-sized grid inside
        // its container, so shift every rect to put the table's
        // top-left at the panel's.
        let Some(bbox) = solved.bounding_box() else {
            scene.pop_pick_scope();
            scene.pop_pick_scope();
            return;
        };
        let offset = crate::geometry::Vec2::new(panel.x0 - bbox.x0, panel.y0 - bbox.y0);
        let translate = |r: Rect| r + offset;

        let table_theme = theme.table_for(self.theme_variant.as_deref());

        // Backgrounds first, under every rule and label.
        for cell in &solved.cells {
            let Some(bg) = &cell.background else {
                continue;
            };
            let rect = translate(cell.rect);
            let defaults = rect_concrete_defaults();
            let bg = bg.cascade(&defaults);
            let Some(fill) = &bg.fill else {
                continue;
            };
            let radius_pt = bg
                .corner_radius
                .or(defaults.corner_radius)
                .map(|l| l.resolve(0.0))
                .unwrap_or(0.0);
            let radius_px = (radius_pt * dpi / 72.0).max(0.0);
            let path: crate::path::Path = if radius_px > 0.0 {
                crate::primitives::rounded_rect(rect, radius_px)
            } else {
                rect.to_path(0.0)
            };
            scene.push_pick_scope(&part_scope(cell.kind.part()));
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &crate::brush::Brush::Solid(fill.resolve(&theme.palette)),
                None,
                &path,
                PickId::Skip,
            );
            scene.pop_pick_scope();
        }

        // Rules between regions.
        self.draw_rules(scene, &solved, offset, table_theme, theme, dpi);

        // Cell text on top.
        for cell in &solved.cells {
            let Some(run) = &cell.run else {
                continue;
            };
            let rect = translate(cell.rect);
            let (pt, pr, pb, pl) = cell.padding_px;
            let content = Rect::new(
                rect.x0 + pl,
                rect.y0 + pt,
                (rect.x1 - pr).max(rect.x0 + pl),
                (rect.y1 - pb).max(rect.y0 + pt),
            );
            let line_h = run.line_box_height();
            let run_w = run.width();
            let x = match cell.align {
                // Justified text is a multi-line concept; table cells
                // draw one line, so it reads as start-aligned.
                HAlign::Start | HAlign::Justify => content.x0,
                HAlign::Center => content.x0 + (content.width() - run_w) / 2.0,
                HAlign::End => (content.x1 - run_w).max(content.x0),
            };
            let y = content.y0 + (content.height() - line_h).max(0.0) / 2.0;
            scene.push_pick_scope(&part_scope(cell.kind.part()));
            if let Some(item) = cell.kind.item() {
                scene.push_pick_scope(&item_scope(item));
            }
            run.draw(
                scene,
                x,
                y,
                &crate::brush::Brush::Solid(cell.color),
                cell.outline.as_ref(),
                Affine::IDENTITY,
                PickId::Skip,
            );
            if cell.kind.item().is_some() {
                scene.pop_pick_scope();
            }
            scene.pop_pick_scope();
        }

        scene.pop_pick_scope();
        scene.pop_pick_scope();
    }

    /// Draw the themed rules at their solved positions: outer edges,
    /// the heading/body separator, and the interior row / column
    /// separators.
    fn draw_rules(
        &self,
        scene: &mut dyn SceneBuilder,
        solved: &SolvedTable,
        offset: crate::geometry::Vec2,
        table_theme: &TableTheme,
        theme: &Theme,
        dpi: f64,
    ) {
        let Some(bbox) = solved.bounding_box().map(|r| r + offset) else {
            return;
        };
        let pt_to_px = dpi / 72.0;

        let draw_rule = |scene: &mut dyn SceneBuilder,
                         position: RulePosition,
                         from: crate::geometry::Point,
                         to: crate::geometry::Point| {
            let Some(line) = table_theme.resolved_rule(position) else {
                return;
            };
            let lw = line.linewidth_pt.expect("concrete defaults set width");
            let width_px = lw.resolve(crate::plot::theme::DEFAULT_LINEWIDTH_PT) * pt_to_px;
            if width_px <= 0.0 {
                return;
            }
            let color = line.color.expect("concrete defaults set color");
            let mut stroke = Stroke::new(width_px);
            if let Some(cap) = line.cap {
                stroke = stroke.with_caps(cap);
            }
            if let Some(join) = line.join {
                stroke = stroke.with_join(join);
            }
            let mut path = crate::path::Path::new();
            path.move_to(from);
            path.line_to(to);
            scene.push_pick_scope(&part_scope(PlotPart::TableRule));
            scene.stroke(
                &stroke,
                Affine::IDENTITY,
                &crate::brush::Brush::Solid(color.resolve(&theme.palette)),
                None,
                &path,
                PickId::Skip,
            );
            scene.pop_pick_scope();
        };

        // Horizontal edges and separators.
        let top = bbox.y0;
        let bottom = bbox.y1;
        for y in [top, bottom] {
            draw_rule(
                scene,
                RulePosition::Outer,
                crate::geometry::Point::new(bbox.x0, y),
                crate::geometry::Point::new(bbox.x1, y),
            );
        }
        if let Some(y) = solved.header_bottom.map(|v| v + offset.y) {
            draw_rule(
                scene,
                RulePosition::Header,
                crate::geometry::Point::new(bbox.x0, y),
                crate::geometry::Point::new(bbox.x1, y),
            );
        }
        for &y in &solved.body_boundaries {
            let y = y + offset.y;
            draw_rule(
                scene,
                RulePosition::BetweenRows,
                crate::geometry::Point::new(bbox.x0, y),
                crate::geometry::Point::new(bbox.x1, y),
            );
        }
        for &x in &solved.column_boundaries {
            let x = x + offset.x;
            draw_rule(
                scene,
                RulePosition::BetweenColumns,
                crate::geometry::Point::new(x, bbox.y0),
                crate::geometry::Point::new(x, bbox.y1),
            );
        }
    }

    /// Build the inner grid from the positioned cells: column tracks
    /// from the columns' width blessings, `Auto` everywhere else, one
    /// measured leaf per cell.
    fn build_grid(
        &self,
        theme: &Theme,
        registry: &crate::plot::scale::ScaleRegistry,
        dpi: f64,
        images: &Arc<crate::image_registry::ImageRegistry>,
    ) -> InnerGrid {
        let (n_rows, n_cols) = self.grid_shape();
        let table_theme = theme.table_for(self.theme_variant.as_deref());
        let root_pt = root_text_pt(theme);
        let pt_to_px = dpi / 72.0;
        let padding_px = {
            let (t, r, b, l) = table_theme.cell_padding.resolve(root_pt);
            (t * pt_to_px, r * pt_to_px, b * pt_to_px, l * pt_to_px)
        };

        let col_tracks: Vec<Track> = (0..n_cols as usize)
            .map(
                |c| match self.columns.get(c).and_then(|col| col.width.clone()) {
                    Some(w) => Track::Fixed(w),
                    None => Track::Auto,
                },
            )
            .collect();
        let row_tracks = vec![Track::Auto; n_rows as usize];

        let cells = &self.cells;
        let mut grid = Grid::new(col_tracks, row_tracks);
        let mut entries = Vec::with_capacity(cells.len());
        for (i, spec) in cells.iter().enumerate() {
            let id = CellId(i as u64 + 1);
            let styled = spec.shape(
                table_theme,
                theme,
                registry,
                root_pt,
                dpi,
                padding_px,
                images,
            );
            let (w, h) = styled.natural_size();
            grid.place_mut(
                Placement::at(spec.row + 1, spec.col + 1).span(spec.row_span, spec.col_span),
                Cell::measured(LeafMeasure { w, h }).id(id),
            );
            entries.push((id, styled));
        }
        InnerGrid { grid, entries }
    }
}

/// What a cell is, structurally. Decides which theme layer styles it
/// and which pick part it reports — a column label is a `ColumnLabel`
/// cell, not "whatever is in row 0".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableCellKind {
    /// The table's title.
    Title,
    /// The table's subtitle.
    Subtitle,
    /// The table's caption.
    Caption,
    /// A label spanning several columns, grouping them under one
    /// heading.
    Spanner,
    /// A column's heading label.
    ColumnLabel,
    /// The stub's own header cell — the corner cell above the stub
    /// columns.
    StubHead,
    /// A row-label cell in a stub column.
    Stub {
        /// 0-based body row ordinal.
        row: usize,
    },
    /// A data value.
    Body {
        /// 0-based body row ordinal.
        row: usize,
    },
    /// A full-width group label above a group of body rows.
    RowGroup,
    /// A summary value, drawn after the body.
    Summary {
        /// 0-based summary row ordinal.
        row: usize,
    },
    /// A footnote.
    Footnote,
}

impl TableCellKind {
    /// The pick part this cell reports.
    pub(crate) fn part(&self) -> PlotPart {
        match self {
            TableCellKind::Title => PlotPart::TableTitle,
            TableCellKind::Subtitle => PlotPart::TableSubtitle,
            TableCellKind::Caption => PlotPart::TableCaption,
            TableCellKind::Spanner => PlotPart::TableSpanner,
            TableCellKind::ColumnLabel => PlotPart::TableColumnLabel,
            TableCellKind::StubHead => PlotPart::TableStubHead,
            TableCellKind::Stub { .. } => PlotPart::TableStub,
            TableCellKind::Body { .. } => PlotPart::TableCell,
            TableCellKind::RowGroup => PlotPart::TableRowGroup,
            TableCellKind::Summary { .. } => PlotPart::TableSummary,
            TableCellKind::Footnote => PlotPart::TableFootnote,
        }
    }

    /// The pick item ordinal for body, stub and summary cells — the
    /// 0-based ordinal within its section. Heading and free-text cells
    /// carry no item.
    pub(crate) fn item(&self) -> Option<u32> {
        match self {
            TableCellKind::Stub { row }
            | TableCellKind::Body { row }
            | TableCellKind::Summary { row } => Some(*row as u32),
            _ => None,
        }
    }
}

/// One cell of a [`Table`]: its grid position, its spans, its
/// structural kind, and its text. Positions are 0-based grid
/// coordinates; spans are in tracks. The caller arranges the grid —
/// hephaestus validates spans and overlaps but never rearranges.
#[derive(Debug, Clone)]
pub struct TableCell {
    /// The cell's text (plain string; markdown when the cell's theme
    /// element opts in).
    pub text: String,
    /// 0-based grid row of the cell's top edge.
    pub row: u16,
    /// 0-based grid column of the cell's left edge.
    pub col: u16,
    /// Rows the cell spans, at least 1.
    pub row_span: u16,
    /// Columns the cell spans, at least 1.
    pub col_span: u16,
    /// The cell's structural kind — decides its theme layer and pick
    /// part.
    pub kind: TableCellKind,
    /// Text alignment. `None` falls back to the theme's resolved
    /// alignment for the kind.
    pub align: Option<HAlign>,
    /// Per-cell aesthetic overrides. Each is a fixed ([`CellAesthetic::Raw`])
    /// or scale-mapped ([`CellAesthetic::Scaled`]) value for one
    /// [`TableScaledAesthetic`] — the same layer, so the two mix freely,
    /// and a later binding of the same aesthetic wins. Sits above the
    /// structural theme.
    pub aesthetics: Vec<(TableScaledAesthetic, CellAesthetic)>,
}

/// One aesthetic value of a cell: fixed or scale-mapped, mirroring the
/// plot side's constant-vs-mapped channel split.
#[derive(Debug, Clone)]
pub enum CellAesthetic {
    /// A fixed, output-space value — a [`Value::Color`] for the color
    /// aesthetics, a [`Value::Number`] (pt) for the size ones. Bypasses
    /// scales, like a `Raw(...)` channel on a geom.
    Raw(Value),
    /// A value mapped through the scale registered under this name on
    /// the composition. Typically the cell's own value, but nothing
    /// ties it to the text — one cell can carry a row-mates' value, or
    /// every cell of a row the same one.
    Scaled(Value, String),
}

/// Resolve a family name to a [`FontFamily`]: the CSS generic names
/// map to generic families, anything else is a named family.
fn parse_font_family(s: &str) -> FontFamily {
    match s.to_ascii_lowercase().as_str() {
        "serif" => FontFamily::Serif,
        "sans-serif" | "sans" => FontFamily::SansSerif,
        "monospace" | "mono" => FontFamily::Mono,
        "cursive" => FontFamily::Cursive,
        "fantasy" => FontFamily::Fantasy,
        "system-ui" => FontFamily::SystemUi,
        _ => FontFamily::Named(vec![s.to_string()]),
    }
}

/// Kebab-case font-width name → [`FontWidth`].
fn parse_font_width(s: &str) -> Option<FontWidth> {
    Some(match s.to_ascii_lowercase().as_str() {
        "ultra-condensed" => FontWidth::UltraCondensed,
        "extra-condensed" => FontWidth::ExtraCondensed,
        "condensed" => FontWidth::Condensed,
        "semi-condensed" => FontWidth::SemiCondensed,
        "normal" => FontWidth::Normal,
        "semi-expanded" => FontWidth::SemiExpanded,
        "expanded" => FontWidth::Expanded,
        "extra-expanded" => FontWidth::ExtraExpanded,
        "ultra-expanded" => FontWidth::UltraExpanded,
        _ => return None,
    })
}

/// Font-style name → [`FontStyle`]. Oblique is not covered: it is a
/// style plus an angle, which no single value carries.
fn parse_font_style(s: &str) -> Option<FontStyle> {
    Some(match s.to_ascii_lowercase().as_str() {
        "normal" => FontStyle::Normal,
        "italic" => FontStyle::Italic,
        _ => return None,
    })
}

/// Alignment name → [`HAlign`], with `left` / `right` as aliases.
fn parse_halign(s: &str) -> Option<HAlign> {
    Some(match s.to_ascii_lowercase().as_str() {
        "start" | "left" => HAlign::Start,
        "center" | "centre" => HAlign::Center,
        "end" | "right" => HAlign::End,
        "justify" => HAlign::Justify,
        _ => return None,
    })
}

/// An aesthetic of a cell that a value can drive. Covers the whole
/// cell-level theme surface — [`RectElement`] and the text-run fields
/// of [`TextElement`] — keyed by the scale output type each maps from:
/// color-producing scales for the colors, number-producing for sizes
/// (pt, or multipliers where the theme field is relative) and font
/// weight, string-producing for the font axes and alignment, booleans
/// for decorations, a linetype for the border dash.
///
/// Deliberately not covered: `valign` / `angle` / `margin` (slot-level
/// fields the cell draw path doesn't read), OpenType features and
/// variation axes (structured tag data no scale output maps to), and
/// oblique angles (a style plus a number — two values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableScaledAesthetic {
    // ── Background (RectElement) ──
    /// Background fill — color-producing scales.
    Fill,
    /// Background border color.
    BorderColor,
    /// Background border width — number-producing scales, in pt.
    BorderWidth,
    /// Background border dash — linetype-producing scales.
    BorderLinetype,
    /// Background corner radius — number-producing scales, in pt.
    CornerRadius,

    // ── Text (TextElement) ──
    /// Text color.
    Color,
    /// Text size — number-producing scales, in pt.
    TextSize,
    /// Text outline color.
    TextOutlineColor,
    /// Text outline width — number-producing scales, in pt.
    TextOutlineWidth,
    /// Font family — string-producing scales. The CSS generic names
    /// (`serif`, `sans-serif`, `monospace`, `cursive`, `fantasy`,
    /// `system-ui`) map to generic families; anything else is a named
    /// family.
    FontFamily,
    /// Font weight — number-producing scales, 1–1000.
    FontWeight,
    /// Font width — string-producing scales: kebab-case width names
    /// (`condensed`, `semi-expanded`, `normal`, …).
    FontWidth,
    /// Font style — string-producing scales: `normal` or `italic`.
    FontStyle,
    /// Line height — number-producing scales, as a multiplier of the
    /// resolved font size (`Rel` semantics).
    LineHeight,
    /// Letter spacing — number-producing scales, in em (`Rel`
    /// semantics).
    Tracking,
    /// Underline — boolean-producing values.
    Underline,
    /// Strikethrough — boolean-producing values.
    Strikethrough,
    /// Horizontal alignment — string-producing scales: `start` /
    /// `center` / `end` / `justify` (`left` / `right` accepted as
    /// aliases).
    Align,
}

impl TableCell {
    /// A single-track cell of `kind` at (`row`, `col`) holding `text`.
    pub fn new(kind: TableCellKind, row: u16, col: u16, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            row,
            col,
            row_span: 1,
            col_span: 1,
            kind,
            align: None,
            aesthetics: Vec::new(),
        }
    }

    /// Set the cell's spans.
    pub fn span(mut self, row_span: u16, col_span: u16) -> Self {
        self.row_span = row_span;
        self.col_span = col_span;
        self
    }

    /// Set the cell's text alignment.
    pub fn align(mut self, align: HAlign) -> Self {
        self.align = Some(align);
        self
    }

    /// Set one aesthetic of the cell to a fixed, output-space value —
    /// bypassing scales, like a `Raw(...)` channel on a geom.
    pub fn fixed(mut self, aesthetic: TableScaledAesthetic, value: impl Into<Value>) -> Self {
        self.aesthetics
            .push((aesthetic, CellAesthetic::Raw(value.into())));
        self
    }

    /// Drive one aesthetic of the cell from `value`, mapped through
    /// the scale registered under `scale` on the composition.
    pub fn scaled(
        mut self,
        aesthetic: TableScaledAesthetic,
        value: impl Into<Value>,
        scale: impl Into<String>,
    ) -> Self {
        self.aesthetics
            .push((aesthetic, CellAesthetic::Scaled(value.into(), scale.into())));
        self
    }

    /// Drive the cell's background fill from `value`, mapped through
    /// the scale registered under `scale` on the composition.
    pub fn scaled_fill(self, value: impl Into<Value>, scale: impl Into<String>) -> Self {
        self.scaled(TableScaledAesthetic::Fill, value, scale)
    }

    /// Drive the cell's text color from `value`, mapped through the
    /// scale registered under `scale` on the composition.
    pub fn scaled_color(self, value: impl Into<Value>, scale: impl Into<String>) -> Self {
        self.scaled(TableScaledAesthetic::Color, value, scale)
    }

    /// The cell's aesthetics as one [`TableCellTheme`] layer: raw
    /// values as given, scaled ones mapped through the registry. A
    /// scaled binding whose scale name isn't registered — or any
    /// binding whose value has the wrong shape for the aesthetic —
    /// contributes nothing, the same as an unresolvable geom channel.
    fn aesthetics_override(
        &self,
        registry: &crate::plot::scale::ScaleRegistry,
    ) -> Option<TableCellTheme> {
        if self.aesthetics.is_empty() {
            return None;
        }
        let mut text = TextElement::default();
        let mut background = RectElement::default();
        let mut any = false;
        for (aesthetic, cell_value) in &self.aesthetics {
            let mapped = match cell_value {
                CellAesthetic::Raw(v) => v.clone(),
                CellAesthetic::Scaled(v, name) => match registry.get(name) {
                    Some(scale) => scale.map(v),
                    None => continue,
                },
            };
            let applied = match aesthetic {
                TableScaledAesthetic::Fill => mapped.as_color().map(|c| {
                    background.fill = Some(ThemeColor::Fixed(c));
                }),
                TableScaledAesthetic::BorderColor => mapped.as_color().map(|c| {
                    background.color = Some(ThemeColor::Fixed(c));
                }),
                TableScaledAesthetic::BorderWidth => mapped.as_number().map(|pt| {
                    background.linewidth_pt = Some(Length::Abs(pt));
                }),
                TableScaledAesthetic::BorderLinetype => mapped.as_linetype().map(|steps| {
                    background.linetype = Some(Arc::from(steps));
                }),
                TableScaledAesthetic::CornerRadius => mapped.as_number().map(|pt| {
                    background.corner_radius = Some(Length::Abs(pt));
                }),
                TableScaledAesthetic::Color => mapped.as_color().map(|c| {
                    text.color = Some(ThemeColor::Fixed(c));
                }),
                TableScaledAesthetic::TextSize => mapped.as_number().map(|pt| {
                    text.size_pt = Some(Length::Abs(pt));
                }),
                TableScaledAesthetic::TextOutlineColor => mapped.as_color().map(|c| {
                    text.text_stroke = Some(ThemeColor::Fixed(c));
                }),
                TableScaledAesthetic::TextOutlineWidth => mapped.as_number().map(|pt| {
                    text.text_linewidth_pt = Some(Length::Abs(pt));
                }),
                TableScaledAesthetic::FontFamily => mapped.as_str().map(|s| {
                    text.font.family = Some(parse_font_family(s));
                }),
                TableScaledAesthetic::FontWeight => mapped.as_number().map(|n| {
                    text.font.weight = Some(FontWeight(n.clamp(1.0, 1000.0) as u16));
                }),
                TableScaledAesthetic::FontWidth => {
                    mapped.as_str().and_then(parse_font_width).map(|w| {
                        text.font.width = Some(w);
                    })
                }
                TableScaledAesthetic::FontStyle => {
                    mapped.as_str().and_then(parse_font_style).map(|s| {
                        text.font.style = Some(s);
                    })
                }
                TableScaledAesthetic::LineHeight => mapped.as_number().map(|m| {
                    text.lineheight = Some(Length::Rel(m));
                }),
                TableScaledAesthetic::Tracking => mapped.as_number().map(|m| {
                    text.tracking = Some(Length::Rel(m));
                }),
                TableScaledAesthetic::Underline => mapped.as_bool().map(|b| {
                    text.underline = Some(b);
                }),
                TableScaledAesthetic::Strikethrough => mapped.as_bool().map(|b| {
                    text.strikethrough = Some(b);
                }),
                TableScaledAesthetic::Align => mapped.as_str().and_then(parse_halign).map(|a| {
                    text.align = Some(a);
                }),
            };
            any |= applied.is_some();
        }
        if !any {
            return None;
        }
        Some(TableCellTheme {
            text: Element::Set(text),
            background: Element::Set(background),
        })
    }
}

impl TableCell {
    /// Resolve the cell's theme style and shape its text. This is the
    /// single point where a cell meets the theme; measure and draw
    /// both go through it, so a cell can't reserve a box styled one
    /// way and paint one styled another.
    #[allow(clippy::too_many_arguments)]
    fn shape(
        &self,
        table_theme: &TableTheme,
        theme: &Theme,
        registry: &crate::plot::scale::ScaleRegistry,
        root_pt: f64,
        dpi: f64,
        padding_px: (f64, f64, f64, f64),
        images: &Arc<crate::image_registry::ImageRegistry>,
    ) -> StyledCell {
        let (text_el, background) = match self.kind {
            TableCellKind::Title => (
                TableTheme::resolved_text_slot(&table_theme.title, &theme.text),
                None,
            ),
            TableCellKind::Subtitle => (
                TableTheme::resolved_text_slot(&table_theme.subtitle, &theme.text),
                None,
            ),
            TableCellKind::Caption => (
                TableTheme::resolved_text_slot(&table_theme.caption, &theme.text),
                None,
            ),
            TableCellKind::Footnote => (
                TableTheme::resolved_text_slot(&table_theme.footnote, &theme.text),
                None,
            ),
            TableCellKind::RowGroup => {
                let r = table_theme.resolved_row_group(&theme.text);
                (r.text, r.background)
            }
            TableCellKind::Summary { .. } => {
                let r = table_theme.resolved_summary(&theme.text);
                (r.text, r.background)
            }
            TableCellKind::Spanner => {
                let r = table_theme.resolved_heading(HeadingRole::Spanner, &theme.text);
                (r.text, r.background)
            }
            TableCellKind::ColumnLabel => {
                let r = table_theme.resolved_heading(HeadingRole::ColumnLabel, &theme.text);
                (r.text, r.background)
            }
            TableCellKind::StubHead => {
                let r = table_theme.resolved_heading(HeadingRole::StubHead, &theme.text);
                (r.text, r.background)
            }
            TableCellKind::Stub { row } => {
                let r = table_theme.resolved_stub(row, &theme.text);
                (r.text, r.background)
            }
            TableCellKind::Body { row } => {
                let r = table_theme.resolved_body(row, self.col as usize, &theme.text);
                (r.text, r.background)
            }
        };
        // The cell's own aesthetics sit above the structural cascade,
        // raw and scaled bindings in one layer.
        let aesthetics = self.aesthetics_override(registry);
        let (text_el, background) = if let Some(o) = &aesthetics {
            let resolved = ResolvedTableCells {
                text: text_el,
                background,
            }
            .with_override(o);
            (resolved.text, resolved.background)
        } else {
            (text_el, background)
        };
        let Some(el) = text_el else {
            return StyledCell {
                kind: self.kind,
                run: None,
                background,
                align: HAlign::Start,
                color: crate::color::Color::TRANSPARENT,
                outline: None,
                padding_px,
            };
        };
        let align = self
            .align
            .or(el.align)
            .expect("text_concrete_defaults sets align");
        let style = text_style_from(&el, root_pt);
        let color = el
            .color
            .clone()
            .or_else(|| text_concrete_defaults().color.clone())
            .expect("text_concrete_defaults sets color")
            .resolve(&theme.palette);
        let outline = text_outline_from(&el, &theme.palette, dpi);
        let rich = rich_chrome_for(&el, theme, dpi, images);
        let run = ChromeRun::shape(&self.text, &style, dpi, rich.as_ref());
        StyledCell {
            kind: self.kind,
            run: Some(run),
            background,
            align,
            color,
            outline,
            padding_px,
        }
    }
}

/// A cell with its style resolved and its text shaped — what both the
/// grid measure and the draw pass consume.
struct StyledCell {
    kind: TableCellKind,
    /// `None` when the cascade hid the cell's text — the background
    /// and padding still occupy their space.
    run: Option<ChromeRun>,
    background: Option<crate::plot::theme::RectElement>,
    align: HAlign,
    color: crate::color::Color,
    outline: Option<crate::plot::chrome::text::TextOutline>,
    /// Cell padding in pixels, `(top, right, bottom, left)`.
    padding_px: (f64, f64, f64, f64),
}

impl StyledCell {
    /// The cell's content size plus padding — the leaf measure the
    /// inner grid solves against.
    fn natural_size(&self) -> (f64, f64) {
        let (t, r, b, l) = self.padding_px;
        match &self.run {
            Some(run) => (run.width() + l + r, run.line_box_height() + t + b),
            None => (l + r, t + b),
        }
    }
}

/// A fixed-size leaf — the table's inner grid only ever solves
/// content-sized tracks, so the measure is a constant.
struct LeafMeasure {
    w: f64,
    h: f64,
}

impl Measure for LeafMeasure {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(self.w)
    }

    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        self.h
    }
}

/// The built inner grid plus its shaped cells, before solving. Shared
/// between the panel measure (which only needs sizes) and the draw
/// pass (which needs the runs and styles too).
struct InnerGrid {
    grid: Grid,
    entries: Vec<(CellId, StyledCell)>,
}

impl InnerGrid {
    /// Solve at `width` pixels and collect every cell's rect, plus the
    /// rule positions derived from the solved geometry.
    fn solve_at(self, width: f64, dpi: f64) -> SolvedTable {
        let layout = self.grid.solve(Size::new(width.max(1.0), 1.0e7), dpi);
        let mut cells = Vec::with_capacity(self.entries.len());
        for (id, styled) in self.entries {
            let Some(rect) = layout.rect(id) else {
                continue;
            };
            cells.push(SolvedCell {
                kind: styled.kind,
                rect,
                run: styled.run,
                background: styled.background,
                align: styled.align,
                color: styled.color,
                outline: styled.outline,
                padding_px: styled.padding_px,
            });
        }
        SolvedTable::from_cells(cells)
    }

    /// The table's natural size at an unconstrained width: the solved
    /// bounding box of every cell.
    fn natural_size(&self, dpi: f64) -> (f64, f64) {
        let layout = self.grid.solve(Size::new(1.0e7, 1.0e7), dpi);
        let mut bbox: Option<Rect> = None;
        for (id, _) in &self.entries {
            if let Some(r) = layout.rect(*id) {
                bbox = Some(match bbox {
                    Some(b) => b.union(r),
                    None => r,
                });
            }
        }
        bbox.map(|b| (b.width(), b.height())).unwrap_or((0.0, 0.0))
    }
}

/// A solved cell: its rect in the inner grid's coordinate space, its
/// shaped run, and everything the draw pass needs.
struct SolvedCell {
    rect: Rect,
    run: Option<ChromeRun>,
    background: Option<crate::plot::theme::RectElement>,
    align: HAlign,
    color: crate::color::Color,
    outline: Option<crate::plot::chrome::text::TextOutline>,
    padding_px: (f64, f64, f64, f64),
    kind: TableCellKind,
}

/// The solved table: cell rects plus derived rule positions, all in
/// the inner grid's coordinate space.
struct SolvedTable {
    cells: Vec<SolvedCell>,
    /// Bottom y of the heading block (the header rule's position).
    header_bottom: Option<f64>,
    /// Boundaries between body rows (each body row's bottom edge
    /// except the last's).
    body_boundaries: Vec<f64>,
    /// Boundaries between columns (each column's right edge except the
    /// last's).
    column_boundaries: Vec<f64>,
}

impl SolvedTable {
    /// Collect the solved cells and derive rule positions from them.
    fn from_cells(cells: Vec<SolvedCell>) -> Self {
        let header_bottom = cells
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    TableCellKind::ColumnLabel | TableCellKind::StubHead | TableCellKind::Spanner
                )
            })
            .map(|c| c.rect.y1)
            .reduce(f64::max);
        let mut body_tops: Vec<f64> = cells
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    TableCellKind::Body { .. } | TableCellKind::Stub { .. }
                )
            })
            .map(|c| c.rect.y0)
            .collect();
        body_tops.sort_by(f64::total_cmp);
        body_tops.dedup();
        let body_boundaries = body_tops.iter().skip(1).copied().collect();
        let mut col_rights: Vec<f64> = cells
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    TableCellKind::Body { .. } | TableCellKind::Stub { .. }
                )
            })
            .map(|c| c.rect.x1)
            .collect();
        col_rights.sort_by(f64::total_cmp);
        col_rights.dedup();
        let column_boundaries = match col_rights.as_slice() {
            [rest @ .., _last] => rest.to_vec(),
            [] => Vec::new(),
        };
        Self {
            cells,
            header_bottom,
            body_boundaries,
            column_boundaries,
        }
    }

    /// The bounding box of every cell — the table's drawn extent.
    fn bounding_box(&self) -> Option<Rect> {
        self.cells.iter().map(|c| c.rect).reduce(|a, b| a.union(b))
    }
}

/// The panel-slot measure: reports the table's natural size so `Auto`
/// composition tracks size to it.
struct TableMeasure {
    inner: InnerGrid,
}

impl Measure for TableMeasure {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        WidthHint::Min(self.inner.natural_size(dpi).0)
    }

    fn height_at(&self, width: f64, dpi: f64) -> f64 {
        let layout = self.inner.grid.solve(Size::new(width.max(1.0), 1.0e7), dpi);
        let mut bbox: Option<Rect> = None;
        for (id, _) in &self.inner.entries {
            if let Some(r) = layout.rect(*id) {
                bbox = Some(match bbox {
                    Some(b) => b.union(r),
                    None => r,
                });
            }
        }
        bbox.map(|b| b.height()).unwrap_or(0.0)
    }
}
