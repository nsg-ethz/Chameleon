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

//! Module for creating and constraining variables for BGP soft constraints.
//!
//! # New Approach
//!
//! In the new approach, we use `r_old` to represent the round up to which the router will know the
//! old route, and `r_new` to represent the round up to which the router will know the new
//! route. Then, Then, we prepare the following:
//!
//! ```text
//! r_old[a] <= max(r_old[b], r_old[c], ...) - 1
//! r_new[a] >= min(r_new[b], r_new[c], ...) + 1
//! ```
//!
//! Further, we say that the router must change somewhere between `r_old` and `r_new`. In other
//! words:
//!
//! ```text
//! r_old[a] <= r <= r_new[a]
//! ```
//!
//! Finally, we create the const constraints to represent the difference of r_old and r_new, which
//! is essentially the time during which the router does not know its selected route.
//!
//! ```text
//! cost[a] == max(r_new[a] - r_old[a], 0)
//! ```
//!
//! Since we constrain `r_old[a] <= r <= r_new[a]`, which is equivalent to `r_old[a] <= r_new[a]`,
//! we can simplify the constraint to:
//!
//! ```text
//! cost[a] == r_new[a] - r_old[a]
//! ```
//!
//! # Previous Approach
//!
//! We perform the check for the soft constraints as follows: First, we create variables that
//! represent either the minimum over a set of new_from dependencies, or a maximum over a set of
//! old_from dependencies. Then, for each router that has this dependency, create a variable that
//! represents the cost. Finally, we create the cost by simply summing everything up. More details
//! follow:
//!
//! ## Old From Constraints
//! An old from constraint states that a router, in the following called `r` will update before the
//! last of its route reflectors will, called `r1`, `r2`, ... In other words, we have the soft
//! constraint:
//!
//! ```text
//! r < max(r1, r2, ...) = x
//! ```
//!
//! In the following, let `x = max(r1, r2, ...)`. The cost is then computed as follows:
//!
//! ```text
//! cost = | 0            if r < x
//!        | r - x + 1    otherwise
//!
//! cost = max(r - x + 1, 0)
//!      = max(r + 1, x) - x
//! ```
//!
//! ## New From Constraints
//! An new from constraint states that a router, in the following called `r` will update after the
//! first of its route reflectors will, called `r1`, `r2`, ... In other words, we have the soft
//! constraint:
//!
//! ```text
//! r > min(r1, r2, ...) = x
//! ```
//!
//! In the following, let `x = min(r1, r2, ...)`. The cost is then computed as follows:
//!
//! ```text
//! cost = | 0            if r > x
//!        | x - r + 1    otherwise
//!
//! cost = max(x - r + 1, 0)
//!      = max(x + 1, r) - r
//! ```

use std::collections::{BTreeSet, HashMap};

use bgpsim::types::RouterId;
use good_lp::{constraint, Expression, ProblemVariables, SolverModel};
use itertools::Itertools;

use crate::decomposition::bgp_dependencies::{BgpDependencies, BgpDependency};

use super::{or_tools::*, IlpVars};

/// Type used for representing the minimum or maximum of dependencies.
pub(super) type MinMaxDepsType = HashMap<(BTreeSet<RouterId>, ConstraintType), MinMaxVariable>;

/// Create variables used for `min_max`.
pub(super) fn min_max_variables(
    p: &mut ProblemVariables,
    bgp_deps: Option<&BgpDependencies>,
    max_steps: usize,
) -> MinMaxDepsType {
    bgp_deps
        .into_iter()
        .flat_map(HashMap::values)
        .flat_map(|BgpDependency { old_from, new_from }| {
            [
                (old_from, ConstraintType::OldFrom),
                (new_from, ConstraintType::NewFrom),
            ]
        })
        .filter(|(rs, _)| rs.len() > 1)
        .unique()
        .map(|(rs, ty)| {
            (
                (rs.clone(), ty),
                MinMaxVariable::new(p, rs.len(), max_steps as f64),
            )
        })
        .collect()
}

