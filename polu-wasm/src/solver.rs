//! A [`Solver`] that hands the problem to a JS function as CPLEX LP text.

use js_sys::{Function, Reflect};
use polu_core::{Assignment, BoxError, Problem, Solver};
use wasm_bindgen::{JsCast, JsValue};

/// Calls `solve(lpText)` and reads back a result shaped like highs-js's (npm `highs`):
/// `{ Status: "Optimal", Columns: { x0: { Primal: 3 }, ... } }`, so `lp => highs.solve(lp)` works as is.
/// Column `x{i}` is route `i` (see [`Problem::to_lp`]). Any other solver can return the same shape.
pub struct JsSolver {
    solve: Function,
}

impl JsSolver {
    pub fn new(solve: Function) -> Self {
        Self { solve }
    }
}

impl Solver for JsSolver {
    fn solve(&self, problem: &Problem) -> Result<Assignment, BoxError> {
        // an LP with no columns has nothing to solve; same answer as HighsSolver
        if problem.routes.is_empty() {
            if problem.demand.iter().all(|d| d.amount == 0) {
                return Ok(problem.assignment(&[]));
            }
            return Err("infeasible: there are no routes to meet the demand".into());
        }

        let result = self
            .solve
            .call1(&JsValue::NULL, &problem.to_lp().into())
            .map_err(message)?;
        let status = get(&result, "Status")?.as_string().unwrap_or_default();
        match status.as_str() {
            "Optimal" => {}
            "Infeasible" => {
                return Err("infeasible: supply and routes can't meet every demand".into());
            }
            other => return Err(format!("the solver stopped without an optimum: {other:?}").into()),
        }

        let columns = get(&result, "Columns")?;
        let values = (0..problem.routes.len())
            .map(|i| {
                // a column the solver dropped carries no flow
                let column = get(&columns, &format!("x{i}"))?;
                Ok(get(&column, "Primal")
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0))
            })
            .collect::<Result<Vec<f64>, BoxError>>()?;
        Ok(problem.assignment(&values))
    }
}

fn get(target: &JsValue, key: &str) -> Result<JsValue, BoxError> {
    if !target.is_object() {
        return Err(format!("the solver's result has no `{key}`").into());
    }
    Reflect::get(target, &key.into()).map_err(message)
}

/// A thrown JS value as an error message.
pub fn message(e: JsValue) -> BoxError {
    match e.dyn_ref::<js_sys::Error>() {
        Some(error) => String::from(error.message()).into(),
        None => e.as_string().unwrap_or_else(|| format!("{e:?}")).into(),
    }
}
