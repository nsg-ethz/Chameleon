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

//! Module that contains the invariants and policies supported bu this crate.

use std::{
    collections::{HashMap, HashSet},
    iter::once,
    ops::Not,
};

use boolinator::Boolinator;
use clap::ValueEnum;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use thiserror::Error;

use bgpsim::{
    config::{ConfigModifier, NetworkConfig},
    event::EventQueue,
    forwarding_state::ForwardingState,
    policies::FwPolicy,
    prelude::{Network, NetworkError, NetworkFormatter},
    types::RouterId,
};

use crate::P;

/// Structure to check a Specification
#[derive(Debug)]
pub struct Checker<'a> {
    /// Specification that is checked
    spec: &'a Specification,
    /// Which expressions are satisfied in which step.
    invariants: HashMap<P, HashMap<Invariant, Vec<bool>>>,
    /// Number of steps already present in the checker
    steps: usize,
}

impl<'a> Checker<'a> {
    /// Create a new specification checker
    pub fn new(spec: &'a Specification) -> Self {
        let invariants = spec
            .iter()
            .map(|(p, expr)| {
                (
                    *p,
                    expr.get_invariants()
                        .into_iter()
                        .map(|i| (i, Vec::new()))
                        .collect(),
                )
            })
            .collect();

        Self {
            spec,
            invariants,
            steps: 0,
        }
    }

    /// Perform a step by adding the next forwarding state to the checker. Then, check if there may
    /// be a futhre in which the specification is safisfied. If so, return `true`.
    pub fn step(&mut self, fw_state: &mut ForwardingState<P>) -> bool {
        for (p, invariants) in self.invariants.iter_mut() {
            for (invariant, sat) in invariants.iter_mut() {
                sat.push(invariant.check(fw_state, *p).is_ok());
            }
        }
        self.steps += 1;
        self.spec.keys().all(|p| self.check_partial_prefix(*p))
    }

    /// Check the specification on the provided set of forwarding states.
    pub fn check(&self) -> bool {
        self.spec.keys().all(|p| self.check_prefix(*p))
    }

    /// Check the specification on the provided set of forwarding states for a given prefix.
    pub fn check_prefix(&self, prefix: P) -> bool {
        if self.steps == 0 {
            true
        } else if let Some(expr) = self.spec.get(&prefix) {
            if let Some(invariants) = self.invariants.get(&prefix) {
                check_rec(expr, invariants, 0, self.steps)
            } else {
                check_rec(expr, &HashMap::new(), 0, self.steps)
            }
        } else {
            true
        }
    }

    /// Check if there exists a future in which the specification for all prefixes is satisfied.
    pub fn check_partial(&self) -> bool {
        self.spec.keys().all(|p| self.check_partial_prefix(*p))
    }

    /// Check if there exists a future in which the specification for the given prefix is satisfied.
    pub fn check_partial_prefix(&self, prefix: P) -> bool {
        if self.steps == 0 {
            true
        } else if let Some(expr) = self.spec.get(&prefix) {
            if let Some(invariants) = self.invariants.get(&prefix) {
                partial_rec(expr, invariants, 0, self.steps).unwrap_or(true)
            } else {
                partial_rec(expr, &HashMap::new(), 0, self.steps).unwrap_or(true)
            }
        } else {
            true
        }
    }

    /// Returns the number of steps registered in the checker.
    pub fn num_steps(&self) -> usize {
        self.steps
    }
}

