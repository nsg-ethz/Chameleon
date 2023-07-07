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

use crate::router::{OspfNeighbor, OspfRoute};

#[test]
fn document_parsing() {
    let doc = include_str!("files/show_ip_ospf_routes.xml");
    let parsed = OspfRoute::from_xml_output(doc).unwrap();
    assert_eq!(parsed.len(), 14);
    assert_eq!(
        parsed[&"1.0.0.1/32".parse().unwrap()],
        OspfRoute {
            net: "1.0.0.1/32".parse().unwrap(),
            area: "0.0.0.0".parse().unwrap(),
            nh_addr: "1.128.0.1".parse().unwrap(),
            nh_iface: String::from("Eth4/25")
        }
    )
}

#[test]
fn neighbors() {
    let table = "\
 OSPF Process ID 10 VRF default
 Total number of neighbors: 4
 Neighbor ID     Pri State            Up Time  Address         Interface
 1.0.2.1           1 FULL/DR          01:33:55 1.128.0.2       Eth4/2
 1.0.1.1           1 FULL/DR          01:33:54 1.128.0.6       Eth4/3
 1.0.3.1           1 FULL/DR          01:33:55 1.128.0.10      Eth4/4
 1.0.4.1           1 FULL/DR          01:33:53 1.128.0.14      Eth4/5";
    let parsed = OspfNeighbor::from_table(table).unwrap();
    assert_eq!(
        parsed,
        vec![
            OspfNeighbor {
                id: "1.0.2.1".parse().unwrap(),
                address: "1.128.0.2".parse().unwrap(),
                iface: String::from("Ethernet4/2"),
            },
            OspfNeighbor {
                id: "1.0.1.1".parse().unwrap(),
                address: "1.128.0.6".parse().unwrap(),
                iface: String::from("Ethernet4/3"),
            },
            OspfNeighbor {
                id: "1.0.3.1".parse().unwrap(),
                address: "1.128.0.10".parse().unwrap(),
                iface: String::from("Ethernet4/4"),
            },
            OspfNeighbor {
                id: "1.0.4.1".parse().unwrap(),
                address: "1.128.0.14".parse().unwrap(),
                iface: String::from("Ethernet4/5"),
            },
        ],
    );
}
