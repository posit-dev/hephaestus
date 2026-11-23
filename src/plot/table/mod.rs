//! The high-level table API: a [`Table`] occupies a named patch in a
//! [`Composition`] the same way a [`Plot`](crate::plot::Plot) does, so
//! tables and plots mix freely in one layout and travel together in a
//! plot document.
//!
//! A table is declared as columns, optional spanners, and body rows of
//! plain strings — number formatting is the producer's job, which is
//! what keeps the model writer-agnostic (ggsql's `TABULATE` maps onto
//! it near-mechanically). Styling comes entirely from
//! [`TableTheme`](crate::plot::theme::TableTheme): the draw path reads
//! only resolved theme values.
//!
//! Internally a table is one measured cell in the patch's
//! [`Slot::Panel`]. Its own geometry — column widths, spanner
//! colspans, label rowspans — is solved by a private inner
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
    rect_concrete_defaults, text_concrete_defaults, HAlign, HeadingRole, ResolvedTableCells,
    RulePosition, TableCellTheme, TableTheme, Theme, ThemePart,
};
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;

/// One column of a [`Table`], in display order after stub columns have
/// moved to the front.
#[derive(Debug, Clone, PartialEq)]
pub struct TableColumn {
    /// Stable key — spanners and rows address columns by name.
    pub name: String,
    /// Displayed column label.
    pub label: String,
    /// Cell alignment for this column's label and body cells. `None`
    /// falls back to the theme's resolved alignment for the role.
    pub align: Option<HAlign>,
    /// Column width. `None` sizes to content; `Some` pins the track —
    /// an [`Extent`] with a `percent` component resolves against the
    /// panel's width.
    pub width: Option<Extent>,
    /// Stub columns render before every other column and style through
    /// the theme's stub layer.
    pub stub: bool,
}

/// A label spanning several columns, one heading row per level. Level
/// 0 sits directly above the column labels; higher levels stack above
/// it. Spanners over non-contiguous columns split into one cell per
/// contiguous run.
#[derive(Debug, Clone, PartialEq)]
pub struct TableSpanner {
    /// Displayed label.
    pub label: String,
    /// Heading level — 0 is nearest the column labels.
    pub level: u16,
    /// Names of the columns the span covers.
    pub columns: Vec<String>,
}

/// A row group: a full-width header row inserted above the group's
/// first body row. A group covers the rows from `start` up to the
/// next group's `start` (or the end of the body); rows before the
/// first group's `start` are ungrouped.
#[derive(Debug, Clone, PartialEq)]
pub struct TableRowGroup {
    /// Displayed group label.
    pub label: String,
    /// 0-based body row the group starts at.
    pub start: usize,
}

