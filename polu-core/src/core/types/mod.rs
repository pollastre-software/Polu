mod sink;
mod source;

mod costmap;

use std::collections::HashSet;

use crate::core::PoluError;

pub use costmap::{CostMap, CostRule};
pub use sink::{Sink, SinkList};
pub use source::{Source, SourceList};

/// Number of category slots in a key. I fixed it fr now to not overcomplicate implementations
pub const SLOTS: usize = 8;

/// Name for each of the category slots.
pub type Header = [Option<Box<str>>; SLOTS];

/// Generic container for 8 category slots to be used in polu.
/// In a costmap rule, a `None` slot matches any value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key([Option<Box<str>>; SLOTS]);

impl Key {
    pub fn new(fields: [Option<Box<str>>; SLOTS]) -> Self {
        Self(fields)
    }

    pub fn get_slot(&self, index: usize) -> Option<&str> {
        self.0.get(index).and_then(|opt| opt.as_deref())
    }

    /// How many slots are set.
    pub fn specificity(&self) -> u8 {
        self.0.iter().filter(|f| f.is_some()).count() as u8
    }

    /// True if every set slot of `self` equals the same slot of `row`.
    pub fn matches(&self, row: &Key) -> bool {
        self.0
            .iter()
            .zip(&row.0)
            .all(|(r, v)| r.is_none() || v.is_none() || r == v)
    }
}

/// Header + rows of (key, quantity). Rows keep their input order, so the formulation is deterministic.
#[derive(Debug, Clone)]
pub struct Table {
    header: Header,
    rows: Vec<(Key, i64)>,
}

impl Table {
    /// Rejects duplicate keys and negative quantities; aggregation belongs upstream.
    pub fn new(header: Header, rows: Vec<(Key, i64)>) -> Result<Self, PoluError> {
        let mut seen = HashSet::with_capacity(rows.len());
        for (i, (key, qty)) in rows.iter().enumerate() {
            if !seen.insert(key) {
                return Err(PoluError::Invalid(format!(
                    "row {i} duplicates an earlier row: {key:?}"
                )));
            }
            if *qty < 0 {
                return Err(PoluError::Invalid(format!(
                    "row {i} has a negative quantity ({qty})"
                )));
            }
        }
        Ok(Self { header, rows })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn rows(&self) -> &[(Key, i64)] {
        &self.rows
    }
}

/// A usable source/sink pair and its unit cost. Indices point into the source and sink tables.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub source: usize,
    pub sink: usize,
    pub cost: f64,
}

/// Solver-neutral transport problem: minimise the sum of cost * flow over `routes`, with integer flows,
/// total flow out of source i `<= supply[i]` and total flow into sink j `== demand[j]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub routes: Vec<Route>,
    pub supply: Vec<i64>,
    pub demand: Vec<i64>,
}

/// Units sent along one route.
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub source: usize,
    pub sink: usize,
    pub units: i64,
}

/// A solved problem.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub flows: Vec<Flow>,
    pub objective: f64,
}
