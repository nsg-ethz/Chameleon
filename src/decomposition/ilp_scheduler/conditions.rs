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

//! Module for creating variables and constraints for the conditions.
//!
//! We create the conditions as follows: For each forwarding property, we extract its kind (either
//! if it is reachability, isolation, or a waypoint towards target x. Then, for each of these kinds,
//! we create boolean variables for each step that should encode wether the router satisfies the
//! constraints at this step. Then, for checking those conditions, we add conststraints for each
//! policy only based on those boolean variables.
//!
//! # Recursive Model
//! We use a recursive model to get the constraints on wether any router satisfies any condition. We
//! do this as follows: At every step, and for every router, we set its property either to a
//! specific value if it is already locally defined (e.g. isolation with a black hole, or
//! reachability with sending to a terminal), or to be equal to the next hop. In case this router
//! may change its forwarding decision, we add an if-then-else clause, which uses the old or the new
//! next-hop.

use std::{
    collections::{HashMap, HashSet},
    iter::{once, repeat_with},
};

use bgpsim::{forwarding_state::ForwardingState, prelude::*};
use good_lp::{constraint, variable, ProblemVariables, SolverModel, Variable};
use itertools::{iproduct, Itertools};

use crate::{
    decomposition::CommandInfo,
    specification::{Invariant, Property, SpecExpr},
    P,
};

use super::{or_tools::*, IlpVars};

/// Type to represent all variables needed to check for conditions.
pub(super) type CondsType = HashMap<Property, HashMap<RouterId, Vec<Variable>>>;
/// Type to represent variables for the specification (LTL).
pub(super) type SpecExprType = HashMap<SpecExprExt, HashMap<usize, Variable>>;

/// Create all variables needed for the specification, i.e., to satisfy the forwarding policies.
pub(super) fn spec_variables<Q>(
    p: &mut ProblemVariables,
    info: &CommandInfo<'_, Q>,
    all_nodes: &HashSet<RouterId>,
    max_steps: usize,
    prefix: P,
) -> (CondsType, SpecExprType) {
    let spec = info.spec.get(&prefix).cloned().unwrap_or(SpecExpr::True);
    // get the set of all spec expressions.
    let props: HashSet<_> = spec
        .get_invariants()
        .into_iter()
        .flat_map(|i| i.prop.get_subprops())
        .collect();
    // Create the boolean variables for every possible condition, router and step.
    let prop_vars = props
        .into_iter()
        .zip(repeat_with(|| {
            all_nodes
                .iter()
                .copied()
                .zip(repeat_with(|| {
                    repeat_with(|| p.add(variable().binary()))
                        .take(max_steps)
                        .collect_vec()
                }))
                .collect()
        }))
        .collect();

    let mut spec_vars = SpecExprType::new();
    build_spec_vars(p, &mut spec_vars, spec, 0, max_steps);
    (prop_vars, spec_vars)
}

