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

//! Module responsible for managing the `changed`, `changed_step`, and `changed_step_path`
//! variables.
//!
//! The `changed_step` and `changed_step_path` variables make sure that no two routers along a path
//! can change at the same time. It requires that only one of them can change in a single round. If
//! we would not add these constraints, then the model would simply perform all updates in the first
//! step.
//!
//! # Force a proper ordering
//! The main idea to force a proper ordering is similar to checking for forwarding policies, but a
//! bit more complex. For this, we need to know if a router has already changed previously to this
//! step or in this step (i.e., `b`), and a variable telling if we have changed in this specific
//! step (i.e., `n`). Then, we create the boolean variables `x` that encode if a router has seen a
//! change in this step by itself, or by any other router along its path. In case the router has
//! changed in this specific step, we need to check both the old and the new path.
//!
//! To enforce this, we set the boolean variable `x` to be the sum of `n` and the same variable
//! of the old or new (or both) next-hops. For the entire thing to work, we need a temporary
//! variable `t`. Additionally, we need the variable `x = x(grouter)` telling if the router has
//! changed, and we also need the variable `y = x(gnh_after)` and `z = x(nh_before)`
//!
//! +-----+--------+---------------------------------------------+
//! | `x` | binary | `x(router)`                                 |
//! | `n` | binary | has changed at this step                    |
//! | `b` | binary | has already changed (this or previous step) |
//! | `y` | binary | `x(nh_after)`                               |
//! | `z` | binary | `x(nh_before)`                              |
//! | `t` | binary | temporary variable                          |
//! +-----+--------+---------------------------------------------+
//!
//! The high-level idea is to encode the following equation. This way, since `x` is a binary
//! variable, it cannot take the value 2 or 3, which means that either the router changes at the
//! current timestep (`n = 1`), or somewhere along the path, something has changed, but not both!
//!
//! ```text
//! x = n + (y if b else z) + (z if n else 0)
//!   = n + t + (y if b else z) with t = (z if n else 0)
//! ```
//!
//! We rewrite this as follows, such that we can use the established way of performing selection:
//!
//! ```text
//!         t = z if n else 0
//! x - n - t = y if b else z
//! ```
//!
//! Using big-M technique with M = 1, we can create the constraints for `t` as follows:
//!
//! ```text
//! t >= z - (n - 1)
//! t <= z + (n - 1)
//! t >= 0 - n
//! t <= 0 + n
//! ```
//!
//! Then, we use the exact same trick to set the constraints for variable `x`:
//!
//! ```text
//! x - n - t >= y - (b - 1)
//! x - n - t <= y + (b - 1)
//! x - n - t >= z - b
//! x - n - t <= z + b
//! ```

use std::{
    collections::{HashMap, HashSet},
    iter::repeat_with,
};

use bgpsim::types::RouterId;
use good_lp::{constraint, variable, Expression, ProblemVariables, SolverModel, Variable};
use itertools::Itertools;

use crate::{decomposition::CommandInfo, P};

use super::{or_tools::*, IlpVars};

/// Type definition for `changed` and `changed_step` variables
pub(super) type HasChangedType = HashMap<RouterId, Vec<Variable>>;
/// Type definition for `changed_step_path`.
pub(super) type HasChangedPathType = HashMap<RouterId, Vec<(Variable, Variable)>>;

/// Create variables used for `changed` and `changed_step`.
pub(super) fn has_changed_variables(
    p: &mut ProblemVariables,
    nodes: &HashSet<RouterId>,
    max_steps: usize,
) -> HasChangedType {
    // Create the boolean variables for every possible condition, router and step.
    nodes
        .iter()
        .copied()
        .zip(repeat_with(|| -> Vec<Variable> {
            repeat_with(|| p.add(variable().binary()))
                .take(max_steps)
                .collect_vec()
        }))
        .collect()
}

