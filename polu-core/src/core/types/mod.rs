mod sink;
mod source;

mod costmap;

use std::collections::{HashMap, HashSet};

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

    /// One limit per row of `caps`, over the `routes` from a source row it matches to a sink row it
    /// matches, where `self` is the source table. Cap columns are matched by name: `sink.<column>`
    /// to the sink columns, `source.<column>` or a bare name to the source columns, so a cap table
    /// of source columns alone caps every route out of the rows it matches. A row matches when every
    /// field the cap sets on its side is set and equal in the row (as `Scale`, `Add` and `Block`
    /// rules match). A cap that matches no route keeps an empty limit, so limit `i` is always cap
    /// row `i`.
    pub fn caps(
        &self,
        sinks: &Table,
        caps: &Table,
        routes: &[Route],
    ) -> Result<Vec<Limit>, PoluError> {
        // (cap slot, table slot), for the source side then the sink side
        let mut slots: [Vec<(usize, usize)>; 2] = Default::default();
        for (c, name) in caps.header.iter().enumerate() {
            let Some(name) = name.as_deref() else {
                continue;
            };
            let (side, column) = cap_column(name);
            let (table, slots) = match side {
                "sink" => (sinks, &mut slots[1]),
                _ => (self, &mut slots[0]),
            };
            let slot = (table.header.iter())
                .position(|h| h.as_deref() == Some(column))
                .ok_or_else(|| {
                    PoluError::Invalid(format!("cap column `{name}` is not in the {side} table"))
                })?;
            if slots.iter().any(|&(_, s)| s == slot) {
                return Err(PoluError::Invalid(format!(
                    "cap columns name `{side}.{column}` twice"
                )));
            }
            slots.push((c, slot));
        }
        let index = [RowIndex::new(self), RowIndex::new(sinks)];
        // the routes out of each source row and into each sink row, so each cap walks its smaller side
        let mut by_row = [
            vec![Vec::new(); self.rows.len()],
            vec![Vec::new(); sinks.rows.len()],
        ];
        for (r, route) in routes.iter().enumerate() {
            by_row[0][route.source].push(r);
            by_row[1][route.sink].push(r);
        }
        Ok(caps
            .rows
            .iter()
            .map(|(cap, amount)| {
                let [sources, sinks] = [0, 1].map(|side| {
                    let mut fields: [Option<Box<str>>; SLOTS] = Default::default();
                    for &(c, slot) in &slots[side] {
                        fields[slot] = cap.0[c].clone();
                    }
                    index[side].rows_matching(&Key(fields), false)
                });
                let size = |rows: &[usize], side: usize| -> usize {
                    rows.iter().map(|&r| by_row[side][r].len()).sum()
                };
                let mut rows: Vec<usize> = if size(&sources, 0) <= size(&sinks, 1) {
                    (sources.iter().flat_map(|&s| &by_row[0][s]))
                        .filter(|&&r| sinks.binary_search(&routes[r].sink).is_ok())
                        .copied()
                        .collect()
                } else {
                    (sinks.iter().flat_map(|&k| &by_row[1][k]))
                        .filter(|&&r| sources.binary_search(&routes[r].source).is_ok())
                        .copied()
                        .collect()
                };
                rows.sort_unstable();
                Limit {
                    rows,
                    amount: *amount,
                }
            })
            .collect())
    }
}

/// The side and column a cap column names: `sink.<column>` a sink column, `source.<column>` or a
/// bare name a source column.
pub(crate) fn cap_column(name: &str) -> (&'static str, &str) {
    match name.strip_prefix("sink.") {
        Some(column) => ("sink", column),
        None => ("source", name.strip_prefix("source.").unwrap_or(name)),
    }
}

/// A usable source/sink pair and its unit cost. Indices point into the source and sink tables.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub source: usize,
    pub sink: usize,
    pub cost: f64,
}

/// Bound on the total flow through a group: of table rows for supply and demand (one row plus every
/// row it covers), of routes for caps.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub rows: Vec<usize>,
    pub amount: i64,
}

