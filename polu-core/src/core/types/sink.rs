use super::{Key, Table};

/// A sink row's categories. Its quantity is a demand: total flow into it is `==` the demand.
pub type Sink = Key;

/// Sinks with their integer demands.
pub type SinkList = Table;
