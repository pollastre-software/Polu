pub(crate) mod traits;
pub(crate) mod types;

use std::{error::Error, fmt};

use traits::{Load, Solver, Store};
use types::{Assignment, CostMap, Limit, Problem, Route, SinkList, SourceList, Table};

/// Error type returned by every adapter (loaders, solvers, stores).
pub type BoxError = Box<dyn Error + Send + Sync>;

#[derive(Debug)]
pub enum PoluError {
    Load(BoxError),
    Invalid(String),
    Solve(BoxError),
    Store(BoxError),
    /// A pipeline step was called before the step it depends on.
    NotReady(&'static str),
}

impl fmt::Display for PoluError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(e) => write!(f, "loading failed: {e}"),
            Self::Invalid(msg) => write!(f, "invalid input: {msg}"),
            Self::Solve(e) => write!(f, "solving failed: {e}"),
            Self::Store(e) => write!(f, "storing failed: {e}"),
            Self::NotReady(msg) => f.write_str(msg),
        }
    }
}

impl Error for PoluError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(e) | Self::Solve(e) | Self::Store(e) => Some(e.as_ref()),
            Self::Invalid(_) | Self::NotReady(_) => None,
        }
    }
}

/// Builds the transport problem. Every source/sink pair the costmap gives a cost becomes a route;
/// pairs without one can't be used. Add a catch-all `Set` rule (all slots `None`, lowest priority) to allow
/// any pair at a penalty cost instead.
pub fn formulate(
    sources: &SourceList,
    sinks: &SinkList,
    costmap: &CostMap,
) -> Result<Problem, PoluError> {
    if costmap.source_header() != sources.header() {
        return Err(PoluError::Invalid(
            "costmap source columns don't match the source header".into(),
        ));
    }
    if costmap.sink_header() != sinks.header() {
        return Err(PoluError::Invalid(
            "costmap sink columns don't match the sink header".into(),
        ));
    }

    // build C^t from the costmap, this is N^2, will need to profile if it's too slow
    let nk = sinks.rows().len();
    let mut routes = Vec::new();
    for (c, &cost) in costmap.resolve(sources, sinks)?.iter().enumerate() {
        if cost.is_nan() {
            continue;
        }
        if !cost.is_finite() {
            return Err(PoluError::Invalid(format!(
                "cost for source {} and sink {} overflowed",
                c / nk,
                c % nk
            )));
        }
        routes.push(Route {
            source: c / nk,
            sink: c % nk,
            cost,
        });
    }

    Ok(Problem {
        routes,
        supply: limits(sources),
        demand: limits(sinks),
    })
}

/// One limit per row, over the row and every row it covers, so a partial row (e.g. business name only)
/// bounds the total of the more specific rows under it and can still take the remainder itself.
fn limits(table: &Table) -> Vec<Limit> {
    table
        .covered()
        .into_iter()
        .zip(table.rows())
        .enumerate()
        .map(|(i, (mut rows, (_, amount)))| {
            rows.insert(0, i);
            Limit {
                rows,
                amount: *amount,
            }
        })
        .collect()
}

/// The load -> formulate -> solve -> store pipeline, over swappable adapters.
/// Ideally this doesn't use dyn to be faster, I think it's premature optimization at this point
/// TODO check how to swap adapters on the fly, maybe not needed for the core but for adapters/implementors yes
pub struct Polu {
    source_loader: Box<dyn Load<SourceList>>,
    sink_loader: Box<dyn Load<SinkList>>,
    costmap_loader: Box<dyn Load<CostMap>>,
    solver: Box<dyn Solver>,
    storer: Box<dyn Store>,
    problem: Option<Problem>,
    solution: Option<Assignment>,
}

impl Polu {
    pub fn new(
        source_loader: Box<dyn Load<SourceList>>,
        sink_loader: Box<dyn Load<SinkList>>,
        costmap_loader: Box<dyn Load<CostMap>>,
        solver: Box<dyn Solver>,
        storer: Box<dyn Store>,
    ) -> Self {
        Self {
            source_loader,
            sink_loader,
            costmap_loader,
            solver,
            storer,
            problem: None,
            solution: None,
        }
    }

