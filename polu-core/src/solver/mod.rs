//! HiGHS backend for [`Solver`], behind the `highs` cargo feature (on by default).

use highs::{ColProblem, HighsModelStatus, Row, Sense};

use crate::core::BoxError;
use crate::core::traits::Solver;
use crate::core::types::{Assignment, Flow, Limit, Problem};

/// Solves the transport problem as a MIP: one integer column per route, one `<=` row per supply limit
/// and one `==` row per demand limit. A route counts towards every limit containing its source or sink.
#[derive(Debug, Clone, Default)]
pub struct HighsSolver {
    /// Print HiGHS's own log to stdout.
    pub verbose: bool,
}

impl Solver for HighsSolver {
    fn solve(&self, problem: &Problem) -> Result<Assignment, BoxError> {
        // HiGHS reports a model with no columns as empty rather than infeasible
        if problem.routes.is_empty() {
            if problem.demand.iter().all(|d| d.amount == 0) {
                return Ok(Assignment {
                    flows: Vec::new(),
                    objective: 0.0,
                });
            }
            return Err("infeasible: there are no routes to meet the demand".into());
        }

        let mut pb = ColProblem::default();
        let supply: Vec<_> = problem
            .supply
            .iter()
            .map(|l| pb.add_row(..=l.amount as f64))
            .collect();
        let demand: Vec<_> = problem
            .demand
            .iter()
            .map(|l| pb.add_row(l.amount as f64..=l.amount as f64))
            .collect();
        let (in_supply, in_demand) = (
            rows_per_member(&problem.supply, &supply),
            rows_per_member(&problem.demand, &demand),
        );
        for route in &problem.routes {
            let factors = in_supply[route.source]
                .iter()
                .chain(&in_demand[route.sink])
                .map(|&row| (row, 1.0));
            pb.add_integer_column(route.cost, 0.., factors);
        }

        let mut model = pb
            .try_optimise(Sense::Minimise)
            .map_err(|status| format!("HiGHS rejected the problem: {status:?}"))?;
        if !self.verbose {
            model.make_quiet();
        }
        let solved = model
            .try_solve()
            .map_err(|status| format!("HiGHS failed: {status:?}"))?;
        match solved.status() {
            HighsModelStatus::Optimal => {}
            HighsModelStatus::Infeasible => {
                return Err("infeasible: supply and routes can't meet every demand".into());
            }
            other => return Err(format!("HiGHS stopped without an optimum: {other:?}").into()),
        }

        let mut flows = Vec::new();
        let mut objective = 0.0;
        for (route, &x) in problem.routes.iter().zip(solved.get_solution().columns()) {
            let units = x.round() as i64;
            if units > 0 {
                flows.push(Flow {
                    source: route.source,
                    sink: route.sink,
                    units,
                });
                // summed from the rounded flows so it matches them exactly
                objective += route.cost * units as f64;
            }
        }

        Ok(Assignment { flows, objective })
    }
}