/// Recursively compute the partial result of the specification.
fn partial_rec(
    expr: &SpecExpr,
    invariants: &HashMap<Invariant, Vec<bool>>,
    k: usize,
    steps: usize,
) -> Option<bool> {
    match expr {
        SpecExpr::True => Some(true),
        SpecExpr::Invariant(i) => Some(invariants[i][k]),
        SpecExpr::Not(x) => partial_rec(x, invariants, k, steps).map(Not::not),
        SpecExpr::All(xs) => {
            let mut all_true = true;
            for x in xs {
                match partial_rec(x, invariants, k, steps) {
                    Some(true) => {}
                    Some(false) => return Some(false),
                    None => all_true = false,
                }
            }
            if all_true {
                Some(true)
            } else {
                None
            }
        }
        SpecExpr::Any(xs) => {
            let mut all_false = true;
            for x in xs {
                match partial_rec(x, invariants, k, steps) {
                    Some(true) => return Some(true),
                    Some(false) => {}
                    None => all_false = false,
                }
            }
            if all_false {
                Some(false)
            } else {
                None
            }
        }
        SpecExpr::Next(x) => {
            if k + 1 >= steps {
                None
            } else {
                partial_rec(x, invariants, k + 1, steps)
            }
        }
        SpecExpr::Globally(x) => {
            if (k..steps).all(|k| partial_rec(x, invariants, k, steps).unwrap_or(true)) {
                None
            } else {
                Some(false)
            }
        }
        SpecExpr::Finally(x) => {
            if (k..steps).any(|k| partial_rec(x, invariants, k, steps).unwrap_or(false)) {
                Some(true)
            } else {
                None
            }
        }
        SpecExpr::Until(a, b) | SpecExpr::WeakUntil(a, b) => {
            if check_rec(expr, invariants, k, steps) {
                Some(true)
            } else {
                let first_b = (k..steps)
                    .find(|k| partial_rec(b, invariants, *k, steps).unwrap_or(true))
                    .unwrap_or(steps);
                if (k..first_b).all(|k| partial_rec(a, invariants, k, steps).unwrap_or(true)) {
                    None
                } else {
                    Some(false)
                }
            }
        }
    }
}

/// Recursively compute the result of the specification.
fn check_rec(
    expr: &SpecExpr,
    invariants: &HashMap<Invariant, Vec<bool>>,
    k: usize,
    steps: usize,
) -> bool {
    match expr {
        SpecExpr::True => true,
        SpecExpr::Not(x) => !check_rec(x, invariants, k, steps),
        SpecExpr::All(xs) => xs.iter().all(|x| check_rec(x, invariants, k, steps)),
        SpecExpr::Any(xs) => xs.iter().any(|x| check_rec(x, invariants, k, steps)),
        SpecExpr::Next(x) => check_rec(x, invariants, (k + 1).min(steps), steps),
        SpecExpr::Globally(x) => (k..steps).all(|k| check_rec(x, invariants, k, steps)),
        SpecExpr::Finally(x) => (k..steps).any(|k| check_rec(x, invariants, k, steps)),
        SpecExpr::Until(a, b) => {
            if let Some(first_b) = (k..steps).find(|k| check_rec(b, invariants, *k, steps)) {
                (k..first_b).all(|k| check_rec(a, invariants, k, steps))
            } else {
                false
            }
        }
        SpecExpr::WeakUntil(a, b) => {
            let first_b = (k..steps)
                .find(|k| check_rec(b, invariants, *k, steps))
                .unwrap_or(steps);
            (k..first_b).all(|k| check_rec(a, invariants, k, steps))
        }
        SpecExpr::Invariant(i) => invariants[i][k],
    }
}

/// Specification, that is, a mapping from a prefix to a specification expression. Each
/// specification expression states a single expression for all properties.
pub type Specification = HashMap<P, SpecExpr>;

