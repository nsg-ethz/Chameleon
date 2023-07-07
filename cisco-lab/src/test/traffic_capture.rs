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

use std::net::Ipv4Addr;

use crate::server::traffic_capture::{CollectorSample, ProberSample};

#[test]
fn collector_sample_parser() {
    assert_eq!(
        CollectorSample::from_line("4.893623173,de:ad:00:7a:05:19,1.0.4.2,3.0.0.1,220"),
        Some(CollectorSample {
            time: 4.893623173,
            mac: [0xde, 0xad, 0x00, 0x7a, 0x05, 0x19],
            src_ip: Ipv4Addr::new(1, 0, 4, 2),
            dst_ip: Ipv4Addr::new(3, 0, 0, 1),
            counter: 220,
        })
    )
}

#[test]
fn prober_sample_parser() {
    assert_eq!(
        ProberSample::from_line("1677771394.974495575,1.0.3.6,100.0.0.1,0").unwrap(),
        ProberSample {
            time: 1677771394.974495575,
            src_ip: "1.0.3.6".parse().unwrap(),
            dst_ip: "100.0.0.1".parse().unwrap(),
            counter: 0
        }
    );
}
