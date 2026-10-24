mod sink;
mod source;

mod costmap;

use std::collections::HashSet;

use crate::core::PoluError;

pub use costmap::{CostMap, CostRule, Layer, Op, Trace, TraceStep};

use costmap::RowIndex;
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

    /// True if every set slot of `self` equals the same slot of `row`, where an empty slot in `row`
    /// matches any value. This is how `Set` layers match, see [`CostRule::matches`].
    pub fn matches(&self, row: &Key) -> bool {
        self.0
            .iter()
            .zip(&row.0)
            .all(|(r, v)| r.is_none() || v.is_none() || r == v)
    }

    /// True if every set slot of `self` equals the same slot of `row`, which must be set too.
    /// This is how `Scale`, `Add` and `Block` layers match, see [`CostRule::matches`].
    pub fn matches_exactly(&self, row: &Key) -> bool {
        self.0
            .iter()
            .zip(&row.0)
            .all(|(r, v)| r.is_none() || r == v)
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

    /// For each row, the other rows it covers: those with the same value in every slot it sets.
    /// A row with only a business name covers every row of that business.
    pub fn covered(&self) -> Vec<Vec<usize>> {
        let index = RowIndex::new(self);
        (0..self.rows.len())
            .map(|i| {
                let mut rows = index.rows_matching(&self.rows[i].0, false);
                rows.retain(|&r| r != i);
                rows
            })
            .collect()
    }
}

/// A usable source/sink pair and its unit cost. Indices point into the source and sink tables.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub source: usize,
    pub sink: usize,
    pub cost: f64,
}

/// Bound on the total flow through a group of table rows: one row plus every row it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub rows: Vec<usize>,
    pub amount: i64,
}

/// Solver-neutral transport problem: minimise the sum of cost * flow over `routes`, with integer flows,
/// total flow out of the rows of each `supply` limit `<= amount` and total flow into the rows of each
/// `demand` limit `== amount`. `supply[i]` and `demand[j]` belong to source row i and sink row j.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub routes: Vec<Route>,
    pub supply: Vec<Limit>,
    pub demand: Vec<Limit>,
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
