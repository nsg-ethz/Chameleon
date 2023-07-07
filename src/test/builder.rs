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

use bgpsim::{prelude::BasicEventQueue, topology_zoo::TopologyZoo};

use crate::experiment::Scenario;

#[test]
fn deterministic_builder_abilene() {
    let topo = TopologyZoo::Abilene;
    for scenario in [Scenario::DelBestRoute, Scenario::NewBestRoute] {
        let (net_a, _, cmd_a) = scenario.build(topo, BasicEventQueue::new(), false).unwrap();
        let (net_b, _, cmd_b) = scenario.build(topo, BasicEventQueue::new(), false).unwrap();
        assert_eq!(cmd_a, cmd_b);
        assert_eq!(net_a, net_b);
    }
}

#[test]
fn deterministic_builder_uninett() {
    let topo = TopologyZoo::Uninett2011;
    for scenario in [Scenario::DelBestRoute, Scenario::NewBestRoute] {
        let (net_a, _, cmd_a) = scenario.build(topo, BasicEventQueue::new(), false).unwrap();
        let (net_b, _, cmd_b) = scenario.build(topo, BasicEventQueue::new(), false).unwrap();
        assert_eq!(cmd_a, cmd_b);
        assert_eq!(net_a, net_b);
    }
}