/// Setup constraints for the minimum and maximum values
pub(super) fn min_max_deps_constraints(problem: &mut impl SolverModel, vars: &IlpVars) {
    for ((nodes, ty), m) in vars.min_max.iter() {
        match ty {
            ConstraintType::OldFrom => {
                let a = nodes
                    .iter()
                    .map(|x| {
                        if let Some(v) = vars.r_old.get(x) {
                            Expression::from(*v)
                        } else {
                            Expression::from(vars.max_steps as i32)
                        }
                    })
                    .collect();
                c_max(problem, m, a, vars.max_steps as f64)
            }
            ConstraintType::NewFrom => {
                let a = nodes
                    .iter()
                    .map(|x| {
                        if let Some(v) = vars.r_new.get(x) {
                            Expression::from(*v)
                        } else {
                            Expression::from(0)
                        }
                    })
                    .collect();
                c_min(problem, m, a, vars.max_steps as f64)
            }
        }
    }
}

/// Setup the constraints for the variables encoding the BGP propagation rules.
pub(super) fn bgp_propagation_constraints(
    p: &mut impl SolverModel,
    vars: &IlpVars,
    bgp_deps: Option<&BgpDependencies>,
) {
    for r_id in vars.r.keys() {
        let r = vars.r[r_id];
        let r_old = vars.r_old[r_id];
        let r_new = vars.r_new[r_id];

        // extract old_from and new_from
        let (old_from, new_from) = match bgp_deps.and_then(|x| x.get(r_id)) {
            Some(BgpDependency { old_from, new_from }) => (old_from.clone(), new_from.clone()),
            None => (Default::default(), Default::default()),
        };

        // set constraints on old_from
        if old_from.is_empty() {
            p.add_constraint(constraint!(r_old <= vars.max_steps as i32));
        } else if old_from.len() == 1 {
            let dep = old_from.iter().next().unwrap();
            if let Some(v) = vars.r_old.get(dep) {
                p.add_constraint(constraint!(r_old <= *v - 1));
            } else {
                p.add_constraint(constraint!(r_old <= vars.max_steps as i32));
            }
        } else {
            let max = vars.min_max[&(old_from, ConstraintType::OldFrom)].x;
            p.add_constraint(constraint!(r_old <= max - 1));
        }

        // set constraints on new_from
        if new_from.is_empty() {
            p.add_constraint(constraint!(r_new >= 0));
        } else if new_from.len() == 1 {
            let dep = new_from.iter().next().unwrap();
            if let Some(v) = vars.r_new.get(dep) {
                p.add_constraint(constraint!(r_new >= *v + 1));
            } else {
                p.add_constraint(constraint!(r_new >= 0));
            }
        } else {
            let min = vars.min_max[&(new_from, ConstraintType::NewFrom)].x;
            p.add_constraint(constraint!(r_new >= min + 1));
        }

        // add the constraint that `r` is in between `r_old` and `r_new`
        p.add_constraint(constraint!(r_old <= r));
        p.add_constraint(constraint!(r <= r_new));
    }
}

/// Compute the complete cost for violating bgp constraints.
pub(super) fn bgp_cost_expression(vars: &IlpVars) -> Expression {
    let mut bgp_cost = Expression::from(0);

    // go through all variables
    for (old, new) in vars.session_needed.values() {
        bgp_cost += *old + *new;
    }

    bgp_cost
}

/// crate the expressions to check if a temporary session is needed.
pub(super) fn temp_session_needed_constraints(problem: &mut impl SolverModel, vars: &IlpVars) {
    let big_m = vars.max_steps as f64 * 2.0;
    for (router, (old, new)) in vars.session_needed.iter() {
        let r_old = vars.r_old[router];
        let r_fw = vars.r[router];
        let r_new = vars.r_new[router];

        inequality(problem, *old, r_old, r_fw, big_m);
        inequality(problem, *new, r_fw, r_new, big_m);
    }
}

/// Description of the type of BGP constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum ConstraintType {
    /// OldFrom constraint: `r < max(r1, r2, ...)`
    OldFrom,
    /// NewFrom constraint: `r > min(r1, r2, ...)`
    NewFrom,
}
