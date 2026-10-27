//! polu-core for the browser, built with `wasm-pack build polu-wasm --target web` (see `demo/build.sh`).
//!
//! A [`Model`] loads sources, sinks, source caps and costmap layers from file contents (CSV, JSON
//! or Excel, as in [`polu_core::load`]), formulates the problem, solves it with a JS LP solver (see
//! [`solver::JsSolver`]), and hands back flows, unused capacity, cost traces and result files.
//!
//! ```js
//! import init, { Model } from "./pkg/polu_wasm.js";
//! import highsLoader from "./vendor/highs.mjs";
//! await init();
//! const highs = await highsLoader({ locateFile: (file) => `./vendor/${file}` });
//! const model = new Model(spec, (lp) => highs.solve(lp));
//! const { objective, flows } = model.solve();
//! ```

mod input;
mod solver;

use js_sys::Function;
use polu_core::store::{csv::CsvStore, json::JsonStore, xlsx::XlsxStore};
use polu_core::{
    Assignment, BoxError, CostRule, Header, Op, Polu, SinkList, Slack, SourceList, Store, Table,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

pub use solver::JsSolver;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** One file: `text` for CSV and JSON, `bytes` for Excel. `format` defaults to `name`'s extension. */
export interface PoluInput {
  name?: string;
  format?: "csv" | "json" | "xlsx";
  text?: string;
  bytes?: Uint8Array;
  /** Excel sheet, the first one by default. */
  sheet?: string;
  /** A named Excel table (.xlsx only), read instead of `sheet`. */
  table?: string;
  /** Sources, sinks and caps: the quantity column. */
  quantity?: string;
  /** Layers: what the layer does. */
  op?: "set" | "scale" | "add" | "block";
}
export interface PoluSpec {
  sources: PoluInput;
  /** Source caps: each row caps the total out of the source rows it matches, with no routes of its own. */
  caps?: PoluInput;
  sinks: PoluInput;
  /** A headerless cost matrix, or a rule table applied as a first `set` layer. */
  base?: PoluInput;
  layers?: PoluInput[];
}
/** What the solve callback returns; highs-js's `highs.solve(lp)` result has this shape. */
export interface LpResult {
  Status: string;
  Columns: Record<string, { Primal: number }>;
}
export interface TableView {
  columns: string[];
  rows: { cells: (string | null)[]; amount: number; label: string }[];
}
export interface RouteView { source: number; sink: number; cost: number }
export interface LayerView {
  name: string;
  op: string;
  rules: { text: string; priority: number; value: number }[];
}
export interface TraceView {
  /** null without a base matrix or when the base has no route for the pair. */
  base: number | null;
  steps: { layer: number; op: string; rule: number | null; cost: number | null }[];
  /** null when the pair can't be used. */
  cost: number | null;
}
export interface SolutionView {
  objective: number;
  flows: { source: number; sink: number; units: number; cost: number }[];
}
export interface SlackView { used: number; amount: number; unused: number; spare: number }
/** One source cap's use: `unused` is what it still allows, `percentUsed` is rounded down. */
export interface CapSlackView { used: number; amount: number; unused: number; percentUsed: number }
/** A sheet or named table of a workbook; `sheet` is the sheet it's on (its own name for a sheet). */
export interface RegionView {
  kind: "sheet" | "table";
  name: string;
  sheet: string;
  /** The header row, empty for an empty sheet. */
  columns: string[];
}
"#;

#[wasm_bindgen(start)]
fn start() {
    // panics surface as `unreachable` in JS otherwise
    std::panic::set_hook(Box::new(|info| console_error(&info.to_string())));
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(message: &str);
}

/// A loaded and formulated problem, ready to solve and inspect.
#[wasm_bindgen]
pub struct Model {
    polu: Polu,
    /// The name of each costmap layer, in costmap order.
    layer_names: Vec<String>,
}

#[wasm_bindgen]
impl Model {
    /// Loads every input and formulates the problem. `solve` is called with CPLEX LP text by
    /// [`Model::solve`], e.g. `(lp) => highs.solve(lp)`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        #[wasm_bindgen(unchecked_param_type = "PoluSpec")] spec: JsValue,
        #[wasm_bindgen(unchecked_param_type = "(lp: string) => LpResult")] solve: Function,
    ) -> Result<Model, JsError> {
        let spec: input::Spec = serde_wasm_bindgen::from_value(spec)?;
        let spec = spec.into_spec().map_err(js)?;
        let mut layer_names: Vec<String> = (spec.base.iter())
            .filter(|base| base.input.is_rule_table())
            .map(|base| name(&base.name, "base"))
            .collect();
        layer_names.extend(spec.layers.iter().map(|l| name(&l.name, "layer")));

        let solver = Box::new(JsSolver::new(solve));
        let mut polu = spec.polu(solver, Box::new(NoStore)).map_err(js)?;
        polu.load_data().map_err(js)?;
        Ok(Self { polu, layer_names })
    }

    #[wasm_bindgen(unchecked_return_type = "TableView")]
    pub fn sources(&self) -> Result<JsValue, JsError> {
        to_js(&TableView::new(self.tables().0))
    }

    #[wasm_bindgen(unchecked_return_type = "TableView")]
    pub fn sinks(&self) -> Result<JsValue, JsError> {
        to_js(&TableView::new(self.tables().1))
    }

    /// The source cap table, or `undefined` without one.
    #[wasm_bindgen(unchecked_return_type = "TableView | undefined")]
    pub fn caps(&self) -> Result<JsValue, JsError> {
        match self.polu.source_caps() {
            Some(caps) => to_js(&TableView::new(caps)),
            None => Ok(JsValue::UNDEFINED),
        }
    }

    /// Every usable source/sink pair and its resolved cost.
    #[wasm_bindgen(unchecked_return_type = "RouteView[]")]
    pub fn routes(&self) -> Result<JsValue, JsError> {
        let routes: Vec<RouteView> = self
            .problem()
            .routes
            .iter()
            .map(|r| RouteView {
                source: r.source,
                sink: r.sink,
                cost: r.cost,
            })
            .collect();
        to_js(&routes)
    }

    /// The costmap layers and their rules, so a trace's `layer` and `rule` can be shown.
    #[wasm_bindgen(unchecked_return_type = "LayerView[]")]
    pub fn layers(&self) -> Result<JsValue, JsError> {
        let costmap = self.polu.costmap().expect("loaded in new");
        let (source, sink) = (costmap.source_header(), costmap.sink_header());
        let layers: Vec<LayerView> = costmap
            .layers()
            .iter()
            .zip(&self.layer_names)
            .map(|(layer, name)| LayerView {
                name: name.clone(),
                op: op_name(layer.op),
                rules: layer
                    .rules
                    .iter()
                    .map(|rule| RuleView {
                        text: rule_text(rule, source, sink),
                        priority: rule.priority,
                        value: rule.value,
                    })
                    .collect(),
            })
            .collect();
        to_js(&layers)
    }

    /// How one pair's cost was reached: the base, then what each layer did.
    #[wasm_bindgen(unchecked_return_type = "TraceView")]
    pub fn trace(&self, source: usize, sink: usize) -> Result<JsValue, JsError> {
        let (sources, sinks) = self.tables();
        if source >= sources.rows().len() || sink >= sinks.rows().len() {
            return Err(JsError::new("no such source or sink row"));
        }
        let costmap = self.polu.costmap().expect("loaded in new");
        let trace = costmap.trace(sources, sinks, source, sink);
        to_js(&TraceView {
            base: trace.base.and_then(number),
            steps: trace
                .steps
                .iter()
                .map(|s| StepView {
                    layer: s.layer,
                    op: op_name(s.op),
                    rule: s.rule,
                    cost: number(s.cost),
                })
                .collect(),
            cost: number(trace.cost()),
        })
    }

    /// The problem as the CPLEX LP text the solve callback receives.
    pub fn lp(&self) -> String {
        self.problem().to_lp()
    }

    /// Solves with the callback given to the constructor.
    #[wasm_bindgen(unchecked_return_type = "SolutionView")]
    pub fn solve(&mut self) -> Result<JsValue, JsError> {
        self.polu.run().map_err(js)?;
        self.solution()
    }

    /// The last solution, or `undefined` before [`Model::solve`].
    #[wasm_bindgen(unchecked_return_type = "SolutionView | undefined")]
    pub fn solution(&self) -> Result<JsValue, JsError> {
        let Some(assignment) = self.polu.solution() else {
            return Ok(JsValue::UNDEFINED);
        };
        let problem = self.problem();
        let cost = |s, k| {
            problem
                .routes
                .iter()
                .find(|r| r.source == s && r.sink == k)
                .map_or(f64::NAN, |r| r.cost)
        };
        to_js(&SolutionView {
            objective: assignment.objective,
            flows: assignment
                .flows
                .iter()
                .map(|f| FlowView {
                    source: f.source,
                    sink: f.sink,
                    units: f.units,
                    cost: cost(f.source, f.sink),
                })
                .collect(),
        })
    }

    /// Unused capacity of every source row in the last solution, see [`polu_core::Problem::slack`].
    #[wasm_bindgen(unchecked_return_type = "SlackView[]")]
    pub fn slack(&self) -> Result<JsValue, JsError> {
        let slack: Vec<SlackView> = self
            .polu
            .slack()
            .ok_or_else(|| JsError::new("call solve() first"))?
            .iter()
            .map(|s| SlackView {
                used: s.used,
                amount: s.amount,
                unused: s.unused(),
                spare: s.spare,
            })
            .collect();
        to_js(&slack)
    }

    /// The flows of the last solution as a `csv`, `json` or `xlsx` file.
    #[wasm_bindgen(js_name = exportFlows)]
    pub fn export_flows(&self, format: &str) -> Result<Vec<u8>, JsError> {
        let assignment = self.assignment()?;
        let (sources, sinks) = self.tables();
        let mut out = Vec::new();
        match format {
            "csv" => CsvStore::new("").write(&mut out, assignment, sources, sinks),
            "json" => JsonStore::new("").write(&mut out, assignment, sources, sinks),
            "xlsx" => XlsxStore::new("")
                .workbook(assignment, sources, sinks)
                .and_then(|mut book| Ok(book.save_to_buffer()?))
                .map(|bytes| out = bytes),
            other => return Err(JsError::new(&format!("unknown format `{other}`"))),
        }
        .map_err(js)?;
        Ok(out)
    }

    /// How much of each source cap the last solution uses, one entry per row of [`Model::caps`];
    /// empty without caps. See [`polu_core::Problem::cap_slack`].
    #[wasm_bindgen(js_name = capSlack, unchecked_return_type = "CapSlackView[]")]
    pub fn cap_slack(&self) -> Result<JsValue, JsError> {
        let slack: Vec<CapSlackView> = self
            .polu
            .cap_slack()
            .ok_or_else(|| JsError::new("call solve() first"))?
            .iter()
            .map(|c| CapSlackView {
                used: c.used,
                amount: c.amount,
                unused: c.unused(),
                percent_used: c.percent_used(),
            })
            .collect();
        to_js(&slack)
    }

    /// The use of each source cap in the last solution as a `csv`, `json` or `xlsx` file.
    #[wasm_bindgen(js_name = exportCapSlack)]
    pub fn export_cap_slack(&self, format: &str) -> Result<Vec<u8>, JsError> {
        let slack = self
            .polu
            .cap_slack()
            .ok_or_else(|| JsError::new("call solve() first"))?;
        let caps =
            (self.polu.source_caps()).ok_or_else(|| JsError::new("there are no source caps"))?;
        let mut out = Vec::new();
        match format {
            "csv" => CsvStore::new("").write_cap_slack(&mut out, &slack, caps),
            "json" => JsonStore::new("").write_cap_slack(&mut out, &slack, caps),
            "xlsx" => XlsxStore::new("")
                .cap_slack_workbook(&slack, caps)
                .and_then(|mut book| Ok(book.save_to_buffer()?))
                .map(|bytes| out = bytes),
            other => return Err(JsError::new(&format!("unknown format `{other}`"))),
        }
        .map_err(js)?;
        Ok(out)
    }

    /// The unused capacity of the last solution as a `csv`, `json` or `xlsx` file.
    #[wasm_bindgen(js_name = exportSlack)]
    pub fn export_slack(&self, format: &str) -> Result<Vec<u8>, JsError> {
        let slack: Vec<Slack> = self
            .polu
            .slack()
            .ok_or_else(|| JsError::new("call solve() first"))?;
        let sources = self.tables().0;
        let mut out = Vec::new();
        match format {
            "csv" => CsvStore::new("").write_slack(&mut out, &slack, sources),
            "json" => JsonStore::new("").write_slack(&mut out, &slack, sources),
            "xlsx" => XlsxStore::new("")
                .slack_workbook(&slack, sources)
                .and_then(|mut book| Ok(book.save_to_buffer()?))
                .map(|bytes| out = bytes),
            other => return Err(JsError::new(&format!("unknown format `{other}`"))),
        }
        .map_err(js)?;
        Ok(out)
    }
}

