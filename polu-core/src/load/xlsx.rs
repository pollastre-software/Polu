//! Excel loaders (xlsx, xlsm, xlsb, xls and ods). They read workbook bytes rather than open files,
//! so the same loaders work from the CLI, WASM and Python; `from_path` is a native shortcut.
//! Each table and layer is one [`Region`] of a workbook, laid out as described in [`crate::load`]:
//! a whole sheet, whose first used row is its header, or a named Excel table (.xlsx only), whose
//! header row is the table's. [`regions`] lists both with their column names.

use std::borrow::Cow;
use std::fmt;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use calamine::{Data, Range, Reader, Sheets, open_workbook_auto_from_rs};

use super::{LayerColumns, TableColumns, base_cost};
use crate::core::BoxError;
use crate::core::traits::Load;
use crate::core::types::{CostMap, Header, Layer, Op, Table};

/// The part of a workbook a loader reads: a whole sheet, or a named Excel table (a ListObject,
/// .xlsx only). A plain name is a sheet, so `XlsxTable::new(book, "sources", ..)` reads the
/// `sources` sheet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Region {
    Sheet(String),
    Table(String),
}

impl Region {
    pub fn name(&self) -> &str {
        match self {
            Self::Sheet(name) | Self::Table(name) => name,
        }
    }
}

impl From<&str> for Region {
    fn from(sheet: &str) -> Self {
        Self::Sheet(sheet.into())
    }
}

impl From<String> for Region {
    fn from(sheet: String) -> Self {
        Self::Sheet(sheet)
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sheet(name) => write!(f, "sheet `{name}`"),
            Self::Table(name) => write!(f, "table `{name}`"),
        }
    }
}

/// One sheet or named table of a workbook and its column names, for a user to pick from.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionInfo {
    pub region: Region,
    /// The sheet it's on; a sheet's own name for a sheet.
    pub sheet: String,
    /// The header row without trailing blanks, empty when the sheet is empty.
    pub columns: Vec<String>,
}

/// Every sheet in workbook order, each followed by the named tables on it.
pub fn regions(workbook: &[u8]) -> Result<Vec<RegionInfo>, BoxError> {
    let mut book = self::workbook(workbook)?;
    if let Sheets::Xlsx(xlsx) = &mut book {
        xlsx.load_tables()?;
    }
    let mut found = Vec::new();
    for sheet in book.sheet_names() {
        let mut columns: Vec<String> = (book.worksheet_range(&sheet).ok())
            .and_then(|range| {
                Some(
                    header_row(&range)
                        .ok()?
                        .into_iter()
                        .map(Cow::into_owned)
                        .collect(),
                )
            })
            .unwrap_or_default();
        // the first row runs as wide as the sheet's widest row
        while columns.last().is_some_and(String::is_empty) {
            columns.pop();
        }
        found.push(RegionInfo {
            region: Region::Sheet(sheet.clone()),
            sheet: sheet.clone(),
            columns,
        });
        if let Sheets::Xlsx(xlsx) = &mut book {
            let tables: Vec<String> = xlsx
                .table_names_in_sheet(&sheet)
                .into_iter()
                .cloned()
                .collect();
            for name in tables {
                let columns = xlsx.table_by_name(&name)?.columns().to_vec();
                found.push(RegionInfo {
                    region: Region::Table(name),
                    sheet: sheet.clone(),
                    columns,
                });
            }
        }
    }
    Ok(found)
}

/// Sources or sinks from one sheet or named table of a workbook, with the quantity column chosen
/// by name.
#[derive(Debug, Clone)]
pub struct XlsxTable {
    workbook: Arc<[u8]>,
    region: Region,
    quantity_column: String,
}

impl XlsxTable {
    /// `workbook` is the file's bytes; pass the same bytes to several loaders to read
    /// sources, sinks and costmap layers from one file.
    pub fn new(
        workbook: impl Into<Arc<[u8]>>,
        region: impl Into<Region>,
        quantity_column: impl Into<String>,
    ) -> Self {
        Self {
            workbook: workbook.into(),
            region: region.into(),
            quantity_column: quantity_column.into(),
        }
    }

    pub fn from_path(
        path: impl AsRef<Path>,
        region: impl Into<Region>,
        quantity_column: impl Into<String>,
    ) -> std::io::Result<Self> {
        Ok(Self::new(std::fs::read(path)?, region, quantity_column))
    }

