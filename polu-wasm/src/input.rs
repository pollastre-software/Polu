//! Inputs handed over from JS: file contents (text or bytes), picked up by polu-core's loaders by format.

use std::sync::Arc;

use polu_core::load::{csv, json, xlsx};
use polu_core::{BoxError, CostMap, Header, Load, Op, Table};
use serde::Deserialize;

/// Everything a model needs, like the TUI's `*.polu.json` but with contents instead of paths.
#[derive(Debug, Deserialize)]
pub struct Spec {
    pub sources: Input,
    pub sinks: Input,
    #[serde(default)]
    pub base: Option<Input>,
    #[serde(default)]
    pub layers: Vec<Input>,
}

/// One file. `format` defaults to the extension of `name`. Sources and sinks need `quantity`,
/// layers need `op`; Excel files take an optional `sheet` (the first one by default).
#[derive(Debug, Deserialize)]
pub struct Input {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, with = "serde_bytes")]
    pub bytes: Option<Vec<u8>>,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub quantity: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Csv,
    Json,
    Excel,
}

impl Input {
    fn format(&self) -> Result<Format, BoxError> {
        let ext = self.name.rsplit_once('.').map(|(_, ext)| ext);
        let format = self.format.as_deref().or(ext).unwrap_or_default();
        match format.to_ascii_lowercase().as_str() {
            "csv" => Ok(Format::Csv),
            "json" => Ok(Format::Json),
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => Ok(Format::Excel),
            _ => Err(format!("{}: set `format` to csv, json or xlsx", self.label()).into()),
        }
    }

    /// The name, for errors.
    fn label(&self) -> &str {
        if self.name.is_empty() {
            "input"
        } else {
            &self.name
        }
    }

    fn text(&self) -> Result<String, BoxError> {
        match (&self.text, &self.bytes) {
            (Some(text), _) => Ok(text.clone()),
            (None, Some(bytes)) => Ok(String::from_utf8(bytes.clone())?),
            (None, None) => Err("no `text` or `bytes`".into()),
        }
    }

    fn workbook(&self) -> Result<(Arc<[u8]>, String), BoxError> {
        let bytes: Arc<[u8]> = self.bytes.as_deref().ok_or("no `bytes`")?.into();
        let sheet = match &self.sheet {
            Some(sheet) => sheet.clone(),
            None => xlsx::sheet_names(&bytes)?
                .into_iter()
                .next()
                .ok_or("the workbook has no sheets")?,
        };
        Ok((bytes, sheet))
    }