    /// Call each adapter to load the data and transform into the problem statement data type (MIP)
    /// Could wire in something to look at the loaded data in the core before run
    pub fn load_data(&mut self) -> Result<&Problem, PoluError> {
        let sources = self.source_loader.load().map_err(PoluError::Load)?;
        let sinks = self.sink_loader.load().map_err(PoluError::Load)?;
        let costmap = self.costmap_loader.load().map_err(PoluError::Load)?;

        self.solution = None;
        Ok(self.problem.insert(formulate(&sources, &sinks, &costmap)?))
    }

    /// runs the assigner and stores the assignment data
    /// also could wire in a view before saving
    pub fn run(&mut self) -> Result<&Assignment, PoluError> {
        let problem = self
            .problem
            .as_ref()
            .ok_or(PoluError::NotReady("call load_data() before run()"))?;
        let solution = self.solver.solve(problem).map_err(PoluError::Solve)?;
        Ok(self.solution.insert(solution))
    }

    pub fn store_data(&self) -> Result<(), PoluError> {
        let solution = self
            .solution
            .as_ref()
            .ok_or(PoluError::NotReady("call run() before store_data()"))?;
        self.storer.store(solution).map_err(PoluError::Store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use types::{CostRule, Flow, Header, Key, Layer, Op, SLOTS, Table};

    /// Builds a key from slot values; "*" leaves a slot empty.
    fn key(values: &[&str]) -> Key {
        let mut fields: [Option<Box<str>>; SLOTS] = Default::default();
        for (slot, v) in fields.iter_mut().zip(values) {
            *slot = (*v != "*").then(|| (*v).into());
        }
        Key::new(fields)
    }

    fn header(names: &[&str]) -> Header {
        let mut h: Header = Default::default();
        for (slot, n) in h.iter_mut().zip(names) {
            *slot = Some((*n).into());
        }
        h
    }

    fn rule(source: &[&str], sink: &[&str], priority: i8, value: f64) -> CostRule {
        CostRule {
            source: key(source),
            sink: key(sink),
            priority,
            value,
        }
    }

    fn layered(layers: Vec<(Op, Vec<CostRule>)>) -> CostMap {
        let layers = layers
            .into_iter()
            .map(|(op, rules)| Layer { op, rules })
            .collect();
        CostMap::new(
            header(&["region", "site"]),
            header(&["region", "store"]),
            layers,
        )
        .unwrap()
    }

    /// A costmap with a single `Set` layer.
    fn costmap(rules: Vec<CostRule>) -> CostMap {
        layered(vec![(Op::Set, rules)])
    }

    /// Cost of one source/sink pair, resolved on its own.
    fn cost_of(cm: &CostMap, source: Key, sink: Key) -> Option<f64> {
        let sources = Table::new(header(&["region", "site"]), vec![(source, 1)]).unwrap();
        let sinks = Table::new(header(&["region", "store"]), vec![(sink, 1)]).unwrap();
        Some(cm.resolve(&sources, &sinks).unwrap()[0]).filter(|c| !c.is_nan())
    }

    fn sources() -> SourceList {
        Table::new(
            header(&["region", "site"]),
            vec![(key(&["N", "w1"]), 10), (key(&["S", "w2"]), 5)],
        )
        .unwrap()
    }

    fn sinks() -> SinkList {
        Table::new(
            header(&["region", "store"]),
            vec![(key(&["N", "s1"]), 4), (key(&["S", "s2"]), 6)],
        )
        .unwrap()
    }

    #[test]
    fn priority_beats_specificity() {
        let cm = costmap(vec![
            rule(&["N", "w1"], &["N", "s1"], 0, 1.0),
            rule(&["*"], &["*"], 1, 9.0),
        ]);
        assert_eq!(
            cost_of(&cm, key(&["N", "w1"]), key(&["N", "s1"])),
            Some(9.0)
        );
    }

    #[test]
    fn specificity_breaks_priority_ties() {
        let cm = costmap(vec![
            rule(&["N"], &["*"], 0, 5.0),
            rule(&["N", "w1"], &["N"], 0, 2.0),
        ]);
        assert_eq!(
            cost_of(&cm, key(&["N", "w1"]), key(&["N", "s1"])),
            Some(2.0)
        );
    }

    #[test]
    fn earlier_row_wins_full_ties() {
        let cm = costmap(vec![
            rule(&["N"], &["*"], 0, 3.0),
            rule(&["*"], &["N"], 0, 4.0),
        ]);
        assert_eq!(
            cost_of(&cm, key(&["N", "w1"]), key(&["N", "s1"])),
            Some(3.0)
        );
    }

    #[test]
    fn unmatched_pairs_get_no_route() {
        let cm = costmap(vec![
            rule(&["N"], &["N"], 0, 1.0),
            rule(&["S"], &["S"], 0, 2.0),
        ]);
        let problem = formulate(&sources(), &sinks(), &cm).unwrap();
        assert_eq!(
            problem.routes,
            vec![
                Route {
                    source: 0,
                    sink: 0,
                    cost: 1.0
                },
                Route {
                    source: 1,
                    sink: 1,
                    cost: 2.0
                }
            ]
        );
        let amounts = |limits: &[Limit]| limits.iter().map(|l| l.amount).collect::<Vec<_>>();
        assert_eq!(amounts(&problem.supply), [10, 5]);
        assert_eq!(amounts(&problem.demand), [4, 6]);
    }

    #[test]
    fn catch_all_rule_opens_every_pair() {
        let cm = costmap(vec![
            rule(&["N"], &["N"], 0, 1.0),
            rule(&["*"], &["*"], i8::MIN, 100.0),
        ]);
        let problem = formulate(&sources(), &sinks(), &cm).unwrap();
        assert_eq!(problem.routes.len(), 4);
        assert_eq!(problem.routes[0].cost, 1.0);
        assert_eq!(problem.routes[3].cost, 100.0);
    }

    #[test]
    fn duplicate_rows_are_rejected() {
        let rows = vec![(key(&["N", "w1"]), 1), (key(&["N", "w1"]), 2)];
        assert!(matches!(
            Table::new(header(&["region", "site"]), rows),
            Err(PoluError::Invalid(_))
        ));
    }

    #[test]
    fn negative_quantities_are_rejected() {
        let rows = vec![(key(&["N", "w1"]), -1)];
        assert!(matches!(
            Table::new(header(&["region", "site"]), rows),
            Err(PoluError::Invalid(_))
        ));
    }

    #[test]
    fn non_finite_costs_are_rejected() {
        let layers = vec![Layer {
            op: Op::Set,
            rules: vec![rule(&["*"], &["*"], 0, f64::INFINITY)],
        }];
        assert!(matches!(
            CostMap::new(header(&["a"]), header(&["b"]), layers),
            Err(PoluError::Invalid(_))
        ));
    }

    #[test]
    fn mismatched_headers_are_rejected() {
        let cm = CostMap::new(header(&["site"]), header(&["region", "store"]), vec![]).unwrap();
        assert!(matches!(
            formulate(&sources(), &sinks(), &cm),
            Err(PoluError::Invalid(_))
        ));
    }

    /// The apples example: a base cost per pair, then a block.
    fn apples() -> (SourceList, SinkList, Vec<CostRule>) {
        let sources = Table::new(
            header(&["region", "site"]),
            vec![
                (key(&["apple inc", "NY"]), 7),
                (key(&["bananas and more", "SF"]), 5),
            ],
        )
        .unwrap();
        let sinks = Table::new(
            header(&["region", "store"]),
            vec![
                (key(&["stores and us", "WA"]), 3),
                (key(&["ohio fruits", "OH"]), 4),
            ],
        )
        .unwrap();
        let base = vec![
            rule(&["apple inc"], &["stores and us"], 0, 2.0),
            rule(&["apple inc"], &["ohio fruits"], 0, 1.0),
            rule(&["bananas and more"], &["stores and us"], 0, 1.0),
            rule(&["bananas and more"], &["ohio fruits"], 0, 3.0),
        ];
        (sources, sinks, base)
    }

    /// Resolved costs, row-major, with `None` for pairs that can't be used.
    fn costs(cm: &CostMap, sources: &SourceList, sinks: &SinkList) -> Vec<Option<f64>> {
        cm.resolve(sources, sinks)
            .unwrap()
            .into_iter()
            .map(|c| Some(c).filter(|c| !c.is_nan()))
            .collect()
    }

    #[test]
    fn block_removes_a_route() {
        let (sources, sinks, base) = apples();
        let cm = layered(vec![
            (Op::Set, base),
            (
                Op::Block,
                vec![rule(&["apple inc"], &["stores and us"], 0, 0.0)],
            ),
        ]);
        assert_eq!(
            costs(&cm, &sources, &sinks),
            [None, Some(1.0), Some(1.0), Some(3.0)]
        );
    }

    #[test]
    fn block_is_final() {
        let (sources, sinks, base) = apples();
        let cm = layered(vec![
            (Op::Set, base),
            (Op::Block, vec![rule(&["apple inc"], &["*"], 0, 0.0)]),
            (Op::Set, vec![rule(&["*"], &["*"], 0, 9.0)]),
        ]);
        assert_eq!(
            costs(&cm, &sources, &sinks),
            [None, None, Some(9.0), Some(9.0)]
        );
    }

    #[test]
    fn scale_and_add_change_existing_costs_only() {
        let (sources, sinks, mut base) = apples();
        base.remove(3); // bananas and more -> ohio fruits has no cost
        let cm = layered(vec![
            (Op::Set, base),
            (Op::Scale, vec![rule(&["bananas and more"], &["*"], 0, 2.0)]),
            (Op::Add, vec![rule(&["*"], &["ohio fruits"], 0, 0.5)]),
        ]);
        assert_eq!(
            costs(&cm, &sources, &sinks),
            [Some(2.0), Some(1.5), Some(2.0), None]
        );
    }

    #[test]
    fn one_rule_wins_per_layer() {
        // overlapping rules in a Scale layer don't compound: the higher priority factor applies alone
        let (sources, sinks, base) = apples();
        let cm = layered(vec![
            (Op::Set, base),
            (
                Op::Scale,
                vec![
                    rule(&["apple inc"], &["*"], 0, 10.0),
                    rule(&["*"], &["ohio fruits"], 1, 3.0),
                ],
            ),
        ]);
        assert_eq!(
            costs(&cm, &sources, &sinks),
            [Some(20.0), Some(3.0), Some(1.0), Some(9.0)]
        );
    }

    #[test]
    fn empty_row_slots_match_any_rule_value() {
        let sources =
            Table::new(header(&["region", "site"]), vec![(key(&["*", "w1"]), 1)]).unwrap();
        let cm = costmap(vec![rule(&["N", "w1"], &["*"], 0, 4.0)]);
        assert_eq!(costs(&cm, &sources, &sinks()), [Some(4.0), Some(4.0)]);
    }

    /// Reference: evaluates every layer for every pair with `Key::matches`, no indexes.
    fn naive(cm: &CostMap, sources: &Table, sinks: &Table) -> Vec<f64> {
        let mut out = Vec::new();
        for (i, (s, _)) in sources.rows().iter().enumerate() {
            for (j, (k, _)) in sinks.rows().iter().enumerate() {
                let start = cm
                    .base()
                    .map_or(f64::NAN, |b| b[i * sinks.rows().len() + j]);
                let (mut cost, mut blocked) = (start, false);
                for layer in cm.layers() {
                    let winner = layer
                        .rules
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.source.matches(s) && r.sink.matches(k))
                        .max_by_key(|(i, r)| (r.priority, r.specificity(), std::cmp::Reverse(*i)));
                    let Some((_, r)) = winner else { continue };
                    if blocked {
                        continue;
                    }
                    match layer.op {
                        Op::Set => cost = r.value,
                        Op::Scale => cost *= r.value,
                        Op::Add => cost += r.value,
                        Op::Block => (cost, blocked) = (f64::NAN, true),
                    }
                }
                out.push(cost);
            }
        }
        out
    }

    #[test]
    fn base_matrix_is_the_starting_cost() {
        let (sources, sinks, _) = apples();
        let cm = layered(vec![
            (Op::Scale, vec![rule(&["bananas and more"], &["*"], 0, 2.0)]),
            (
                Op::Block,
                vec![rule(&["apple inc"], &["stores and us"], 0, 0.0)],
            ),
        ])
        .with_base(vec![2.0, 1.0, 1.0, f64::NAN])
        .unwrap();
        assert_eq!(
            costs(&cm, &sources, &sinks),
            [None, Some(1.0), Some(2.0), None]
        );
    }

    #[test]
    fn base_matrix_must_fit_the_tables() {
        let cm = costmap(vec![]).with_base(vec![1.0; 3]).unwrap();
        assert!(matches!(
            formulate(&sources(), &sinks(), &cm),
            Err(PoluError::Invalid(_))
        ));
        assert!(costmap(vec![]).with_base(vec![f64::INFINITY]).is_err());
    }

    #[test]
    fn partial_rows_limit_the_rows_they_cover() {
        let sources = Table::new(
            header(&["region", "site"]),
            vec![
                (key(&["apple inc", "large"]), 3),
                (key(&["apple inc"]), 5),
                (key(&["bananas and more", "large"]), 4),
                (key(&["*", "large"]), 6),
            ],
        )
        .unwrap();
        let cm = costmap(vec![rule(&["*"], &["*"], 0, 1.0)]);
        let problem = formulate(&sources, &sinks(), &cm).unwrap();
        let rows = |limits: &[Limit]| limits.iter().map(|l| l.rows.clone()).collect::<Vec<_>>();
        // [apple inc, *] covers [apple inc, large]; [*, large] covers both large rows
        assert_eq!(
            rows(&problem.supply),
            [vec![0], vec![1, 0], vec![2], vec![3, 0, 2]]
        );
        assert_eq!(rows(&problem.demand), [vec![0], vec![1]]);
    }

    /// Business total 5 over [large] = 3 and [small] = 7: at most 5 apple inc units ship in total,
    /// while each size keeps its own cost.
    #[cfg(feature = "highs")]
    #[test]
    fn business_total_caps_every_row_it_covers() {
        use crate::solver::HighsSolver;

        let sources = Table::new(
            header(&["region", "site"]),
            vec![
                (key(&["apple inc"]), 5),
                (key(&["apple inc", "large"]), 3),
                (key(&["apple inc", "small"]), 7),
                (key(&["bananas and more"]), 20),
            ],
        )
        .unwrap();
        let sinks = |demand| {
            Table::new(
                header(&["region", "store"]),
                vec![(key(&["ohio fruits"]), demand)],
            )
            .unwrap()
        };
        let cm = costmap(vec![
            rule(&["apple inc"], &["*"], 0, 3.0),
            rule(&["apple inc", "large"], &["*"], 0, 1.0),
            rule(&["apple inc", "small"], &["*"], 0, 2.0),
            rule(&["bananas and more"], &["*"], 0, 10.0),
        ]);

        let problem = formulate(&sources, &sinks(9), &cm).unwrap();
        // the business limit spans all three apple inc rows; each size row keeps its own cost
        assert_eq!(
            problem.supply[0],
            Limit {
                rows: vec![0, 1, 2],
                amount: 5
            }
        );
        let route_costs: Vec<f64> = problem.routes[1..].iter().map(|r| r.cost).collect();
        assert_eq!(route_costs, [1.0, 2.0, 10.0]);

        // 9 demanded: apple inc rows are cheaper but capped at 5 together, bananas and more covers 4
        let a = HighsSolver::default().solve(&problem).unwrap();
        let shipped = |rows: &[usize]| -> i64 {
            a.flows
                .iter()
                .filter(|f| rows.contains(&f.source))
                .map(|f| f.units)
                .sum()
        };
        assert_eq!(shipped(&[0, 1, 2]), 5);
        assert_eq!(shipped(&[3]), 4);

        // without bananas and more, 9 can't be met: 3 + 7 looks available, but the cap is 5
        let apples_only =
            Table::new(header(&["region", "site"]), sources.rows()[..3].to_vec()).unwrap();
        let problem = formulate(&apples_only, &sinks(9), &cm).unwrap();
        assert!(HighsSolver::default().solve(&problem).is_err());
    }

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, m: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % m
        }

