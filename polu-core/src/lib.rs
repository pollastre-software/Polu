mod core;
/// Loaders, one module per format behind its cargo feature, e.g. `load::csv::CsvTable`.
pub mod load;
#[cfg(feature = "highs")]
mod solver;
/// Stores, one module per format behind its cargo feature, e.g. `store::csv::CsvStore`.
pub mod store;

// pipeline
pub use crate::core::{BoxError, Polu, PoluError};
// ports, implemented by adapters
pub use crate::core::traits::{Load, Solver, Store};
// inputs (what loaders build)
pub use crate::core::types::{
    CostMap, CostRule, Header, Key, Layer, Op, SinkList, SourceList, Table, Trace, TraceStep,
};
// solver contract (what solvers read and return, and stores read)
pub use crate::core::types::{Assignment, Flow, Limit, Problem, Route, Slack};

// solver adapter, behind the `highs` cargo feature (on by default)
#[cfg(feature = "highs")]
pub use solver::HighsSolver;
