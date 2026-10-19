use std::{collections::HashMap};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sink(
    ///
    /// Generic container for 8 category slots to be used in polu
    ///
    [Option<Box<str>>; 8],
);

impl Sink {
    pub fn new(fields: [Option<Box<str>>; 8]) -> Self {
        Self(fields)
    }

    pub fn get_slot(&self, index: usize) -> Option<&str> {
        self.0.get(index).and_then(|opt| opt.as_deref())
    }
}

pub struct SinkList {
    header: [Option<Box<str>>; 8], // name for each of the source fields
    demand: HashMap<Sink, i64>,    // hashmap for speed
}
