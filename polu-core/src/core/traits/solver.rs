use std::{
    error::Error,
    fmt::{Debug, Display},
};

use crate::core::types::{CostMap, SinkList, SourceList};

pub(crate) trait Solver {
    ///
    /// Solver trait for polu. prepare method allows for intermediate optimizations on the underlying data
    ///

    type DataStatement: Debug;
    type Solution: Debug;
    type Error: Error + Default;

    fn prepare(
        &self,
        source_list: &SourceList,
        sink_list: &SinkList,
        cost_map: &CostMap,
    ) -> Self::DataStatement;

    fn solve(&self, data_statement: &Self::DataStatement) -> Result<Self::Solution, Self::Error>;
}
