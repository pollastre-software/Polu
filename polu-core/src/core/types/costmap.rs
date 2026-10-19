use crate::core::types::{Sink, Source};
use std::collections::HashMap;

#[derive(Debug, Clone, Hash)]
pub struct CostKey {
    ///
    /// A combination between a source and a sink, used to classify the cost needed to move 1 unit through these two
    ///
    source: Source,
    sink: Sink,

    priority: i8,
    specificity: i8,
}

pub struct CostMap {
    header: Vec<Option<Box<str>>>,
    costmap: HashMap<CostKey, f64>,
}
