//! HiGHS backend for [`Solver`], behind the `highs` cargo feature (on by default).

use highs::{ColProblem, HighsModelStatus, Sense};

use crate::core::BoxError;
use crate::core::traits::Solver;
use crate::core::types::{Assignment, Flow, Problem};

/// Solves the transport problem as a MIP: one integer column per route,
/// one `<= supply` row per source and one `== demand` row per sink.
#[derive(Debug, Clone, Default)]
pub struct HighsSolver {
    /// Print HiGHS's own log to stdout.
    pub verbose: bool,
}

impl Solver for HighsSolver {
    fn solve(&self, problem: &Problem) -> Result<Assignment, BoxError> {
        // HiGHS reports a model with no columns as empty rather than infeasible
        if problem.routes.is_empty() {
            if problem.demand.iter().all(|&d| d == 0) {
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
            .map(|&s| pb.add_row(..=s as f64))
            .collect();
        let demand: Vec<_> = problem
            .demand
            .iter()
            .map(|&d| pb.add_row(d as f64..=d as f64))
            .collect();
        for route in &problem.routes {
            pb.add_integer_column(
                route.cost,
                0..,
                [(supply[route.source], 1.0), (demand[route.sink], 1.0)],
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::Route;

    fn route(source: usize, sink: usize, cost: f64) -> Route {
        Route { source, sink, cost }
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
            supply: vec![6, 10],
            demand: vec![4, 5],
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
            supply: vec![2, 2],
            demand: vec![3],
        };
        let a = HighsSolver::default().solve(&problem).unwrap();
        assert_eq!(a.flows.iter().map(|f| f.units).sum::<i64>(), 3);
        assert_eq!(a.objective, 3.0);
    }

    #[test]
    fn short_supply_is_infeasible() {
        let problem = Problem {
            routes: vec![route(0, 0, 1.0)],
            supply: vec![2],
            demand: vec![3],
        };
        assert!(HighsSolver::default().solve(&problem).is_err());
    }

    #[test]
    fn unreachable_sink_is_infeasible() {
        let problem = Problem {
            routes: vec![route(0, 0, 1.0)],
            supply: vec![10],
            demand: vec![1, 1],
        };
        assert!(HighsSolver::default().solve(&problem).is_err());
    }

    #[test]
    fn no_routes_and_no_demand_is_empty() {
        let problem = Problem {
            routes: vec![],
            supply: vec![5],
            demand: vec![0],
        };
        let a = HighsSolver::default().solve(&problem).unwrap();
        assert!(a.flows.is_empty());
    }
}
