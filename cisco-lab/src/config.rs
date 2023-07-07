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

//! This module contains the code for reading the configuration.

use std::{cmp::Reverse, net::Ipv4Addr};

use ipnet::Ipv4Net;
use itertools::Itertools;
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Deserializer};

macro_rules! expect {
    ($result:expr, $($rest:tt)*) => {
        $result.unwrap_or_else(|e| {
            eprintln!("Error: {}: {}\n", format!($($rest)*), e);
            panic!()
        })
    };
    ($result:expr) => {
        $result.unwrap_or_else(|e| {
            eprintln!("{}\n", e)
            panic!()
        })
    };
}

lazy_static! {
    pub static ref CONFIG_DIR: String = {
        if cfg!(test) {
            concat!(env!("OUT_DIR"), "/.config").to_string()
        } else {
            expect!(
                std::env::var("LAB_SETUP_CONFIG"),
                "Environment variable 'LAB_SETUP_CONFIG' is not defined!"
            )
        }
    };
    pub static ref CONFIG: Config = {
        let config_str = expect!(
            std::fs::read_to_string(format!("{}/config.toml", *CONFIG_DIR)),
            "Cannot read '{}/config.toml'",
            *CONFIG_DIR
        );
        expect!(
            toml::from_str(&config_str),
            "Cannot parse '{}/config.toml'",
            *CONFIG_DIR
        )
    };
    pub static ref VDCS: Vec<RouterProperties> = {
        #[derive(Debug, Deserialize)]
        struct RoutersConfigFile {
            vdcs: Vec<String>,
        }
        let routers_str = expect!(
            std::fs::read_to_string(format!("{}/routers.toml", *CONFIG_DIR)),
            "Cannot read '{}/routers.toml'",
            *CONFIG_DIR
        );
        let routers: RoutersConfigFile = expect!(
            toml::from_str(&routers_str),
            "Cannot parse '{}/routers.toml'",
            *CONFIG_DIR
        );
        routers
            .vdcs
            .into_iter()
            .map(|nane| {
                let file = format!("{}/{}.toml", *CONFIG_DIR, nane);
                let router_str = expect!(std::fs::read_to_string(&file), "Cannot read '{}'", file);
                expect!(
                    toml::from_str::<RouterProperties>(&router_str),
                    "Cannot parse '{}'",
                    file
                )
            })
            .sorted_by_key(|x| Reverse(x.ifaces.len()))
            .collect()
    };
    pub static ref ROUTERS: Vec<String> = {
        #[derive(Debug, Deserialize)]
        struct RoutersConfigFile {
            routers: Vec<String>,
        }
        let routers_str = expect!(
            std::fs::read_to_string(format!("{}/routers.toml", *CONFIG_DIR)),
            "Cannot read '{}/routers.toml'",
            *CONFIG_DIR
        );
        let routers: RoutersConfigFile = expect!(
            toml::from_str(&routers_str),
            "Cannot parse '{}/routers.toml'",
            *CONFIG_DIR
        );
        routers.routers
    };
}

/// Properties of routers.
#[derive(Debug, Clone, Deserialize)]
pub struct RouterProperties {
    /// The ssh hostname to reach the router
    pub ssh_name: String,
    /// The ip address of the management interface.
    pub mgnt_addr: Ipv4Addr,
    /// A vector of all available ports.
    #[serde(deserialize_with = "deserialize_interfaces")]
    pub ifaces: Vec<RouterIface>,
}

/// Information about interfaces.
#[derive(Debug, Clone)]
pub struct RouterIface {
    /// The name of the interface
    pub iface: String,
    /// The MAC address.
    pub mac: [u8; 6],
    /// The tofino interface to which the current interface is connected to.
    pub tofino_iface: String,
    /// The tofino port used for writing the controller
    pub tofino_port: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub tofino: TofinoConfig,
    pub addresses: AddressConfig,
}