/// A table bound to a named patch. See the module docs for the model;
/// the builder methods below are the canonical construction path.
#[derive(Debug, Clone)]
pub struct Table {
    patch_id: Arc<str>,
    columns: Vec<TableColumn>,
    spanners: Vec<TableSpanner>,
    rows: Vec<Vec<String>>,
    /// Row groups, ordered by `start`.
    row_groups: Vec<TableRowGroup>,
    /// Summary rows, drawn after the body rows.
    summary_rows: Vec<Vec<String>>,
    /// Footnotes, one full-width row each, below the caption.
    footnotes: Vec<String>,
    title: Option<String>,
    subtitle: Option<String>,
    caption: Option<String>,
    theme_override: Option<ThemePart>,
    theme_variant: Option<String>,
    /// Per-body-row style overrides, layered above the row's parity
    /// layer in the cascade.
    row_styles: std::collections::HashMap<usize, crate::plot::theme::TableCellTheme>,
    /// Per-cell style overrides, keyed by (body row, column name) —
    /// the outermost layer of the cascade.
    cell_styles: std::collections::HashMap<(usize, String), crate::plot::theme::TableCellTheme>,
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
            columns: Vec::new(),
            spanners: Vec::new(),
            rows: Vec::new(),
            row_groups: Vec::new(),
            summary_rows: Vec::new(),
            footnotes: Vec::new(),
            title: None,
            subtitle: None,
            caption: None,
            theme_override: None,
            theme_variant: None,
            row_styles: std::collections::HashMap::new(),
            cell_styles: std::collections::HashMap::new(),
        })
    }

    /// Build a table from already-decoded parts, validating the way
    /// the builder methods would: the patch must exist and every row
    /// must match the column count. This is the document reader's
    /// reconstruction path; callers assembling a table by hand want
    /// the builder methods.
    #[cfg(feature = "document-read")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        composition: &Composition,
        patch_id: String,
        columns: Vec<TableColumn>,
        spanners: Vec<TableSpanner>,
        rows: Vec<Vec<String>>,
        row_groups: Vec<TableRowGroup>,
        summary_rows: Vec<Vec<String>>,
        footnotes: Vec<String>,
        title: Option<String>,
        subtitle: Option<String>,
        caption: Option<String>,
        theme_override: Option<ThemePart>,
        theme_variant: Option<String>,
        row_styles: std::collections::HashMap<usize, crate::plot::theme::TableCellTheme>,
        cell_styles: std::collections::HashMap<(usize, String), crate::plot::theme::TableCellTheme>,
    ) -> Result<Self, PlotError> {
        if let Some(row) = rows.iter().find(|r| r.len() != columns.len()) {
            return Err(PlotError::TableRowWidth {
                patch: patch_id,
                got: row.len(),
                expected: columns.len(),
            });
        }
        if let Some(row) = summary_rows.iter().find(|r| r.len() != columns.len()) {
            return Err(PlotError::TableRowWidth {
                patch: patch_id,
                got: row.len(),
                expected: columns.len(),
            });
        }
        let mut table = Self::try_new(composition, patch_id)?;
        table.columns = columns;
        table.spanners = spanners;
        table.rows = rows;
        table.row_groups = row_groups;
        table.summary_rows = summary_rows;
        table.footnotes = footnotes;
        table.title = title;
        table.subtitle = subtitle;
        table.caption = caption;
        table.theme_override = theme_override;
        table.theme_variant = theme_variant;
        table.row_styles = row_styles;
        table.cell_styles = cell_styles;
        Ok(table)
    }

    /// The patch this table is bound to.
    pub fn patch_id(&self) -> &str {
        &self.patch_id
    }

    /// Append a column with the given name and label.
    pub fn column(mut self, name: impl Into<String>, label: impl Into<String>) -> Self {
        self.columns.push(TableColumn {
            name: name.into(),
            label: label.into(),
            align: None,
            width: None,
            stub: false,
        });
        self
    }

    /// Append a fully-specified column.
    pub fn column_full(mut self, column: TableColumn) -> Self {
        self.columns.push(column);
        self
    }

    /// Mark the named column as a stub column. Stub columns render
    /// first, regardless of declaration order. Panics when no column
    /// carries the name.
    pub fn stub_column(mut self, name: &str) -> Self {
        let col = self
            .columns
            .iter_mut()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no column named {name:?}"));
        col.stub = true;
        self
    }

    /// Add a spanner over the named columns at `level` (0 is nearest
    /// the column labels).
    pub fn spanner(
        mut self,
        label: impl Into<String>,
        level: u16,
        columns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.spanners.push(TableSpanner {
            label: label.into(),
            level,
            columns: columns.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// Append a body row. Values pair with the declared columns in
    /// declaration order. Panics when the row's length differs from
    /// the column count.
    pub fn row(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let row: Vec<String> = values.into_iter().map(Into::into).collect();
        assert_eq!(
            row.len(),
            self.columns.len(),
            "row has {} values but the table declares {} columns",
            row.len(),
            self.columns.len(),
        );
        self.rows.push(row);
        self
    }

    /// Begin a row group: append the group's rows and draw a
    /// full-width group header above them. Groups must be appended in
    /// order; every following [`Table::row`] joins the latest group.
    pub fn row_group(
        mut self,
        label: impl Into<String>,
        rows: impl IntoIterator<Item = impl IntoIterator<Item = impl Into<String>>>,
    ) -> Self {
        let start = self.rows.len();
        for row in rows {
            self = self.row(row);
        }
        assert!(
            self.rows.len() > start,
            "a row group must contain at least one row"
        );
        self.row_groups.push(TableRowGroup {
            label: label.into(),
            start,
        });
        self
    }

    /// Append a summary row, drawn after the body rows and styled
    /// through the theme's `summary` layer. Same width rule as
    /// [`Table::row`].
    pub fn summary_row(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let row: Vec<String> = values.into_iter().map(Into::into).collect();
        assert_eq!(
            row.len(),
            self.columns.len(),
            "summary row has {} values but the table declares {} columns",
            row.len(),
            self.columns.len(),
        );
        self.summary_rows.push(row);
        self
    }

    /// Append a footnote — a full-width row below the caption, styled
    /// through the theme's `footnote` element.
    pub fn footnote(mut self, s: impl Into<String>) -> Self {
        self.footnotes.push(s.into());
        self
    }

    /// Set the table's title, drawn above the heading block.
    pub fn title(mut self, s: impl Into<String>) -> Self {
        self.title = Some(s.into());
        self
    }

    /// Set the table's subtitle, drawn below the title.
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    /// Set the table's caption, drawn below the body.
    pub fn caption(mut self, s: impl Into<String>) -> Self {
        self.caption = Some(s.into());
        self
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

    /// Style one body row above the theme's cascade — bold text, a
    /// background, or any other [`TableCellTheme`] field, applied to
    /// every cell of the row (stub cells included). Layers above the
    /// row's parity layer; a [`Table::cell_style`] on the same row
    /// wins field-for-field.
    pub fn row_style(mut self, row: usize, style: crate::plot::theme::TableCellTheme) -> Self {
        self.row_styles.insert(row, style);
        self
    }

    /// Style one body cell, addressed by its 0-based body row and its
    /// column's name. The outermost layer of the cascade: every field
    /// it sets wins over the row, the parity layer, and the theme.
    pub fn cell_style(
        mut self,
        row: usize,
        column: &str,
        style: crate::plot::theme::TableCellTheme,
    ) -> Self {
        self.cell_styles.insert((row, column.to_string()), style);
        self
    }

    // ── Accessors (the document codec reads through these) ───────────

    /// The declared columns, in declaration order (stub flags
    /// included; display order puts stubs first).
    pub fn columns(&self) -> &[TableColumn] {
        &self.columns
    }

    /// The declared spanners.
    pub fn spanners(&self) -> &[TableSpanner] {
        &self.spanners
    }

    /// The body rows, each pairing with [`Self::columns`] by position.
    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    /// The row groups, ordered by start row.
    pub fn row_groups(&self) -> &[TableRowGroup] {
        &self.row_groups
    }

    /// The summary rows, drawn after the body.
    pub fn summary_rows(&self) -> &[Vec<String>] {
        &self.summary_rows
    }

    /// The footnotes, one row each below the caption.
    pub fn footnotes(&self) -> &[String] {
        &self.footnotes
    }

    /// The table's title, if set.
    pub fn title_ref(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The table's subtitle, if set.
    pub fn subtitle_ref(&self) -> Option<&str> {
        self.subtitle.as_deref()
    }

    /// The table's caption, if set.
    pub fn caption_ref(&self) -> Option<&str> {
        self.caption.as_deref()
    }

    /// The table's sparse theme override, if set.
    pub fn theme_override_ref(&self) -> Option<&ThemePart> {
        self.theme_override.as_ref()
    }

    /// The named theme variant the table opted into, if any.
    pub fn theme_variant_ref(&self) -> Option<&str> {
        self.theme_variant.as_deref()
    }

    /// The per-row style overrides, keyed by 0-based body row.
    pub fn row_styles(
        &self,
    ) -> &std::collections::HashMap<usize, crate::plot::theme::TableCellTheme> {
        &self.row_styles
    }

    /// The per-cell style overrides, keyed by (0-based body row,
    /// column name).
    pub fn cell_styles(
        &self,
    ) -> &std::collections::HashMap<(usize, String), crate::plot::theme::TableCellTheme> {
        &self.cell_styles
    }

    // ── Layout and drawing ───────────────────────────────────────────

    /// Attach the table's measured panel cell to `patch`. The outer
    /// margin band is sized from the table theme's `margin`.
    pub(crate) fn wire_panel(
        &self,
        patch: Patch,
        theme: &Theme,
        dpi: f64,
        images: &Arc<crate::image_registry::ImageRegistry>,
    ) -> Patch {
        let table_theme = theme.table_for(self.theme_variant.as_deref());
        let root_pt = root_text_pt(theme);
        let (mt, mr, mb, ml) = table_theme.margin.resolve(root_pt);
        let pt_to_px = dpi / 72.0;
        let mut patch = patch;
        let inset = crate::layout::Inset::default()
            .top(Extent::px(mt * pt_to_px))
            .right(Extent::px(mr * pt_to_px))
            .bottom(Extent::px(mb * pt_to_px))
            .left(Extent::px(ml * pt_to_px));
        patch = patch.margin(inset);
        let cells = self.build_cells();
        let measure = TableMeasure {
            inner: self.build_grid(&cells, theme, dpi, images),
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
        if let Some(bg_slot) = theme.plot_background.as_set() {
            if let Some(rect) = layout.get(&self.patch_id, Slot::Background) {
                if rect.x1 > rect.x0 && rect.y1 > rect.y0 {
                    let bg = bg_slot.cascade(&theme.rect);
                    let defaults = rect_concrete_defaults();
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
                    scene.push_pick_scope(&crate::plot::pick::region_scope(Slot::Background));
                    scene.push_pick_scope(&part_scope(PlotPart::PlotBackground));
                    if let Some(fill) = &bg.fill {
                        scene.fill(
                            FillRule::NonZero,
                            Affine::IDENTITY,
                            &crate::brush::Brush::Solid(fill.resolve(&theme.palette)),
                            None,
                            &path,
                            PickId::Skip,
                        );
                    }
                    let lw = bg
                        .linewidth_pt
                        .or(defaults.linewidth_pt)
                        .expect("rect linewidth default");
                    if lw.resolve(1.0) > 0.0 {
                        let stroke = Stroke::new(lw.resolve(1.0) * dpi / 72.0);
                        let color = bg.color.or(defaults.color).expect("rect color default");
                        scene.stroke(
                            &stroke,
                            Affine::IDENTITY,
                            &crate::brush::Brush::Solid(color.resolve(&theme.palette)),
                            None,
                            &path,
                            PickId::Skip,
                        );
                    }
                    scene.pop_pick_scope();
                    scene.pop_pick_scope();
                }
            }
        }

        scene.push_pick_scope(&crate::plot::pick::region_scope(Slot::Panel));

        let cells = self.build_cells();
        let inner = self.build_grid(&cells, theme, dpi, images);
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

    /// The columns in display order — stubs first, declaration order
    /// preserved within each group.
    fn display_columns(&self) -> Vec<&TableColumn> {
        self.columns
            .iter()
            .filter(|c| c.stub)
            .chain(self.columns.iter().filter(|c| !c.stub))
            .collect()
    }

    /// Lay out every cell's grid position and role, without any theme
    /// input. Row 0 is the topmost heading row; see the struct docs
    /// for the stacking order.
    fn build_cells(&self) -> Vec<CellSpec> {
        let cols = self.display_columns();
        let ncol = cols.len().max(1) as u16;
        let mut cells = Vec::new();
        let mut row = 0u16;

        // Title / subtitle rows span the full width.
        for (text, kind) in [
            (&self.title, CellKind::Title),
            (&self.subtitle, CellKind::Subtitle),
        ] {
            if let Some(text) = text {
                cells.push(CellSpec {
                    text: text.clone(),
                    row,
                    col: 0,
                    row_span: 1,
                    col_span: ncol,
                    kind,
                    align: None,
                    column: None,
                });
                row += 1;
            }
        }

        // ── Heading block ──
        //
        // `n_levels` spanner rows, highest level on top, then the
        // column-label row. A spanner at level `l` occupies row
        // `n_levels - 1 - l` and stretches down over the rows of lower
        // levels none of its columns participate in. A column label
        // stretches up to just below the lowest spanner covering it.
        let max_level = self.spanners.iter().map(|s| s.level).max();
        let n_levels = max_level.map(|l| l + 1).unwrap_or(0);
        let header_top = row;
        let label_row = header_top + n_levels;

        // Per-column lowest covering spanner level.
        let col_spanner_at = |col: &str, level: u16| -> Option<&TableSpanner> {
            self.spanners
                .iter()
                .find(|s| s.level == level && s.columns.iter().any(|c| c == col))
        };
        let col_min_level = |col: &str| -> Option<u16> {
            (0..n_levels).find(|l| col_spanner_at(col, *l).is_some())
        };

        // Spanner cells, split into contiguous runs.
        for level in (0..n_levels).rev() {
            let grid_row = header_top + (n_levels - 1 - level);
            let mut c = 0usize;
            while c < cols.len() {
                let Some(spanner) = col_spanner_at(&cols[c].name, level) else {
                    c += 1;
                    continue;
                };
                // The contiguous run of columns sharing this spanner.
                let start = c;
                while c < cols.len()
                    && col_spanner_at(&cols[c].name, level)
                        .is_some_and(|s| std::ptr::eq(s, spanner))
                {
                    c += 1;
                }
                // Stretch down over lower levels none of the run's
                // columns participate in.
                let lower = (0..level).rev().find(|l| {
                    cols[start..c]
                        .iter()
                        .any(|col| col_spanner_at(&col.name, *l).is_some())
                });
                let bottom_row = match lower {
                    Some(l) => header_top + (n_levels - 1 - l),
                    None => label_row,
                };
                cells.push(CellSpec {
                    text: spanner.label.clone(),
                    row: grid_row,
                    col: start as u16,
                    row_span: bottom_row - grid_row,
                    col_span: (c - start) as u16,
                    kind: CellKind::Spanner,
                    align: None,
                    column: None,
                });
            }
        }

        // Column labels (and the stub head, an empty corner cell when
        // any stub column exists).
        for (c, col) in cols.iter().enumerate() {
            let start = match col_min_level(&col.name) {
                Some(l) => header_top + (n_levels - l),
                None => header_top,
            };
            let kind = if col.stub {
                CellKind::StubHead
            } else {
                CellKind::ColumnLabel
            };
            cells.push(CellSpec {
                text: col.label.clone(),
                row: start,
                col: c as u16,
                row_span: label_row + 1 - start,
                col_span: 1,
                kind,
                align: col.align,
                column: None,
            });
        }
        row = label_row + 1;

        // ── Body, with row-group headers interleaved ──
        let mut groups = self.row_groups.iter().peekable();
        for (r, body_row) in self.rows.iter().enumerate() {
            if groups.peek().is_some_and(|g| g.start == r) {
                let group = groups.next().expect("peeked");
                cells.push(CellSpec {
                    text: group.label.clone(),
                    row,
                    col: 0,
                    row_span: 1,
                    col_span: ncol,
                    kind: CellKind::RowGroup,
                    align: None,
                    column: None,
                });
                row += 1;
            }
            // Rows are declared against declaration order; re-index to
            // display order.
            for (c, col) in cols.iter().enumerate() {
                let decl_idx = self
                    .columns
                    .iter()
                    .position(|d| d.name == col.name)
                    .expect("display column must exist in declaration order");
                let kind = if col.stub {
                    CellKind::Stub { row: r }
                } else {
                    CellKind::Body { row: r }
                };
                cells.push(CellSpec {
                    text: body_row[decl_idx].clone(),
                    row,
                    col: c as u16,
                    row_span: 1,
                    col_span: 1,
                    kind,
                    align: col.align,
                    column: Some(col.name.clone()),
                });
            }
            row += 1;
        }

        // ── Summary rows ──
        for (sr, summary) in self.summary_rows.iter().enumerate() {
            for (c, col) in cols.iter().enumerate() {
                let decl_idx = self
                    .columns
                    .iter()
                    .position(|d| d.name == col.name)
                    .expect("display column must exist in declaration order");
                cells.push(CellSpec {
                    text: summary[decl_idx].clone(),
                    row,
                    col: c as u16,
                    row_span: 1,
                    col_span: 1,
                    kind: CellKind::Summary { row: sr },
                    align: col.align,
                    column: Some(col.name.clone()),
                });
            }
            row += 1;
        }

        // Caption, then one full-width row per footnote.
        if let Some(text) = &self.caption {
            cells.push(CellSpec {
                text: text.clone(),
                row,
                col: 0,
                row_span: 1,
                col_span: ncol,
                kind: CellKind::Caption,
                align: None,
                column: None,
            });
            row += 1;
        }
        for note in &self.footnotes {
            cells.push(CellSpec {
                text: note.clone(),
                row,
                col: 0,
                row_span: 1,
                col_span: ncol,
                kind: CellKind::Footnote,
                align: None,
                column: None,
            });
            row += 1;
        }

        cells
    }

    /// Build the inner grid for a set of laid-out cells: column tracks
    /// from the columns' width overrides, `Auto` everywhere else, one
    /// measured leaf per cell.
    fn build_grid(
        &self,
        cells: &[CellSpec],
        theme: &Theme,
        dpi: f64,
        images: &Arc<crate::image_registry::ImageRegistry>,
    ) -> InnerGrid {
        let cols = self.display_columns();
        let table_theme = theme.table_for(self.theme_variant.as_deref());
        let root_pt = root_text_pt(theme);
        let pt_to_px = dpi / 72.0;
        let padding_px = {
            let (t, r, b, l) = table_theme.cell_padding.resolve(root_pt);
            (t * pt_to_px, r * pt_to_px, b * pt_to_px, l * pt_to_px)
        };

        let col_tracks: Vec<Track> = cols
            .iter()
            .map(|c| match &c.width {
                Some(w) => Track::Fixed(w.clone()),
                None => Track::Auto,
            })
            .collect();
        let n_rows = cells.iter().map(|c| c.row + c.row_span).max().unwrap_or(0);
        let row_tracks = vec![Track::Auto; n_rows as usize];

        let mut grid = Grid::new(col_tracks, row_tracks);
        let mut entries = Vec::with_capacity(cells.len());
        for (i, spec) in cells.iter().enumerate() {
            let id = CellId(i as u64 + 1);
            // Row and cell overrides apply to body and stub cells only;
            // heading and free-text cells style through the theme.
            let (row_style, cell_style) = match spec.kind {
                CellKind::Body { row } | CellKind::Stub { row } => {
                    let row_style = self.row_styles.get(&row);
                    let cell_style = spec
                        .column
                        .as_ref()
                        .and_then(|c| self.cell_styles.get(&(row, c.clone())));
                    (row_style, cell_style)
                }
                _ => (None, None),
            };
            let styled = spec.shape(
                table_theme,
                theme,
                root_pt,
                dpi,
                padding_px,
                images,
                row_style,
                cell_style,
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
/// and which pick part it reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellKind {
    Title,
    Subtitle,
    Caption,
    Spanner,
    ColumnLabel,
    StubHead,
    Stub {
        /// 0-based body row index.
        row: usize,
    },
    Body {
        /// 0-based body row index.
        row: usize,
    },
    RowGroup,
    Summary {
        /// 0-based summary row index.
        row: usize,
    },
    Footnote,
}

impl CellKind {
    /// The pick part this cell reports.
    fn part(&self) -> PlotPart {
        match self {
            CellKind::Title => PlotPart::TableTitle,
            CellKind::Subtitle => PlotPart::TableSubtitle,
            CellKind::Caption => PlotPart::TableCaption,
            CellKind::Spanner => PlotPart::TableSpanner,
            CellKind::ColumnLabel => PlotPart::TableColumnLabel,
            CellKind::StubHead => PlotPart::TableStubHead,
            CellKind::Stub { .. } => PlotPart::TableStub,
            CellKind::Body { .. } => PlotPart::TableCell,
            CellKind::RowGroup => PlotPart::TableRowGroup,
            CellKind::Summary { .. } => PlotPart::TableSummary,
            CellKind::Footnote => PlotPart::TableFootnote,
        }
    }

    /// The pick item ordinal for body, stub and summary cells — the
    /// 0-based index within its section. Heading and free-text cells
    /// carry no item.
    fn item(&self) -> Option<u32> {
        match self {
            CellKind::Stub { row } | CellKind::Body { row } | CellKind::Summary { row } => {
                Some(*row as u32)
            }
            _ => None,
        }
    }
}

/// A cell's position and role in the table grid, before any theme or
/// shaping is applied.
#[derive(Debug, Clone)]
struct CellSpec {
    text: String,
    row: u16,
    col: u16,
    row_span: u16,
    col_span: u16,
    kind: CellKind,
    align: Option<HAlign>,
    /// The column's name, for body and stub cells — the key per-cell
    /// style overrides address. `None` for heading and free-text cells.
    column: Option<String>,
}

impl CellSpec {
    /// Resolve the cell's theme style and shape its text. This is the
    /// single point where a cell meets the theme; measure and draw
    /// both go through it, so a cell can't reserve a box styled one
    /// way and paint one styled another.
    #[allow(clippy::too_many_arguments)]
    fn shape(
        &self,
        table_theme: &TableTheme,
        theme: &Theme,
        root_pt: f64,
        dpi: f64,
        padding_px: (f64, f64, f64, f64),
        images: &Arc<crate::image_registry::ImageRegistry>,
        row_style: Option<&TableCellTheme>,
        cell_style: Option<&TableCellTheme>,
    ) -> StyledCell {
        let (text_el, background) = match self.kind {
            CellKind::Title => (
                TableTheme::resolved_text_slot(&table_theme.title, &theme.text),
                None,
            ),
            CellKind::Subtitle => (
                TableTheme::resolved_text_slot(&table_theme.subtitle, &theme.text),
                None,
            ),
            CellKind::Caption => (
                TableTheme::resolved_text_slot(&table_theme.caption, &theme.text),
                None,
            ),
            CellKind::Footnote => (
                TableTheme::resolved_text_slot(&table_theme.footnote, &theme.text),
                None,
            ),
            CellKind::RowGroup => {
                let r = table_theme.resolved_row_group(&theme.text);
                (r.text, r.background)
            }
            CellKind::Summary { .. } => {
                let r = table_theme.resolved_summary(&theme.text);
                (r.text, r.background)
            }
            CellKind::Spanner => {
                let r = table_theme.resolved_heading(HeadingRole::Spanner, &theme.text);
                (r.text, r.background)
            }
            CellKind::ColumnLabel => {
                let r = table_theme.resolved_heading(HeadingRole::ColumnLabel, &theme.text);
                (r.text, r.background)
            }
            CellKind::StubHead => {
                let r = table_theme.resolved_heading(HeadingRole::StubHead, &theme.text);
                (r.text, r.background)
            }
            CellKind::Stub { row } => {
                let r = table_theme.resolved_stub(row, &theme.text);
                (r.text, r.background)
            }
            CellKind::Body { row } => {
                let r = table_theme.resolved_body(row, self.col as usize, &theme.text);
                (r.text, r.background)
            }
        };
        // Per-row and per-cell overrides sit above every structural
        // layer, row first so the cell wins field-for-field.
        let (text_el, background) = if row_style.is_some() || cell_style.is_some() {
            let mut resolved = ResolvedTableCells {
                text: text_el,
                background,
            };
            if let Some(o) = row_style {
                resolved = resolved.with_override(o);
            }
            if let Some(o) = cell_style {
                resolved = resolved.with_override(o);
            }
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
    kind: CellKind,
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
    kind: CellKind,
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
                    CellKind::ColumnLabel | CellKind::StubHead | CellKind::Spanner
                )
            })
            .map(|c| c.rect.y1)
            .reduce(f64::max);
        let mut body_tops: Vec<f64> = cells
            .iter()
            .filter(|c| matches!(c.kind, CellKind::Body { .. } | CellKind::Stub { .. }))
            .map(|c| c.rect.y0)
            .collect();
        body_tops.sort_by(f64::total_cmp);
        body_tops.dedup();
        let body_boundaries = body_tops.iter().skip(1).copied().collect();
        let mut col_rights: Vec<f64> = cells
            .iter()
            .filter(|c| matches!(c.kind, CellKind::Body { .. } | CellKind::Stub { .. }))
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