/// recursive function to build the spec expr variables
fn build_spec_vars(
    p: &mut ProblemVariables,
    vars: &mut SpecExprType,
    expr: SpecExpr,
    round: usize,
    max_steps: usize,
) {
    // add the variable for the current expression
    let entry = vars.entry(expr.clone().into()).or_default();
    // skip if the entry already exists
    if entry.contains_key(&round) {
        return;
    }
    // add the entry for the given round.
    entry
        .entry(round)
        .or_insert_with(|| p.add(variable().binary()));
    match expr {
        SpecExpr::Invariant(_) | SpecExpr::True => {}
        SpecExpr::Not(e) => build_spec_vars(p, vars, *e, round, max_steps),
        SpecExpr::Next(e) => build_spec_vars(p, vars, *e, (round + 1).min(max_steps), max_steps),
        SpecExpr::Finally(e) | SpecExpr::Globally(e) => {
            let e = *e;
            for k in round..max_steps {
                build_spec_vars(p, vars, e.clone(), k, max_steps)
            }
        }
        SpecExpr::All(es) | SpecExpr::Any(es) => {
            for e in es {
                build_spec_vars(p, vars, e, round, max_steps)
            }
        }
        SpecExpr::Until(a, b) => {
            let a = *a;
            let b = *b;
            for k in round..max_steps {
                vars.entry(SpecExprExt::UntilFixed(
                    Box::new(a.clone().into()),
                    Box::new(b.clone().into()),
                    k,
                ))
                .or_default()
                .entry(round)
                .or_insert_with(|| p.add(variable().binary()));
            }
            for k in round..max_steps {
                build_spec_vars(p, vars, a.clone(), k, max_steps);
                build_spec_vars(p, vars, b.clone(), k, max_steps);
            }
        }
        SpecExpr::WeakUntil(a, b) => {
            let a = *a;
            let b = *b;
            for k in round..max_steps {
                vars.entry(SpecExprExt::UntilFixed(
                    Box::new(a.clone().into()),
                    Box::new(b.clone().into()),
                    k,
                ))
                .or_default()
                .entry(round)
                .or_insert_with(|| p.add(variable().binary()));
            }
            // also, insert the global expression.
            vars.entry(SpecExprExt::Globally(Box::new(a.clone().into())))
                .or_default()
                .entry(round)
                .or_insert_with(|| p.add(variable().binary()));
            for k in round..max_steps {
                build_spec_vars(p, vars, a.clone(), k, max_steps);
                build_spec_vars(p, vars, b.clone(), k, max_steps);
            }
        }
    }
}

/// Setup all constraints for all properties
pub(super) fn prop_constraints<Q>(
    problem: &mut impl SolverModel,
    vars: &IlpVars,
    info: &CommandInfo<'_, Q>,
    prefix: P,
) {
    for (prop, r, round) in iproduct!(vars.props(), info.routers(), vars.steps()) {
        // depending on the condition, add the constraints.
        let c = vars.get_c(prop, r, round);
        match prop {
            // Special case for external routers
            Property::Reachability | Property::Waypoint(_)
                if info.net_before.get_device(r).is_external() =>
            {
                match prop.local_sat(r, None, &info.fw_before, prefix) {
                    Some(true) => problem.add_constraint(constraint!(c == 1)),
                    Some(false) => problem.add_constraint(constraint!(c == 0)),
                    None => unreachable!(),
                };
            }
            // Reachability and waypoint properties depend on the next-hop that they have.
            Property::Reachability | Property::Waypoint(_) => {
                // TODO deal with black holes here!
                //
                // extract the next hop. Notice, that z is the state before the update (i.e., b = 0), and
                // y is the state after the update (i.e., b = 1).
                let nh_old = info.fw_before.get_next_hops(r, prefix).last().copied();
                let nh_new = info.fw_after.get_next_hops(r, prefix).last().copied();

                // check if the condition can be satisfied just by considering the next-hop.
                let sat_old = prop.local_sat(r, nh_old, &info.fw_before, prefix);
                let sat_new = prop.local_sat(r, nh_new, &info.fw_after, prefix);

                // prepare the c variable to be constrained in this round.
                let c_nh_new = vars.get_c(prop, nh_new.unwrap(), round);
                let c_nh_old = vars.get_c(prop, nh_old.unwrap(), round);

                // check if there was a change
                if nh_old == nh_new {
                    // no update happens. Set the cond variable either to true or false if the the
                    // condition is already satisfied by the next-hop, or to the value of the
                    // next-hop.
                    match sat_old {
                        Some(true) => problem.add_constraint(constraint!(c == 1)),
                        Some(false) => problem.add_constraint(constraint!(c == 0)),
                        None => problem.add_constraint(constraint!(c == c_nh_old)),
                    };
                } else {
                    // helper function to turn a boolean into a float.
                    let b2f = |b| if b { 1.0 } else { 0.0 };
                    // update happens. Assign `c` to the value of the new next-hop or old next-hop,
                    // depending if the router has already changed its routing decision.
                    let has_changed = vars.get_b(r, round);
                    match (sat_old, sat_new) {
                        (None, None) => {
                            c_if_then_else(problem, has_changed, c, c_nh_new, c_nh_old);
                        }
                        (None, Some(nh_new_sat)) => {
                            c_if_then_else(problem, has_changed, c, b2f(nh_new_sat), c_nh_old);
                        }
                        (Some(nh_old_sat), None) => {
                            c_if_then_else(problem, has_changed, c, c_nh_new, b2f(nh_old_sat));
                        }
                        (Some(nh_old_sat), Some(nh_new_sat)) => {
                            c_if_then_else_yz(problem, has_changed, c, nh_new_sat, nh_old_sat)
                        }
                    }
                }
            }
            Property::All(ps) => {
                let ys = ps.iter().map(|p| vars.get_c(p, r, round)).collect_vec();
                c_all(problem, c, ys);
            }
            Property::Any(ps) => {
                let ys = ps.iter().map(|p| vars.get_c(p, r, round)).collect_vec();
                c_any(problem, c, ys);
            }
            Property::Not(p) => {
                let y = vars.get_c(p, r, round);
                problem.add_constraint(constraint!(c == 1 - y));
            }
            Property::True => {
                problem.add_constraint(constraint!(c == 1));
            }
        }
    }
}

