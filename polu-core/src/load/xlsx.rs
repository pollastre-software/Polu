//! Excel loaders (xlsx, xlsm, xlsb, xls and ods). They read workbook bytes rather than open files,
//! so the same loaders work from the CLI, WASM and Python; `from_path` is a native shortcut.
//! Each table and layer is one sheet, laid out as described in [`crate::load`]; a sheet's first
//! used row is its header.

use std::borrow::Cow;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use calamine::{Data, Range, Reader, Sheets, open_workbook_auto_from_rs};

use super::{LayerColumns, TableColumns, base_cost};
use crate::core::BoxError;
use crate::core::traits::Load;
use crate::core::types::{CostMap, Header, Layer, Op, Table};

/// Sources or sinks from one sheet of a workbook, with the quantity column chosen by name.
#[derive(Debug, Clone)]
pub struct XlsxTable {
    workbook: Arc<[u8]>,
    sheet: String,
    quantity_column: String,
}

impl XlsxTable {
    /// `workbook` is the file's bytes; pass the same bytes to several loaders to read
    /// sources, sinks and costmap layers from one file.
    pub fn new(
        workbook: impl Into<Arc<[u8]>>,
        sheet: impl Into<String>,
        quantity_column: impl Into<String>,
    ) -> Self {
        Self {
            workbook: workbook.into(),
            sheet: sheet.into(),
            quantity_column: quantity_column.into(),
        }
    }

    pub fn from_path(
        path: impl AsRef<Path>,
        sheet: impl Into<String>,
        quantity_column: impl Into<String>,
    ) -> std::io::Result<Self> {
        Ok(Self::new(std::fs::read(path)?, sheet, quantity_column))
    }

    /// The category columns, in slot order. Pass these to [`XlsxCostMap::new`].
    pub fn header(&self) -> Result<Header, BoxError> {
        let range = sheet(&mut workbook(&self.workbook)?, &self.sheet)?;
        Ok(self.columns(&range)?.header)
    }

    /// Every column name, the quantity column included, e.g. to let a user pick the quantity column.
    pub fn column_names(&self) -> Result<Vec<String>, BoxError> {
        let range = sheet(&mut workbook(&self.workbook)?, &self.sheet)?;
        let header = header_row(&range).map_err(|e| format!("sheet `{}`: {e}", self.sheet))?;
        Ok(header.into_iter().map(Cow::into_owned).collect())
    }

    fn columns(&self, range: &Range<Data>) -> Result<TableColumns, BoxError> {
        let header = header_row(range).map_err(|e| format!("sheet `{}`: {e}", self.sheet))?;
        TableColumns::new(&header, &self.quantity_column)
            .map_err(|e| format!("sheet `{}`: {e}", self.sheet).into())
    }
}

impl Load<Table> for XlsxTable {
    fn load(&self) -> Result<Table, BoxError> {
        let range = sheet(&mut workbook(&self.workbook)?, &self.sheet)?;
        let columns = self.columns(&range)?;
        let mut rows = Vec::new();
        for (row, cells) in body(&range) {
            let at = |e| format!("sheet `{}` row {row}: {e}", self.sheet);
            rows.push(columns.row(&texts(cells).map_err(at)?).map_err(at)?);
        }
        Ok(Table::new(columns.header, rows)?)
    }
}

/// A layered costmap from one workbook: an optional base matrix sheet, then one sheet per layer,
/// applied in order.
#[derive(Debug, Clone)]
pub struct XlsxCostMap {
    workbook: Arc<[u8]>,
    source_header: Header,
    sink_header: Header,
    base: Option<String>,
    layers: Vec<(Op, String)>,
}

impl XlsxCostMap {
    pub fn new(workbook: impl Into<Arc<[u8]>>, source_header: Header, sink_header: Header) -> Self {
        Self {
            workbook: workbook.into(),
            source_header,
            sink_header,
            base: None,
            layers: Vec::new(),
        }
    }

