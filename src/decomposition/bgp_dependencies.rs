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

//! This module analyzes the difference in BGP state and computes the high-level dependencies of the
//! control-plane (for which violations should be minimized).

use std::collections::{BTreeSet, HashMap};

use bgpsim::{
    bgp::{BgpRoute, BgpState},
    prelude::*,
};
use log::info;

use super::CommandInfo;
use crate::P;

/// Extract all BGP dependencies from the BGP state before and after. This function will extract not
/// only the selected routes, but also all routes that are received from other routers, but are
/// essentially the same.
pub fn find_dependencies<Q>(info: &'_ CommandInfo<'_, Q>) -> HashMap<P, BgpDependencies> {
    info!("Extract the BGP Dependencies.");
    let mut result = HashMap::new();

    let is_internal = |r: RouterId| info.net_before.get_device(r).is_internal();

    // iterate over all prefixes
    for prefix in info.prefixes.iter().copied() {
        let mut deps = BgpDependencies::new();

        let bgp_before = info.bgp_before.get(&prefix);
        let bgp_after = info.bgp_after.get(&prefix);

        // iterate over all internal routers
        for router in info.net_before.get_routers() {
            if !is_internal(router) {
                continue;
            }
            match (
                bgp_before.and_then(|x| x.get(router)),
                bgp_after.and_then(|x| x.get(router)),
            ) {
                (None, Some((new_from, new_route))) if is_internal(new_from) => {
                    deps.insert(
                        router,
                        BgpDependency {
                            old_from: BTreeSet::new(),
                            new_from: get_peers_advertising_route(
                                info, bgp_after, router, new_from, new_route,
                            ),
                        },
                    );
                }
                (Some((old_from, old_route)), None) if is_internal(old_from) => {
                    deps.insert(
                        router,
                        BgpDependency {
                            old_from: get_peers_advertising_route(
                                info, bgp_before, router, old_from, old_route,
                            ),
                            new_from: BTreeSet::new(),
                        },
                    );
                }
                (Some((old_from, old_route)), Some((new_from, new_route)))
                    if (old_from, old_route) != (new_from, new_route) =>
                {
                    deps.insert(
                        router,
                        BgpDependency {
                            old_from: get_peers_advertising_route(
                                info, bgp_before, router, old_from, old_route,
                            ),
                            new_from: get_peers_advertising_route(
                                info, bgp_after, router, new_from, new_route,
                            ),
                        },
                    );
                }
                _ => {}
            }
        }

        result.insert(prefix, deps);
    }

    result
}

/// Compute the list of peers that advertise `route`.
///
/// We use a custom equality here, that ignores `cluster_list` and `from_type`, and compares
/// `originator_id.unwrap_or(from_id)` instead of `originator_id` or `from_id`.
#[inline]
fn get_peers_advertising_route<Q>(
    info: &CommandInfo<'_, Q>,
    bgp_state: Option<&BgpState<P>>,
    router: RouterId,
    from: RouterId,
    route: &BgpRoute<P>,
) -> BTreeSet<RouterId> {
    let is_internal = |r: RouterId| info.net_before.get_device(r).is_internal();
    let mut result: BTreeSet<RouterId> = bgp_state
        .iter()
        .flat_map(|x| x.incoming(router))
        .filter(|(f, _)| is_internal(*f))
        .filter(|(f, r)| {
            (
                &r.as_path,
                &r.community,
                &r.local_pref,
                &r.med,
                &r.next_hop,
                &r.prefix,
                r.originator_id.as_ref().unwrap_or(f),
            ) == (
                &route.as_path,
                &route.community,
                &route.local_pref,
                &route.med,
                &route.next_hop,
                &route.prefix,
                route.originator_id.as_ref().unwrap_or(&from),
            )
        })
        .map(|(f, _)| f)
        .collect();
    if is_internal(from) {
        result.insert(from);
    }
    result
}

/// BGP dependencies for a specific prefix.
pub type BgpDependencies = HashMap<RouterId, BgpDependency>;

/// A single BGP dependency for an individual router and prefix. It captures from where the old /
/// new rotue was / will be learned (or multiple if multiple route reflectors advertise the same
/// route).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct BgpDependency {
    /// Routers from where the old rotue was learned.
    pub old_from: BTreeSet<RouterId>,
    /// Rotuers from where the new route will be learned.
    pub new_from: BTreeSet<RouterId>,
}
