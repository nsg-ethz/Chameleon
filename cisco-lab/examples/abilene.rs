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

use std::time::Duration;

use bgpsim::{
    builder::*,
    prelude::{BasicEventQueue, NetworkFormatter},
    topology_zoo::TopologyZoo,
    types::SimplePrefix as P,
};
use cisco_lab::{CiscoLab, CiscoLabError};
use tokio::time::timeout;

mod utils;

#[tokio::main]
async fn main() -> Result<(), CiscoLabError> {
    pretty_env_logger::init();

    // create the network
    let topo = TopologyZoo::Abilene;
    let mut net = topo.build(BasicEventQueue::<P>::new());
    let p = P::from(0);
    let se = net.get_router_id("Seattle")?;
    let ny = net.get_router_id("NewYork")?;
    let la = net.get_router_id("LosAngeles")?;
    let sn = net.get_router_id("Sunnyvale")?;
    let ka = net.get_router_id("KansasCity")?;
    let at = net.get_router_id("Atlanta")?;
    net.build_external_routers(|_, _| vec![se, ny, la], ())?;
    let e_se = net.get_router_id("Seattle_ext_11")?;
    let e_ny = net.get_router_id("NewYork_ext_12")?;
    let e_la = net.get_router_id("LosAngeles_ext_13")?;
    net.build_link_weights(uniform_integer_link_weight, (10, 100))?;
    net.build_ibgp_route_reflection(|_, _| vec![sn, ka, at], ())?;
    net.build_ebgp_sessions()?;
    net.build_advertisements(p, |_, _| vec![vec![e_ny], vec![e_se, e_la]], 3)?;

    // create the lab
    let mut lab = CiscoLab::new(&net)?;

    // set all link delays to 10ms
    lab.set_link_delays_from_geolocation(topo.geo_location());

    // write configuration to a file for debugging
    utils::write_config(&mut lab)?;

    // connect the network
    let mut lab = lab.connect().await?;
    lab.wait_for_convergence().await?;

    // start the capture
    let mut capture = lab.start_capture(10).await?;

    // wait for ctrl-c
    let mut pos = 0;
    println!("Network is running! Press Ctrl-C to exit!");
    loop {
        match timeout(Duration::from_millis(100), tokio::signal::ctrl_c()).await {
            Ok(_) => break,
            Err(_) => {
                let new_pos = capture.get_samples().await?.len();
                let new_samples = new_pos - pos;
                pos = new_pos;
                println!("Num samples: {new_samples}")
            }
        }
    }
    println!();

    let result = lab.stop_capture(capture).await?;
    let zero = result
        .values()
        .flat_map(|s| s.iter().map(|(t, _, _, _)| t))
        .min_by(|a, b| a.partial_cmp(b).unwrap())
        .copied()
        .unwrap_or(0.0);
    for ((src, pfx, _), samples) in result {
        println!("Samples for the flow {} -> {}:", src.fmt(&net), pfx);
        for (t_send, t_recv, dst, i) in samples {
            println!(
                "  {i}: {:0>9.5} {:0>9.5} (to {})",
                (t_send - zero),
                (t_recv - zero),
                dst.fmt(&net)
            );
        }
    }

    // disconnect the network.
    let _ = lab.disconnect().await?;

    // wait for one second
    tokio::time::sleep(Duration::from_secs(1)).await;

    Ok(())
}
