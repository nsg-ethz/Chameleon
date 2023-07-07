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

use itertools::Itertools;
use std::time::Duration;

use bgpsim::prelude::*;
use cisco_lab::{export_capture_to_csv, CiscoLab, CiscoLabError};

#[tokio::main]
async fn main() -> Result<(), CiscoLabError> {
    pretty_env_logger::init();

    let (net, e2) = net! {
        Prefix = SinglePrefix;
        sessions = {
            e1!(100) -> r;
            e2!(200) -> r;
        };
        routes = {
            e1 -> SinglePrefix as {path: [100, 100, 100, 100, 1000]};
        };
        return e2
    };

    // create the lab
    let mut lab = CiscoLab::new(&net)?;
    let capture_frequency = 10_000;
    let num_samples = 10;

    // setup route flapping
    for _ in 0..num_samples {
        lab.step_external_time();
        lab.advertise_route(
            e2,
            &BgpRoute {
                prefix: SinglePrefix,
                as_path: vec![200.into(), 1000.into()],
                next_hop: e2,
                local_pref: None,
                med: None,
                community: Default::default(),
                originator_id: None,
                cluster_list: Default::default(),
            },
        )
        .unwrap();
        lab.step_external_time();
        lab.withdraw_route(e2, SinglePrefix).unwrap();
    }

    // connect the network
    let mut lab = lab.connect().await?;
    lab.wait_for_convergence().await?;

    // add some data-plane traffic with 1Gbps and UDP traffic
    let iperf_handle = lab.start_iperf(1, true).await?;

    for i in 0..2 * num_samples {
        if i % 2 == 0 {
            println!("Announcing new best route:");
        } else {
            println!("Withdraw best route:");
        }

        // start the capture
        let capture = lab.start_capture(capture_frequency).await?;

        tokio::time::sleep(Duration::from_secs(10)).await;
        lab.get_exabgp_handle().step().await?;
        tokio::time::sleep(Duration::from_secs(8)).await;

        lab.wait_for_no_bgp_messages(Duration::from_secs(2)).await?;

        let capture_result = lab.stop_capture(capture).await?;

        for ((rid, _, _), samples) in capture_result.iter().sorted_by(|a, b| a.0 .0.cmp(&b.0 .0)) {
            let len = samples.len();
            let total_num_samples = (samples.iter().map(|x| x.2).max().unwrap_or(0)
                - samples.iter().map(|x| x.2).min().unwrap_or(0))
                as usize
                + 1;
            println!(
                "router {:?}: found {:?}/{:?} ({:.2}%) --> violation: ~{:.2}ms",
                rid,
                len,
                total_num_samples,
                (len * 100) as f64 / total_num_samples as f64,
                (total_num_samples - len) as f64 / (capture_frequency as f64 / 1000.0),
            );
        }

        export_capture_to_csv(
            &net,
            &capture_result,
            "example_data",
            if i % 2 == 0 {
                "flapping_announce"
            } else {
                "flapping_withdraw"
            },
        )?;

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    lab.stop_iperf(iperf_handle).await?;

    // disconnect the network.
    let _ = lab.disconnect().await?;

    // wait for one second
    tokio::time::sleep(Duration::from_secs(1)).await;

    Ok(())
}
