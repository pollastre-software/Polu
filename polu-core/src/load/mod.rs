use crate::core::types::{CostKey, CostMap, Sink, SinkList, Source, SourceList};

use crate::core::traits::Load;

pub struct CSVLoad<T> {
    file_path: &str,
    separator: char,
}