impl Model {
    fn tables(&self) -> (&SourceList, &SinkList) {
        self.polu.tables().expect("loaded in new")
    }

    fn problem(&self) -> &polu_core::Problem {
        self.polu.problem().expect("formulated in new")
    }

    fn assignment(&self) -> Result<&Assignment, JsError> {
        self.polu
            .solution()
            .ok_or_else(|| JsError::new("call solve() first"))
    }
}

/// Every column of a sources or sinks file, so the page can offer the quantity column.
#[wasm_bindgen(js_name = columnNames)]
pub fn column_names(
    #[wasm_bindgen(unchecked_param_type = "PoluInput")] input: JsValue,
) -> Result<Vec<String>, JsError> {
    let input: input::Input = serde_wasm_bindgen::from_value(input)?;
    input.adapter().map_err(js)?.column_names().map_err(js)
}

/// Every sheet of an Excel workbook in order, each followed by the named tables on it, with their
/// columns, so the page can offer them. Pass one on as an input's `sheet` or `table`.
#[wasm_bindgen(unchecked_return_type = "RegionView[]")]
pub fn regions(workbook: &[u8]) -> Result<JsValue, JsError> {
    use polu_core::load::xlsx::{self, Region};
    let regions: Vec<RegionView> = xlsx::regions(workbook)
        .map_err(js)?
        .into_iter()
        .map(|info| {
            let (kind, name) = match info.region {
                Region::Sheet(name) => ("sheet", name),
                Region::Table(name) => ("table", name),
            };
            RegionView {
                kind,
                name,
                sheet: info.sheet,
                columns: info.columns,
            }
        })
        .collect();
    to_js(&regions)
}

