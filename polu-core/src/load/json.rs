//! JSON loaders for wide tables: column names under `headers`, rows of cells under `data`, laid out
//! as described in [`crate::load`]. They parse JSON text rather than open files, so the same
//! loaders work from the CLI, WASM and Python; `from_path` is a native shortcut.
//!
//! ```json
//! {"headers": ["business", "size", "capacity"],
//!  "data": [["apple inc", null, 5], ["apple inc", "large", 3]]}
//! ```
//!
//! Cells can be strings, numbers, booleans or `null` (an empty cell).

use std::borrow::Cow;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::{LayerColumns, TableColumns};
use crate::core::BoxError;
use crate::core::traits::Load;
use crate::core::types::{CostMap, Header, Layer, Op, Table};

/// Sources or sinks from wide JSON, with the quantity column chosen by name.
#[derive(Debug, Clone)]
pub struct JsonTable {
    text: String,
    quantity_column: String,
}

impl JsonTable {
    pub fn new(text: impl Into<String>, quantity_column: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            quantity_column: quantity_column.into(),
        }
    }

    pub fn from_path(
        path: impl AsRef<Path>,
        quantity_column: impl Into<String>,
    ) -> std::io::Result<Self> {
        Ok(Self::new(std::fs::read_to_string(path)?, quantity_column))
    }

    /// The category columns, in slot order. Pass these to [`JsonCostMap::new`].
    pub fn header(&self) -> Result<Header, BoxError> {
        #[derive(Deserialize)]
        struct Headers {
            headers: Vec<String>,
        }
        let Headers { headers } = serde_json::from_str(&self.text)?;
        Ok(TableColumns::new(&headers, &self.quantity_column)?.header)
    }
}

impl Load<Table> for JsonTable {
    fn load(&self) -> Result<Table, BoxError> {
        let wide: Wide<Value> = serde_json::from_str(&self.text)?;
        let columns = TableColumns::new(&wide.headers, &self.quantity_column)?;
        let mut rows = Vec::with_capacity(wide.data.len());
        for (r, cells) in wide.rows()? {
            rows.push(
                columns
                    .row(&texts(cells)?)
                    .map_err(|e| format!("data[{r}]: {e}"))?,
            );
        }
        Ok(Table::new(columns.header, rows)?)
    }
}

/// A layered costmap from wide JSON: an optional base matrix, then one document per layer,
/// applied in order.
///
/// ```json
/// {"headers": ["source.business", "sink.store", "value"],
///  "data": [["bananas and more", null, 2]]}
/// ```
///
/// The base matrix only needs `data`: one row per source, one number per sink, `null` where the
/// pair can't be used.
#[derive(Debug, Clone)]
pub struct JsonCostMap {
    source_header: Header,
    sink_header: Header,
    base: Option<String>,
    layers: Vec<(Op, String)>,
}

impl JsonCostMap {
    pub fn new(source_header: Header, sink_header: Header) -> Self {
        Self {
            source_header,
            sink_header,
            base: None,
            layers: Vec::new(),
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

    fn parse_layer(&self, op: Op, text: &str) -> Result<Layer, BoxError> {
        let wide: Wide<Value> = serde_json::from_str(text)?;
        let columns = LayerColumns::new(op, &wide.headers, &self.source_header, &self.sink_header)?;
        let mut rules = Vec::with_capacity(wide.data.len());
        for (r, cells) in wide.rows()? {
            rules.push(
                columns
                    .rule(&texts(cells)?)
                    .map_err(|e| format!("data[{r}]: {e}"))?,
            );
        }
        Ok(Layer { op, rules })
    }
}

impl Load<CostMap> for JsonCostMap {
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
        let base: Wide<Option<f64>> =
            serde_json::from_str(base).map_err(|e| format!("base: {e}"))?;
        let costs = base.data.into_iter().flatten();
        Ok(costmap.with_base(costs.map(|c| c.unwrap_or(f64::NAN)).collect())?)
    }
}

/// The wide layout: `headers` (optional for the base matrix) and `data`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wide<C> {
    #[serde(default)]
    headers: Vec<String>,
    data: Vec<Vec<C>>,
}

impl<C> Wide<C> {
    /// The rows with their indexes, checked to have one cell per header.
    fn rows(&self) -> Result<impl Iterator<Item = (usize, &[C])>, BoxError> {
        if let Some((r, row)) = self
            .data
            .iter()
            .enumerate()
            .find(|(_, row)| row.len() != self.headers.len())
        {
            return Err(format!(
                "data[{r}] has {} cells, `headers` has {}",
                row.len(),
                self.headers.len()
            )
            .into());
        }
        Ok(self.data.iter().map(Vec::as_slice).enumerate())
    }
}