/// Configuration for the assigned IP addresses.
#[derive(Debug, Clone, Deserialize)]
pub struct AddressConfig {
    /// IP Address range used for all internal networks and all links, both connecting two internal
    /// routers and connecting an internal and an external router.
    pub internal_ip_range: Ipv4Net,
    /// IP Address range for networks of external routers.
    pub external_ip_range: Ipv4Net,
    /// Prefix length for networks that are assigned to internal routers.
    #[serde(deserialize_with = "deserialize_prefix_len")]
    pub local_prefix_len: u8,
    /// Prefix length of links (connecting an internal router with either an external or another
    /// internal router).
    #[serde(deserialize_with = "deserialize_prefix_len")]
    pub link_prefix_len: u8,
    /// Prefix length for networks that are assigned to external routers.
    #[serde(deserialize_with = "deserialize_prefix_len")]
    pub external_prefix_len: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// The ssh hostname to reach the server
    pub ssh_name: String,
    /// Filename for the netplan configuration file, to configure exabgp interfaces.
    ///
    /// **Warning**: Make sure that regular users can write this file! Set the permissions to `666`.
    pub netplan_config_filename: String,
    /// Interface name which is used by ExaBGP
    pub exabgp_iface: String,
    /// Filename for the ExaBGP runner script on the server
    pub exabgp_runner_filename: String,
    /// Filename for the ExaBGP configuration on the server
    pub exabgp_config_filename: String,
    /// Filename of the text file to interact with the ExaBGP runner script on the server
    pub exabgp_runner_control_filename: String,
    /// Filename for the configuration file of the prober on the server.
    pub prober_config_filename: String,
    /// Interface name used to generate traffic on (using the prober).
    pub prober_iface: String,
    /// The port on the tofino to which the delayer interface is connected
    pub delayer_tofino_ports: Vec<u8>,
    /// Offset of delay values to account for the extra time of passing through the delayer loop
    pub delayer_loop_offset: i8,
    /// The iperf client's IP address to send traffic from
    pub iperf_client_ip: String,
    /// The port on the tofino to which the iperf client interface is connected
    pub iperf_client_tofino_port: u8,
    /// The iperf server's IP address to send traffic to
    pub iperf_server_ip: String,
    /// The fake iperf source IP address used to replicate traffic to the routers, used to filter
    /// out traffic that returns to the Tofino
    pub iperf_filter_src_ip: String,
    /// The port on the tofino to which the iperf server interface is connected
    pub iperf_server_tofino_port: u8,
    /// Set to true to enable the full traffic monitoring, can be true/false
    pub traffic_monitor_enable: bool,
    /// Path on the server where to place the recorded pcap files
    pub traffic_monitor_pcap_path: String,
    /// The server interface on which the full traffic will be monitored, should be connected to traffic_monitor_tofino_port
    pub traffic_monitor_iface: String,
    /// The port on the tofino to which the full traffic should be cloned, should be connected to traffic_monitor_iface. Set to 0 to disable.
    pub traffic_monitor_tofino_port: u8,
}

/// Configuration for the tofino
#[derive(Debug, Clone, Deserialize)]
pub struct TofinoConfig {
    /// The ssh hostname to reach the tofino>
    pub ssh_name: String,
    /// Filename for storing the controller script.
    pub controller_filename: String,
    /// Path towards the port setup file
    pub ports_setup_filename: String,
    /// Path towards the file used to disable or enable specific ports
    pub ucli_script_filename: String,
    /// Path towards the Barefoot SDE. This is to be sourced before executing `run_bfshell.sh`
    pub bf_sde_path: String,
    /// Full path for the Barefoot SDE shell.
    pub bf_sde_shell: String,
}

fn deserialize_prefix_len<'de, D>(de: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let x = u8::deserialize(de)?;
    if (0..=32).contains(&x) {
        Ok(x)
    } else {
        panic!("Prefix length must be between 0 and 32, but was {x}");
    }
}

fn deserialize_interfaces<'de, D>(de: D) -> Result<Vec<RouterIface>, D::Error>
where
    D: Deserializer<'de>,
{
    let x: Vec<(String, u64, String, u8)> = Vec::deserialize(de)?;
    Ok(x.into_iter()
        .map(|(iface, mac, tofino_iface, tofino_port)| {
            if mac > 0xffff_ffff_ffff {
                panic!("MAC Address exceeds 6 bytes!")
            }
            let mac = [
                ((mac & 0xff00_0000_0000) >> 40) as u8,
                ((mac & 0x00ff_0000_0000) >> 32) as u8,
                ((mac & 0x0000_ff00_0000) >> 24) as u8,
                ((mac & 0x0000_00ff_0000) >> 16) as u8,
                ((mac & 0x0000_0000_ff00) >> 8) as u8,
                (mac & 0x0000_0000_00ff) as u8,
            ];
            lazy_static! {
                static ref IFACE_RE: Regex = Regex::new(r"^[a-zA-Z]*[0-9]/[0-9][0-9]?$").unwrap();
                static ref TOFINO_IFACE_RE: Regex = Regex::new(r"^[0-9][0-9]?/[0-3]$").unwrap();
            }
            if !IFACE_RE.is_match(&iface) {
                panic!("Invalid interface string: {iface} (should be 'EthernetX/YY')");
            }
            if !TOFINO_IFACE_RE.is_match(&tofino_iface) {
                panic!("Invalid tofino interface string: {tofino_iface} (should be 'XX/Y')");
            }
            RouterIface {
                iface,
                mac,
                tofino_iface,
                tofino_port,
            }
        })
        .collect())
}