/// Create variables used for `changed_step_path` variables.
pub(super) fn has_changed_path_variables(
    p: &mut ProblemVariables,
    all_nodes: &HashSet<RouterId>,
    max_steps: usize,
) -> HasChangedPathType {
    // Create the boolean variables for every possible condition, router and step.
    all_nodes
        .iter()
        .copied()
        .zip(repeat_with(|| -> Vec<(Variable, Variable)> {
            repeat_with(|| (p.add(variable().binary()), p.add(variable().binary())))
                .take(max_steps)
                .collect_vec()
        }))
        .collect()
}

/// Setup all constraints for the boolean variable encoding if a router has already changed its
/// decision at this point.
///
/// Create constraints to make all `b`s to encode if the router has already changed. We do that in
/// two steps: (1) we constrain that `b[i] >= b[i - 1]`, and second, we constrain that the sum
/// `max_steps - sum(b[i]) == round`.
pub(super) fn has_changed_constraints(problem: &mut impl SolverModel, vars: &IlpVars) {
    for (node, round) in vars.r.iter() {
        let bs = &vars.b[node];

        for (b_prev, b_next) in bs[0..(bs.len() - 1)]
            .iter()
            .copied()
            .zip(bs[1..].iter().copied())
        {
            problem.add_constraint(constraint!(b_prev <= b_next));
        }

        let sum = bs.iter().fold(Expression::from(0), |acc, b| acc + (1 - *b));
        problem.add_constraint(constraint!(sum == round));
    }
}

/// Setup all constraints for the boolean variable encoding if a router has already changed its
/// decision at this point.
pub(super) fn has_changed_path_constraints<Q>(
    problem: &mut impl SolverModel,
    vars: &IlpVars,
    info: &CommandInfo<'_, Q>,
    prefix: P,
) {
    // create the changed_step variables. To do that, reate the constraints to make `n = 1` if
    // `round == step` and `n = 0` otherwise. This is done by subtracting: `n[i] = b[i] - b[i-1]`.
    for r in vars.r.keys() {
        let bs = &vars.b[r];
        let ns = &vars.n[r];
        for i in 0..bs.len() {
            let b_next = bs[i];
            let n = ns[i];
            if i == 0 {
                problem.add_constraint(constraint!(n == b_next));
            } else {
                let b_prev = bs[i - 1];
                problem.add_constraint(constraint!(n == b_next - b_prev));
            }
        }
    }

    // create the condition for the path.
    let ps = &vars.p;

    for (r, r_ps) in ps {
        let nhz = info.fw_before.get_next_hops(*r, prefix).last().copied();
        let nhy = info.fw_after.get_next_hops(*r, prefix).last().copied();
        for (round, (p, t)) in r_ps.iter().enumerate() {
            match (nhy, nhz) {
                (None, None) => {
                    problem.add_constraint(constraint!(*p == 0));
                }
                (Some(nhy), Some(nhz)) if nhy == nhz => {
                    problem.add_constraint(constraint!(*p == ps[&nhy][round].0));
                }
                (None, Some(nhz)) => {
                    let n = vars.n[r][round];
                    let b = vars.b[r][round];
                    let y = 0.0;
                    let z = ps[&nhz][round].0;
                    c_if_then_else(problem, n, *t, z, 0.0);
                    c_if_then_else(problem, b, (*p) - n - (*t), y, z);
                }
                (Some(nhy), None) => {
                    let n = vars.n[r][round];
                    let b = vars.b[r][round];
                    let y = ps[&nhy][round].0;
                    let z = 0.0;
                    c_if_then_else(problem, n, *t, z, 0.0);
                    c_if_then_else(problem, b, (*p) - n - (*t), y, z);
                }
                (Some(nhy), Some(nhz)) => {
                    let n = vars.n[r][round];
                    let b = vars.b[r][round];
                    let y = ps[&nhy][round].0;
                    let z = ps[&nhz][round].0;
                    c_if_then_else(problem, n, *t, z, 0.0);
                    c_if_then_else(problem, b, (*p) - n - (*t), y, z);
                }
            }
        }
    }
}