/// Create the constraints for all specifications. Further, assert that the root specificatoin is
/// satisfied in round 0.
pub(super) fn spec_constraints<Q>(
    problem: &mut impl SolverModel,
    vars: &IlpVars,
    info: &CommandInfo<'_, Q>,
    prefix: P,
) {
    // add the constraints to build all specificatoin entries
    let max_round = vars.max_steps;
    for (expr, x) in vars.s.iter() {
        for (round, s) in x.iter().map(|(r, s)| (*r, *s)) {
            match expr {
                SpecExprExt::True => {
                    problem.add_constraint(constraint!(s == 1));
                }
                SpecExprExt::Not(x) => {
                    let x = vars.get_s(x, round);
                    problem.add_constraint(constraint!(s == 1.0 - x));
                }
                SpecExprExt::All(xs) => {
                    let xs = xs.iter().map(|x| vars.get_s(x, round)).collect();
                    c_all(problem, s, xs);
                }
                SpecExprExt::Any(xs) => {
                    let xs = xs.iter().map(|x| vars.get_s(x, round)).collect();
                    c_any(problem, s, xs);
                }
                SpecExprExt::Next(x) => {
                    let x = vars.get_s(x, (round + 1).min(max_round));
                    problem.add_constraint(constraint!(s == x));
                }
                SpecExprExt::Finally(x) => {
                    let xs = (round..max_round).map(|k| vars.get_s(x, k)).collect();
                    c_any(problem, s, xs);
                }
                SpecExprExt::Globally(x) => {
                    let xs = (round..max_round).map(|k| vars.get_s(x, k)).collect();
                    c_all(problem, s, xs);
                }
                SpecExprExt::Until(a, b) => {
                    let xs = (round..max_round)
                        .map(|k| {
                            vars.get_s(&SpecExprExt::UntilFixed(a.clone(), b.clone(), k), round)
                        })
                        .collect();
                    c_any(problem, s, xs);
                }
                SpecExprExt::UntilFixed(a, b, round_sat) => {
                    let ss: Vec<Variable> = (round..*round_sat)
                        .map(|k| vars.get_s(a, k))
                        .chain(once(vars.get_s(b, *round_sat)))
                        .collect();
                    c_all(problem, s, ss);
                }
                SpecExprExt::WeakUntil(a, b) => {
                    let xs = (round..max_round)
                        .map(|k| {
                            vars.get_s(&SpecExprExt::UntilFixed(a.clone(), b.clone(), k), round)
                        })
                        .chain(once(vars.get_s(&SpecExprExt::Globally(a.clone()), round)))
                        .collect();
                    c_any(problem, s, xs);
                }
                SpecExprExt::Invariant(Invariant { router, prop }) => {
                    let c = vars.get_c(prop, *router, round);
                    problem.add_constraint(constraint!(s == c));
                }
            }
        }

        // finally, assert that the main expression is true
        let root = info
            .spec
            .get(&prefix)
            .map(|x| x.clone().into())
            .unwrap_or(SpecExprExt::True);
        if max_round > 0 {
            let root_s = vars.get_s(&root, 0);
            problem.add_constraint(constraint!(root_s == 1.0));
        }
    }
}

