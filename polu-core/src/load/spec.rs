//! A run's inputs as data, without formats. Each input is an [`Input`]: a port that format
//! adapters implement, such as `csv::CsvInput`, `json::JsonInput` and `xlsx::XlsxInput`, each
//! behind its cargo feature and keeping its own options (a delimiter, a workbook's sheet or table)
//! as typed fields. A front end picks the adapter for each file, and [`Spec::polu`] wires them
//! into a [`Polu`]: it decides what each input is read as, labels errors with the input's name,
//! and turns a rule-table base into the first `Set` layer. Adapters for other data, e.g. Python
//! dataframes, can live outside this crate.
//!
//! ```no_run
//! # #[cfg(all(feature = "highs", feature = "csv"))] {
//! use polu_core::load::csv::CsvInput;
//! use polu_core::load::spec::{LayerInput, Spec, TableInput};
//! use polu_core::{HighsSolver, Op};
//! # struct NoStore;
//! # impl polu_core::Store for NoStore {
//! #     fn store(&self, _: &polu_core::Assignment, _: &polu_core::SourceList, _: &polu_core::SinkList) -> Result<(), polu_core::BoxError> { Ok(()) }
//! # }
//! let csv = |path| -> std::io::Result<_> { Ok(Box::new(CsvInput::from_path(path)?)) };
//! let spec = Spec {
//!     sources: TableInput::new("sources.csv", csv("sources.csv")?, "capacity"),
//!     caps: None,
//!     sinks: TableInput::new("sinks.csv", csv("sinks.csv")?, "demand"),
//!     base: None,
//!     layers: vec![LayerInput::new("costs.csv", csv("costs.csv")?, Op::Set)],
//! };
//! let mut polu = spec.polu(Box::new(HighsSolver::default()), Box::new(NoStore))?;
//! polu.load_data()?;
//! # }
//! # Ok::<(), polu_core::BoxError>(())
//! ```

use super::TableColumns;
use crate::core::traits::{Load, Solver, Store};
use crate::core::types::{CostMap, Header, Layer, Op, Table};
use crate::core::{BoxError, Polu};

/// One table of input, read as whatever the spec needs it for. Rows and columns follow the
/// layouts in [`crate::load`].
pub trait Input {
    /// Every column name, the quantity column included, e.g. to let a user pick the quantity column.
    fn column_names(&self) -> Result<Vec<String>, BoxError>;

    /// Sources, sinks or source caps, with `quantity` as the quantity column.
    fn table(&self, quantity: &str) -> Result<Table, BoxError>;

    /// A costmap layer doing `op`; its `source.` and `sink.` columns name fields of these headers.
    fn layer(&self, op: Op, source: &Header, sink: &Header) -> Result<Layer, BoxError>;

    /// A headerless base matrix: one row per source and one cost per sink, row-major, `NaN` where
    /// the pair can't be used.
    fn base(&self) -> Result<Vec<f64>, BoxError>;

    /// The category columns of [`Input::table`], in slot order, without reading the rows.
    fn header(&self, quantity: &str) -> Result<Header, BoxError> {
        Ok(TableColumns::new(&self.column_names()?, quantity)?.header)
    }

    /// Whether this is a rule table (`source.` or `sink.` columns, like a layer) rather than a
    /// headerless matrix. A rule-table base is applied as a `Set` layer before the others.
    fn is_rule_table(&self) -> bool {
        self.column_names().is_ok_and(|columns| {
            columns.iter().any(|c| {
                let c = c.to_ascii_lowercase();
                c.starts_with("source.") || c.starts_with("sink.")
            })
        })
    }
}

/// Everything a run reads.
pub struct Spec {
    pub sources: TableInput,
    /// Caps over the source rows each of its rows matches, see [`Table::caps`].
    pub caps: Option<TableInput>,
    pub sinks: TableInput,
    /// A headerless cost matrix, or a rule table applied as a `Set` layer before the others.
    pub base: Option<BaseInput>,
    pub layers: Vec<LayerInput>,
}

/// Sources, sinks or source caps: an input, the name errors call it by, and its quantity column.
pub struct TableInput {
    pub name: String,
    pub input: Box<dyn Input>,
    pub quantity: String,
}