    /// The category columns, in slot order. Pass these to [`XlsxCostMap::new`].
    pub fn header(&self) -> Result<Header, BoxError> {
        let range = read(&mut workbook(&self.workbook)?, &self.region)?;
        Ok(self.columns(&range)?.header)
    }

    /// Every column name, the quantity column included, e.g. to let a user pick the quantity column.
    pub fn column_names(&self) -> Result<Vec<String>, BoxError> {
        let range = read(&mut workbook(&self.workbook)?, &self.region)?;
        let header = header_row(&range).map_err(|e| format!("{}: {e}", self.region))?;
        Ok(header.into_iter().map(Cow::into_owned).collect())
    }

    fn columns(&self, range: &Range<Data>) -> Result<TableColumns, BoxError> {
        let header = header_row(range).map_err(|e| format!("{}: {e}", self.region))?;
        TableColumns::new(&header, &self.quantity_column)
            .map_err(|e| format!("{}: {e}", self.region).into())
    }
}

impl Load<Table> for XlsxTable {
    fn load(&self) -> Result<Table, BoxError> {
        let range = read(&mut workbook(&self.workbook)?, &self.region)?;
        let columns = self.columns(&range)?;
        let mut rows = Vec::new();
        for (row, cells) in body(&range) {
            let at = |e| format!("{} row {row}: {e}", self.region);
            rows.push(columns.row(&texts(cells).map_err(at)?).map_err(at)?);
        }
        Ok(Table::new(columns.header, rows)?)
    }
}

/// A layered costmap from one workbook: an optional base matrix, then one sheet or named table per
/// layer, applied in order.
#[derive(Debug, Clone)]
pub struct XlsxCostMap {
    workbook: Arc<[u8]>,
    source_header: Header,
    sink_header: Header,
    base: Option<Region>,
    layers: Vec<(Op, Region)>,
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

    /// The base matrix: a headerless sheet, or a named table whose header row is skipped (e.g. to
    /// label the sinks). Excel trims empty trailing columns of a sheet, so write `nan` in the last
    /// sink's column when none of its pairs can be used.
    pub fn base(mut self, region: impl Into<Region>) -> Self {
        self.base = Some(region.into());
        self
    }