/// Modal and Logical Operators to build a specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub enum SpecExpr {
    /// *Logical Operator*: Always true.
    True,
    /// *Logical Operator*: Negate the specification
    Not(Box<SpecExpr>),
    /// *Logical Operator*: Conjunction of multiple specification
    All(Vec<SpecExpr>),
    /// *Logical Operator*: Disjunction of multiple specification
    Any(Vec<SpecExpr>),
    /// *Modal Operator*: The speciifcation is true in the next step.
    Next(Box<SpecExpr>),
    /// *Modal Operator*: Eventually, the specification becomes true. The specification must become
    /// true eventually.
    Finally(Box<SpecExpr>),
    /// *Modal Operator*: From now on, the specification is true.
    Globally(Box<SpecExpr>),
    /// *Modal Operator*: Specification A is true from now on, until specification B becomes
    /// true. Specification B must become true eventually. At the step, where B is true, A does not
    /// need to be true.
    Until(Box<SpecExpr>, Box<SpecExpr>),
    /// *Modal Operator*: Specification A is true from now on, until specification B becomes
    /// true. B can never become true if A holds indefinitely. At the step, where B is true, A does
    /// not need to be true.
    WeakUntil(Box<SpecExpr>, Box<SpecExpr>),
    /// *Propositional Variable*
    Invariant(Invariant),
}

impl SpecExpr {
    /// Extract the set of all invariants present in the expression.
    pub fn get_invariants(&self) -> HashSet<Invariant> {
        match self {
            SpecExpr::True => HashSet::new(),
            SpecExpr::Invariant(i) => once(i.clone()).collect(),
            SpecExpr::Next(x) | SpecExpr::Finally(x) | SpecExpr::Globally(x) | SpecExpr::Not(x) => {
                x.get_invariants()
            }
            SpecExpr::All(xs) | SpecExpr::Any(xs) => {
                xs.iter().flat_map(|x| x.get_invariants()).collect()
            }
            SpecExpr::Until(a, b) | SpecExpr::WeakUntil(a, b) => {
                [a, b].iter().flat_map(|x| x.get_invariants()).collect()
            }
        }
    }

    /// Extract a set of all subexpressions, including self.
    pub fn get_subexpr(&self) -> HashSet<SpecExpr> {
        match self {
            SpecExpr::True | SpecExpr::Invariant(_) => once(self.clone()).collect(),
            SpecExpr::Next(x) | SpecExpr::Finally(x) | SpecExpr::Globally(x) | SpecExpr::Not(x) => {
                let mut set = x.get_subexpr();
                set.insert(self.clone());
                set
            }
            SpecExpr::All(xs) | SpecExpr::Any(xs) => xs
                .iter()
                .flat_map(|x| x.get_subexpr())
                .chain(once(self.clone()))
                .collect(),
            SpecExpr::Until(a, b) | SpecExpr::WeakUntil(a, b) => [a, b]
                .into_iter()
                .flat_map(|x| x.get_subexpr())
                .chain(once(self.clone()))
                .collect(),
        }
    }

    /// Get the invariant if self is `Self::All`.
    pub fn all(self) -> Option<Vec<SpecExpr>> {
        match self {
            Self::All(x) => Some(x),
            _ => None,
        }
    }

    /// Get the invariant if self is `Self::All`.
    pub fn all_ref(&self) -> Option<&Vec<SpecExpr>> {
        match self {
            Self::All(x) => Some(x),
            _ => None,
        }
    }

    /// Get the invariant if self is `Self::Invariant`.
    pub fn invariant(self) -> Option<Invariant> {
        match self {
            Self::Invariant(x) => Some(x),
            _ => None,
        }
    }

    /// Get the invariant if self is `Self::Invariant`.
    pub fn invariant_ref(&self) -> Option<&Invariant> {
        match self {
            Self::Invariant(x) => Some(x),
            _ => None,
        }
    }
}

/// Invariant on the forwarding state. Adding such a property immediately requires that the `node`
/// can reach the `prefix` during the entire migration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub struct Invariant {
    /// Which is the source node for the invariant
    pub router: RouterId,
    /// What are the properties on the path that need to be satisfied.
    pub prop: Property,
}

/// Enumeration of different kinds of properties
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub enum Property {
    /// Conjunction of different properties
    All(Vec<Property>),
    /// Disjunction of different properties
    Any(Vec<Property>),
    /// Negation of different properties
    Not(Box<Property>),
    /// Waypoint property
    Waypoint(RouterId),
    /// Reachability
    Reachability,
    /// property is always satisfied.
    True,
}