/// For each table row, the HiGHS rows of the limits it belongs to.
fn rows_per_member(limits: &[Limit], rows: &[Row]) -> Vec<Vec<Row>> {
    let mut members = vec![Vec::new(); limits.len()];
    for (limit, &row) in limits.iter().zip(rows) {
        for &member in &limit.rows {
            members[member].push(row);
        }
    }
    members
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{Limit, Route};

    fn route(source: usize, sink: usize, cost: f64) -> Route {
        Route { source, sink, cost }
    }

    /// One independent limit per row.
    fn limits(amounts: &[i64]) -> Vec<Limit> {
        amounts
            .iter()
            .enumerate()
            .map(|(i, &amount)| Limit {
                rows: vec![i],
                amount,
            })
            .collect()
    }

    fn limit(rows: &[usize], amount: i64) -> Limit {
        Limit {
            rows: rows.to_vec(),
            amount,
        }
    }

    #[test]
    fn partial_source_caps_the_rows_it_covers() {
        // source 0 is [apple inc, *] = 5, covering source 1 [apple inc, large] = 3
        let supply = vec![limit(&[0, 1], 5), limit(&[1], 3)];
        let routes = vec![route(0, 0, 2.0), route(1, 0, 1.0)];
        let short = Problem {
            routes: routes.clone(),
            supply: supply.clone(),
            demand: limits(&[8]),
        };
        assert!(HighsSolver::default().solve(&short).is_err()); // independent pools would allow 8
        let fits = Problem {
            routes,
            supply,
            demand: limits(&[5]),
        };
        let a = HighsSolver::default().solve(&fits).unwrap();
        // the 3 large apples go first (cheaper), the business total caps the rest at 2
        assert_eq!(
            a.flows,
            vec![
                Flow {
                    source: 0,
                    sink: 0,
                    units: 2
                },
                Flow {
                    source: 1,
                    sink: 0,
                    units: 3
                }
            ]
        );
    }

    #[test]
    fn partial_sink_takes_the_remainder() {
        // sink 0 is [ohio fruits, *] = 10, covering sink 1 [ohio fruits, large] = 4
        let problem = Problem {
            routes: vec![route(0, 0, 1.0), route(0, 1, 1.0)],
            supply: limits(&[20]),
            demand: vec![limit(&[0, 1], 10), limit(&[1], 4)],
        };
        let a = HighsSolver::default().solve(&problem).unwrap();
        assert_eq!(
            a.flows,
            vec![
                Flow {
                    source: 0,
                    sink: 0,
                    units: 6
                },
                Flow {
                    source: 0,
                    sink: 1,
                    units: 4
                }
            ]
        );
    }

    #[test]
    fn picks_the_cheapest_assignment() {
        // source 0 is cheap for sink 0, source 1 for sink 1, but source 0 can't cover both
        let problem = Problem {
            routes: vec![
                route(0, 0, 1.0),
                route(0, 1, 2.0),
                route(1, 0, 5.0),
                route(1, 1, 3.0),
            ],
            supply: limits(&[6, 10]),
            demand: limits(&[4, 5]),
        };
        let a = HighsSolver::default().solve(&problem).unwrap();
        // 4 units 0->0 (4), 2 units 0->1 (4), 3 units 1->1 (9)
        assert_eq!(a.objective, 17.0);
        assert_eq!(
            a.flows,
            vec![
                Flow {
                    source: 0,
                    sink: 0,
                    units: 4
                },
                Flow {
                    source: 0,
                    sink: 1,
                    units: 2
                },
                Flow {
                    source: 1,
                    sink: 1,
                    units: 3
                },
            ]
        );
    }

    #[test]
    fn flows_are_integers() {
        // an LP relaxation could split 3 units 1.5/1.5; the MIP must not
        let problem = Problem {
            routes: vec![route(0, 0, 1.0), route(1, 0, 1.0)],
            supply: limits(&[2, 2]),
            demand: limits(&[3]),
        };
        let a = HighsSolver::default().solve(&problem).unwrap();
        assert_eq!(a.flows.iter().map(|f| f.units).sum::<i64>(), 3);
        assert_eq!(a.objective, 3.0);
    }

    #[test]
    fn short_supply_is_infeasible() {
        let problem = Problem {
            routes: vec![route(0, 0, 1.0)],
            supply: limits(&[2]),
            demand: limits(&[3]),
        };
        assert!(HighsSolver::default().solve(&problem).is_err());
    }

    #[test]
    fn unreachable_sink_is_infeasible() {
        let problem = Problem {
            routes: vec![route(0, 0, 1.0)],
            supply: limits(&[10]),
            demand: limits(&[1, 1]),
        };
        assert!(HighsSolver::default().solve(&problem).is_err());
    }

    #[test]
    fn no_routes_and_no_demand_is_empty() {
        let problem = Problem {
            routes: vec![],
            supply: limits(&[5]),
            demand: limits(&[0]),
        };
        let a = HighsSolver::default().solve(&problem).unwrap();
        assert!(a.flows.is_empty());
    }
}
