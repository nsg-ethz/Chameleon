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

//! Test the system with a scenario that is simple and has no dependencies whatsoever.

use bgpsim::{
    builder::{constant_link_weight, unique_preferences, NetworkBuilder},
    config::{ConfigExpr, ConfigModifier},
    prelude::*,
};
use test_log::test;

use crate::{
    decomposition::decompose,
    runtime::sim::run,
    specification::{Specification, SpecificationBuilder},
    P,
};

fn get_net() -> Network<P, BasicEventQueue<P>> {
    let _ = pretty_env_logger::formatted_builder()
        .is_test(true)
        .try_init();

    let mut net: Network<P, BasicEventQueue<P>> =
        NetworkBuilder::build_complete_graph(BasicEventQueue::<P>::new(), 4);
    net.build_external_routers(|_, _| vec![RouterId::from(0), RouterId::from(2)], ())
        .unwrap();
    net.build_ibgp_route_reflection(|_, _| vec![RouterId::from(3)], ())
        .unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_link_weights(constant_link_weight, 10.0).unwrap();
    net.set_link_weight(0.into(), 3.into(), 1.0).unwrap();
    net.set_link_weight(3.into(), 1.into(), 1.0).unwrap();
    net.set_link_weight(2.into(), 1.into(), 1.0).unwrap();
    net.set_link_weight(3.into(), 0.into(), 1.0).unwrap();
    net.set_link_weight(1.into(), 3.into(), 1.0).unwrap();
    net.set_link_weight(1.into(), 2.into(), 1.0).unwrap();
    net
}

#[allow(clippy::type_complexity)]
fn prepare() -> (
    Network<P, BasicEventQueue<P>>,
    RouterId,
    RouterId,
    Specification,
    P,
) {
    let mut net = get_net();
    let p = P::from(0);
    let ads = net.build_advertisements(p, unique_preferences, 2).unwrap();
    let spec = SpecificationBuilder::Reachability.build_all(&net, None, [p]);

    let e = ads[0][0];
    let r = net
        .get_device(e)
        .unwrap_external()
        .get_bgp_sessions()
        .iter()
        .next()
        .copied()
        .unwrap();

    (net, r, e, spec, p)
}

/// Clique with 4 nodes, and two external nodes, changing from the old to the new one.
#[test]
fn remove_session() {
    let (net, r, e, spec, _) = prepare();

    let command = ConfigModifier::Remove(ConfigExpr::BgpSession {
        source: r,
        target: e,
        session_type: BgpSessionType::EBgp,
    });

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}

#[test]
fn add_session() {
    let (mut net, r, e, spec, _) = prepare();

    net.set_bgp_session(r, e, None).unwrap();

    let command = ConfigModifier::Insert(ConfigExpr::BgpSession {
        source: r,
        target: e,
        session_type: BgpSessionType::EBgp,
    });

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}

#[allow(clippy::type_complexity)]
fn prepare_2_prefixes() -> (
    Network<P, BasicEventQueue<P>>,
    RouterId,
    RouterId,
    Specification,
    Vec<P>,
) {
    let mut net = get_net();
    let p0 = P::from(0);
    let p1 = P::from(1);
    let ads = net.build_advertisements(p0, unique_preferences, 2).unwrap();
    let ads = net.build_advertisements(p1, |_, _| ads, ()).unwrap();
    let spec = SpecificationBuilder::Reachability.build_all(&net, None, [p0, p1]);

    let e = ads[0][0];
    let r = net
        .get_device(e)
        .unwrap_external()
        .get_bgp_sessions()
        .iter()
        .next()
        .copied()
        .unwrap();

    (net, r, e, spec, vec![p0, p1])
}

/// Clique with 4 nodes, and two external nodes, changing from the old to the new one.
#[test]
fn remove_session_2_prefixes() {
    let (net, r, e, spec, _) = prepare_2_prefixes();

    let command = ConfigModifier::Remove(ConfigExpr::BgpSession {
        source: r,
        target: e,
        session_type: BgpSessionType::EBgp,
    });

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}

#[test]
fn add_session_2_prefixes() {
    let (mut net, r, e, spec, _) = prepare_2_prefixes();

    net.set_bgp_session(r, e, None).unwrap();

    let command = ConfigModifier::Insert(ConfigExpr::BgpSession {
        source: r,
        target: e,
        session_type: BgpSessionType::EBgp,
    });

    let decomposition = decompose(&net, command, &spec).unwrap();
    run(net, decomposition, &spec).unwrap();
}
