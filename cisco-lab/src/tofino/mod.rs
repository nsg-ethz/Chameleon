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

//! This module is responsible for managing the Tofino in the lab setup. It provides methods to
//! generate the configurations and an interface to apply the configuration to the tofino.

use std::{collections::HashMap, fmt::Write, fs::read_to_string, time::Duration};

use bgpsim::{
    export::{Addressor, ExportError},
    prelude::NetworkFormatter,
    types::{NetworkError, Prefix, RouterId},
};
use geoutils::Location;
use itertools::Itertools;

use crate::{
    config::{CONFIG, CONFIG_DIR, VDCS},
    Active, CiscoLab, CiscoLabError,
};

mod session;
pub use session::TofinoSession;

/// Speed of light in a fiber cable is ~2/3 of the speed of light
/// https://en.wikipedia.org/wiki/Fiber-optic_cable#Propagation_speed_and_delay
const SPEED_OF_LIGHT: f64 = 0.66 * 299_792_458.0;
const DELAY_ADDRS: &str = concat!(
    "'src_addr': ",
    "EUI('de:ad:aa:bb:cc:dd')",
    ", 'dst_addr': ",
    "EUI('de:ad:bb:cc:dd:ee')",
);

impl<'n, P: Prefix, Q, S> CiscoLab<'n, P, Q, S> {
    /// Set the delay of a specific link to a certian time in microseconds. This value must fit into
    /// 24 bits, so the longest delay is around 16 seconds.
    ///
    /// This function will do nothing if the two routers are not connected in `self.net`.
    pub fn set_link_delay(&mut self, src: RouterId, dst: RouterId, delay_us: u32) {
        assert!(delay_us < 1 << 24);

        if self.net.get_topology().find_edge(src, dst).is_some() {
            self.link_delays.insert((src, dst), delay_us);
        }
    }

    /// Set the link delays according to the geolocation of each router. The delay is computed by
    /// computing the distance between two nodes, and how long light takes to travel through a fibre
    /// optic cable of this length.
    /// Any missing geo location (or any geo location set to 0,0) will be set to the center point of
    /// all others.
    pub fn set_link_delays_from_geolocation(
        &mut self,
        geo: impl Into<HashMap<RouterId, Location>>,
    ) {
        let mut geo: HashMap<RouterId, Location> = geo.into();
        geo.retain(|_, v| v.latitude() != 0f64 || v.longitude() != 0f64);

        // set all missing routers to the center of all other points.
        let center_point = Location::center(&geo.values().collect_vec());
        self.routers.keys().for_each(|r| {
            geo.entry(*r).or_insert(center_point);
        });

        // compute the distance between all nodes
        for e in self.net.get_topology().edge_indices() {
            let (a, b) = self.net.get_topology().edge_endpoints(e).unwrap();
            if a > b {
                continue;
            }

            // get the distance between a and b
            let zero = Location::new(0, 0);
            let a_loc = *geo.get(&a).unwrap_or(&zero);
            let b_loc = *geo.get(&b).unwrap_or(&zero);

            // check if either a_loc or b_loc is pointing to zero.
            if a_loc == zero || b_loc == zero {
                continue;
            }

            let distance = a_loc
                .distance_to(&b_loc)
                .unwrap_or_else(|_| a_loc.haversine_distance_to(&b_loc))
                .meters();
            let delay = distance / SPEED_OF_LIGHT;
            let delay_us = (delay * 1_000_000f64) as u32;
            self.set_link_delay(a, b, delay_us);
            self.set_link_delay(b, a, delay_us);
        }
    }