/// Invariant violation
#[derive(Debug, Clone, Error)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub enum Violation {
    /// Path Violation
    #[error("Path violation for {1:?} ({0}) with path {2:?} (valid: {3})")]
    Path(P, Property, Vec<RouterId>, bool),
}

impl Invariant {
    /// Check the invariant holds on the forwarding state for a given prefix.
    pub fn check(&self, fw_state: &mut ForwardingState<P>, prefix: P) -> Result<(), Violation> {
        match fw_state.get_paths(self.router, prefix) {
            Ok(mut paths) if paths.len() == 1 => {
                let path = paths.pop().unwrap();
                self.prop
                    .check(&path, true)
                    .ok_or_else(|| Violation::Path(prefix, self.prop.clone(), path, true))
            }
            Err(NetworkError::ForwardingBlackHole(p)) | Err(NetworkError::ForwardingLoop(p)) => {
                self.prop
                    .check(&p, false)
                    .ok_or_else(|| Violation::Path(prefix, self.prop.clone(), p, false))
            }
            Ok(_) => unimplemented!("ECMP is not implemented."),
            Err(e) => unreachable!("Unexpected error {e} trown!"),
        }
    }
}

impl Property {
    /// Check the property is satisfied on the given path.
    pub fn check(&self, path: &[RouterId], reachable: bool) -> bool {
        match self {
            Self::All(ps) => ps.iter().all(|p| p.check(path, reachable)),
            Self::Any(ps) => ps.iter().any(|p| p.check(path, reachable)),
            Self::Not(p) => !p.check(path, reachable),
            Self::Waypoint(w) => !reachable || path.contains(w),
            Self::Reachability => reachable,
            Self::True => true,
        }
    }

    /// Generate a set of all subproperties, including self.
    pub fn get_subprops(&self) -> HashSet<Self> {
        let mut props: HashSet<Self> = match self {
            Self::All(xs) | Self::Any(xs) => xs.iter().flat_map(Self::get_subprops).collect(),
            Self::Not(x) => x.get_subprops(),
            Self::True | Self::Waypoint(_) | Self::Reachability => HashSet::with_capacity(1),
        };
        props.insert(self.clone());
        props
    }
}

/// Helper struct to conveniently build invariants based on the forwarding state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ValueEnum)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum SpecificationBuilder {
    /// Build a reachability invariant.
    Reachability,
    /// Build a reachability invariant, and require that the traffic can only leave the network
    /// either via the old egress, or the new egress.
    EgressWaypoint,
    /// Build an invariant that requires the old egress to be replaced by the new egress exactly
    /// once, while also maintaining reachability during the migration.
    OldUntilNewEgress,
    /// Build requirements similar to `OldUntilNewEgress`. We start by having reachability
    /// everywhere. Then, we add `x` temporal expressions for `x` random routers, depending on the
    /// argument of this builder.
    #[clap(skip)]
    Scalable(usize),
    /// Build requirements similar to We start by having reachability everywhere. Then, we add `x`
    /// non-temporal expressions for `x` routers, depending on the argument of this builder. For
    /// each router, we assert that either the old or the new egress is used.
    #[clap(skip)]
    ScalableNonTemporal(usize),
}

impl SpecificationBuilder {
    /// Build all invariants for all nodes in the network, and all specified routers
    pub fn build_all<Q: EventQueue<P> + Clone>(
        self,
        net: &Network<P, Q>,
        command: Option<&ConfigModifier<P>>,
        prefixes: impl IntoIterator<Item = P>,
    ) -> Specification {
        let mut old_fws = net.get_forwarding_state();
        let mut new_fws = if let Some(command) = command {
            let mut new_net = net.clone();
            new_net.apply_modifier(command).unwrap();
            new_net.get_forwarding_state()
        } else {
            old_fws.clone()
        };

        prefixes
            .into_iter()
            .map(|p| {
                (
                    p,
                    self.build(&mut old_fws, &mut new_fws, net.get_routers(), p),
                )
            })
            .collect()
    }

