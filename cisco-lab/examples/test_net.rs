// BgpSim: BGP Network Simulator written in Rust
// Copyright (C) 2022-2023 Tibor Schneider <sctibor@ethz.ch>
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

use bgpsim::{
    builder::{constant_link_weight, NetworkBuilder},
    prelude::*,
    route_map::{RouteMapBuilder, RouteMapDirection::Incoming},
};

/// Generate the network for examples.
pub fn generate_net<P: Prefix>() -> Result<Network<P, BasicEventQueue<P>>, NetworkError> {
    let mut net = Network::new(BasicEventQueue::<P>::new());

    net.add_router("R0");
    net.add_router("R1");
    net.add_router("R2");
    net.add_router("R3");
    net.add_router("R4");

    net.add_link(0.into(), 1.into());
    net.add_link(0.into(), 2.into());
    net.add_link(0.into(), 3.into());
    net.add_link(0.into(), 4.into());
    net.add_link(1.into(), 2.into());
    net.add_link(1.into(), 4.into());
    net.add_link(2.into(), 3.into());

    net.build_external_routers(|_, _| vec![0.into(), 4.into()], ())?;
    net.build_link_weights(constant_link_weight, 10.0)?;
    net.build_ebgp_sessions()?;
    net.build_ibgp_route_reflection(|_, _| vec![2.into()], ())?;
    net.build_advertisements(P::from(0), |_, _| vec![vec![5.into()], vec![6.into()]], ())?;

    net.set_bgp_route_map(
        0.into(),
        5.into(),
        Incoming,
        RouteMapBuilder::new()
            .allow()
            .order(100)
            .set_community(5)
            .build(),
    )?;

    net.set_bgp_route_map(
        4.into(),
        6.into(),
        Incoming,
        RouteMapBuilder::new()
            .allow()
            .order(100)
            .set_community(6)
            .build(),
    )?;

    Ok(net)
}

#[allow(dead_code)]
fn main() {}
