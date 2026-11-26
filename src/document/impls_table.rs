//! Codec for tables and their parts.
//!
//! `TableColumn`, `TableRow`, `TableCell` and the two enums are plain
//! data with public fields, so they go through [`impl_codec`]. `Table`
//! itself is read through its accessors and rebuilt through
//! [`Table::from_cells`], so a document with overlapping cells is
//! refused by the same grid validation a render would apply.

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{read_record, Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{write_record, Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;
use crate::plot::table::{
    CellAesthetic, TableCell, TableCellKind, TableColumn, TableColumnRole, TableRow, TableRowRole,
    TableScaledAesthetic,
};
#[cfg(feature = "document-read")]
use crate::plot::theme::ThemePart;
#[cfg(feature = "document-read")]
use crate::plot::Table;

impl_codec! {
    record TableColumn { width, role }
    record TableRow { role }

    enum TableColumnRole {
        0 => Body,
        1 => Stub,
    }
    record TableCell { text, row, col, row_span, col_span, kind, align, aesthetics }

    enum TableScaledAesthetic {
        0 => Fill,
        1 => BorderColor,
        2 => BorderWidth,
        3 => BorderLinetype,
        4 => CornerRadius,
        5 => Color,
        6 => TextSize,
        7 => TextOutlineColor,
        8 => TextOutlineWidth,
        9 => FontFamily,
        10 => FontWeight,
        11 => FontWidth,
        12 => FontStyle,
        13 => LineHeight,
        14 => Tracking,
        15 => Underline,
        16 => Strikethrough,
        17 => Align,
        18 => BorderWidthTop,
        19 => BorderWidthRight,
        20 => BorderWidthBottom,
        21 => BorderWidthLeft,
        22 => Valign,
    }

    enum CellAesthetic {
        0 => Raw(value),
        1 => Scaled(value, name),
    }

    enum TableRowRole {
        0 => Body,
        1 => Header,
        2 => Footer,
    }

    enum TableCellKind {
        0 => Title,
        1 => Subtitle,
        2 => Caption,
        3 => Spanner,
        4 => ColumnLabel,
        5 => StubHead,
        6 => Stub { row },
        7 => Body { row },
        8 => RowGroup,
        9 => Summary { row },
        10 => Footnote,
    }
}

#[cfg(feature = "document-write")]
impl Encode for crate::plot::Table {
    fn encode(&self, w: &mut Writer) {
        write_record(w, |w| {
            self.cells_ref().encode(w);
            self.columns().encode(w);
            self.rows().encode(w);
            self.theme_override_ref().cloned().encode(w);
            self.theme_variant_ref().map(str::to_string).encode(w);
        });
    }
}

/// Decode a table's body and rebuild it bound to `patch`, validating
/// against the bare composition's patch ids the way
/// [`Table::try_new`](crate::plot::Table::try_new) would. The patch id
/// travels as the map key in the `TABL` chunk body, so the caller
/// threads it in.
#[cfg(feature = "document-read")]
pub(crate) fn decode_table_for(
    r: &mut Reader<'_>,
    bare: &crate::composition::Composition,
    patch: String,
) -> Result<Table, DocumentError> {
    let (cells, columns, rows, theme_override, theme_variant) = read_record(r, "Table", |r| {
        Ok((
            Vec::<TableCell>::decode(r)?,
            Vec::<TableColumn>::decode(r)?,
            Vec::<TableRow>::decode(r)?,
            Option::<ThemePart>::decode(r)?,
            Option::<String>::decode(r)?,
        ))
    })?;
    Table::from_cells(
        bare,
        patch.clone(),
        cells,
        columns,
        rows,
        theme_override,
        theme_variant,
    )
    .map_err(|e| DocumentError::Invalid {
        what: "table",
        why: e.to_string(),
    })
}
