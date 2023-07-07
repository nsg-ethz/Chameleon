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

use std::collections::BTreeMap;

use bgpsim::types::AsId;
use maplit::{btreemap, btreeset};
use pretty_assertions::assert_eq;

use crate::router::{BgpNeighbor, BgpPathType, BgpRoute};

#[test]
fn neighbors_table() {
    let table = "\
BGP summary information for VRF default, address family IPv4 Unicast
BGP router identifier 1.0.0.1, local AS number 65535
BGP table version is 57, IPv4 Unicast config peers 2, capable peers 1
1 network entries and 2 paths using 360 bytes of memory
BGP attribute entries [2/336], BGP AS path entries [0/0]
BGP community entries [0/0], BGP clusterlist entries [0/0]

Neighbor        V    AS MsgRcvd MsgSent   TblVer  InQ OutQ Up/Down  State/PfxRcd
1.0.1.1         4 65535     170     168       57    0    0 02:41:42 1
1.192.0.2       4     5     111     110        0    0    0 00:55:54 Idle";

    let parsed = BgpNeighbor::from_table(table).unwrap();
    assert_eq!(
        parsed,
        vec![
            BgpNeighbor {
                id: "1.0.1.1".parse().unwrap(),
                as_id: AsId(65535),
                connected: true,
                routes_received: 1
            },
            BgpNeighbor {
                id: "1.192.0.2".parse().unwrap(),
                as_id: AsId(5),
                connected: false,
                routes_received: 0
            },
        ]
    )
}

#[test]
fn bgp_table_details() -> Result<(), Box<dyn std::error::Error>> {
    let table = include_str!("files/bgp_table_detail");
    let routes = BgpRoute::from_detail(table)
        .unwrap()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        routes,
        btreemap! {
            "1.0.0.0/8".parse()? => vec![
                BgpRoute {
                    net: "1.0.0.0/8".parse()?,
                    next_hop: "1.0.2.1".parse()?,
                    med: None,
                    local_pref: Some(100),
                    weight: 100,
                    igp_cost: 105,
                    path: Default::default(),
                    communities: Default::default(),
                    neighbor: "1.0.2.1".parse()?,
                    neighbor_id: "1.0.2.1".parse()?,
                    valid: true,
                    selected: false,
                    path_type: BgpPathType::Internal,
                },
                BgpRoute {
                    net: "1.0.0.0/8".parse()?,
                    next_hop: "1.0.1.1".parse()?,
                    med: None,
                    local_pref: Some(100),
                    weight: 100,
                    igp_cost: 182,
                    path: Default::default(),
                    communities: Default::default(),
                    neighbor: "1.0.1.1".parse()?,
                    neighbor_id: "1.0.1.1".parse()?,
                    valid: true,
                    selected: false,
                    path_type: BgpPathType::Internal,
                },
                BgpRoute {
                    net: "1.0.0.0/8".parse()?,
                    next_hop: "1.0.3.1".parse()?,
                    med: None,
                    local_pref: Some(100),
                    weight: 100,
                    igp_cost: 60,
                    path: Default::default(),
                    communities: Default::default(),
                    neighbor: "1.0.3.1".parse()?,
                    neighbor_id: "1.0.3.1".parse()?,
                    valid: true,
                    selected: false,
                    path_type: BgpPathType::Internal,
                },
                BgpRoute {
                    net: "1.0.0.0/8".parse()?,
                    next_hop: "0.0.0.0".parse()?,
                    med: None,
                    local_pref: Some(100),
                    weight: 32768,
                    igp_cost: 0,
                    path: Default::default(),
                    communities: Default::default(),
                    neighbor: "0.0.0.0".parse()?,
                    neighbor_id: "1.0.5.1".parse()?,
                    valid: true,
                    selected: true,
                    path_type: BgpPathType::Local,
                },
            ],
            "3.0.0.0/24".parse()? => vec![
                BgpRoute {
                    net: "3.0.0.0/24".parse()?,
                    next_hop: "1.192.0.6".parse()?,
                    med: Some(200),
                    local_pref: Some(100),
                    weight: 100,
                    igp_cost: 0,
                    path: vec![AsId(12), AsId(12), AsId(101)],
                    communities: btreeset![(AsId(65535), 20), (AsId(65535), 10003)],
                    neighbor: "1.192.0.6".parse()?,
                    neighbor_id: "2.0.1.1".parse()?,
                    valid: true,
                    selected: false,
                    path_type: BgpPathType::External,
                },
                BgpRoute {
                    net: "3.0.0.0/24".parse()?,
                    next_hop: "1.0.3.1".parse()?,
                    med: None,
                    local_pref: Some(100),
                    weight: 100,
                    igp_cost: 60,
                    path: vec![AsId(11), AsId(101)],
                    communities: btreeset![(AsId(65535), 10001)],
                    neighbor: "1.0.2.1".parse()?,
                    neighbor_id: "1.0.2.1".parse()?,
                    valid: false,
                    selected: false,
                    path_type: BgpPathType::Internal,
                },
                BgpRoute {
                    net: "3.0.0.0/24".parse()?,
                    next_hop: "1.0.3.1".parse()?,
                    med: None,
                    local_pref: Some(100),
                    weight: 100,
                    igp_cost: 60,
                    path: vec![AsId(11), AsId(101)],
                    communities: btreeset![(AsId(65535), 10001)],
                    neighbor: "1.0.1.1".parse()?,
                    neighbor_id: "1.0.1.1".parse()?,
                    valid: true,
                    selected: false,
                    path_type: BgpPathType::Internal,
                },
                BgpRoute {
                    net: "3.0.0.0/24".parse()?,
                    next_hop: "1.0.3.1".parse()?,
                    med: None,
                    local_pref: Some(100),
                    weight: 200,
                    igp_cost: 60,
                    path: vec![AsId(11), AsId(101)],
                    communities: btreeset![(AsId(65535), 10001)],
                    neighbor: "1.0.3.1".parse()?,
                    neighbor_id: "1.0.3.1".parse()?,
                    valid: true,
                    selected: true,
                    path_type: BgpPathType::Internal,
                },
            ],
        }
    );

    Ok(())
}

#[test]
fn invalid_path() -> Result<(), Box<dyn std::error::Error>> {
    let table = "\
BGP routing table entry for 1.0.0.0/8, version 161
Paths: (4 available, best #4)
Flags: (0x080002) (high32 00000000) on xmit-list, is not in urib

  Path type: external, path is invalid, no labeled nexthop
  AS-Path: 13 100 , path sourced external to AS
    1.192.0.10 (metric 0) from 1.192.0.10 (2.0.2.1)
      Origin IGP, MED not set, localpref 100, weight 100

  Path-id 1 advertised to peers:
    1.0.0.1            1.0.4.1            1.0.5.1            1.0.6.1
    1.0.7.1            1.0.8.1            1.0.9.1            1.0.10.1
    1.192.0.10
";
    let routes = BgpRoute::from_detail(table)
        .unwrap()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        routes,
        btreemap! {
            "1.0.0.0/8".parse()? => vec![
                BgpRoute {
                    net: "1.0.0.0/8".parse()?,
                    next_hop: "1.192.0.10".parse()?,
                    med: None,
                    local_pref: Some(100),
                    weight: 100,
                    igp_cost: 0,
                    path: vec![AsId(13), AsId(100)],
                    communities: btreeset![],
                    neighbor: "1.192.0.10".parse()?,
                    neighbor_id: "2.0.2.1".parse()?,
                    valid: false,
                    selected: false,
                    path_type: BgpPathType::External,
                },
            ]
        }
    );

    Ok(())
}