    /// Generate the string for the tofino controller.
    pub fn generate_tofino_controller(&mut self) -> Result<String, CiscoLabError> {
        // generate all configurations first, such that all interfaces between internal routers are
        // initialized
        let _ = self.generate_router_config_all()?;

        // generate the port list
        let mut port_list = String::new();
        for router in VDCS.iter() {
            let rules = router.ifaces.iter().map(|x| x.tofino_port).join(", ");
            writeln!(&mut port_list, "    # Router {}", router.ssh_name)?;
            writeln!(&mut port_list, "    {rules},")?;
        }
        let pl = port_list.trim_end_matches('\n');

        // generate the forwarding
        let mut l2_rules = String::new();
        for router in VDCS.iter() {
            let rules = router
                .ifaces
                .iter()
                .map(|x| {
                    format!(
                        "EUI(\"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\"): {{'port': {}}},",
                        x.mac[0], x.mac[1], x.mac[2], x.mac[3], x.mac[4], x.mac[5], x.tofino_port,
                    )
                })
                .join("\n    ");
            writeln!(&mut l2_rules)?;
            writeln!(&mut l2_rules, "    # Router {}", router.ssh_name)?;
            writeln!(&mut l2_rules, "    {rules}")?;
        }
        let l2 = l2_rules.trim_start_matches('\n');

        // generate the static routes
        let mut sr = String::new();
        for ((a, a_idx), (b, b_idx)) in self.addressor.list_links().into_iter().sorted() {
            if let Some(a_port) = self
                .routers
                .get(&a)
                .and_then(|(c, _)| c.ifaces.get(a_idx))
                .map(|x| x.tofino_port)
            {
                if let Some(b_port) = self
                    .routers
                    .get(&b)
                    .and_then(|(c, _)| c.ifaces.get(b_idx))
                    .map(|x| x.tofino_port)
                {
                    writeln!(
                        &mut sr,
                        "    {a_port: >3}: {{'port': {b_port: >3}}}, {b_port: >3}: {{'port': {a_port: >3}}},"
                    )?;
                }
            }
        }

        let mut delay_port = CONFIG.server.delayer_tofino_ports.iter().cycle();
        let mut delays_rules = String::new();
        for ((a, b), delay) in self.link_delays.iter() {
            if let (Ok(a_idx), Ok(b_idx)) = (
                self.addressor.iface_index(*a, *b),
                self.addressor.iface_index(*b, *a),
            ) {
                if let (Some(a_port), Some(b_port)) = (
                    self.routers
                        .get(a)
                        .and_then(|(c, _)| c.ifaces.get(a_idx))
                        .map(|x| x.tofino_port),
                    self.routers
                        .get(b)
                        .and_then(|(c, _)| c.ifaces.get(b_idx))
                        .map(|x| x.tofino_port),
                ) {
                    writeln!(
                        &mut delays_rules,
                        "    {a_port: >3}: {{ {DELAY_ADDRS}, 'delay': {}, 'delay_port': {}, 'receiver_port': {b_port} }},",
                        (*delay as i32 + CONFIG.server.delayer_loop_offset as i32).max(0),
                        delay_port.next().unwrap(),
                    )?;
                }
            }
        }
        let delays = delays_rules.trim_matches('\n');

        // generate basic data-plane traffic with iperf
        let iperf_client_port = CONFIG.server.iperf_client_tofino_port.to_string();
        let iperf_client_ip = CONFIG.server.iperf_client_ip.to_string();
        let iperf_server_port = CONFIG.server.iperf_server_tofino_port.to_string();
        let iperf_server_ip = CONFIG.server.iperf_server_ip.to_string();
        let iperf_filter_src_ip = CONFIG.server.iperf_filter_src_ip.to_string();
        let iperf_replication_specs = self
            .routers
            .iter()
            .map(|(r, (vdc, _))| {
                let mut ifaces = self.addressor.list_ifaces(*r);
                ifaces.sort();
                if ifaces.len() <= 1 {
                    log::warn!(
                        "Not enough interfaces for basic data-plane traffic on router {} [vdc {}]",
                        r.fmt(self.net),
                        vdc.ssh_name
                    );
                    Ok(Vec::new())
                } else {
                    (0..ifaces.len())
                        .map(|in_idx| {
                            Ok(format!(
                                "    {}: {{'dst_mac': EUI(\"{}\"), 'dst_ip': IPAddress(\"{}\")}},",
                                vdc.ifaces[in_idx].tofino_port,
                                vdc.ifaces[in_idx]
                                    .mac
                                    .map(|b: u8| format!("{b:02x}"))
                                    .join(":"),
                                {
                                    let out_idx = (in_idx + 1) % ifaces.len();
                                    let dst_iface = self.addressor.iface(ifaces[out_idx].0, *r)?;
                                    dst_iface.0
                                },
                            ))
                        })
                        .collect::<Result<Vec<_>, ExportError>>()
                }
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .sorted()
            .join("\n");
        let traffic_monitor = if CONFIG.server.traffic_monitor_enable {
            format!(
                "{{'mirror_session': 3, 'mirror_port': {}}}",
                CONFIG.server.traffic_monitor_tofino_port
            )
        } else {
            "None".to_string()
        };

        Ok(
            read_to_string(format!("{}/tofino_controller.py", *CONFIG_DIR))?
                .replace("{{PORT_LIST}}", pl)
                .replace("{{L2_RULES}}", l2)
                .replace("{{STATIC_ROUTES}}", sr.as_str())
                .replace("{{DELAY_ROUTES}}", delays)
                .replace("{{IPERF_CLIENT_PORT}}", iperf_client_port.as_str())
                .replace("{{IPERF_CLIENT_IP}}", iperf_client_ip.as_str())
                .replace("{{IPERF_SERVER_PORT}}", iperf_server_port.as_str())
                .replace("{{IPERF_SERVER_IP}}", iperf_server_ip.as_str())
                .replace("{{IPERF_FILTER_SRC_IP}}", iperf_filter_src_ip.as_str())
                .replace(
                    "{{IPERF_REPLICATION_SPECS}}",
                    iperf_replication_specs.as_str(),
                )
                .replace("{{MIRROR_ALL}}", traffic_monitor.as_str()),
        )
    }
}

impl<'n, P: Prefix, Q> CiscoLab<'n, P, Q, Active> {
    /// Configure the tofino by uploading the controller and running it.
    pub(crate) async fn configure_tofino(&mut self) -> Result<(), CiscoLabError> {
        let controller = self.generate_tofino_controller()?;
        self.state.tofino.setup_ports().await?;
        self.state.tofino.configure(controller).await?;
        Ok(())
    }

    /// Enable a link between two nodes
    pub async fn enable_link(&self, a: RouterId, b: RouterId) -> Result<(), CiscoLabError> {
        self.state
            .tofino
            .enable_ports(&self.find_link_tofino_ports(a, b)?)
            .await?;
        Ok(())
    }

    /// Disable a link between two nodes
    pub async fn disable_link(&self, a: RouterId, b: RouterId) -> Result<(), CiscoLabError> {
        self.state
            .tofino
            .disable_ports(&self.find_link_tofino_ports(a, b)?)
            .await?;
        Ok(())
    }

    /// Enable a link between two nodes with a programmed delay. This will spawn a thread that will
    /// disable the link later in the execution. Once triggered, this can no longer be stopped. Make
    /// sure you don't schedule a different link disable at the same time!
    pub fn enable_link_scheduled(
        &mut self,
        a: RouterId,
        b: RouterId,
        delay: Duration,
    ) -> Result<(), CiscoLabError> {
        let ports = self.find_link_tofino_ports(a, b)?;
        let session = self.state.tofino.clone();
        tokio::task::spawn(async move {
            tokio::time::sleep(delay).await;
            match session.enable_ports(&ports).await {
                Ok(()) => {}
                Err(e) => log::error!("[{}] Cannot enable ports! {e}", session.name()),
            }
        });
        Ok(())
    }

    /// Disable a link between two nodes with a programmed delay. This will spawn a thread that will
    /// disable the link later in the execution. Once triggered, this can no longer be stopped. Make
    /// sure you don't schedule a different link disable at the same time!
    pub fn disable_link_scheduled(
        &mut self,
        a: RouterId,
        b: RouterId,
        delay: Duration,
    ) -> Result<(), CiscoLabError> {
        let ports = self.find_link_tofino_ports(a, b)?;
        let handle = self.state.tofino.clone();
        let link_text = format!("{} -- {}", a.fmt(self.net), b.fmt(self.net));
        tokio::task::spawn(async move {
            tokio::time::sleep(delay).await;
            log::info!("disable link {link_text}");
            match handle.disable_ports(&ports).await {
                Ok(()) => {}
                Err(e) => log::error!("[{}] Cannot disable ports! {e}", handle.name()),
            }
        });
        Ok(())
    }

    /// Find the tofino ports that connect router a and b. If there is no link present, an error is
    /// returned. If one of the routers is an external router, only the internal router's port is
    /// returned as the external router's port is shared among all external routers.
    fn find_link_tofino_ports(
        &self,
        a: RouterId,
        b: RouterId,
    ) -> Result<Vec<&'static str>, CiscoLabError> {
        let addressor = self.addressor();

        let port_a = if let Some((vdc, _)) = self.routers.get(&a) {
            let if_idx = addressor
                .list_ifaces(a)
                .into_iter()
                .find(|(r, _, _, _)| *r == b)
                .ok_or_else(|| NetworkError::LinkNotFound(a, b))?
                .3;
            Some(vdc.ifaces[if_idx].tofino_iface.as_str())
        } else {
            None
        };

        let port_b = if let Some((vdc, _)) = self.routers.get(&b) {
            let if_idx = addressor
                .list_ifaces(b)
                .into_iter()
                .find(|(r, _, _, _)| *r == a)
                .ok_or_else(|| NetworkError::LinkNotFound(a, b))?
                .3;
            Some(vdc.ifaces[if_idx].tofino_iface.as_str())
        } else {
            None
        };

        let ports: Vec<_> = [port_a, port_b].into_iter().flatten().collect();
        if ports.is_empty() {
            Err(NetworkError::DeviceNotFound(a))?
        }
        Ok(ports)
    }
}
