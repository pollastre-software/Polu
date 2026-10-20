use super::{Key, Table};

/// A source row's categories. Its quantity is a capacity: total flow out of it is `<=` the capacity.
pub type Source = Key;

/// Sources with their integer capacities.
pub type SourceList = Table;
