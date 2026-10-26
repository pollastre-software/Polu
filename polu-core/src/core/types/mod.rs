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

    /// One limit per row of `caps`, over the rows of this table it matches: every field the cap sets
    /// must be set and equal in the row (as `Scale`, `Add` and `Block` rules match). Cap columns are
    /// matched to this table's columns by name. A cap that matches no row is left out.
    pub fn caps(&self, caps: &Table) -> Result<Vec<Limit>, PoluError> {
        // (cap slot, table slot)
        let mut slots = Vec::new();
        for (c, name) in caps.header.iter().enumerate() {
            let Some(name) = name.as_deref() else {
                continue;
            };
            let slot = self
                .header
                .iter()
                .position(|h| h.as_deref() == Some(name))
                .ok_or_else(|| {
                    PoluError::Invalid(format!("cap column `{name}` is not in the table"))
                })?;
            slots.push((c, slot));
        }
        let index = RowIndex::new(self);
        Ok(caps
            .rows
            .iter()
            .filter_map(|(cap, amount)| {
                let mut fields: [Option<Box<str>>; SLOTS] = Default::default();
                for &(c, slot) in &slots {
                    fields[slot] = cap.0[c].clone();
                }
                let rows = index.rows_matching(&Key(fields), false);
                (!rows.is_empty()).then_some(Limit {
                    rows,
                    amount: *amount,
                })
            })
            .collect())
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
/// total flow out of the rows of each `supply` and `caps` limit `<= amount` and total flow into the rows
/// of each `demand` limit `== amount`. `supply[i]` and `demand[j]` belong to source row i and sink row j;
/// `caps` come from a source cap table (see [`Table::caps`]) and belong to no row, so they add no routes.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub routes: Vec<Route>,
    pub supply: Vec<Limit>,
    pub caps: Vec<Limit>,
    pub demand: Vec<Limit>,
}

impl Problem {
    /// Unused capacity of each supply limit under `assignment`; `slack[i]` belongs to source row i.
    /// Computed from the flows rather than read from the solver, so every solver reports it the same way.
    pub fn slack(&self, assignment: &Assignment) -> Vec<Slack> {
        let mut shipped = vec![0i64; self.supply.len()];
        for flow in &assignment.flows {
            shipped[flow.source] += flow.units;
        }
        let used = |limit: &Limit| -> i64 { limit.rows.iter().map(|&r| shipped[r]).sum() };
        // a row can ship more only while every limit over it, caps included, has room
        let mut spare: Vec<i64> = vec![i64::MAX; self.supply.len()];
        for limit in self.supply.iter().chain(&self.caps) {
            let unused = limit.amount - used(limit);
            for &r in &limit.rows {
                spare[r] = spare[r].min(unused);
            }
        }
        self.supply
            .iter()
            .map(|limit| (limit, used(limit)))
            .zip(spare)
            .map(|((limit, used), spare)| Slack {
                used,
                amount: limit.amount,
                spare,
            })
            .collect()
    }

    /// The assignment for one solved value per route (`columns[i]` is `routes[i]`), rounded to whole
    /// units. The objective is summed from the rounded flows so it matches them exactly.
    pub fn assignment(&self, columns: &[f64]) -> Assignment {
        let mut flows = Vec::new();
        let mut objective = 0.0;
        for (route, &x) in self.routes.iter().zip(columns) {
            let units = x.round() as i64;
            if units > 0 {
                flows.push(Flow {
                    source: route.source,
                    sink: route.sink,
                    units,
                });
                objective += route.cost * units as f64;
            }
        }
        Assignment { flows, objective }
    }

    /// The problem in CPLEX LP format, for solvers that take text (e.g. HiGHS in the browser).
    /// Column `x{i}` is `routes[i]`; row `s{i}` is `supply[i]`, `c{i}` is `caps[i]` and `d{j}` is
    /// `demand[j]`. Supply limits and caps no route touches always hold and are left out.
    pub fn to_lp(&self) -> String {
        use std::fmt::Write;

        let (sources, sinks) = (self.supply.len(), self.demand.len());
        let (in_supply, in_caps, in_demand) = (
            members(&self.supply, sources, |r| r.source, &self.routes),
            members(&self.caps, sources, |r| r.source, &self.routes),
            members(&self.demand, sinks, |r| r.sink, &self.routes),
        );
        let mut lp = String::from("Minimize\n obj:");
        terms(&mut lp, self.routes.iter().map(|r| r.cost).enumerate());
        lp.push_str("\nSubject To\n");
        for (name, limits, members, op) in [
            ('s', &self.supply, &in_supply, "<="),
            ('c', &self.caps, &in_caps, "<="),
            ('d', &self.demand, &in_demand, "="),
        ] {
            for (i, (limit, columns)) in limits.iter().zip(members).enumerate() {
                if columns.is_empty() && op == "<=" {
                    continue;
                }
                let _ = write!(lp, " {name}{i}:");
                if columns.is_empty() {
                    // an unreachable demand still needs its row, so the solver reports it infeasible
                    lp.push_str(" 0 x0");
                }
                terms(&mut lp, columns.iter().map(|&c| (c, 1.0)));
                let _ = writeln!(lp, " {op} {}", limit.amount);
            }
        }
        lp.push_str("General\n");
        for c in 0..self.routes.len() {
            let _ = write!(lp, " x{c}");
            if c % 16 == 15 {
                lp.push('\n');
            }
        }
        lp.push_str("\nEnd\n");
        lp
    }
}

/// For each limit over a table of `rows` rows, the routes whose source (or sink, per `side`) is one of
/// its rows.
fn members(
    limits: &[Limit],
    rows: usize,
    side: impl Fn(&Route) -> usize,
    routes: &[Route],
) -> Vec<Vec<usize>> {
    let mut in_limits = vec![Vec::new(); rows];
    for (l, limit) in limits.iter().enumerate() {
        for &row in &limit.rows {
            in_limits[row].push(l);
        }
    }
    let mut members = vec![Vec::new(); limits.len()];
    for (c, route) in routes.iter().enumerate() {
        for &l in &in_limits[side(route)] {
            members[l].push(c);
        }
    }
    members
}

/// Appends ` + 2 x0 - 1.5 x3 ...`, breaking lines so LP readers with a line limit accept them.
fn terms(lp: &mut String, terms: impl Iterator<Item = (usize, f64)>) {
    use std::fmt::Write;

    for (n, (c, coef)) in terms.enumerate() {
        let sign = if coef < 0.0 { '-' } else { '+' };
        if n > 0 && n % 8 == 0 {
            lp.push_str("\n   ");
        }
        let _ = write!(lp, " {sign} {} x{c}", coef.abs());
    }
}

/// How much of one supply limit a solution leaves unused.
#[derive(Debug, Clone, PartialEq)]
pub struct Slack {
    /// Units shipped from the limit's rows (the row and every row it covers).
    pub used: i64,
    /// The limit's amount.
    pub amount: i64,
    /// Units the row itself could still ship before some limit over it is full: the smallest
    /// [`unused`](Slack::unused) of every limit that includes the row, caps included.
    pub spare: i64,
}

impl Slack {
    /// `amount - used`: the limit's slack, as the solver's row slack would report it.
    pub fn unused(&self) -> i64 {
        self.amount - self.used
    }
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
