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

use std::{fs::read_to_string, time::Duration};

use cisco_lab::{config::VDCS, router::CiscoSession};
use itertools::Itertools;
use pretty_assertions::assert_eq;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();

    let router = &VDCS.first().unwrap().ssh_name;
    let session = CiscoSession::new(router).await?;
    let config_1 = session.show("running-config").await?;
    let mut shell = session.shell().await?;
    let config_2 = shell.get_running_config().await?;

    let config_1 = config_1
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with('!'))
        .join("\n");
    let config_2 = config_2
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with('!'))
        .join("\n");

    assert_eq!(config_1, config_2);

    // read the heanet configuration
    let config = read_to_string(format!("example_data/{router}.conf")).unwrap();

    // reset the current config
    shell.reset_configuration().await?;
    shell.configure(&config).await?;

    // print the ospf state
    println!("{:#?}", shell.get_ospf_neighbors().await?);
    println!("{:#?}", shell.get_ospf_state().await?);
    println!("{:#?}", shell.get_bgp_neighbors().await?);
    println!("{:#?}", shell.get_bgp_routes().await?);

    // wait for 10 seconds
    sleep(Duration::from_secs(10)).await;

    // print the ospf state again
    println!("{:#?}", shell.get_ospf_neighbors().await?);
    println!("{:#?}", shell.get_ospf_state().await?);
    println!("{:#?}", shell.get_bgp_neighbors().await?);
    println!("{:#?}", shell.get_bgp_routes().await?);

    Ok(())
}