/// Cell text as the column rules read it: trimmed strings, numbers as written (`5.0` is `5`),
/// `null` as empty.
fn texts(cells: &[Value]) -> Result<Vec<Cow<'_, str>>, BoxError> {
    cells
        .iter()
        .map(|cell| match cell {
            Value::String(s) => Ok(Cow::Borrowed(s.trim())),
            Value::Null => Ok(Cow::Borrowed("")),
            Value::Number(n) => Ok(Cow::Owned(match n.as_i64() {
                Some(i) => i.to_string(),
                None => n.as_f64().unwrap_or(f64::NAN).to_string(),
            })),
            Value::Bool(b) => Ok(Cow::Owned(b.to_string())),
            other => Err(format!("`{other}` is not a cell").into()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &str = r#"{"headers": ["business", "size", "capacity"],
        "data": [["apple inc", null, 5],
                 ["apple inc", "large", 3.0],
                 ["apple inc", "small", 7],
                 ["bananas and more", "", 20]]}"#;
    const SINKS: &str = r#"{"headers": ["store", "state", "demand"],
        "data": [["stores and us", "WA", 3], ["ohio fruits", "OH", 6]]}"#;

    fn names(header: &Header) -> Vec<&str> {
        header.iter().flatten().map(|h| &**h).collect()
    }

    #[test]
    fn table_reads_wide_json() {
        let table = JsonTable::new(SOURCES, "capacity").load().unwrap();
        assert_eq!(names(table.header()), ["business", "size"]);
        let (key, amount) = &table.rows()[0];
        assert_eq!(
            (key.get_slot(0), key.get_slot(1), *amount),
            (Some("apple inc"), None, 5)
        );
        assert_eq!(table.rows()[1].1, 3);
        assert_eq!(table.rows()[3].0.get_slot(1), None);
        let header = JsonTable::new(SINKS, "demand").header().unwrap();
        assert_eq!(names(&header), ["store", "state"]);
    }

    #[test]
    fn table_errors_name_the_problem() {
        let error = |text: &str| JsonTable::new(text, "q").load().unwrap_err().to_string();
        assert!(
            error(r#"{"headers": ["a", "q"], "data": [["x"]]}"#).contains("data[0] has 1 cells")
        );
        assert!(error(r#"{"headers": ["a", "q"], "data": [["x", 1.5]]}"#).contains("data[0]"));
        assert!(error(r#"{"headers": ["a", "q"], "data": [[["x"], 1]]}"#).contains("not a cell"));
        assert!(error(r#"{"header": ["a", "q"], "data": []}"#).contains("unknown field"));
        assert!(error(r#"{"headers": ["a"], "data": []}"#).contains("no `q` column"));
        assert!(error(r#"{"headers": ["a", "q"], "data": [["x", 1], ["x", 2]]}"#).contains("x"));
    }

    fn costmap() -> JsonCostMap {
        let sources = JsonTable::new(SOURCES, "capacity").header().unwrap();
        let sinks = JsonTable::new(SINKS, "demand").header().unwrap();
        JsonCostMap::new(sources, sinks)
    }

    #[test]
    fn layers_and_base_read_wide_json() {
        let cm = costmap()
            .layer(
                Op::Set,
                r#"{"headers": ["source.business", "sink.store", "value", "priority"],
                    "data": [["apple inc", null, 2, null], [null, "ohio fruits", 1, 3]]}"#,
            )
            .layer(
                Op::Block,
                r#"{"headers": ["source.size"], "data": [["small"]]}"#,
            )
            .base(r#"{"data": [[1, 2], [3, null], [null, 4.5], [5, 6]]}"#)
            .load()
            .unwrap();
        let set = &cm.layers()[0];
        assert_eq!(set.rules[0].source.get_slot(0), Some("apple inc"));
        assert_eq!(set.rules[0].sink.specificity(), 0);
        assert_eq!((set.rules[1].priority, set.rules[1].value), (3, 1.0));
        assert_eq!(cm.layers()[1].rules[0].source.get_slot(1), Some("small"));
        let base = cm.base().unwrap();
        assert_eq!(base.len(), 8);
        assert_eq!((base[0], base[5], base[7]), (1.0, 4.5, 6.0));
        assert!(base[3].is_nan() && base[4].is_nan());

        let no_value = costmap()
            .layer(
                Op::Scale,
                r#"{"headers": ["source.size"], "data": [["small"]]}"#,
            )
            .load()
            .unwrap_err();
        assert!(
            no_value
                .to_string()
                .contains("costmap layer 0: a `value` column")
        );
        let bad_base = costmap().base(r#"{"data": [["x"]]}"#).load().unwrap_err();
        assert!(bad_base.to_string().starts_with("base:"));
    }

    /// The CSV end-to-end example, read from and written back to wide JSON.
    #[cfg(feature = "highs")]
    #[test]
    fn json_pipeline_end_to_end() {
        use crate::store::json::JsonStore;
        use crate::{HighsSolver, Polu};

        let base = r#"{"headers": ["source.business", "sink.store", "value"],
            "data": [["apple inc", "stores and us", 2],
                     ["apple inc", "ohio fruits", 1],
                     ["bananas and more", "stores and us", 4],
                     ["bananas and more", "ohio fruits", 3]]}"#;
        let path = std::env::temp_dir().join(format!("polu-json-test-{}.json", std::process::id()));
        let mut polu = Polu::new(
            Box::new(JsonTable::new(SOURCES, "capacity")),
            Box::new(JsonTable::new(SINKS, "demand")),
            Box::new(
                costmap()
                    .layer(Op::Set, base)
                    .layer(
                        Op::Scale,
                        r#"{"headers": ["source.size", "value"], "data": [["small", 2]]}"#,
                    )
                    .layer(
                        Op::Block,
                        r#"{"headers": ["source.business", "sink.store"],
                            "data": [["apple inc", "stores and us"]]}"#,
                    ),
            ),
            Box::new(HighsSolver::default()),
            Box::new(JsonStore::new(&path)),
        );
        polu.load_data().unwrap();
        let assignment = polu.run().unwrap();
        assert_eq!(
            assignment.objective,
            3.0 * 8.0 + 3.0 * 1.0 + 2.0 * 2.0 + 1.0 * 6.0
        );
        polu.store_data().unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        // what the store writes, the table loader reads back
        let flows = JsonTable::new(written.as_str(), "units").load().unwrap();
        assert_eq!(
            names(flows.header()),
            ["source.business", "source.size", "sink.store", "sink.state"]
        );
        assert_eq!(flows.rows().iter().map(|(_, units)| units).sum::<i64>(), 9);
        assert!(written.contains(r#"["bananas and more",null,"stores and us","WA",3]"#));
    }
}
