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

//! Integration test that executes an update on abilene with three route reflectors.

use bgpsim::{
    builder::{constant_link_weight, NetworkBuilder},
    config::{ConfigExpr, ConfigModifier},
    prelude::*,
    route_map::{RouteMapBuilder, RouteMapDirection::Incoming},
    topology_zoo::TopologyZoo,
};
use test_log::test;

use crate::{
    decomposition::decompose,
    runtime::sim::run,
    specification::{Specification, SpecificationBuilder},
    P,
};

/// Get the router for new york
#[allow(dead_code)]
fn ny() -> RouterId {
    0.into()
}

/// Get the router for chicago
#[allow(dead_code)]
fn ch() -> RouterId {
    1.into()
}

/// Get the router for  washington_dc.
#[allow(dead_code)]
fn dc() -> RouterId {
    2.into()
}

/// Get the router for  seattle.
#[allow(dead_code)]
fn se() -> RouterId {
    3.into()
}

/// Get the router for  sunnyvale.
#[allow(dead_code)]
fn su() -> RouterId {
    4.into()
}

/// Get the router for  los_angeles.
#[allow(dead_code)]
fn la() -> RouterId {
    5.into()
}

/// Get the router for  denver.
#[allow(dead_code)]
fn dv() -> RouterId {
    6.into()
}

/// Get the router for  kansas_city.
#[allow(dead_code)]
fn ka() -> RouterId {
    7.into()
}

/// Get the router for  houston.
#[allow(dead_code)]
fn hs() -> RouterId {
    8.into()
}

/// Get the router for  atlanta.
#[allow(dead_code)]
fn at() -> RouterId {
    9.into()
}

/// Get the router for  indianapolis.
#[allow(dead_code)]
fn ia() -> RouterId {
    10.into()
}

/// Get the external router on houston
#[allow(dead_code)]
fn hs_ext() -> RouterId {
    11.into()
}

/// Get the external router on sunnyvalse
#[allow(dead_code)]
fn su_ext() -> RouterId {
    12.into()
}

/// Get the external router on new york
#[allow(dead_code)]
fn ny_ext() -> RouterId {
    13.into()
}

/// get the network. The routing advertisements are: `hs, (su, ny)`.
fn get_net() -> (Network<P, BasicEventQueue<P>>, P, Specification) {
    let mut net = TopologyZoo::Abilene.build(BasicEventQueue::<P>::new());
    let p = P::from(0);

    assert_eq!(hs_ext(), net.add_external_router("houston_ext", 101));
    assert_eq!(su_ext(), net.add_external_router("sunnyvale_ext", 102));
    assert_eq!(ny_ext(), net.add_external_router("new_york_ext", 103));

    net.add_link(hs_ext(), hs());
    net.add_link(su_ext(), su());
    net.add_link(ny_ext(), ny());

    net.build_link_weights(constant_link_weight, 10.0).unwrap();
    net.build_ibgp_route_reflection(|_, _| [dv(), at(), la()], ())
        .unwrap();
    net.build_ebgp_sessions().unwrap();

    net.advertise_external_route(hs_ext(), p, vec![101, 200, 300], None, None)
        .unwrap();
    net.advertise_external_route(su_ext(), p, vec![102, 200, 300], None, None)
        .unwrap();
    net.advertise_external_route(ny_ext(), p, vec![103, 200, 300], None, None)
        .unwrap();

    net.set_bgp_route_map(
        hs(),
        hs_ext(),
        Incoming,
        RouteMapBuilder::new()
            .order(10)
            .allow()
            .set_local_pref(200)
            .build(),
    )
    .unwrap();
    net.set_bgp_route_map(
        su(),
        su_ext(),
        Incoming,
        RouteMapBuilder::new()
            .order(10)
            .allow()
            .set_local_pref(150)
            .build(),
    )
    .unwrap();
    net.set_bgp_route_map(
        ny(),
        ny_ext(),
        Incoming,
        RouteMapBuilder::new()
            .order(10)
            .allow()
            .set_local_pref(150)
            .build(),
    )
    .unwrap();

    let spec = SpecificationBuilder::Reachability.build_all(&net, None, [p]);

    (net, p, spec)
}

#[allow(clippy::type_complexity)]
fn get_net_two_prefixes() -> (Network<P, BasicEventQueue<P>>, (P, P), Specification) {
    let (mut net, p0, _) = get_net();
    let p1 = P::from(1);

    net.advertise_external_route(hs_ext(), p1, vec![101, 200, 300], None, None)
        .unwrap();
    net.advertise_external_route(su_ext(), p1, vec![102, 200, 300], None, None)
        .unwrap();
    net.advertise_external_route(ny_ext(), p1, vec![103, 200, 300], None, None)
        .unwrap();

    let spec = SpecificationBuilder::Reachability.build_all(&net, None, [p0, p1]);

    (net, (p0, p1), spec)
}