    pub fn from_path(
        path: impl AsRef<Path>,
        source_header: Header,
        sink_header: Header,
    ) -> std::io::Result<Self> {
        Ok(Self::new(std::fs::read(path)?, source_header, sink_header))
    }

    /// The base matrix sheet. Excel trims empty trailing columns, so write `nan` in the last
    /// sink's column when none of its pairs can be used.
    pub fn base(mut self, sheet: impl Into<String>) -> Self {
        self.base = Some(sheet.into());
        self
    }

    pub fn layer(mut self, op: Op, sheet: impl Into<String>) -> Self {
        self.layers.push((op, sheet.into()));
        self
    }

    fn parse_layer(&self, op: Op, range: &Range<Data>) -> Result<Layer, BoxError> {
        let header = header_row(range)?;
        let columns = LayerColumns::new(op, &header, &self.source_header, &self.sink_header)?;
        let mut rules = Vec::new();
        for (row, cells) in body(range) {
            let at = |e| format!("row {row}: {e}");
            rules.push(columns.rule(&texts(cells).map_err(at)?).map_err(at)?);
        }
        Ok(Layer { op, rules })
    }
}

impl Load<CostMap> for XlsxCostMap {
    fn load(&self) -> Result<CostMap, BoxError> {
        let mut book = workbook(&self.workbook)?;
        let mut layers = Vec::new();
        for (l, (op, name)) in self.layers.iter().enumerate() {
            let range = sheet(&mut book, name)?;
            let layer = self
                .parse_layer(*op, &range)
                .map_err(|e| format!("costmap layer {l} (sheet `{name}`): {e}"))?;
            layers.push(layer);
        }
        let costmap = CostMap::new(self.source_header.clone(), self.sink_header.clone(), layers)?;
        let Some(name) = &self.base else {
            return Ok(costmap);
        };
        let range = sheet(&mut book, name)?;
        let first = range.start().map_or(0, |(row, _)| row as usize + 1);
        let mut costs = Vec::with_capacity(range.width() * range.height());
        for (r, cells) in range.rows().enumerate() {
            for cell in cells {
                let cost = match cell {
                    Data::Float(v) => Ok(*v),
                    Data::Int(v) => Ok(*v as f64),
                    Data::Empty => Ok(f64::NAN),
                    Data::String(s) => base_cost(s.trim()),
                    other => Err(format!("`{other}` is not a cost").into()),
                };
                costs
                    .push(cost.map_err(|e| format!("base sheet `{name}` row {}: {e}", first + r))?);
            }
        }
        Ok(costmap.with_base(costs)?)
    }
}

/// The sheet names of a workbook, in workbook order.
pub fn sheet_names(workbook: &[u8]) -> Result<Vec<String>, BoxError> {
    Ok(self::workbook(workbook)?.sheet_names())
}

type Workbook<'a> = Sheets<Cursor<&'a [u8]>>;

fn workbook(bytes: &[u8]) -> Result<Workbook<'_>, BoxError> {
    Ok(open_workbook_auto_from_rs(Cursor::new(bytes))?)
}

fn sheet(book: &mut Workbook, name: &str) -> Result<Range<Data>, BoxError> {
    if !book.sheet_names().iter().any(|s| s == name) {
        return Err(format!("no `{name}` sheet").into());
    }
    Ok(book
        .worksheet_range(name)
        .map_err(|e| format!("sheet `{name}`: {e}"))?)
}

fn header_row(range: &Range<Data>) -> Result<Vec<Cow<'_, str>>, BoxError> {
    texts(range.rows().next().ok_or("the sheet is empty")?)
}

/// The rows after the header with their 1-based Excel row numbers, skipping blank rows.
fn body(range: &Range<Data>) -> impl Iterator<Item = (usize, &[Data])> {
    let first = range.start().map_or(0, |(row, _)| row as usize + 1);
    range
        .rows()
        .enumerate()
        .skip(1)
        .map(move |(r, cells)| (first + r, cells))
        .filter(|(_, cells)| cells.iter().any(|c| *c != Data::Empty))
}