/// Results are handed back with `exportFlows`/`exportSlack` instead of written by a store.
struct NoStore;

impl Store for NoStore {
    fn store(&self, _: &Assignment, _: &SourceList, _: &SinkList) -> Result<(), BoxError> {
        Err("use exportFlows() in the browser".into())
    }
}

#[derive(Serialize)]
struct TableView {
    columns: Vec<String>,
    rows: Vec<RowView>,
}

/// One table row; `label` is its set fields joined, e.g. `apple inc / large`.
#[derive(Serialize)]
struct RowView {
    cells: Vec<Option<String>>,
    amount: i64,
    label: String,
}

impl TableView {
    fn new(table: &Table) -> Self {
        let slots = named(table.header());
        Self {
            columns: slots
                .iter()
                .map(|&s| table.header()[s].as_deref().unwrap_or_default().to_owned())
                .collect(),
            rows: table
                .rows()
                .iter()
                .map(|(key, amount)| {
                    let cells: Vec<Option<&str>> = slots.iter().map(|&s| key.get_slot(s)).collect();
                    RowView {
                        label: cells
                            .iter()
                            .flatten()
                            .copied()
                            .collect::<Vec<_>>()
                            .join(" / "),
                        cells: cells.iter().map(|c| c.map(String::from)).collect(),
                        amount: *amount,
                    }
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct RouteView {
    source: usize,
    sink: usize,
    cost: f64,
}

#[derive(Serialize)]
struct LayerView {
    name: String,
    op: &'static str,
    rules: Vec<RuleView>,
}

#[derive(Serialize)]
struct RuleView {
    text: String,
    priority: i8,
    value: f64,
}

#[derive(Serialize)]
struct TraceView {
    base: Option<f64>,
    steps: Vec<StepView>,
    cost: Option<f64>,
}

#[derive(Serialize)]
struct StepView {
    layer: usize,
    op: &'static str,
    rule: Option<usize>,
    cost: Option<f64>,
}

#[derive(Serialize)]
struct SolutionView {
    objective: f64,
    flows: Vec<FlowView>,
}

#[derive(Serialize)]
struct FlowView {
    source: usize,
    sink: usize,
    units: i64,
    cost: f64,
}

#[derive(Serialize)]
struct SlackView {
    used: i64,
    amount: i64,
    unused: i64,
    spare: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CapSlackView {
    used: i64,
    amount: i64,
    unused: i64,
    percent_used: i64,
}

#[derive(Serialize)]
struct RegionView {
    kind: &'static str,
    name: String,
    sheet: String,
    columns: Vec<String>,
}

fn to_js(value: &impl Serialize) -> Result<JsValue, JsError> {
    // `None` as `null`, as the TypeScript types say
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
    Ok(value.serialize(&serializer)?)
}

fn js(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// `NaN` (no route) as `None`.
fn number(cost: f64) -> Option<f64> {
    (!cost.is_nan()).then_some(cost)
}

fn name(name: &str, fallback: &str) -> String {
    match name {
        "" => fallback.into(),
        name => name.into(),
    }
}

fn op_name(op: Op) -> &'static str {
    match op {
        Op::Set => "set",
        Op::Scale => "scale",
        Op::Add => "add",
        Op::Block => "block",
    }
}

/// The slots that have a column name.
fn named(header: &Header) -> Vec<usize> {
    (0..header.len()).filter(|&s| header[s].is_some()).collect()
}

/// A rule's fields, e.g. `source.business = apple inc, sink.store = ohio fruits`, or `any pair`.
fn rule_text(rule: &CostRule, source: &Header, sink: &Header) -> String {
    let mut fields = Vec::new();
    for (prefix, key, header) in [("source", &rule.source, source), ("sink", &rule.sink, sink)] {
        for slot in named(header) {
            if let Some(value) = key.get_slot(slot) {
                let column = header[slot].as_deref().unwrap_or_default();
                fields.push(format!("{prefix}.{column} = {value}"));
            }
        }
    }
    if fields.is_empty() {
        "any pair".into()
    } else {
        fields.join(", ")
    }
}
