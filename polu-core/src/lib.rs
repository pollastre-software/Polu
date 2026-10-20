mod core;
mod load;
#[cfg(feature = "highs")]
mod solver;
mod store;

// pipeline
pub use crate::core::{BoxError, Polu, PoluError};
// ports, implemented by adapters
pub use crate::core::traits::{Load, Solver, Store};
// inputs (what loaders build)
pub use crate::core::types::{
    CostMap, CostRule, Header, Key, Layer, Op, SinkList, SourceList, Table,
};
// solver contract (what solvers read and return, and stores read)
pub use crate::core::types::{Assignment, Flow, Limit, Problem, Route};

#[cfg(feature = "highs")]
pub use solver::HighsSolver;