impl TableInput {
    pub fn new(
        name: impl Into<String>,
        input: Box<dyn Input>,
        quantity: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            input,
            quantity: quantity.into(),
        }
    }
}

/// The base: an input and the name errors call it by.
pub struct BaseInput {
    pub name: String,
    pub input: Box<dyn Input>,
}

impl BaseInput {
    pub fn new(name: impl Into<String>, input: Box<dyn Input>) -> Self {
        Self {
            name: name.into(),
            input,
        }
    }
}

/// One costmap layer: an input, the name errors call it by, and what it does.
pub struct LayerInput {
    pub name: String,
    pub input: Box<dyn Input>,
    pub op: Op,
}

impl LayerInput {
    pub fn new(name: impl Into<String>, input: Box<dyn Input>, op: Op) -> Self {
        Self {
            name: name.into(),
            input,
            op,
        }
    }
}

impl Spec {
    /// A [`Polu`] that loads these inputs. The source and sink headers are read now, for the
    /// costmap; everything else when it loads.
    ///
    /// Errors name the input: `sources (sources.csv): no `capacity` column`, and layers also their
    /// op and place, `scale layer 2 (sizes.csv): ...`.
    pub fn polu(self, solver: Box<dyn Solver>, store: Box<dyn Store>) -> Result<Polu, BoxError> {
        let (sources, source_header) = self.sources.loader("sources")?;
        let (sinks, sink_header) = self.sinks.loader("sinks")?;
        let caps = self.caps.map(|caps| caps.loader("caps")).transpose()?;
        let costmap = Costmap {
            base: self.base,
            layers: self.layers,
            source_header,
            sink_header,
        };
        let polu = Polu::new(sources, sinks, Box::new(costmap), solver, store);
        Ok(match caps {
            Some((caps, _)) => polu.with_source_caps(caps),
            None => polu,
        })
    }
}

impl TableInput {
    /// The loader, its errors labelled with `role`, and the header the costmap needs.
    fn loader(self, role: &str) -> Result<(Box<dyn Load<Table>>, Header), BoxError> {
        let label = label(role, &self.name);
        let header = (self.input)
            .header(&self.quantity)
            .map_err(|e| format!("{label}: {e}"))?;
        Ok((Box::new(TableLoader { label, table: self }), header))
    }
}

struct TableLoader {
    label: String,
    table: TableInput,
}

impl Load<Table> for TableLoader {
    fn load(&self) -> Result<Table, BoxError> {
        let TableInput {
            input, quantity, ..
        } = &self.table;
        input
            .table(quantity)
            .map_err(|e| format!("{}: {e}", self.label).into())
    }
}

/// The base and every layer, each read by its own adapter, as one costmap, so inputs can mix
/// formats (e.g. an Excel base with CSV layers).
struct Costmap {
    base: Option<BaseInput>,
    layers: Vec<LayerInput>,
    source_header: Header,
    sink_header: Header,
}

impl Load<CostMap> for Costmap {
    fn load(&self) -> Result<CostMap, BoxError> {
        let (source, sink) = (&self.source_header, &self.sink_header);
        let mut base = None;
        let mut layers = Vec::new();
        if let Some(BaseInput { name, input }) = &self.base {
            let at = |e: BoxError| -> BoxError { format!("{}: {e}", label("base", name)).into() };
            if input.is_rule_table() {
                let layer = input.layer(Op::Set, source, sink).map_err(at)?;
                layer.check().map_err(|e| at(e.into()))?;
                layers.push(layer);
            } else {
                base = Some((input.base().map_err(at)?, at));
            }
        }
        for (l, LayerInput { name, input, op }) in self.layers.iter().enumerate() {
            let role = format!("{} layer {}", op_name(*op), l + 1);
            let at = |e: BoxError| -> BoxError { format!("{}: {e}", label(&role, name)).into() };
            let layer = input.layer(*op, source, sink).map_err(at)?;
            layer.check().map_err(|e| at(e.into()))?;
            layers.push(layer);
        }
        let costmap = CostMap::new(source.clone(), sink.clone(), layers)?;
        match base {
            Some((costs, at)) => costmap.with_base(costs).map_err(|e| at(e.into())),
            None => Ok(costmap),
        }
    }
}