    pub fn op(&self) -> Result<Op, BoxError> {
        match self.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("set") => Ok(Op::Set),
            Some("scale") => Ok(Op::Scale),
            Some("add") => Ok(Op::Add),
            Some("block") => Ok(Op::Block),
            other => Err(format!(
                "{}: `op` must be set, scale, add or block, not {other:?}",
                self.label()
            )
            .into()),
        }
    }

    /// A loader for sources or sinks, and the header the costmap loader needs.
    pub fn table(&self, what: &str) -> Result<(Box<dyn Load<Table>>, Header), BoxError> {
        let at = |e: BoxError| -> BoxError { format!("{what} ({}): {e}", self.label()).into() };
        let quantity = self
            .quantity
            .clone()
            .ok_or("no `quantity` column")
            .map_err(|e| at(e.into()))?;
        let (loader, header): (Box<dyn Load<Table>>, _) = match self.format().map_err(at)? {
            Format::Csv => {
                let table = csv::CsvTable::new(self.text().map_err(at)?, quantity);
                let header = table.header().map_err(at)?;
                (Box::new(table), header)
            }
            Format::Json => {
                let table = json::JsonTable::new(self.text().map_err(at)?, quantity);
                let header = table.header().map_err(at)?;
                (Box::new(table), header)
            }
            Format::Excel => {
                let (bytes, sheet) = self.workbook().map_err(at)?;
                let table = xlsx::XlsxTable::new(bytes, sheet, quantity);
                let header = table.header().map_err(at)?;
                (Box::new(table), header)
            }
        };
        let what = format!("{what} ({})", self.label());
        Ok((Box::new(Named(what, loader)), header))
    }

    /// Every column, so the page can offer the quantity column.
    pub fn column_names(&self) -> Result<Vec<String>, BoxError> {
        match self.format()? {
            Format::Csv => csv::CsvTable::new(self.text()?, "").column_names(),
            Format::Json => json::JsonTable::new(self.text()?, "").column_names(),
            Format::Excel => {
                let (bytes, sheet) = self.workbook()?;
                xlsx::XlsxTable::new(bytes, sheet, "").column_names()
            }
        }
    }

    /// Whether a base is a rule table (`source.`/`sink.` columns) rather than a headerless matrix.
    /// A rule table base is applied as a `Set` layer before the others, as in the TUI.
    pub fn is_rules(&self) -> bool {
        self.column_names().is_ok_and(|columns| {
            columns.iter().any(|c| {
                let c = c.to_ascii_lowercase();
                c.starts_with("source.") || c.starts_with("sink.")
            })
        })
    }

    /// This file alone as a costmap: a layer doing `op`, or the base matrix when `op` is `None`.
    fn costmap(&self, op: Option<Op>, source: &Header, sink: &Header) -> Result<CostMap, BoxError> {
        let (s, k) = (source.clone(), sink.clone());
        match self.format()? {
            Format::Csv => {
                let cm = csv::CsvCostMap::new(s, k);
                let text = self.text()?;
                match op {
                    Some(op) => cm.layer(op, text),
                    None => cm.base(text),
                }
                .load()
            }
            Format::Json => {
                let cm = json::JsonCostMap::new(s, k);
                let text = self.text()?;
                match op {
                    Some(op) => cm.layer(op, text),
                    None => cm.base(text),
                }
                .load()
            }
            Format::Excel => {
                let (bytes, sheet) = self.workbook()?;
                let cm = xlsx::XlsxCostMap::new(bytes, s, k);
                match op {
                    Some(op) => cm.layer(op, sheet),
                    None => cm.base(sheet),
                }
                .load()
            }
        }
    }
}

/// Parses the base and every layer, each with the loader of its own format, into one costmap,
/// so files can mix formats (e.g. an Excel base with CSV layers).
pub struct Costmap {
    pub base: Option<Input>,
    pub layers: Vec<Input>,
    pub source: Header,
    pub sink: Header,
}

impl Load<CostMap> for Costmap {
    fn load(&self) -> Result<CostMap, BoxError> {
        let mut base = None;
        let mut layers = Vec::new();
        if let Some(input) = &self.base {
            let at = |e: BoxError| -> BoxError { format!("base ({}): {e}", input.label()).into() };
            if input.is_rules() {
                let cm = input
                    .costmap(Some(Op::Set), &self.source, &self.sink)
                    .map_err(at)?;
                layers.extend(cm.layers().iter().cloned());
            } else {
                let cm = input.costmap(None, &self.source, &self.sink).map_err(at)?;
                base = cm.base().map(<[f64]>::to_vec);
            }
        }
        for (l, input) in self.layers.iter().enumerate() {
            let at =
                |e: BoxError| -> BoxError { format!("layer {l} ({}): {e}", input.label()).into() };
            let cm = input
                .costmap(Some(input.op().map_err(at)?), &self.source, &self.sink)
                .map_err(at)?;
            layers.extend(cm.layers().iter().cloned());
        }
        let costmap = CostMap::new(self.source.clone(), self.sink.clone(), layers)?;
        Ok(match base {
            Some(base) => costmap.with_base(base)?,
            None => costmap,
        })
    }
}

/// A loader whose errors start with which input failed.
struct Named<T>(String, Box<dyn Load<T>>);

impl<T> Load<T> for Named<T> {
    fn load(&self) -> Result<T, BoxError> {
        self.1.load().map_err(|e| format!("{}: {e}", self.0).into())
    }
}