    pub fn layer(mut self, op: Op, region: impl Into<Region>) -> Self {
        self.layers.push((op, region.into()));
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
        for (l, (op, region)) in self.layers.iter().enumerate() {
            let range = read(&mut book, region)?;
            let layer = self
                .parse_layer(*op, &range)
                .map_err(|e| format!("costmap layer {l} ({region}): {e}"))?;
            layers.push(layer);
        }
        let costmap = CostMap::new(self.source_header.clone(), self.sink_header.clone(), layers)?;
        let Some(region) = &self.base else {
            return Ok(costmap);
        };
        let range = read(&mut book, region)?;
        // a table's header row labels the matrix; a sheet's matrix has none
        let skip = matches!(region, Region::Table(_)) as usize;
        let first = range.start().map_or(0, |(row, _)| row as usize + 1);
        let mut costs = Vec::with_capacity(range.width() * range.height());
        for (r, cells) in range.rows().enumerate().skip(skip) {
            for cell in cells {
                let cost = match cell {
                    Data::Float(v) => Ok(*v),
                    Data::Int(v) => Ok(*v as f64),
                    Data::Empty => Ok(f64::NAN),
                    Data::String(s) => base_cost(s.trim()),
                    other => Err(format!("`{other}` is not a cost").into()),
                };
                costs.push(cost.map_err(|e| format!("base {region} row {}: {e}", first + r))?);
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

/// A region's cells, header row first.
fn read(book: &mut Workbook, region: &Region) -> Result<Range<Data>, BoxError> {
    match region {
        Region::Sheet(name) => sheet(book, name),
        Region::Table(name) => table(book, name),
    }
}

fn sheet(book: &mut Workbook, name: &str) -> Result<Range<Data>, BoxError> {
    if !book.sheet_names().iter().any(|s| s == name) {
        return Err(format!("no `{name}` sheet").into());
    }
    Ok(book
        .worksheet_range(name)
        .map_err(|e| format!("sheet `{name}`: {e}"))?)
}

/// A named table as a range: its column names on the row above its data, every cell at its
/// place on the sheet, so errors name the sheet's own row numbers. A totals row is left out.
fn table(book: &mut Workbook, name: &str) -> Result<Range<Data>, BoxError> {
    let Sheets::Xlsx(xlsx) = book else {
        return Err(
            format!("no `{name}` table: named tables are read from .xlsx files only").into(),
        );
    };
    xlsx.load_tables()?;
    if !xlsx.table_names().iter().any(|t| *t == name) {
        return Err(format!("no `{name}` table").into());
    }
    let table = xlsx
        .table_by_name(name)
        .map_err(|e| format!("table `{name}`: {e}"))?;
    let (columns, data) = (table.columns(), table.data());
    // a table with no data rows has no position; its rows then count from the top
    let (row, col) = data.start().unwrap_or((1, 0));
    let header = row.saturating_sub(1);
    let width = columns.len().max(1) as u32;
    let mut range = Range::new(
        (header, col),
        (header + data.height() as u32, col + width - 1),
    );
    for (c, column) in columns.iter().enumerate() {
        range.set_value((header, col + c as u32), Data::String(column.clone()));
    }
    for (r, cells) in data.rows().enumerate() {
        for (c, cell) in cells.iter().take(width as usize).enumerate() {
            range.set_value((header + 1 + r as u32, col + c as u32), cell.clone());
        }
    }
    Ok(range)
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

    /// Writes `rows` (header first) at `(row, col)` and makes them the named table `name`.
    fn put_table(
        sheet: &mut rust_xlsxwriter::Worksheet,
        name: &str,
        (row, col): (u32, u16),
        rows: &[&[&str]],
    ) {
        use rust_xlsxwriter::{Table as Listed, TableColumn};
        for (r, cells) in rows.iter().enumerate() {
            for (c, cell) in cells.iter().enumerate() {
                let (r, c) = (row + r as u32, col + c as u16);
                match cell.parse::<f64>() {
                    _ if cell.is_empty() => continue,
                    Ok(v) => sheet.write_number(r, c, v).unwrap(),
                    Err(_) => sheet.write_string(r, c, *cell).unwrap(),
                };
            }
        }
        let columns: Vec<_> = rows[0]
            .iter()
            .map(|h| TableColumn::new().set_header(*h))
            .collect();
        let table = Listed::new().set_name(name).set_columns(&columns);
        let (last_row, last_col) = (row + rows.len() as u32 - 1, col + rows[0].len() as u16 - 1);
        sheet
            .add_table(row, col, last_row, last_col, &table)
            .unwrap();
    }

    /// The apples example with sources, the set layer and a base as named tables: `Sources` and
    /// `Base` on a `data` sheet under a title, `Costs` at the top of `layers`, sinks on a sheet.
    fn tables() -> Vec<u8> {
        let mut book = Workbook::new();
        let data = book.add_worksheet();
        data.set_name("data").unwrap();
        data.write_string(0, 0, "Fruit network").unwrap();
        put_table(
            data,
            "Sources",
            (2, 2),
            &[
                &["business", "size", "capacity"],
                &["apple inc", "", "5"],
                &["apple inc", "large", "3"],
                &["apple inc", "small", "7"],
                &["bananas and more", "", "20"],
            ],
        );
        put_table(
            data,
            "Base",
            (2, 7),
            &[
                &["stores and us", "ohio fruits"],
                &["1", "2"],
                &["3", "nan"],
                &["5", "6"],
                &["7", "8"],
            ],
        );
        let sinks = book.add_worksheet();
        sinks.set_name("sinks").unwrap();
        for (r, row) in [
            ["store", "state", "demand"],
            ["stores and us", "WA", "3"],
            ["ohio fruits", "OH", "6"],
        ]
        .iter()
        .enumerate()
        {
            for (c, cell) in row.iter().enumerate() {
                match cell.parse::<f64>() {
                    Ok(v) => sinks.write_number(r as u32, c as u16, v).unwrap(),
                    Err(_) => sinks.write_string(r as u32, c as u16, *cell).unwrap(),
                };
            }
        }
        let layers = book.add_worksheet();
        layers.set_name("layers").unwrap();
        put_table(
            layers,
            "Costs",
            (0, 0),
            &[
                &["source.business", "sink.store", "value"],
                &["apple inc", "stores and us", "2"],
                &["apple inc", "ohio fruits", "1"],
                &["bananas and more", "stores and us", "4"],
                &["bananas and more", "ohio fruits", "3"],
            ],
        );
        book.save_to_buffer().unwrap()
    }

    fn table(name: &str) -> Region {
        Region::Table(name.into())
    }

    #[test]
    fn named_tables_read_like_sheets() {
        let sources = XlsxTable::new(tables(), table("Sources"), "capacity");
        let loaded = sources.load().unwrap();
        // the title above the table and the Base table beside it are left out
        assert_eq!(names(loaded.header()), ["business", "size"]);
        assert_eq!(loaded.rows().len(), 4);
        assert_eq!(loaded.rows()[3].0.get_slot(0), Some("bananas and more"));
        assert_eq!(loaded.rows()[3].1, 20);
        assert_eq!(
            sources.column_names().unwrap(),
            ["business", "size", "capacity"]
        );
        // a plain name is still a sheet
        let sinks = XlsxTable::new(tables(), "sinks", "demand").load().unwrap();
        assert_eq!(sinks.rows().len(), 2);
    }

    #[test]
    fn regions_list_each_sheet_then_its_tables() {
        let found = regions(&tables()).unwrap();
        let listed: Vec<(String, &str, usize)> = found
            .iter()
            .map(|r| (r.region.to_string(), r.sheet.as_str(), r.columns.len()))
            .collect();
        assert_eq!(
            listed,
            [
                ("sheet `data`".into(), "data", 1),
                ("table `Sources`".into(), "data", 3),
                ("table `Base`".into(), "data", 2),
                ("sheet `sinks`".into(), "sinks", 3),
                ("sheet `layers`".into(), "layers", 3),
                ("table `Costs`".into(), "layers", 3),
            ]
        );
        assert_eq!(found[0].columns, ["Fruit network"]);
        assert_eq!(found[2].columns, ["stores and us", "ohio fruits"]);
    }

    #[test]
    fn table_errors_name_the_table_and_its_sheet_row() {
        let missing = XlsxTable::new(tables(), table("Stock"), "capacity")
            .load()
            .unwrap_err();
        assert!(
            missing.to_string().contains("no `Stock` table"),
            "{missing}"
        );
        let column = XlsxTable::new(tables(), table("Sources"), "demand")
            .load()
            .unwrap_err();
        assert!(
            column
                .to_string()
                .contains("table `Sources`: no `demand` column"),
            "{column}"
        );
        let mut book = Workbook::new();
        let sheet = book.add_worksheet();
        put_table(
            sheet,
            "T",
            (3, 1),
            &[&["business", "capacity"], &["a", "1"], &["b", "lots"]],
        );
        let bad = XlsxTable::new(book.save_to_buffer().unwrap(), table("T"), "capacity")
            .load()
            .unwrap_err();
        // header on Excel row 4, so `lots` is on row 6
        assert!(bad.to_string().contains("table `T` row 6"), "{bad}");
    }

    #[test]
    fn layers_and_base_read_from_tables() {
        let book: Arc<[u8]> = tables().into();
        let sources = XlsxTable::new(book.clone(), table("Sources"), "capacity");
        let sinks = XlsxTable::new(book.clone(), "sinks", "demand");
        let cm = XlsxCostMap::new(book, sources.header().unwrap(), sinks.header().unwrap())
            .layer(Op::Set, table("Costs"))
            .base(table("Base"))
            .load()
            .unwrap();
        assert_eq!(cm.layers()[0].rules.len(), 4);
        assert_eq!(cm.layers()[0].rules[3].value, 3.0);
        // the base table's header row names the sinks and is skipped
        let base = cm.base().unwrap();
        assert_eq!(base.len(), 8);
        assert_eq!((base[0], base[2], base[7]), (1.0, 3.0, 8.0));
        assert!(base[3].is_nan());
    }

    /// The CSV end-to-end example, read from one workbook and written back to another.
    #[cfg(feature = "highs")]
    #[test]
    fn xlsx_pipeline_end_to_end() {
        use calamine::DataType;

        use crate::store::xlsx::XlsxStore;
        use crate::{HighsSolver, Polu, Store};

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

        // slack goes to its own file, on a `slack` sheet: bananas and more ships 4 of 20
        let store = XlsxStore::new(&path);
        let (sources, _) = polu.tables().unwrap();
        store.store_slack(&polu.slack().unwrap(), sources).unwrap();
        let mut written = calamine::open_workbook_auto(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let slack = written.worksheet_range("slack").unwrap();
        let last: Vec<String> = slack
            .rows()
            .next_back()
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(last, ["bananas and more", "", "4", "20", "16", "16"]);
    }
}