/// Cell text as the column rules read it: trimmed strings, numbers as written (`5.0` is `5`),
/// empty cells as empty.
fn texts(cells: &[Data]) -> Result<Vec<Cow<'_, str>>, BoxError> {
    cells
        .iter()
        .map(|cell| match cell {
            Data::String(s) => Ok(Cow::Borrowed(s.trim())),
            Data::Empty => Ok(Cow::Borrowed("")),
            Data::Error(e) => Err(format!("cell error `{e}`").into()),
            other => Ok(Cow::Owned(other.to_string())),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rust_xlsxwriter::Workbook;

    use super::*;

    /// A workbook with one sheet per `(name, rows)`; cells that parse as numbers are written as
    /// numbers, empty cells are left blank.
    fn book(sheets: &[(&str, &[&[&str]])]) -> Vec<u8> {
        let mut book = Workbook::new();
        for (name, rows) in sheets {
            let sheet = book.add_worksheet();
            sheet.set_name(*name).unwrap();
            for (r, row) in rows.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    let (r, c) = (r as u32, c as u16);
                    match cell.parse::<f64>() {
                        _ if cell.is_empty() => continue,
                        Ok(v) => sheet.write_number(r, c, v).unwrap(),
                        Err(_) => sheet.write_string(r, c, *cell).unwrap(),
                    };
                }
            }
        }
        book.save_to_buffer().unwrap()
    }

    fn apples() -> Vec<u8> {
        book(&[
            (
                "sources",
                &[
                    &["business", "size", "capacity"],
                    &["apple inc", "", "5"],
                    &["apple inc", "large", "3"],
                    &["", "", ""],
                    &["apple inc", "small", "7"],
                    &["bananas and more", "", "20"],
                ],
            ),
            (
                "sinks",
                &[
                    &["store", "state", "demand"],
                    &["stores and us", "WA", "3"],
                    &["ohio fruits", "OH", "6"],
                ],
            ),
            (
                "costs",
                &[
                    &["source.business", "sink.store", "value"],
                    &["apple inc", "stores and us", "2"],
                    &["apple inc", "ohio fruits", "1"],
                    &["bananas and more", "stores and us", "4"],
                    &["bananas and more", "ohio fruits", "3"],
                ],
            ),
            ("scale", &[&["source.size", "value"], &["small", "2"]]),
            (
                "block",
                &[
                    &["source.business", "sink.store"],
                    &["apple inc", "stores and us"],
                ],
            ),
            (
                "base",
                &[
                    &["1", "2"],
                    &["3", ""],
                    &["nan", "4.5"],
                    &["5", "6"],
                    &["7", "8"],
                ],
            ),
        ])
    }

    fn names(header: &Header) -> Vec<&str> {
        header.iter().flatten().map(|h| &**h).collect()
    }

    #[test]
    fn table_reads_sheet_and_skips_blank_rows() {
        let table = XlsxTable::new(apples(), "sources", "capacity")
            .load()
            .unwrap();
        assert_eq!(names(table.header()), ["business", "size"]);
        assert_eq!(table.rows().len(), 4);
        let all = XlsxTable::new(apples(), "sinks", "")
            .column_names()
            .unwrap();
        assert_eq!(all, ["store", "state", "demand"]);
        assert_eq!(sheet_names(&apples()).unwrap()[..2], ["sources", "sinks"]);
        let (key, amount) = &table.rows()[0];
        assert_eq!(
            (key.get_slot(0), key.get_slot(1), *amount),
            (Some("apple inc"), None, 5)
        );
        assert_eq!(table.rows()[2].0.get_slot(1), Some("small"));
    }

    #[test]
    fn errors_name_the_sheet_and_row() {
        let missing = XlsxTable::new(apples(), "stock", "capacity")
            .load()
            .unwrap_err();
        assert!(missing.to_string().contains("no `stock` sheet"));
        let column = XlsxTable::new(apples(), "sinks", "capacity")
            .load()
            .unwrap_err();
        assert!(
            column
                .to_string()
                .contains("sheet `sinks`: no `capacity` column")
        );
        let bad = book(&[("t", &[&["business", "capacity"], &["a", "lots"]])]);
        let bad = XlsxTable::new(bad, "t", "capacity").load().unwrap_err();
        assert!(bad.to_string().contains("sheet `t` row 2"), "{bad}");
        let fractional = book(&[("t", &[&["business", "capacity"], &["a", "1.5"]])]);
        assert!(XlsxTable::new(fractional, "t", "capacity").load().is_err());
    }

    fn costmap() -> XlsxCostMap {
        let book: Arc<[u8]> = apples().into();
        let sources = XlsxTable::new(book.clone(), "sources", "capacity");
        let sinks = XlsxTable::new(book.clone(), "sinks", "demand");
        XlsxCostMap::new(book, sources.header().unwrap(), sinks.header().unwrap())
    }

    #[test]
    fn layers_and_base_read_from_sheets() {
        let cm = costmap()
            .layer(Op::Set, "costs")
            .layer(Op::Block, "block")
            .base("base")
            .load()
            .unwrap();
        let set = &cm.layers()[0];
        assert_eq!(set.rules.len(), 4);
        assert_eq!(set.rules[1].sink.get_slot(0), Some("ohio fruits"));
        assert_eq!(set.rules[1].value, 1.0);
        assert_eq!(cm.layers()[1].op, Op::Block);
        let base = cm.base().unwrap();
        assert_eq!(base.len(), 10);
        assert_eq!((base[0], base[5], base[9]), (1.0, 4.5, 8.0));
        assert!(base[3].is_nan() && base[4].is_nan());

        let missing = costmap().layer(Op::Scale, "nope").load().unwrap_err();
        assert!(missing.to_string().contains("no `nope` sheet"));
        let wrong = costmap().layer(Op::Scale, "block").load().unwrap_err();
        assert!(
            wrong
                .to_string()
                .contains("costmap layer 0 (sheet `block`)")
        );
    }

    /// The CSV end-to-end example, read from one workbook and written back to another.
    #[cfg(feature = "highs")]
    #[test]
    fn xlsx_pipeline_end_to_end() {
        use calamine::DataType;

        use crate::store::xlsx::XlsxStore;
        use crate::{HighsSolver, Polu};

        let book: Arc<[u8]> = apples().into();
        let path = std::env::temp_dir().join(format!("polu-xlsx-test-{}.xlsx", std::process::id()));
        let mut polu = Polu::new(
            Box::new(XlsxTable::new(book.clone(), "sources", "capacity")),
            Box::new(XlsxTable::new(book.clone(), "sinks", "demand")),
            Box::new(
                costmap()
                    .layer(Op::Set, "costs")
                    .layer(Op::Scale, "scale")
                    .layer(Op::Block, "block"),
            ),
            Box::new(HighsSolver::default()),
            Box::new(XlsxStore::new(&path)),
        );
        polu.load_data().unwrap();
        let assignment = polu.run().unwrap();
        assert_eq!(assignment.objective, 3.0 * 4.0 + 5.0 * 1.0 + 1.0 * 3.0);
        polu.store_data().unwrap();

        let mut written = calamine::open_workbook_auto(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let flows = written.worksheet_range("flows").unwrap();
        let header: Vec<String> = flows
            .rows()
            .next()
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(
            header,
            [
                "source.business",
                "source.size",
                "sink.store",
                "sink.state",
                "units"
            ]
        );
        let units: f64 = flows.rows().skip(1).map(|r| r[4].as_f64().unwrap()).sum();
        assert_eq!(units, 9.0);
    }
}
