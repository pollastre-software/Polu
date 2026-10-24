//! Loaders, one module per input format, each behind the cargo feature of the same name
//! (`csv` is on by default). Every format reads the same shapes, a header row then rows of cells,
//! so the column rules live here and a format module only turns its input into cells.
//!
//! Sources and sinks: one quantity column plus up to 8 category columns, in header order.
//! An empty category cell means the row doesn't specify it (it matches any value in the costmap,
//! and a row that leaves fields empty covers, and caps, the more specific rows under it).
//!
//! | business  | size  | capacity |
//! |-----------|-------|----------|
//! | apple inc |       | 5        |
//! | apple inc | large | 3        |
//!
//! Costmap layers: `source.<column>` and `sink.<column>` (names from the source and sink headers;
//! leave a column out, or a cell empty, to match any value), an optional `priority` (default 0)
//! and `value` (cost, factor or amount; not needed for `Block` layers).
//!
//! The base matrix has no header: one row per source and one cell per sink, in table row order.
//! An empty cell or `nan` means the pair can't be used.

#![cfg_attr(not(feature = "csv"), allow(dead_code))]

#[cfg(feature = "csv")]
pub mod csv;

use crate::core::BoxError;
use crate::core::types::{CostRule, Header, Key, Op, SLOTS};

/// Where a table's slots and quantity sit among its columns, read from its header row.
struct TableColumns {
    header: Header,
    slots: Vec<usize>,
    quantity: usize,
}

impl TableColumns {
    fn new<S: AsRef<str>>(columns: &[S], quantity_column: &str) -> Result<Self, BoxError> {
        let quantity = columns
            .iter()
            .position(|c| c.as_ref() == quantity_column)
            .ok_or_else(|| format!("no `{quantity_column}` column"))?;
        let slots: Vec<usize> = (0..columns.len()).filter(|&c| c != quantity).collect();
        if slots.len() > SLOTS {
            return Err(format!(
                "{} category columns, at most {SLOTS} are supported",
                slots.len()
            )
            .into());
        }
        let mut header: Header = Default::default();
        for (name, &c) in header.iter_mut().zip(&slots) {
            *name = Some(columns[c].as_ref().into());
        }
        Ok(Self {
            header,
            slots,
            quantity,
        })
    }

    fn row<S: AsRef<str>>(&self, cells: &[S]) -> Result<(Key, i64), BoxError> {
        let amount = cell(cells, self.quantity);
        let amount = amount
            .parse::<i64>()
            .map_err(|e| format!("`{amount}` is not an integer quantity: {e}"))?;
        Ok((key(self.slots.iter().map(|&c| cell(cells, c))), amount))
    }
}

/// Where a layer's rule fields sit among its columns, read from its header row.
struct LayerColumns {
    op: Op,
    /// `(slot, column)` pairs
    source: Vec<(usize, usize)>,
    sink: Vec<(usize, usize)>,
    priority: Option<usize>,
    value: Option<usize>,
}

impl LayerColumns {
    fn new<S: AsRef<str>>(
        op: Op,
        columns: &[S],
        source_header: &Header,
        sink_header: &Header,
    ) -> Result<Self, BoxError> {
        let mut layer = Self {
            op,
            source: Vec::new(),
            sink: Vec::new(),
            priority: None,
            value: None,
        };
        for (c, name) in columns.iter().map(AsRef::as_ref).enumerate() {
            match name {
                "priority" => layer.priority = Some(c),
                "value" => layer.value = Some(c),
                _ => {
                    let (field, header, cols) = match name.split_once('.') {
                        Some(("source", field)) => (field, source_header, &mut layer.source),
                        Some(("sink", field)) => (field, sink_header, &mut layer.sink),
                        _ => return Err(format!("unexpected column `{name}`").into()),
                    };
                    let slot = header
                        .iter()
                        .position(|h| h.as_deref() == Some(field))
                        .ok_or_else(|| format!("column `{name}` is not in that table's header"))?;
                    cols.push((slot, c));
                }
            }
        }
        if layer.value.is_none() && op != Op::Block {
            return Err("a `value` column is required".into());
        }
        Ok(layer)
    }

    fn rule<S: AsRef<str>>(&self, cells: &[S]) -> Result<CostRule, BoxError> {
        let filled = |c: Option<usize>| c.map(|c| cell(cells, c)).filter(|s| !s.is_empty());
        let value = match filled(self.value) {
            Some(s) => s.parse().map_err(|e| format!("`{s}`: {e}"))?,
            None if self.op == Op::Block => 0.0,
            None => return Err("missing value".into()),
        };
        let priority = match filled(self.priority) {
            Some(s) => s.parse().map_err(|e| format!("priority `{s}`: {e}"))?,
            None => 0,
        };
        Ok(CostRule {
            source: slotted(&self.source, cells),
            sink: slotted(&self.sink, cells),
            priority,
            value,
        })
    }
}

/// One base matrix cell; empty or `nan` means the pair can't be used.
fn base_cost(cell: &str) -> Result<f64, BoxError> {
    if cell.is_empty() || cell.eq_ignore_ascii_case("nan") {
        return Ok(f64::NAN);
    }
    Ok(cell.parse().map_err(|e| format!("`{cell}`: {e}"))?)
}

/// The cell at `column`, or empty when the row is shorter.
fn cell<S: AsRef<str>>(cells: &[S], column: usize) -> &str {
    cells.get(column).map_or("", AsRef::as_ref)
}

/// A key from cell values in slot order; empty cells leave the slot empty.
fn key<'a>(cells: impl Iterator<Item = &'a str>) -> Key {
    let mut fields: [Option<Box<str>>; SLOTS] = Default::default();
    for (slot, cell) in fields.iter_mut().zip(cells) {
        *slot = (!cell.is_empty()).then(|| cell.into());
    }
    Key::new(fields)
}

/// A key from `(slot, column)` pairs; slots without a column stay empty.
fn slotted<S: AsRef<str>>(columns: &[(usize, usize)], cells: &[S]) -> Key {
    let mut fields: [Option<Box<str>>; SLOTS] = Default::default();
    for &(slot, c) in columns {
        fields[slot] = Some(cell(cells, c))
            .filter(|s| !s.is_empty())
            .map(Into::into);
    }
    Key::new(fields)
}
