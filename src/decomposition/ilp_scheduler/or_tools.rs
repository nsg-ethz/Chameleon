// Chameleon: Taming the transient while reconfiguring BGP
// Copyright (C) 2023 Tibor Schneider <sctibor@ethz.ch>
//
// This program is free software; you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation; either version 2 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, write to the Free Software Foundation, Inc.,
// 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA.

//! OR tools and utilities to create MILP systems.

use std::iter::repeat_with;

use good_lp::{
    constraint, variable, Expression, IntoAffineExpression, ProblemVariables, SolverModel, Variable,
};

/// Structure storing the variables needed to execute a min or a max operation of a set of other
/// variables.
#[derive(Debug)]
pub struct MinMaxVariable {
    /// Variable that represents the minimum or maximum.
    pub x: Variable,
    /// binary variable that represents the decision.
    pub b: Vec<Variable>,
}

impl MinMaxVariable {
    /// Create a new `MinMaxVariable`
    pub fn new(problem: &mut ProblemVariables, k: usize, upper_bound: f64) -> Self {
        Self {
            x: problem.add(variable().integer().min(0).max(upper_bound)),
            b: repeat_with(|| problem.add(variable().binary()))
                .take(k)
                .collect(),
        }
    }
}

/// Create the constraint to make `m.x` be equal to the minimum value of `a`. For each variable in
/// `a`, this function will create the following two constraints:
///
/// ```text
/// x <= a_i
/// x >= a_i - M * (1 - b_i)
/// ```
///
/// As a result, `x` must be smaller than any of the `a_i`, and it must be equal to `a_i` only if
/// `b_i == 1`. Finally, we assert that the sum of all `b_i` is equal to 1.
pub fn c_min(
    problem: &mut impl SolverModel,
    m: &MinMaxVariable,
    mut a: Vec<impl IntoAffineExpression>,
    big_m: f64,
) {
    assert_eq!(a.len(), m.b.len());
    match m.b.len() {
        0 => {}
        1 => {
            problem.add_constraint(constraint!(m.x == a.pop().unwrap().into_expression()));
        }
        _ => {
            for (a_i, b_i) in a
                .into_iter()
                .map(|x| x.into_expression())
                .zip(m.b.iter().copied())
            {
                problem.add_constraint(constraint!(m.x <= a_i.clone()));
                problem.add_constraint(constraint!(m.x >= a_i - 2.0 * big_m * (1 - b_i)));
            }
            let sum_b =
                m.b.iter()
                    .copied()
                    .map(Expression::from)
                    .reduce(|a, b| a + b)
                    .unwrap();
            problem.add_constraint(constraint!(sum_b == 1));
        }
    }
}

/// Create the constraint to make `m.x` be equal to the maximum value of `a`. For each variable in
/// `a`, this function will create the following two constraints:
///
/// ```text
/// x >= a_i
/// x <= a_i + M * (1 - b_i)
/// ```
///
/// As a result, `x` must be smaller than any of the `a_i`, and it must be equal to `a_i` only if
/// `b_i == 1`. Finally, we assert that the sum of all `b_i` is equal to 1.
pub fn c_max(
    problem: &mut impl SolverModel,
    m: &MinMaxVariable,
    mut a: Vec<impl IntoAffineExpression>,
    big_m: f64,
) {
    assert_eq!(a.len(), m.b.len());
    match m.b.len() {
        0 => {}
        1 => {
            problem.add_constraint(constraint!(m.x == a.pop().unwrap().into_expression()));
        }
        _ => {
            for (a_i, b_i) in a
                .into_iter()
                .map(|x| x.into_expression())
                .zip(m.b.iter().copied())
            {
                problem.add_constraint(constraint!(m.x >= a_i.clone()));
                problem.add_constraint(constraint!(m.x <= a_i + 2.0 * big_m * (1 - b_i)));
            }
            let sum_b =
                m.b.iter()
                    .copied()
                    .map(Expression::from)
                    .reduce(|a, b| a + b)
                    .unwrap();
            problem.add_constraint(constraint!(sum_b == 1));
        }
    }
}

/// Implement a simple if then else. This will implement the following: `x = y if b else z`. It is
/// implemented as follows:
///
/// ```text
/// x >= y - (1 - b)
/// x <= y + (1 - b)
/// x >= z - b
/// x <= z + b
/// ```
///
/// Make sure that all variables are binary decision variables!
pub fn c_if_then_else(
    problem: &mut impl SolverModel,
    b: impl IntoAffineExpression + Clone,
    x: impl IntoAffineExpression + Clone,
    y: impl IntoAffineExpression + Clone,
    z: impl IntoAffineExpression + Clone,
) {
    let b = || b.clone().into_expression();
    let x = || x.clone().into_expression();
    let y = || y.clone().into_expression();
    let z = || z.clone().into_expression();
    problem.add_constraint(constraint!(x() >= y() - (1.0 - b())));
    problem.add_constraint(constraint!(x() <= y() + (1.0 - b())));
    problem.add_constraint(constraint!(x() >= z() - b()));
    problem.add_constraint(constraint!(x() <= z() + b()));
}

/// Implement a simple if then else. This will implement the following: `x = y if b else z`. Here,
/// both `y` and `z` are already known boolean values. This is implemented as follows:
///
/// ```text
/// x = 1     if  y &&  z
///     0     if !y && !z
///     b     if  y && !z
///     1 - b if !y &&  z
/// ```
pub fn c_if_then_else_yz(
    problem: &mut impl SolverModel,
    b: Variable,
    x: Variable,
    y: bool,
    z: bool,
) {
    problem.add_constraint(match (y, z) {
        (true, true) => constraint!(x == 1),
        (true, false) => constraint!(x == b),
        (false, true) => constraint!(x == 1 - b),
        (false, false) => constraint!(x == 0),
    });
}

/// Implement a conjunction of all variables. This is done in the following way:
///
/// ```text
/// x, a, b, c: Bool,
/// x >= (a + b + c) - 2
/// x <= a
/// x <= b
/// x <= c
/// ```
pub fn c_all(problem: &mut impl SolverModel, x: Variable, vars: Vec<Variable>) {
    for y in vars.iter() {
        problem.add_constraint(constraint!(x <= *y));
    }
    let n = vars.len() as f64;
    let sum: Expression = vars.into_iter().sum();
    problem.add_constraint(constraint!(x >= sum - n + 1.0));
}

/// Implement a disjunction of all variables. This is done in the following way:
///
/// ```text
/// x, a, b, c: Bool,
/// x <= (a + b + c)
/// x >= a
/// x >= b
/// x >= c
/// ```
pub fn c_any(problem: &mut impl SolverModel, x: Variable, vars: Vec<Variable>) {
    for y in vars.iter() {
        problem.add_constraint(constraint!(x >= *y));
    }
    let sum: Expression = vars.into_iter().sum();
    problem.add_constraint(constraint!(x <= sum));
}

/// Add constraints of inequality. In other words, add constraints such that `x = 1 if a < b else 0`.
/// This function requires that `a <= b`, and that `x` is a boolean balue. We do this in the
/// following way, using big-M notation:
///
/// ```text
/// x <= b - a
/// x >= b - a / M
/// ```
pub fn inequality(
    problem: &mut impl SolverModel,
    x: Variable,
    smaller: Variable,
    larger: Variable,
    big_m: f64,
) {
    let small_m = 1.0 / big_m;
    problem.add_constraint(constraint!(x <= larger - smaller));
    problem.add_constraint(constraint!(x >= (larger - smaller) * small_m));
}
