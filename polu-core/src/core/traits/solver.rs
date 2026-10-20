use crate::core::BoxError;
use crate::core::types::{Assignment, Problem};

/// Solver backend for polu (HiGHS, ...). The problem is already formulated by the core;
/// implementors only translate it, solve it with integer flows, and translate the result back.
/// An infeasible problem should be reported as an error.
pub trait Solver {
    fn solve(&self, problem: &Problem) -> Result<Assignment, BoxError>;
}