    /// Build the invariant for a given router and prefix.
    pub fn build(
        self,
        old_fws: &mut ForwardingState<P>,
        new_fws: &mut ForwardingState<P>,
        mut routers: Vec<RouterId>,
        p: P,
    ) -> SpecExpr {
        use Property::{Any, Reachability as Reach, Waypoint as Wpt};
        use SpecExpr::{All, Globally, Invariant as Prop, Until};
        match self {
            SpecificationBuilder::Reachability => Globally(Box::new(All(routers
                .into_iter()
                .map(|router| {
                    Prop(Invariant {
                        router,
                        prop: Reach,
                    })
                })
                .collect()))),
            SpecificationBuilder::EgressWaypoint => Globally(Box::new(All(routers
                .into_iter()
                .flat_map(|router| {
                    let old = Wpt(*old_fws.get_paths(router, p).unwrap()[0].last().unwrap());
                    let new = Wpt(*new_fws.get_paths(router, p).unwrap()[0].last().unwrap());
                    [
                        Prop(Invariant {
                            router,
                            prop: Reach,
                        }),
                        Prop(Invariant {
                            router,
                            prop: if old == new { old } else { Any(vec![old, new]) },
                        }),
                    ]
                })
                .collect()))),
            SpecificationBuilder::OldUntilNewEgress => {
                let mut changing = Vec::new();
                let mut constant = Vec::new();
                for router in routers.iter().copied() {
                    let old_nh = Wpt(*old_fws.get_paths(router, p).unwrap()[0].last().unwrap());
                    let new_nh = Wpt(*new_fws.get_paths(router, p).unwrap()[0].last().unwrap());
                    if old_nh == new_nh {
                        constant.push(Prop(Invariant {
                            router,
                            prop: old_nh,
                        }))
                    } else {
                        changing.push(Until(
                            Box::new(Prop(Invariant {
                                router,
                                prop: old_nh,
                            })),
                            Box::new(Globally(Box::new(Prop(Invariant {
                                router,
                                prop: new_nh,
                            })))),
                        ));
                    }
                }
                SpecExpr::All(vec![
                    Globally(Box::new(All(routers
                        .into_iter()
                        .map(|router| {
                            Prop(Invariant {
                                router,
                                prop: Reach,
                            })
                        })
                        .collect()))),
                    Globally(Box::new(All(constant))),
                    All(changing),
                ])
            }
            SpecificationBuilder::Scalable(x) => {
                // first, sort against the router id, and then against the next hop. The second sort
                // is stable, meaning that the outcome is predictable.
                routers.sort();
                routers.sort_by_key(|r| new_fws.get_next_hops(*r, p)[0]);

                let mut changing = Vec::new();
                let mut constant = Vec::new();
                // only handle `x` routers.
                for router in routers.iter().copied().take(x) {
                    let old_nh = Wpt(*old_fws.get_paths(router, p).unwrap()[0].last().unwrap());
                    let new_nh = Wpt(*new_fws.get_paths(router, p).unwrap()[0].last().unwrap());
                    if old_nh == new_nh {
                        constant.push(Prop(Invariant {
                            router,
                            prop: old_nh,
                        }))
                    } else {
                        changing.push(Until(
                            Box::new(Prop(Invariant {
                                router,
                                prop: old_nh,
                            })),
                            Box::new(Globally(Box::new(Prop(Invariant {
                                router,
                                prop: new_nh,
                            })))),
                        ));
                    }
                }
                SpecExpr::All(vec![
                    Globally(Box::new(All(routers
                        .into_iter()
                        .map(|router| {
                            Prop(Invariant {
                                router,
                                prop: Reach,
                            })
                        })
                        .collect()))),
                    Globally(Box::new(All(constant))),
                    All(changing),
                ])
            }
            SpecificationBuilder::ScalableNonTemporal(x) => {
                // first, sort against the router id, and then against the next hop. The second sort
                // is stable, meaning that the outcome is predictable.
                routers.sort();
                routers.sort_by_key(|r| new_fws.get_next_hops(*r, p)[0]);

                let mut spec = Vec::new();
                // add all reachability constraints
                for router in routers.iter().copied() {
                    spec.push(SpecExpr::Invariant(Invariant {
                        router,
                        prop: Reach
                    }));
                }
                // only handle `x` routers.
                for router in routers.iter().copied().take(x) {
                    let old_nh = Wpt(*old_fws.get_paths(router, p).unwrap()[0].last().unwrap());
                    let new_nh = Wpt(*new_fws.get_paths(router, p).unwrap()[0].last().unwrap());
                    if old_nh == new_nh {
                        spec.push(SpecExpr::Invariant(Invariant {
                            router,
                            prop: old_nh,
                        }))
                    } else {
                        spec.push(SpecExpr::Any(vec![
                            Prop(Invariant {
                                router,
                                prop: old_nh,
                            }),
                            Prop(Invariant {
                                router,
                                prop: new_nh,
                            }),
                        ]));
                    }
                }

                SpecExpr::Globally(Box::new(All(spec)))
            }
        }
    }
}

