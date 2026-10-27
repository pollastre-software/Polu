//! Inputs handed over from JS: file contents (text or bytes), each read by polu-core's input
//! adapter for its format.

use polu_core::load::spec::{self, BaseInput, LayerInput, TableInput};
use polu_core::load::{csv, json, xlsx};
use polu_core::{BoxError, Op};
use serde::Deserialize;

/// Everything a model needs, like the TUI's `*.polu.json` but with contents instead of paths.
#[derive(Debug, Deserialize)]
pub struct Spec {
    pub sources: Input,
    #[serde(default)]
    pub caps: Option<Input>,
    pub sinks: Input,
    #[serde(default)]
    pub base: Option<Input>,
    #[serde(default)]
    pub layers: Vec<Input>,
}

/// One file. `format` defaults to the extension of `name`. Sources, sinks and caps need `quantity`,
/// layers need `op`; Excel files take an optional `sheet` or named `table` (.xlsx only), and read
/// the first sheet without either. A `table` wins over a `sheet`.
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
    pub table: Option<String>,
    #[serde(default)]
    pub quantity: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
}

impl Spec {
    /// The polu-core spec; errors name the input that failed.
    pub fn into_spec(self) -> Result<spec::Spec, BoxError> {
        let table = |input: Input, role: &str| -> Result<TableInput, BoxError> {
            let at = |e| input.at(role, e);
            let quantity = input.quantity.as_deref().ok_or("no `quantity` column");
            let quantity = quantity.map_err(|e| at(e.into()))?;
            Ok(TableInput::new(
                &input.name,
                input.adapter().map_err(at)?,
                quantity,
            ))
        };
        let layer = |(l, input): (usize, Input)| -> Result<LayerInput, BoxError> {
            let at = |e| input.at(&format!("layer {}", l + 1), e);
            let op = input.op().map_err(at)?;
            Ok(LayerInput::new(
                &input.name,
                input.adapter().map_err(at)?,
                op,
            ))
        };
        let base = |input: Input| -> Result<BaseInput, BoxError> {
            let adapter = input.adapter().map_err(|e| input.at("base", e))?;
            Ok(BaseInput::new(&input.name, adapter))
        };
        Ok(spec::Spec {
            sources: table(self.sources, "sources")?,
            caps: self.caps.map(|caps| table(caps, "caps")).transpose()?,
            sinks: table(self.sinks, "sinks")?,
            base: self.base.map(base).transpose()?,
            layers: (self.layers.into_iter().enumerate())
                .map(layer)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl Input {
    /// The polu-core adapter for this input's format: its text, or for Excel its bytes and sheet
    /// or table.
    pub fn adapter(&self) -> Result<Box<dyn spec::Input>, BoxError> {
        let ext = self.name.rsplit_once('.').map(|(_, ext)| ext);
        let format = self.format.as_deref().or(ext).unwrap_or_default();
        match format.to_ascii_lowercase().as_str() {
            "csv" => Ok(Box::new(csv::CsvInput::new(self.text()?))),
            "json" => Ok(Box::new(json::JsonInput::new(self.text()?))),
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => {
                let bytes = self.bytes.as_deref().ok_or("no `bytes`")?;
                let input = xlsx::XlsxInput::new(bytes);
                Ok(Box::new(match (&self.table, &self.sheet) {
                    (Some(table), _) => input.with_region(xlsx::Region::Table(table.clone())),
                    (None, Some(sheet)) => input.with_region(xlsx::Region::Sheet(sheet.clone())),
                    (None, None) => input,
                }))
            }
            _ => Err("set `format` to csv, json or xlsx".into()),
        }
    }

    fn text(&self) -> Result<String, BoxError> {
        match (&self.text, &self.bytes) {
            (Some(text), _) => Ok(text.clone()),
            (None, Some(bytes)) => Ok(String::from_utf8(bytes.clone())?),
            (None, None) => Err("no `text` or `bytes`".into()),
        }
    }

    fn op(&self) -> Result<Op, BoxError> {
        match self.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("set") => Ok(Op::Set),
            Some("scale") => Ok(Op::Scale),
            Some("add") => Ok(Op::Add),
            Some("block") => Ok(Op::Block),
            other => Err(format!("`op` must be set, scale, add or block, not {other:?}").into()),
        }
    }

    /// `e`, starting with which input failed: `sources (sources.csv): ...`.
    fn at(&self, role: &str, e: BoxError) -> BoxError {
        match self.name.as_str() {
            "" => format!("{role}: {e}"),
            name => format!("{role} ({name}): {e}"),
        }
        .into()
    }
}
