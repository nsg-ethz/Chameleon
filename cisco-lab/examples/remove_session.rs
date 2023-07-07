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
    config::{ConfigExpr, ConfigModifier},
    types::{RouterId, SimplePrefix as P},
};
use cisco_lab::{CiscoLab, CiscoLabError};
use tokio::time::sleep;

mod test_net;
mod utils;

#[tokio::main]
async fn main() -> Result<(), CiscoLabError> {
    pretty_env_logger::init();

    // create the network
    let net = test_net::generate_net::<P>()?;

    // create the lab
    let mut lab = CiscoLab::new(&net)?;

    // set all link delays to 10ms
    let topo = net.get_topology();
    for e in topo.edge_indices() {
        let (a, b) = topo.edge_endpoints(e).unwrap();
        lab.set_link_delay(a, b, (10_000 + 100 * e.index()) as u32);
    }

    // write config
    utils::write_config(&mut lab)?;

    // connect the network
    let mut lab = lab.connect().await?;

    // wait for convergence
    lab.wait_for_convergence().await?;

    // start the capture
    log::info!("start capture");
    let mut capture = lab.start_capture(1000).await?;

    // wait for 5 seconds
    sleep(Duration::from_secs(5)).await;

    // reset the capture
    log::info!("reset capture");
    capture.take_samples().await?;

    log::info!("wait for 10 seconds");
    sleep(Duration::from_secs(10)).await;

    // remove the session
    log::info!("perform the update");
    lab.apply_command(ConfigModifier::Remove(ConfigExpr::BgpSession {
        source: 0.into(),
        target: 5.into(),
        session_type: bgpsim::prelude::BgpSessionType::EBgp,
    }))
    .await?;

    // wait for 5 seconds
    log::info!("wait for 10 seconds");
    sleep(Duration::from_secs(10)).await;

    // capture the results
    log::info!("collect the results");
    let result = lab.stop_capture(capture).await?;

    // disconnect the network.
    let _ = lab.disconnect().await?;

    // parse the results and write to files
    for ((src, _, _), samples) in result.into_iter() {
        for dst in 5..=6 {
            let dst: RouterId = dst.into();
            let times: Vec<f64> = samples
                .iter()
                .filter(|(_, ext, _)| *ext == dst)
                .map(|(t, _, _)| *t)
                .collect();
            utils::write_lines(times, format!("samples_r{}_e{}", src.index(), dst.index()))?;
        }
    }

    Ok(())
}