impl SpecExpr {
    /// Get the global invariants from a SpecExpr. This will extract all invariants that must hold
    /// during the entire migration. This function will report warninigs for all expressions that
    /// could not be converted.
    pub fn as_global_invariants<Q>(self, net: &Network<P, Q>) -> Vec<Invariant> {
        match self {
            SpecExpr::All(es) => {
                let mut invariants = Vec::new();
                for e in es {
                    match e {
                        SpecExpr::Globally(x)
                            if x.all_ref()
                                .map(|xs| xs.iter().all(|x| x.invariant_ref().is_some()))
                                .unwrap_or(false) =>
                        {
                            for x in x.all().unwrap() {
                                invariants.push(x.invariant().unwrap())
                            }
                        }
                        _ => {
                            log::warn!(
                                "Expression cannot be turned into global invariants: {}",
                                e.fmt(net)
                            )
                        }
                    }
                }
                invariants
            }
            SpecExpr::Globally(x)
                if x.all_ref()
                    .map(|xs| xs.iter().all(|x| x.invariant_ref().is_some()))
                    .unwrap_or(false) =>
            {
                x.all()
                    .unwrap()
                    .into_iter()
                    .filter_map(SpecExpr::invariant)
                    .collect()
            }
            spec => {
                log::warn!(
                    "Expression cannot be turned into global invariants: {}",
                    spec.fmt(net)
                );
                Vec::new()
            }
        }
    }
}

impl Invariant {
    /// Try to transform the invariant into a vector of forewarding policies. This function will
    /// ignore any policy that it cannot transform, and log a warning.
    pub fn as_fw_policies<Q>(self, net: &Network<P, Q>, prefix: P) -> Vec<FwPolicy<P>> {
        self.prop.as_fw_policies(net, self.router, prefix)
    }
}

impl Property {
    /// Try to transform the invariant into a vector of forewarding policies. This function will
    /// ignore any policy that it cannot transform, and log a warning.
    pub fn as_fw_policies<Q>(
        self,
        net: &Network<P, Q>,
        router: RouterId,
        prefix: P,
    ) -> Vec<FwPolicy<P>> {
        match self {
            Property::All(x) => x
                .into_iter()
                .flat_map(|x| x.as_fw_policies(net, router, prefix))
                .collect(),
            Property::Any(_) | Property::Not(_) => {
                log::warn!(
                    "Cannot interpret {} as a set of forwarding policies!",
                    self.fmt(net)
                );
                Vec::new()
            }
            Property::True => Vec::new(),
            Property::Waypoint(wp) => vec![FwPolicy::PathCondition(
                router,
                prefix,
                bgpsim::policies::PathCondition::Node(wp),
            )],
            Property::Reachability => vec![FwPolicy::Reachable(router, prefix)],
        }
    }
}