/// Solver-neutral transport problem: minimise the sum of cost * flow over `routes`, with integer flows,
/// total flow out of the rows of each `supply` limit `<= amount`, total flow along the routes of each
/// `caps` limit `<= amount` and total flow into the rows of each `demand` limit `== amount`.
/// `supply[i]` and `demand[j]` belong to source row i and sink row j. `caps` come from a cap table
/// (see [`Table::caps`]): `caps[i]` is cap row `i` and its `rows` index into `routes`, so caps are
/// extra rows over the same columns and add no routes.
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
        let used = self.shipped(assignment);
        // a row can ship more only while every limit over it has room: its supply limits, and the
        // caps over every route out of it (a cap over some of them doesn't bound the row)
        let mut spare: Vec<i64> = vec![i64::MAX; self.supply.len()];
        for limit in &self.supply {
            let unused = limit.amount - used(limit);
            for &r in &limit.rows {
                spare[r] = spare[r].min(unused);
            }
        }
        let mut out = vec![0usize; self.supply.len()];
        for route in &self.routes {
            out[route.source] += 1;
        }
        let mut in_cap = vec![0usize; self.supply.len()];
        for (cap, slack) in self.caps.iter().zip(self.cap_slack(assignment)) {
            let source = |r: usize| self.routes[r].source;
            for &r in &cap.rows {
                in_cap[source(r)] += 1;
            }
            for &r in &cap.rows {
                // the first route of a row takes its count, so the row's later routes see 0
                let s = source(r);
                if std::mem::take(&mut in_cap[s]) == out[s] {
                    spare[s] = spare[s].min(slack.unused());
                }
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

    /// How much of each cap `assignment` uses; `cap_slack[i]` belongs to cap row i. Empty without
    /// caps.
    pub fn cap_slack(&self, assignment: &Assignment) -> Vec<CapSlack> {
        let mut units = HashMap::new();
        for flow in &assignment.flows {
            *units.entry((flow.source, flow.sink)).or_insert(0) += flow.units;
        }
        let units = |r: usize| {
            let route = &self.routes[r];
            units.get(&(route.source, route.sink)).copied().unwrap_or(0)
        };
        self.caps
            .iter()
            .map(|limit| CapSlack {
                used: limit.rows.iter().map(|&r| units(r)).sum(),
                amount: limit.amount,
            })
            .collect()
    }

    /// Units shipped out of a limit's rows under `assignment`.
    fn shipped(&self, assignment: &Assignment) -> impl Fn(&Limit) -> i64 {
        let mut shipped = vec![0i64; self.supply.len()];
        for flow in &assignment.flows {
            shipped[flow.source] += flow.units;
        }
        move |limit| limit.rows.iter().map(|&r| shipped[r]).sum()
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
            self.caps.iter().map(|cap| cap.rows.clone()).collect(),
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
    /// [`unused`](Slack::unused) of every supply limit that includes the row and of every cap over
    /// all of its routes. A cap over only some of its routes doesn't bound the row; its use is in
    /// [`Problem::cap_slack`].
    pub spare: i64,
}

impl Slack {
    /// `amount - used`: the limit's slack, as the solver's row slack would report it.
    pub fn unused(&self) -> i64 {
        self.amount - self.used
    }
}

/// How much of one cap a solution uses.
#[derive(Debug, Clone, PartialEq)]
pub struct CapSlack {
    /// Units shipped along the routes the cap matches.
    pub used: i64,
    /// The cap.
    pub amount: i64,
}

impl CapSlack {
    /// `amount - used`: what the cap still allows.
    pub fn unused(&self) -> i64 {
        self.amount - self.used
    }

    /// `used` as a whole percentage of `amount`, rounded down so only a full cap reads 100. A cap
    /// of 0 is full.
    pub fn percent_used(&self) -> i64 {
        match self.amount {
            0 => 100,
            amount => self.used * 100 / amount,
        }
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
