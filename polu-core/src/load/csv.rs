//! CSV loaders. They parse CSV text rather than open files, so the same loaders work from the
//! CLI, WASM and Python; `from_path` is a native shortcut. Column rules are in [`crate::load`].

use std::path::Path;

use ::csv::{Reader, ReaderBuilder, Trim};

use super::{LayerColumns, TableColumns, base_cost};
use crate::core::BoxError;
use crate::core::traits::Load;
use crate::core::types::{CostMap, Header, Layer, Op, Table};

/// Sources or sinks from CSV text, with the quantity column chosen by name.
///
/// ```csv
/// business,size,capacity
/// apple inc,,5
/// apple inc,large,3
/// ```
#[derive(Debug, Clone)]
pub struct CsvTable {
    text: String,
    quantity_column: String,
    delimiter: u8,
}

impl CsvTable {
    pub fn new(text: impl Into<String>, quantity_column: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            quantity_column: quantity_column.into(),
            delimiter: b',',
        }
    }

    pub fn from_path(
        path: impl AsRef<Path>,
        quantity_column: impl Into<String>,
    ) -> std::io::Result<Self> {
        Ok(Self::new(std::fs::read_to_string(path)?, quantity_column))
    }

    pub fn with_delimiter(mut self, delimiter: u8) -> Self {
        self.delimiter = delimiter;
        self
    }

    /// The category columns, in slot order. Pass these to [`CsvCostMap::new`].
    pub fn header(&self) -> Result<Header, BoxError> {
        Ok(self.columns()?.header)
    }

    fn columns(&self) -> Result<TableColumns, BoxError> {
        let columns = reader(&self.text, self.delimiter, true).headers()?.clone();
        TableColumns::new(&columns.iter().collect::<Vec<_>>(), &self.quantity_column)
    }
}

impl Load<Table> for CsvTable {
    fn load(&self) -> Result<Table, BoxError> {
        let columns = self.columns()?;
        let mut rows = Vec::new();
        for (line, record) in reader(&self.text, self.delimiter, true)
            .records()
            .enumerate()
        {
            let record = record?;
            let line = line + 2; // 1-based, after the header
            let cells: Vec<&str> = record.iter().collect();
            rows.push(
                columns
                    .row(&cells)
                    .map_err(|e| format!("line {line}: {e}"))?,
            );
        }
        Ok(Table::new(columns.header, rows)?)
    }
}

/// A layered costmap from CSV: an optional base matrix, then one CSV per layer, applied in order.
///
/// ```csv
/// source.business,sink.store,value
/// bananas and more,,2
/// ```
#[derive(Debug, Clone)]
pub struct CsvCostMap {
    source_header: Header,
    sink_header: Header,
    base: Option<String>,
    layers: Vec<(Op, String)>,
    delimiter: u8,
}

impl CsvCostMap {
    pub fn new(source_header: Header, sink_header: Header) -> Self {
        Self {
            source_header,
            sink_header,
            base: None,
            layers: Vec::new(),
            delimiter: b',',
        }
    }

    pub fn base(mut self, text: impl Into<String>) -> Self {
        self.base = Some(text.into());
        self
    }

    pub fn layer(mut self, op: Op, text: impl Into<String>) -> Self {
        self.layers.push((op, text.into()));
        self
    }

    pub fn with_delimiter(mut self, delimiter: u8) -> Self {
        self.delimiter = delimiter;
        self
    }

    fn parse_layer(&self, op: Op, text: &str) -> Result<Layer, BoxError> {
        let mut reader = reader(text, self.delimiter, true);
        let columns = reader.headers()?.clone();
        let columns = LayerColumns::new(
            op,
            &columns.iter().collect::<Vec<_>>(),
            &self.source_header,
            &self.sink_header,
        )?;
        let mut rules = Vec::new();
        for (line, record) in reader.records().enumerate() {
            let record = record?;
            let line = line + 2;
            let cells: Vec<&str> = record.iter().collect();
            rules.push(
                columns
                    .rule(&cells)
                    .map_err(|e| format!("line {line}: {e}"))?,
            );
        }
        Ok(Layer { op, rules })
    }
}

impl Load<CostMap> for CsvCostMap {
    fn load(&self) -> Result<CostMap, BoxError> {
        let layers = self
            .layers
            .iter()
            .enumerate()
            .map(|(l, (op, text))| {
                self.parse_layer(*op, text)
                    .map_err(|e| format!("costmap layer {l}: {e}").into())
            })
            .collect::<Result<Vec<_>, BoxError>>()?;
        let costmap = CostMap::new(self.source_header.clone(), self.sink_header.clone(), layers)?;
        let Some(base) = &self.base else {
            return Ok(costmap);
        };
        let mut costs = Vec::new();
        for (line, record) in reader(base, self.delimiter, false).records().enumerate() {
            for cell in record?.iter() {
                costs.push(base_cost(cell).map_err(|e| format!("base line {}: {e}", line + 1))?);
            }
        }
        Ok(costmap.with_base(costs)?)
    }
}

