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

//! Module that contains the variable and constraints for loop protection
//!
//! The loop protection works similar to the idea of satisfying conditions on the forwarding
//! path. We create a new variable (this time, it is a regular, positive (real) number). Then, we
//! set the value of any router to be the value of its next hop **plus 1**. By adding 1 to the
//! number, we essentially prohibit loops, as `x = x + 2` is unsolvable. We need to make sure,
//! however, that we properly deal with changes in the graph, so we take either the old or the new
//! next-hop.

use std::{collections::HashSet, iter::zip};

use bgpsim::prelude::*;
use good_lp::{constraint, Expression, ProblemVariables, SolverModel};

use crate::{
    decomposition::{all_loops::all_loops, CommandInfo},
    P,
};

use super::IlpVars;

/// Type used for loop protection
pub(super) type LoopProtectionType = ();

/// Create all variables needed for the loop protection (i.e., none)
pub(super) fn loop_protection_variables(
    _p: &mut ProblemVariables,
    _all_nodes: &HashSet<RouterId>,
    _max_steps: usize,
) -> LoopProtectionType {
}

/// Setup the loop protection thing by computing all possible loops.
pub(super) fn loop_protection_constraints<Q>(
    problem: &mut impl SolverModel,
    vars: &IlpVars,
    info: &CommandInfo<'_, Q>,
    prefix: P,
) {
    #[allow(clippy::let_unit_value)]
    let _ = vars.loop_protection;
    // compute all loops
    for cycle in all_loops(info, prefix) {
        let mut cycle_shift = cycle.clone();
        cycle_shift.rotate_left(1);
        let cycle_state: Vec<_> = zip(cycle.into_iter(), cycle_shift.into_iter())
            .filter(|(a, _)| vars.r.contains_key(a))
            .map(|(a, b)| {
                (
                    a,
                    info.fw_after.get_next_hops(a, prefix).first() == Some(&b),
                )
            })
            .collect();
        assert!(cycle_state.len() >= 2);
        for step in 0..(vars.max_steps) {
            let sum = cycle_state
                .iter()
                .map(|(r, b)| {
                    if *b {
                        Expression::from(vars.b[r][step])
                    } else {
                        1 - vars.b[r][step]
                    }
                })
                .reduce(|a, b| a + b)
                .unwrap();
            problem.add_constraint(constraint!(sum <= cycle_state.len() as f64 - 1.0));
        }
    }
}

/*
use std::{
    collections::{HashMap, HashSet},
    iter::repeat_with,
};

use good_lp::{constraint, variable, ProblemVariables, SolverModel, Variable};
use itertools::Itertools;
use bgpsim::types::{Prefix, RouterId};

use crate::decomposition::CommandInfo;

use super::{or_tools::*, IlpVars};

/// Type used for loop protection
pub(super) type LoopProtectionType = HashMap<RouterId, Vec<Variable>>;

/// Create all variables needed for the loop protection
pub(super) fn loop_protection_variables(
    p: &mut ProblemVariables,
    all_nodes: &HashSet<RouterId>,
    max_steps: usize,
) -> LoopProtectionType {
    // Create the boolean variables for every possible condition, router and step.
    all_nodes
        .iter()
        .copied()
        .zip(repeat_with(|| -> Vec<Variable> {
            repeat_with(|| p.add(variable()))
                .take(max_steps)
                .collect_vec()
        }))
        .collect()
}

/// Setup the loop protection thing.
pub(super) fn loop_protection_constraints<Q>(
    problem: &mut impl SolverModel,
    vars: &IlpVars,
    info: &CommandInfo<'_, Q>,
    prefix: Prefix,
) {
    let vs = &vars.loop_protection;
    for (r, r_vs) in vs.iter().sorted_by_key(|x| x.0.index()) {
        // extract the next hop. Notice, that z is the state before the update (i.e., b = 0), and
        // y is the state after the update (i.e., b = 1).
        let nhz = info.fw_before.get_next_hops(*r, prefix).last().copied();
        let nhy = info.fw_after.get_next_hops(*r, prefix).last().copied();
        // go through every round
        for (round, dist_at_round) in r_vs.iter().enumerate() {
            match (nhz, nhy) {
                (None, None) => {
                    // No next hop in both cases
                    problem.add_constraint(constraint!(*dist_at_round == 0.0));
                }
                (Some(nhz), Some(nhy)) if nhz == nhy => {
                    // same next-hop in both cases
                    problem.add_constraint(constraint!(*dist_at_round == vs[&nhz][round] + 1.0));
                }
                (Some(nhz), Some(nhy)) => {
                    // Change in next-hop
                    let b = vars.changed[r][round];
                    let y = vs[&nhy][round] + 1.0;
                    let z = vs[&nhz][round] + 1.0;
                    c_if_then_else(problem, b, dist_at_round, y, z);
                }
                (None, Some(nhy)) => {
                    // From black hole to new next-hop
                    let b = vars.changed[r][round];
                    let y = vs[&nhy][round] + 1.0;
                    let z = 0.0;
                    c_if_then_else(problem, b, dist_at_round, y, z);
                }
                (Some(nhz), None) => {
                    // From next-hop to black-hole
                    let b = vars.changed[r][round];
                    let y = 0.0;
                    let z = vs[&nhz][round] + 1.0;
                    c_if_then_else(problem, b, dist_at_round, y, z);
                }
            }
        }
    }
}
*/
