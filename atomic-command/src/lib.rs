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

#![doc(html_logo_url = "https://iospf.tibors.ch/images/bgpsim/dark_only.svg")]

//! This library contains the definition for an atomic command. This is used by `atomic_bgp`, as
//! well as `bgpsim_web` with the feature `atomic_bgp`.

use std::{collections::BTreeSet, iter::once};

use bgpsim::{
    bgp::BgpRibEntry,
    config::{ConfigModifier, NetworkConfig},
    event::EventQueue,
    prelude::{Network, NetworkFormatter},
    types::{NetworkError, Prefix, PrefixMap, RouterId},
};
use itertools::Itertools;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Atomic command, along with its pre and postconditions.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[cfg_attr(
    feature = "serde",
    serde(bound(deserialize = "P: for<'a> Deserialize<'a>"))
)]
pub struct AtomicCommand<P: Prefix> {
    /// Atomic command that only affects a single router (if used in the prepared order). This is a
    /// set of commands that need to be applied to only a single router.
    pub command: AtomicModifier<P>,
    /// Pre-conditions that need to be satisfied before applying this command. This may only depend
    /// on the convergence of BGP inside of the network. For instance, it requires that a specific
    /// route was advertised to that router.
    pub precondition: AtomicCondition<P>,
    /// Post-conditions that need to be satisfied such that this command has converged. This is
    /// typically that a next-hop needs to be changed, or a specific route must be selected.
    pub postcondition: AtomicCondition<P>,
}

/// This is the actual modifier on the network. This can either be a [`ConfigModifier`], or it can
/// be something more specific. To get a vector of `ConfigModifier`, simply call `c.into()`.
///
/// This type exists to make visualizations easier, because it captures the semantics of the
/// command.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[cfg_attr(
    feature = "serde",
    serde(bound(deserialize = "P: for<'a> Deserialize<'a>"))
)]
pub enum AtomicModifier<P: Prefix> {
    /// A raw config modifier that cannot be simplified any further.
    Raw(ConfigModifier<P>),
    /// A command to use a temporary session for a specific prefix.
    UseTempSession {
        /// On which router should the static route be configured
        router: RouterId,
        /// Neighbor that should send the BGP route to `router`.
        neighbor: RouterId,
        /// For which prefix should the static route apply.
        prefix: P,
        /// The raw command.
        raw: ConfigModifier<P>,
    },
    /// A command to ignore the route over a temporary session for a given prefix.
    IgnoreTempSession {
        /// On which router should the static route be configured
        router: RouterId,
        /// Neighbor that should send the BGP route to `router`.
        neighbor: RouterId,
        /// For which prefix should the static route apply.
        prefix: P,
        /// The raw command.
        raw: ConfigModifier<P>,
    },
    /// A command to add a temporary session.
    AddTempSession {
        /// On which router should the static route be configured
        router: RouterId,
        /// Neighbor that should send the BGP route to `router`.
        neighbor: RouterId,
        /// The raw command.
        raw: Vec<ConfigModifier<P>>,
    },
    /// A command to remove a temporary session.
    RemoveTempSession {
        /// On which router should the static route be configured
        router: RouterId,
        /// Neighbor that should send the BGP route to `router`.
        neighbor: RouterId,
        /// The raw command.
        raw: Vec<ConfigModifier<P>>,
    },
    /// A command to change which routes should be preferred.
    ChangePreference {
        /// On which router should the preference be changed
        router: RouterId,
        /// For which prefix should the new preference apply
        prefix: P,
        /// The neighbor from which we prefer the new route
        neighbor: RouterId,
        /// The raw commands
        raw: Vec<ConfigModifier<P>>,
    },
    /// A command to clear any local preference changes for the migration.
    ClearPreference {
        /// On which router should the preference be changed
        router: RouterId,
        /// For which prefix should the new preference apply
        prefix: P,
        /// The raw commands
        raw: Vec<ConfigModifier<P>>,
    },
}

impl<P: Prefix> From<AtomicModifier<P>> for Vec<ConfigModifier<P>> {
    fn from(value: AtomicModifier<P>) -> Self {
        match value {
            AtomicModifier::Raw(raw)
            | AtomicModifier::IgnoreTempSession { raw, .. }
            | AtomicModifier::UseTempSession { raw, .. } => vec![raw],
            AtomicModifier::ChangePreference { raw, .. }
            | AtomicModifier::ClearPreference { raw, .. }
            | AtomicModifier::AddTempSession { raw, .. }
            | AtomicModifier::RemoveTempSession { raw, .. } => raw,
        }
    }
}

