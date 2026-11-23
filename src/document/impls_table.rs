//! Codec for tables and their parts.
//!
//! `TableColumn` and `TableSpanner` are plain data with public fields,
//! so they go through [`impl_codec`]. `Table` itself is read through
//! its accessors and rebuilt through [`Table::from_parts`], so a
//! document with a row that doesn't match the column count is refused
//! by the same validation the builder applies.

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{read_record, Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{write_record, Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;
use crate::plot::table::{TableColumn, TableRowGroup, TableSpanner};
use crate::plot::theme::TableCellTheme;
#[cfg(feature = "document-read")]
use crate::plot::theme::ThemePart;
#[cfg(feature = "document-read")]
use crate::plot::Table;

impl_codec! {
    record TableColumn { name, label, align, width, stub }
    record TableSpanner { label, level, columns }
    record TableRowGroup { label, start }
}

#[cfg(feature = "document-write")]
impl Encode for crate::plot::Table {
    fn encode(&self, w: &mut Writer) {
        write_record(w, |w| {
            self.columns().encode(w);
            self.spanners().encode(w);
            self.rows().encode(w);
            self.title_ref().map(str::to_string).encode(w);
            self.subtitle_ref().map(str::to_string).encode(w);
            self.caption_ref().map(str::to_string).encode(w);
            self.theme_override_ref().cloned().encode(w);
            self.theme_variant_ref().map(str::to_string).encode(w);
            // Overrides travel as sorted vectors, so one document maps
            // to one byte stream regardless of hash iteration order.
            let mut row_styles: Vec<(usize, TableCellTheme)> = self
                .row_styles()
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect();
            row_styles.sort_unstable_by_key(|(k, _)| *k);
            row_styles.encode(w);
            let mut cell_styles: Vec<((usize, String), TableCellTheme)> = self
                .cell_styles()
                .iter()
                .map(|(k, v)| ((k.0, k.1.clone()), v.clone()))
                .collect();
            cell_styles.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
            cell_styles.encode(w);
            self.row_groups().encode(w);
            self.summary_rows().encode(w);
            self.footnotes().encode(w);
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
    let (
        columns,
        spanners,
        rows,
        title,
        subtitle,
        caption,
        theme_override,
        theme_variant,
        row_styles,
        cell_styles,
        row_groups,
        summary_rows,
        footnotes,
    ) = read_record(r, "Table", |r| {
        Ok((
            Vec::<TableColumn>::decode(r)?,
            Vec::<TableSpanner>::decode(r)?,
            Vec::<Vec<String>>::decode(r)?,
            Option::<String>::decode(r)?,
            Option::<String>::decode(r)?,
            Option::<String>::decode(r)?,
            Option::<ThemePart>::decode(r)?,
            Option::<String>::decode(r)?,
            Vec::<(usize, TableCellTheme)>::decode(r)?,
            Vec::<((usize, String), TableCellTheme)>::decode(r)?,
            Vec::<TableRowGroup>::decode(r)?,
            Vec::<Vec<String>>::decode(r)?,
            Vec::<String>::decode(r)?,
        ))
    })?;
    Table::from_parts(
        bare,
        patch.clone(),
        columns,
        spanners,
        rows,
        row_groups,
        summary_rows,
        footnotes,
        title,
        subtitle,
        caption,
        theme_override,
        theme_variant,
        row_styles.into_iter().collect(),
        cell_styles.into_iter().collect(),
    )
    .map_err(|e| DocumentError::Invalid {
        what: "table",
        why: e.to_string(),
    })
}