#[test]
fn remove_session() {
    let (net, _p, spec) = get_net();
    let command = ConfigModifier::Remove(ConfigExpr::BgpSession {
        source: hs(),
        target: hs_ext(),
        session_type: BgpSessionType::EBgp,
    });

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}

#[test]
fn remove_route_map() {
    let (net, _p, spec) = get_net();
    let command = ConfigModifier::Remove(ConfigExpr::BgpRouteMap {
        router: hs(),
        neighbor: hs_ext(),
        direction: Incoming,
        map: RouteMapBuilder::new()
            .order(10)
            .allow()
            .set_local_pref(200)
            .build(),
    });

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}

#[test]
fn make_lowest_route_map() {
    let (net, _p, spec) = get_net();
    let command = ConfigModifier::Update {
        from: ConfigExpr::BgpRouteMap {
            router: hs(),
            neighbor: hs_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .set_local_pref(200)
                .build(),
        },
        to: ConfigExpr::BgpRouteMap {
            router: hs(),
            neighbor: hs_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .set_local_pref(50)
                .build(),
        },
    };

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}

#[test]
fn deny_route_map() {
    let (net, _p, spec) = get_net();
    let command = ConfigModifier::Update {
        from: ConfigExpr::BgpRouteMap {
            router: hs(),
            neighbor: hs_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .set_local_pref(200)
                .build(),
        },
        to: ConfigExpr::BgpRouteMap {
            router: hs(),
            neighbor: hs_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new().order(10).deny().build(),
        },
    };

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}

#[test]
fn empty_command() {
    let (net, p, spec) = get_net();
    let command = ConfigModifier::Update {
        from: ConfigExpr::BgpRouteMap {
            router: su(),
            neighbor: su_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .set_local_pref(150)
                .build(),
        },
        to: ConfigExpr::BgpRouteMap {
            router: su(),
            neighbor: su_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .set_local_pref(190)
                .build(),
        },
    };

    let decomposition = decompose(&net, command, &spec).unwrap();

    assert_eq!(decomposition.setup_commands.len(), 1);
    assert!(decomposition.setup_commands[0].is_empty());
    assert_eq!(decomposition.atomic_before.len(), 1);
    assert!(decomposition.atomic_before[&p].is_empty());
    assert_eq!(decomposition.main_commands.len(), 1);
    assert_eq!(decomposition.main_commands[0].len(), 1);
    assert_eq!(decomposition.atomic_after.len(), 1);
    assert!(decomposition.atomic_after[&p].is_empty());
    assert_eq!(decomposition.cleanup_commands.len(), 1);
    assert!(decomposition.cleanup_commands[0].is_empty());

    run(net, decomposition, &spec).unwrap();
}

#[test]
fn increase_other_route_map() {
    let (net, _p, spec) = get_net();
    let command = ConfigModifier::Update {
        from: ConfigExpr::BgpRouteMap {
            router: su(),
            neighbor: su_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .set_local_pref(150)
                .build(),
        },
        to: ConfigExpr::BgpRouteMap {
            router: su(),
            neighbor: su_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .set_local_pref(200)
                .build(),
        },
    };

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}

#[test]
fn inc_and_dec() {
    let (mut net, (p0, p1), spec) = get_net_two_prefixes();

    net.set_bgp_route_map(
        hs(),
        hs_ext(),
        Incoming,
        RouteMapBuilder::new()
            .order(10)
            .allow()
            .match_prefix(p0)
            .set_community(1)
            .build(),
    )
    .unwrap();
    net.set_bgp_route_map(
        hs(),
        hs_ext(),
        Incoming,
        RouteMapBuilder::new()
            .order(20)
            .allow()
            .match_community(1)
            .set_local_pref(200)
            .build(),
    )
    .unwrap();

    let command = ConfigModifier::Update {
        from: ConfigExpr::BgpRouteMap {
            router: hs(),
            neighbor: hs_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .match_prefix(p0)
                .set_community(1)
                .build(),
        },
        to: ConfigExpr::BgpRouteMap {
            router: hs(),
            neighbor: hs_ext(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .order(10)
                .allow()
                .match_prefix(p1)
                .set_community(1)
                .build(),
        },
    };

    let decomposition = decompose(&net, command, &spec).unwrap();
    assert!(!decomposition.atomic_before.is_empty());
    assert!(!decomposition.atomic_after.is_empty());
    run(net, decomposition, &spec).unwrap();
}