impl<P: Prefix> AtomicModifier<P> {
    /// Get the router(s) that are affected by the modifier.
    pub fn routers(&self) -> Vec<RouterId> {
        match self {
            AtomicModifier::Raw(raw) => raw.routers(),
            AtomicModifier::ChangePreference { router, .. }
            | AtomicModifier::ClearPreference { router, .. }
            | AtomicModifier::UseTempSession { router, .. }
            | AtomicModifier::IgnoreTempSession { router, .. } => vec![*router],
            AtomicModifier::AddTempSession {
                router, neighbor, ..
            }
            | AtomicModifier::RemoveTempSession {
                router, neighbor, ..
            } => vec![*router, *neighbor],
        }
    }

    /// Apply the modifier to the network.
    pub fn apply<Q>(&self, net: &mut Network<P, Q>) -> Result<(), NetworkError>
    where
        Q: EventQueue<P>,
    {
        match self {
            AtomicModifier::Raw(raw)
            | AtomicModifier::IgnoreTempSession { raw, .. }
            | AtomicModifier::UseTempSession { raw, .. } => net.apply_modifier(raw),
            AtomicModifier::ChangePreference { raw, .. }
            | AtomicModifier::ClearPreference { raw, .. }
            | AtomicModifier::AddTempSession { raw, .. }
            | AtomicModifier::RemoveTempSession { raw, .. } => {
                raw.iter().try_for_each(|c| net.apply_modifier(c))
            }
        }
    }

    /// Transform the atomic modifier into a vector of config modifiers. This function will consume
    /// `self` and return the `raw` values, stored within `self`.
    pub fn into_raw(self) -> Vec<ConfigModifier<P>> {
        match self {
            AtomicModifier::Raw(raw)
            | AtomicModifier::IgnoreTempSession { raw, .. }
            | AtomicModifier::UseTempSession { raw, .. } => vec![raw],
            AtomicModifier::ChangePreference { raw, .. }
            | AtomicModifier::ClearPreference { raw, .. }
            | AtomicModifier::AddTempSession { raw, .. }
            | AtomicModifier::RemoveTempSession { raw, .. } => raw,
        }
    }
}

impl<'a, 'n, P: Prefix, Q> NetworkFormatter<'a, 'n, P, Q> for AtomicModifier<P> {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        match self {
            AtomicModifier::Raw(r) => r.fmt(net),
            AtomicModifier::ChangePreference {
                router,
                prefix,
                neighbor,
                ..
            } => format!(
                "Make {} prefer routes for {prefix} from {}",
                router.fmt(net),
                neighbor.fmt(net),
            ),
            AtomicModifier::ClearPreference { router, prefix, .. } => {
                format!("Clear route preference on {} for {prefix}", router.fmt(net),)
            }
            AtomicModifier::UseTempSession {
                router,
                neighbor,
                prefix,
                ..
            } if neighbor == router => {
                format!("Make {} use drop traffic for {prefix}", router.fmt(net))
            }
            AtomicModifier::UseTempSession {
                router,
                neighbor,
                prefix,
                ..
            } => format!(
                "Make {} use temporary BGP session with {} for {prefix}",
                router.fmt(net),
                neighbor.fmt(net),
            ),
            AtomicModifier::IgnoreTempSession {
                router,
                neighbor,
                prefix,
                ..
            } => format!(
                "Make {} ignore temporary BGP session with {} for {prefix}",
                router.fmt(net),
                neighbor.fmt(net),
            ),
            AtomicModifier::AddTempSession {
                router, neighbor, ..
            } => format!(
                "Add temporary BGP session between {} and {}",
                router.fmt(net),
                neighbor.fmt(net)
            ),
            AtomicModifier::RemoveTempSession {
                router, neighbor, ..
            } => format!(
                "Remove temporary BGP session between {} and {}",
                router.fmt(net),
                neighbor.fmt(net)
            ),
        }
    }
}