/// `role (name)`, or just `role` without a name.
fn label(role: &str, name: &str) -> String {
    match name {
        "" => role.into(),
        name => format!("{role} ({name})"),
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

#[cfg(all(test, feature = "csv"))]
mod tests {
    use super::super::csv::{CsvCostMap, CsvInput, CsvTable};
    use super::*;
    use crate::core::types::{Assignment, SinkList, SourceList};

    const SOURCES: &str = "business,size,capacity\napple inc,,5\napple inc,large,3\napple inc,small,7\nbananas and more,,20\n";
    const SINKS: &str = "store,state,demand\nstores and us,WA,3\nohio fruits,OH,6\n";
    const COSTS: &str = "source.business,sink.store,value\napple inc,stores and us,2\napple inc,ohio fruits,1\nbananas and more,stores and us,4\nbananas and more,ohio fruits,3\n";
    const SCALE: &str = "source.size,value\nsmall,2\n";

    struct NoSolver;

    impl Solver for NoSolver {
        fn solve(&self, _: &crate::Problem) -> Result<Assignment, BoxError> {
            Err("no solver".into())
        }
    }

    struct NoStore;

    impl Store for NoStore {
        fn store(&self, _: &Assignment, _: &SourceList, _: &SinkList) -> Result<(), BoxError> {
            Err("no store".into())
        }
    }

    fn csv(text: &str) -> Box<dyn Input> {
        Box::new(CsvInput::new(text))
    }

    fn spec() -> Spec {
        Spec {
            sources: TableInput::new("sources.csv", csv(SOURCES), "capacity"),
            caps: None,
            sinks: TableInput::new("sinks.csv", csv(SINKS), "demand"),
            base: None,
            layers: vec![
                LayerInput::new("costs.csv", csv(COSTS), Op::Set),
                LayerInput::new("sizes.csv", csv(SCALE), Op::Scale),
            ],
        }
    }

    /// The LP text of the loaded problem, or the load error.
    fn lp(spec: Spec) -> Result<String, String> {
        let mut polu =
            (spec.polu(Box::new(NoSolver), Box::new(NoStore))).map_err(|e| e.to_string())?;
        let problem = polu.load_data().map_err(|e| e.to_string())?;
        Ok(problem.to_lp())
    }

    /// The LP text of the same tables loaded with the CSV loaders directly.
    fn direct(layers: fn(CsvCostMap) -> CsvCostMap) -> String {
        let sources = CsvTable::new(SOURCES, "capacity");
        let sinks = CsvTable::new(SINKS, "demand");
        let costmap = CsvCostMap::new(sources.header().unwrap(), sinks.header().unwrap());
        let mut polu = Polu::new(
            Box::new(sources),
            Box::new(sinks),
            Box::new(layers(costmap)),
            Box::new(NoSolver),
            Box::new(NoStore),
        );
        polu.load_data().unwrap().to_lp()
    }

    #[test]
    fn a_spec_loads_like_the_loaders_it_wires() {
        let layers = |cm: CsvCostMap| cm.layer(Op::Set, COSTS).layer(Op::Scale, SCALE);
        assert_eq!(lp(spec()).unwrap(), direct(layers));
    }

    #[test]
    fn a_base_is_a_matrix_or_a_first_set_layer() {
        let mut rules = spec();
        rules.base = Some(BaseInput::new("base.csv", csv(COSTS)));
        rules.layers.remove(0);
        assert_eq!(lp(rules).unwrap(), lp(spec()).unwrap());

        const MATRIX: &str = "2,1\n3,nan\n4,2\n5,3\n";
        let mut matrix = spec();
        matrix.base = Some(BaseInput::new("base.csv", csv(MATRIX)));
        matrix.layers.remove(0);
        let layers = |cm: CsvCostMap| cm.base(MATRIX).layer(Op::Scale, SCALE);
        assert_eq!(lp(matrix).unwrap(), direct(layers));
    }

    /// An adapter outside the format modules, built from public types only, as a Python
    /// binding might keep typed rows: sinks as `(store, state, demand)`.
    struct Stores(Vec<(&'static str, &'static str, i64)>);

    impl Input for Stores {
        fn column_names(&self) -> Result<Vec<String>, BoxError> {
            Ok(["store", "state", "demand"].map(String::from).to_vec())
        }

        fn table(&self, quantity: &str) -> Result<Table, BoxError> {
            let header = self.header(quantity)?;
            let rows = self.0.iter().map(|&(store, state, demand)| {
                let mut fields: [Option<Box<str>>; crate::core::types::SLOTS] = Default::default();
                fields[0] = Some(store.into());
                fields[1] = Some(state.into());
                (crate::Key::new(fields), demand)
            });
            Ok(Table::new(header, rows.collect())?)
        }

        fn layer(&self, _: Op, _: &Header, _: &Header) -> Result<Layer, BoxError> {
            Err("not a layer".into())
        }

        fn base(&self) -> Result<Vec<f64>, BoxError> {
            Err("not a matrix".into())
        }
    }

    #[test]
    fn any_adapter_can_be_an_input() {
        let mut stores = spec();
        let rows = vec![("stores and us", "WA", 3), ("ohio fruits", "OH", 6)];
        stores.sinks.input = Box::new(Stores(rows));
        assert_eq!(lp(stores).unwrap(), lp(spec()).unwrap());
    }

    #[test]
    fn errors_name_the_input() {
        let error = |change: fn(&mut Spec)| {
            let mut spec = spec();
            change(&mut spec);
            lp(spec).unwrap_err()
        };
        assert_eq!(
            error(|s| s.sources.quantity = "units".into()),
            "sources (sources.csv): no `units` column"
        );
        assert_eq!(
            error(|s| s.sources =
                TableInput::new("", csv("business,capacity\napple inc,five\n"), "capacity")),
            "loading failed: sources: line 2: `five` is not an integer quantity: invalid digit found in string"
        );
        assert_eq!(
            error(|s| s.layers[1].input = csv("source.colour,value\nred,2\n")),
            "loading failed: scale layer 2 (sizes.csv): column `source.colour` is not in that table's header"
        );
        assert_eq!(
            error(|s| s.layers[0].input = csv("source.business,value\napple inc,inf\n")),
            "loading failed: set layer 1 (costs.csv): rule 0 has a non-finite value"
        );
        assert_eq!(
            error(|s| s.base = Some(BaseInput::new("base.csv", csv("1,x\n")))),
            "loading failed: base (base.csv): base line 1: `x`: invalid float literal"
        );
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn workbooks_read_their_region_or_the_first_sheet() {
        use super::super::xlsx::{Region, XlsxInput};
        use rust_xlsxwriter::Workbook;
        let mut book = Workbook::new();
        for (name, text) in [("sources", SOURCES), ("sinks", SINKS)] {
            let sheet = book.add_worksheet();
            sheet.set_name(name).unwrap();
            for (r, line) in text.lines().enumerate() {
                for (c, cell) in line.split(',').enumerate() {
                    match cell.parse::<f64>() {
                        _ if cell.is_empty() => continue,
                        Ok(v) => sheet.write_number(r as u32, c as u16, v).unwrap(),
                        Err(_) => sheet.write_string(r as u32, c as u16, cell).unwrap(),
                    };
                }
            }
        }
        let bytes: std::sync::Arc<[u8]> = book.save_to_buffer().unwrap().into();
        let mut workbook = spec();
        workbook.sources.input = Box::new(XlsxInput::new(bytes.clone()));
        workbook.sinks.input = Box::new(XlsxInput::new(bytes.clone()).with_region("sinks"));
        assert_eq!(lp(workbook).unwrap(), lp(spec()).unwrap());

        let mut missing = spec();
        missing.sinks = TableInput::new(
            "book.xlsx",
            Box::new(XlsxInput::new(bytes).with_region(Region::Table("Sinks".into()))),
            "demand",
        );
        assert_eq!(
            lp(missing).unwrap_err(),
            "sinks (book.xlsx): no `Sinks` table"
        );
    }
}