impl Property {
    /// Returns `Some(true)` or `Some(false)` if the router with the selected next hop already
    /// satisfies of violates the condition. If this cannot be determined only by considering this
    /// local view, return `None`.
    fn local_sat(
        &self,
        r: RouterId,
        nh: Option<RouterId>,
        fw: &ForwardingState<P>,
        prefix: P,
    ) -> Option<bool> {
        match self {
            Property::Reachability => {
                // check if the next-hop is a terminal
                match nh {
                    Some(r) if fw.is_terminal(r, prefix) => Some(true),
                    Some(_) => None,
                    None => Some(fw.is_terminal(r, prefix)),
                }
            }
            Property::Waypoint(wp) => {
                if *wp == r || nh == Some(*wp) {
                    Some(true)
                } else {
                    match nh {
                        Some(r) if fw.is_terminal(r, prefix) => Some(false),
                        Some(_) => None,
                        None => Some(false),
                    }
                }
            }
            _ => unreachable!("local_set should only be called on Reachability or Waypoint!"),
        }
    }
}

/// Extended spec expression that also contains Until and WeakUntil with a fixed transition point.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum SpecExprExt {
    /// *Logical Operator*: Always true.
    True,
    /// *Logical Operator*: Negate the specification
    Not(Box<SpecExprExt>),
    /// *Logical Operator*: Conjunction of multiple specification
    All(Vec<SpecExprExt>),
    /// *Logical Operator*: Disjunction of multiple specification
    Any(Vec<SpecExprExt>),
    /// *Modal Operator*: The speciifcation is true in the next step.
    Next(Box<SpecExprExt>),
    /// *Modal Operator*: Eventually, the specification becomes true. The specification must become
    /// true eventually.
    Finally(Box<SpecExprExt>),
    /// *Modal Operator*: From now on, the specification is true.
    Globally(Box<SpecExprExt>),
    /// *Modal Operator*: Specification A is true from now on, until specification B becomes
    /// true. Specification B must become true eventually. At the step, where B is true, A does not
    /// need to be true.
    Until(Box<SpecExprExt>, Box<SpecExprExt>),
    /// *Modal Operator*: Until expression with a known point where it should switch.
    UntilFixed(Box<SpecExprExt>, Box<SpecExprExt>, usize),
    /// *Modal Operator*: Specification A is true from now on, until specification B becomes
    /// true. B can never become true if A holds indefinitely. At the step, where B is true, A does
    /// not need to be true.
    WeakUntil(Box<SpecExprExt>, Box<SpecExprExt>),
    /// *Propositional Variable*
    Invariant(Invariant),
}

impl From<SpecExpr> for SpecExprExt {
    fn from(val: SpecExpr) -> Self {
        match val {
            SpecExpr::True => SpecExprExt::True,
            SpecExpr::Not(x) => SpecExprExt::Not(Box::new((*x).into())),
            SpecExpr::All(xs) => SpecExprExt::All(xs.into_iter().map(|x| x.into()).collect()),
            SpecExpr::Any(xs) => SpecExprExt::Any(xs.into_iter().map(|x| x.into()).collect()),
            SpecExpr::Next(x) => SpecExprExt::Next(Box::new((*x).into())),
            SpecExpr::Finally(x) => SpecExprExt::Finally(Box::new((*x).into())),
            SpecExpr::Globally(x) => SpecExprExt::Globally(Box::new((*x).into())),
            SpecExpr::Until(a, b) => {
                SpecExprExt::Until(Box::new((*a).into()), Box::new((*b).into()))
            }
            SpecExpr::WeakUntil(a, b) => {
                SpecExprExt::WeakUntil(Box::new((*a).into()), Box::new((*b).into()))
            }
            SpecExpr::Invariant(i) => SpecExprExt::Invariant(i),
        }
    }
}