/// Simple atomic conditions that can only check basic properties. They are designed to checked on
/// Cisco devices that do not expose all parameters to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[cfg_attr(
    feature = "serde",
    serde(bound(deserialize = "P: for<'a> Deserialize<'a>"))
)]
pub enum AtomicCondition<P: Prefix> {
    /// No condition necessary. This condition is satisfied automatically.
    None,
    /// Condition on the current RIB entry (selected entry) of a router and a prefix. This condition
    /// requires that a route for this prefix is available. In addition, one can choose to check
    /// that the route is coming from a speicfic neighbor.
    SelectedRoute {
        /// Which router should be checked
        router: RouterId,
        /// Which prefix should be checked
        prefix: P,
        /// The selected route was learned from this neighbor. If this is set to `None`, then the
        /// neighbor will not be checked.
        neighbor: Option<RouterId>,
        /// The selected route has a given (local) weight. If `None`, then the weight is ignored.
        weight: Option<u32>,
        /// The selected route has a next hop via x
        next_hop: Option<RouterId>,
    },
    /// Condition on the availability of a given route. It implies that there exists at least one
    /// route that is from either one of the given neighbors. If `neighbors` is  `None`, then it
    /// just asserts that a route for this prefix is available.
    AvailableRoute {
        /// Which router should be checked
        router: RouterId,
        /// Which prefix should be checked
        prefix: P,
        /// The available route was learned from this neighbor. If this is set to `None`, then the
        /// neighbor will not be checked.
        neighbor: Option<RouterId>,
        /// The available route has a given (local) weight. If `None`, then the weight is ignored.
        weight: Option<u32>,
        /// The selected route has a next hop via x
        next_hop: Option<RouterId>,
    },
    /// The BGP session with a given neighbor is established.
    BgpSessionEstablished {
        /// Which router should be checked
        router: RouterId,
        /// Neighbor to which a BGP session must be established.
        neighbor: RouterId,
    },
    /// Condition that checks if all routes received by a node are less preferred than the provided
    /// ones, before actually removing the weight rewrite.
    RoutesLessPreferred {
        /// Which routers should be checkd
        router: RouterId,
        /// Destination which must be checked.
        prefix: P,
        /// Which neighbors are supposed to be preferred and can be ignored
        good_neighbors: BTreeSet<RouterId>,
        /// the route that we will receive from the good neighbors. Any other route must be less
        /// preferred than this one.
        route: BgpRibEntry<P>,
    },
}

impl<P: Prefix> AtomicCondition<P> {
    /// Returns `true` if the condition is None.
    pub fn is_none(&self) -> bool {
        matches!(self, AtomicCondition::None)
    }

    /// Check the atomic condition.
    pub fn check<Q>(&self, net: &Network<P, Q>) -> Result<bool, NetworkError> {
        AtomicConditionExt::from(self.clone()).check(net)
    }
}

impl<P: Prefix> From<AtomicCondition<P>> for AtomicConditionExt<P> {
    fn from(value: AtomicCondition<P>) -> Self {
        match value {
            AtomicCondition::None => Self::None,
            AtomicCondition::SelectedRoute {
                router,
                prefix,
                neighbor,
                weight,
                next_hop,
            } => AtomicConditionExt::CurrentRib {
                router,
                prefix,
                cond: Some(RibCond::And(
                    neighbor
                        .iter()
                        .map(|x| RibCond::LearnedFrom(*x))
                        .chain(once(RibCond::Prefix(prefix)))
                        .chain(weight.iter().map(|x| RibCond::Weight(*x)))
                        .chain(next_hop.iter().map(|x| RibCond::NextHop(*x)))
                        .collect(),
                )),
            },
            AtomicCondition::AvailableRoute {
                router,
                prefix,
                neighbor,
                weight,
                next_hop,
            } => AtomicConditionExt::AnyKnownRoute {
                router,
                cond: RibCond::And(
                    neighbor
                        .iter()
                        .map(|x| RibCond::LearnedFrom(*x))
                        .chain(once(RibCond::Prefix(prefix)))
                        .chain(weight.iter().map(|x| RibCond::Weight(*x)))
                        .chain(next_hop.iter().map(|x| RibCond::NextHop(*x)))
                        .collect(),
                ),
            },
            AtomicCondition::BgpSessionEstablished { router, neighbor } => {
                AtomicConditionExt::BgpSessionEstablished { router, neighbor }
            }
            AtomicCondition::RoutesLessPreferred {
                router,
                prefix,
                good_neighbors,
                route,
            } => AtomicConditionExt::RoutesLessPreferred {
                router,
                prefix,
                good_neighbors,
                route,
            },
        }
    }
}