        /// A key over 3 slots, each empty with probability `p_empty` percent.
        fn key(&mut self, p_empty: u64) -> Key {
            let slots: Vec<&str> = (0..3)
                .map(|_| match self.below(100) < p_empty {
                    true => "*",
                    false => ["x", "y", "z"][self.below(3) as usize],
                })
                .collect();
            key(&slots)
        }

        fn table(&mut self, n: usize) -> Table {
            let mut seen = std::collections::HashSet::new();
            let rows = (0..n)
                .map(|_| self.key(15))
                .filter(|k| seen.insert(k.clone()))
                .map(|k| (k, 1))
                .collect();
            Table::new(header(&["a", "b", "c"]), rows).unwrap()
        }

        fn layer(&mut self, op: Op) -> Layer {
            let rules = (0..12)
                .map(|_| CostRule {
                    source: self.key(60),
                    sink: self.key(60),
                    priority: self.below(3) as i8,
                    value: 1.0 + self.below(5) as f64,
                })
                .collect();
            Layer { op, rules }
        }
    }

    #[test]
    fn indexed_resolution_matches_the_reference() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..50 {
            let (sources, sinks) = (rng.table(25), rng.table(20));
            let ops = [Op::Set, Op::Scale, Op::Add, Op::Block, Op::Set];
            let layers = ops.iter().map(|&op| rng.layer(op)).collect();
            let h = header(&["a", "b", "c"]);
            let mut cm = CostMap::new(h.clone(), h, layers).unwrap();
            if rng.below(2) == 0 {
                // half the runs start from a base matrix, with some pairs unusable
                let cells = sources.rows().len() * sinks.rows().len();
                let base = (0..cells)
                    .map(|_| match rng.below(5) {
                        0 => f64::NAN,
                        n => n as f64,
                    })
                    .collect();
                cm = cm.with_base(base).unwrap();
            }
            let fast = cm.resolve(&sources, &sinks).unwrap();
            let slow = naive(&cm, &sources, &sinks);
            assert!(
                fast.iter()
                    .zip(&slow)
                    .all(|(a, b)| a == b || (a.is_nan() && b.is_nan()))
            );
        }
    }

    struct Fixed<T>(T);

    impl<T: Clone> Load<T> for Fixed<T> {
        fn load(&self) -> Result<T, BoxError> {
            Ok(self.0.clone())
        }
    }

    /// Sends each sink's demand along its first route.
    struct FirstRoute;

    impl Solver for FirstRoute {
        fn solve(&self, problem: &Problem) -> Result<Assignment, BoxError> {
            let mut flows = Vec::new();
            let mut objective = 0.0;
            for (j, demand) in problem.demand.iter().enumerate() {
                let route = problem
                    .routes
                    .iter()
                    .find(|r| r.sink == j)
                    .ok_or("infeasible")?;
                flows.push(Flow {
                    source: route.source,
                    sink: j,
                    units: demand.amount,
                });
                objective += route.cost * demand.amount as f64;
            }
            Ok(Assignment { flows, objective })
        }
    }

    struct Capture(Rc<RefCell<Option<Assignment>>>);

    impl Store for Capture {
        fn store(&self, assignment: &Assignment) -> Result<(), BoxError> {
            *self.0.borrow_mut() = Some(assignment.clone());
            Ok(())
        }
    }

    #[test]
    fn pipeline_runs_in_order() {
        let stored = Rc::new(RefCell::new(None));
        let cm = costmap(vec![
            rule(&["N"], &["N"], 0, 1.0),
            rule(&["S"], &["S"], 0, 2.0),
        ]);
        let mut polu = Polu::new(
            Box::new(Fixed(sources())),
            Box::new(Fixed(sinks())),
            Box::new(Fixed(cm)),
            Box::new(FirstRoute),
            Box::new(Capture(stored.clone())),
        );

        assert!(matches!(polu.run(), Err(PoluError::NotReady(_))));
        assert!(matches!(polu.store_data(), Err(PoluError::NotReady(_))));

        polu.load_data().unwrap();
        assert_eq!(polu.run().unwrap().objective, 4.0 * 1.0 + 6.0 * 2.0);
        polu.store_data().unwrap();
        assert_eq!(stored.borrow().as_ref().unwrap().flows.len(), 2);
    }

    #[test]
    fn solver_errors_are_wrapped() {
        let cm = costmap(vec![]);
        let mut polu = Polu::new(
            Box::new(Fixed(sources())),
            Box::new(Fixed(sinks())),
            Box::new(Fixed(cm)),
            Box::new(FirstRoute),
            Box::new(Capture(Rc::default())),
        );
        polu.load_data().unwrap();
        assert!(matches!(polu.run(), Err(PoluError::Solve(_))));
    }
}