fn reader(text: &str, delimiter: u8, has_headers: bool) -> Reader<&[u8]> {
    ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(has_headers)
        .trim(Trim::All)
        .from_reader(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &str = "business,size,capacity
apple inc,,5
apple inc,large,3
apple inc,small,7
bananas and more,,20
";
    const SINKS: &str = "store,state,demand
stores and us,WA,3
ohio fruits,OH,6
";

    fn names(header: &Header) -> Vec<&str> {
        header.iter().flatten().map(|h| &**h).collect()
    }

    #[test]
    fn table_reads_categories_and_quantities() {
        let table = CsvTable::new(SOURCES, "capacity").load().unwrap();
        assert_eq!(names(table.header()), ["business", "size"]);
        let (key, amount) = &table.rows()[0];
        assert_eq!(
            (key.get_slot(0), key.get_slot(1), *amount),
            (Some("apple inc"), None, 5)
        );
        assert_eq!(table.rows()[2].0.get_slot(1), Some("small"));
    }

    #[test]
    fn quantity_column_can_sit_anywhere() {
        let table = CsvTable::new("demand;store\n4;ohio fruits\n", "demand")
            .with_delimiter(b';')
            .load()
            .unwrap();
        assert_eq!(names(table.header()), ["store"]);
        assert_eq!(table.rows()[0].1, 4);
    }

    #[test]
    fn table_errors_name_the_problem() {
        let missing = CsvTable::new(SOURCES, "stock").load().unwrap_err();
        assert!(missing.to_string().contains("no `stock` column"));
        let bad = CsvTable::new("business,capacity\napple inc,lots\n", "capacity")
            .load()
            .unwrap_err();
        assert!(bad.to_string().contains("line 2"));
        let wide = CsvTable::new("a,b,c,d,e,f,g,h,i,q\n1,2,3,4,5,6,7,8,9,1\n", "q")
            .load()
            .unwrap_err();
        assert!(wide.to_string().contains("at most 8"));
        let duplicate = CsvTable::new("business,capacity\na,1\na,2\n", "capacity").load();
        assert!(duplicate.is_err());
    }

    fn costmap() -> CsvCostMap {
        let sources = CsvTable::new(SOURCES, "capacity").header().unwrap();
        let sinks = CsvTable::new(SINKS, "demand").header().unwrap();
        CsvCostMap::new(sources, sinks)
    }

    #[test]
    fn layers_read_rules_with_wildcards() {
        let cm = costmap()
            .layer(
                Op::Set,
                "source.business,sink.store,value,priority\napple inc,,2,\n,ohio fruits,1,3\n",
            )
            .layer(Op::Block, "source.size\nsmall\n")
            .load()
            .unwrap();
        let set = &cm.layers()[0];
        assert_eq!(set.op, Op::Set);
        assert_eq!(set.rules[0].source.get_slot(0), Some("apple inc"));
        assert_eq!(set.rules[0].sink.specificity(), 0);
        assert_eq!((set.rules[1].priority, set.rules[1].value), (3, 1.0));
        assert_eq!(cm.layers()[1].rules[0].source.get_slot(1), Some("small"));
    }

    #[test]
    fn layer_errors_name_the_problem() {
        let unknown = costmap()
            .layer(Op::Set, "source.colour,value\nred,1\n")
            .load()
            .unwrap_err();
        assert!(unknown.to_string().contains("source.colour"));
        let stray = costmap()
            .layer(Op::Set, "business,value\na,1\n")
            .load()
            .unwrap_err();
        assert!(stray.to_string().contains("unexpected column"));
        let no_value = costmap()
            .layer(Op::Scale, "source.size\nsmall\n")
            .load()
            .unwrap_err();
        assert!(no_value.to_string().contains("`value`"));
    }

    #[test]
    fn base_matrix_reads_nan_and_empty_cells() {
        let cm = costmap().base("1,2\n3,\nnan,4.5\n5,6\n").load().unwrap();
        let base = cm.base().unwrap();
        assert_eq!(base.len(), 8);
        assert_eq!((base[0], base[5], base[7]), (1.0, 4.5, 6.0));
        assert!(base[3].is_nan() && base[4].is_nan());
    }

    /// CSV in, HiGHS, CSV out: the apples example with a business cap, a scale layer and a block.
    #[cfg(feature = "highs")]
    #[test]
    fn csv_pipeline_end_to_end() {
        use crate::store::csv::CsvStore;
        use crate::{HighsSolver, Polu};

        let base = "source.business,sink.store,value
apple inc,stores and us,2
apple inc,ohio fruits,1
bananas and more,stores and us,4
bananas and more,ohio fruits,3
";
        let path = std::env::temp_dir().join(format!("polu-csv-test-{}.csv", std::process::id()));
        let mut polu = Polu::new(
            Box::new(CsvTable::new(SOURCES, "capacity")),
            Box::new(CsvTable::new(SINKS, "demand")),
            Box::new(
                costmap()
                    .layer(Op::Set, base)
                    .layer(Op::Scale, "source.size,value\nsmall,2\n")
                    .layer(
                        Op::Block,
                        "source.business,sink.store\napple inc,stores and us\n",
                    ),
            ),
            Box::new(HighsSolver::default()),
            Box::new(CsvStore::new(&path)),
        );
        polu.load_data().unwrap();
        let assignment = polu.run().unwrap();
        // rows with no size also match the `small` scale rule (empty fields match any rule value),
        // so only [apple inc, large] keeps its base cost of 1; everything else is doubled.
        // WA (3): apple inc is blocked, bananas at 8. Ohio (6): apple inc is capped at 5 in total,
        // 3 large at 1 and 2 more at 2; the last unit from bananas at 6.
        assert_eq!(
            assignment.objective,
            3.0 * 8.0 + 3.0 * 1.0 + 2.0 * 2.0 + 1.0 * 6.0
        );
        polu.store_data().unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let mut lines = written.lines();
        assert_eq!(
            lines.next(),
            Some("source.business,source.size,sink.store,sink.state,units")
        );
        let units: i64 = lines
            .map(|l| l.rsplit(',').next().unwrap().parse::<i64>().unwrap())
            .sum();
        assert_eq!(units, 9);
        assert!(written.contains("bananas and more,,stores and us,WA,3"));
    }
}