impl<'a, 'n, P: Prefix, Q> NetworkFormatter<'a, 'n, P, Q> for AtomicCondition<P> {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        match self {
            Self::None => String::from("None"),
            Self::SelectedRoute {
                router,
                prefix,
                neighbor,
                weight,
                next_hop,
            } => {
                let from = neighbor
                    .as_ref()
                    .map(|n| format!(" from {}", n.fmt(net)))
                    .unwrap_or_default();
                let weight = weight
                    .map(|x| format!(" with weight {x}"))
                    .unwrap_or_default();
                let nh = next_hop
                    .map(|x| format!(" via {}", x.fmt(net)))
                    .unwrap_or_default();
                format!(
                    "{} selects route for {prefix}{from}{nh}{weight}",
                    router.fmt(net)
                )
            }
            Self::AvailableRoute {
                router,
                prefix,
                neighbor,
                weight,
                next_hop,
            } => {
                let from = neighbor
                    .as_ref()
                    .map(|n| format!(" from {}", n.fmt(net)))
                    .unwrap_or_default();
                let weight = weight
                    .map(|x| format!(" with weight {x}"))
                    .unwrap_or_default();
                let nh = next_hop
                    .map(|x| format!(" via {}", x.fmt(net)))
                    .unwrap_or_default();
                format!(
                    "{} knows route for {prefix}{from}{nh}{weight}",
                    router.fmt(net)
                )
            }
            AtomicCondition::BgpSessionEstablished { router, neighbor } => {
                format!(
                    "BGP session berween {} and {} is established.",
                    router.fmt(net),
                    neighbor.fmt(net)
                )
            }
            AtomicCondition::RoutesLessPreferred {
                router,
                prefix,
                good_neighbors,
                ..
            } => {
                format!(
                    "Routes at {} for {prefix} from {} are most preferred.",
                    router.fmt(net),
                    good_neighbors.iter().map(|n| n.fmt(net)).join(" and ")
                )
            }
        }
    }
}

/// Condition for a command to be executed / or to check if it has been executed. These conditions
/// are the extended version of [`AtomicCondition`] that cat check arbitrary expressions.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[cfg_attr(
    feature = "serde",
    serde(bound(deserialize = "P: for<'a> Deserialize<'a>"))
)]
pub enum AtomicConditionExt<P: Prefix> {
    /// No condition necessary.
    None,
    /// Condition on the selected route of a router.
    CurrentRib {
        /// Router which must be checked
        router: RouterId,
        /// Destination which must be checked.
        prefix: P,
        /// Condition on the currently selected RIB entry.
        cond: Option<RibCond<P>>,
    },
    /// Condition that must match on all incoming RIB entries.
    AllKnownRoutes {
        /// Router which must be checked
        router: RouterId,
        /// Condition on a single rib entry
        cond: RibCond<P>,
    },
    /// Condition that must match on at least one incoming RIB entry.
    AnyKnownRoute {
        /// Router which must be checked
        router: RouterId,
        /// Condition on a single rib entry
        cond: RibCond<P>,
    },
    /// A given BGP session is established
    BgpSessionEstablished {
        /// Router which must be checked
        router: RouterId,
        /// Neighbor that must be active.
        neighbor: RouterId,
    },
    /// Condition that checks if all routes received by a node are less preferred than the provided
    /// ones, before actually removing the weight rewrite.
    RoutesLessPreferred {
        /// Which routers should be checkd
        router: RouterId,
        /// Destination which must be checked.
        prefix: P,
        /// Which neighbors are supposed to be preferred and can be ignored
        good_neighbors: BTreeSet<RouterId>,
        /// the route that we will receive from the good neighbors. Any other route must be less
        /// preferred than this one.
        route: BgpRibEntry<P>,
    },
}

impl<P: Prefix> AtomicConditionExt<P> {
    /// Check if the condition holds for a given RIB entry.
    pub fn check<Q>(&self, net: &Network<P, Q>) -> Result<bool, NetworkError> {
        match self {
            AtomicConditionExt::None | AtomicConditionExt::BgpSessionEstablished { .. } => Ok(true),
            AtomicConditionExt::CurrentRib {
                router,
                prefix,
                cond,
            } => {
                let rib = net
                    .get_device(*router)
                    .internal_or_err()?
                    .get_selected_bgp_route(*prefix);
                Ok(match (rib, cond) {
                    (None, None) => true,
                    (Some(rib), Some(cond)) => cond.check(rib),
                    _ => false,
                })
            }
            AtomicConditionExt::AllKnownRoutes { router, cond } => Ok(net
                .get_device(*router)
                .internal_or_err()?
                .get_processed_bgp_rib()
                .values()
                .flatten()
                .all(|(x, _)| cond.check(x))),
            AtomicConditionExt::AnyKnownRoute { router, cond } => Ok(net
                .get_device(*router)
                .internal_or_err()?
                .get_processed_bgp_rib()
                .values()
                .flatten()
                .any(|(x, _)| cond.check(x))),
            AtomicConditionExt::RoutesLessPreferred {
                router,
                prefix,
                good_neighbors,
                route,
            } => {
                let rib_in = net
                    .get_device(*router)
                    .internal_or_err()?
                    .get_processed_bgp_rib()
                    .get(prefix)
                    .cloned();

                Ok(rib_in
                    .iter()
                    .flatten()
                    .filter(|(e, _)| !good_neighbors.contains(&e.from_id))
                    .all(|(e, _)| e < route)
                    && rib_in
                        .iter()
                        .flatten()
                        .filter(|(e, _)| good_neighbors.contains(&e.from_id))
                        .all(|(e, _)| e.route.next_hop == route.route.next_hop))
            }
        }
    }

