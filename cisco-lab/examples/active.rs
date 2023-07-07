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

use bgpsim::types::SimplePrefix as P;
use cisco_lab::{CiscoLab, CiscoLabError};
use tokio::time::timeout;

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

    // start the capture
    let mut capture = lab.start_capture(100).await?;

    // wait for ctrl-c
    let mut pos = 0;
    println!("Network is running! Press Ctrl-C to exit!");
    loop {
        match timeout(Duration::from_secs(1), tokio::signal::ctrl_c()).await {
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
    println!(
        "Num samples: {}",
        result.values().map(|x| x.len()).sum::<usize>()
    );

    // disconnect the network.
    let _ = lab.disconnect().await?;

    // wait for one second
    tokio::time::sleep(Duration::from_secs(1)).await;

    Ok(())
}