    /// Returns `true` if the condition is None.
    pub fn is_none(&self) -> bool {
        matches!(self, AtomicConditionExt::None)
    }
}

/// Condition on a single RIB entry, recursively.
// #[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[cfg_attr(
    feature = "serde",
    serde(bound(deserialize = "P: for<'a> Deserialize<'a>"))
)]
pub enum RibCond<P: Prefix> {
    /// Inverting a condition on the RIB entry.
    Not(Box<RibCond<P>>),
    /// Conjunctive condition
    And(Vec<RibCond<P>>),
    /// Disjunctive condition
    Or(Vec<RibCond<P>>),
    /// Check the prefix
    Prefix(P),
    /// Check the learned-from attribute
    LearnedFrom(RouterId),
    /// Check that a specific community is set
    CommunityContains(u32),
    /// Check that the route has the given weight
    Weight(u32),
    /// Check that the route has the given next-hop
    NextHop(RouterId),
}

impl<P: Prefix> RibCond<P> {
    /// Check if the condition holds for a given RIB entry.
    pub fn check(&self, rib: &BgpRibEntry<P>) -> bool {
        match self {
            RibCond::Not(c) => !c.check(rib),
            RibCond::And(cs) => cs.iter().all(|c| c.check(rib)),
            RibCond::Or(cs) => cs.iter().any(|c| c.check(rib)),
            RibCond::Prefix(p) => rib.route.prefix == *p,
            RibCond::LearnedFrom(r) => rib.from_id == *r,
            RibCond::CommunityContains(c) => rib.route.community.contains(c),
            RibCond::Weight(w) => rib.weight == *w,
            RibCond::NextHop(nh) => rib.route.next_hop == *nh,
        }
    }
}

impl<'a, 'n, P: Prefix, Q> NetworkFormatter<'a, 'n, P, Q> for AtomicConditionExt<P> {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        match self {
            AtomicConditionExt::None => String::from("None"),
            AtomicConditionExt::CurrentRib {
                router,
                prefix,
                cond: Some(cond),
            } => {
                format!(
                    "RibCurrent at {} for {}: {}",
                    router.fmt(net),
                    prefix,
                    cond.fmt(net)
                )
            }
            AtomicConditionExt::CurrentRib {
                router,
                prefix,
                cond: None,
            } => {
                format!("RibCurrent at {} for {}: None", router.fmt(net), prefix,)
            }
            AtomicConditionExt::AllKnownRoutes { router, cond } => {
                format!("RibInAll at {}: {}", router.fmt(net), cond.fmt(net))
            }
            AtomicConditionExt::AnyKnownRoute { router, cond } => {
                format!("RibInAny at {}: {}", router.fmt(net), cond.fmt(net))
            }
            AtomicConditionExt::BgpSessionEstablished { router, neighbor } => format!(
                "BGP Session between {} and {} established",
                router.fmt(net),
                neighbor.fmt(net)
            ),
            AtomicConditionExt::RoutesLessPreferred {
                router,
                prefix,
                good_neighbors,
                ..
            } => format!(
                "Routes at {} for {prefix} from {} are most preferred",
                router.fmt(net),
                good_neighbors.iter().map(|n| n.fmt(net)).join(" or ")
            ),
        }
    }
}

impl<'a, 'n, P: Prefix, Q> NetworkFormatter<'a, 'n, P, Q> for RibCond<P> {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        match self {
            RibCond::Not(c) => format!("!{}", c.fmt(net)),
            RibCond::And(cs) => format!("({})", cs.iter().map(|x| x.fmt(net)).join(" && ")),
            RibCond::Or(cs) => format!("({})", cs.iter().map(|x| x.fmt(net)).join(" || ")),
            RibCond::Prefix(p) => p.to_string(),
            RibCond::LearnedFrom(r) => format!("from {}", r.fmt(net)),
            RibCond::CommunityContains(c) => format!("Community {c}"),
            RibCond::Weight(w) => format!("Weight {w}"),
            RibCond::NextHop(x) => format!("nh {}", x.fmt(net)),
        }
    }
}
