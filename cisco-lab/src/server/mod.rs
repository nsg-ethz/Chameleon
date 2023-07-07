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

//! This module is responsible for managing the server in the Cisco-Lab setup. It manages the ExaBGP
//! process, as well as the physical interface setup, and generating or capturing traffic.

use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsStr,
    fmt::Write,
    io::Write as IoWrote,
    net::Ipv4Addr,
    path::PathBuf,
    time::Duration,
};

use bgpsim::{
    export::{Addressor, ExaBgpCfgGen, ExportError, ExternalCfgGen},
    prelude::*,
};
use ipnet::Ipv4Net;
use itertools::Itertools;
use time::{format_description, OffsetDateTime};

mod cmd;
mod exabgp;
mod session;
pub(crate) mod traffic_capture;
pub use cmd::{CmdError, CmdHandle};
pub use exabgp::ExaBgpHandle;
pub use session::ServerSession;
pub use traffic_capture::{CaptureSample, TrafficCaptureError, TrafficCaptureHandle, TrafficFlow};

use crate::{config::CONFIG, ssh::SshSession, Active, CiscoLab, CiscoLabError, Inactive};

pub type Capture<P> = HashMap<(RouterId, P, Ipv4Addr), Vec<(f64, f64, RouterId, u64)>>;

impl<'n, P: Prefix, Q> CiscoLab<'n, P, Q, Inactive> {
    /// Prepare all external routers (used in the constructor of `CiscoLab`).
    pub(super) fn prepare_external_routers(
        net: &'n Network<P, Q>,
    ) -> Result<BTreeMap<RouterId, ExaBgpCfgGen<P>>, CiscoLabError> {
        net.get_external_routers()
            .into_iter()
            .map(|r| Ok((r, ExaBgpCfgGen::new(net, r)?)))
            .collect()
    }
}

impl<'n, P: Prefix, Q, S> CiscoLab<'n, P, Q, S> {
    /// Generate the configuration for exabgp
    pub fn generate_exabgp_config(&mut self) -> Result<String, CiscoLabError> {
        let mut c = format!(
            "process announce-routes {{\n    run /usr/bin/env python3 {};\n    encoder json;\n}}\n\n",
            CONFIG.server.exabgp_runner_filename,
        );

        for gen in self.external_routers.values_mut() {
            c.push_str(&gen.generate_config(self.net, &mut self.addressor)?);
            c.push('\n');
        }

        Ok(c)
    }

    /// Generate the configuration for netplan to work with exabgp
    pub fn generate_exabgp_netplan_config(&mut self) -> Result<String, CiscoLabError> {
        let iface_nets = self
            .addressor
            .subnet_for_external_links()
            .subnets(CONFIG.addresses.link_prefix_len)
            .unwrap();
        let last_addr = iface_nets.last().unwrap().hosts().next().unwrap();
        let last_addr = Ipv4Net::new(last_addr, CONFIG.addresses.link_prefix_len).unwrap();
        let mut c = String::new();
        writeln!(&mut c, "network:")?;
        writeln!(&mut c, "  version: 2")?;
        writeln!(&mut c, "  renderer: networkd")?;
        writeln!(&mut c, "  ethernets:")?;
        writeln!(&mut c, "    {}:", CONFIG.server.exabgp_iface)?;
        writeln!(&mut c, "      link-local: []")?;
        writeln!(&mut c, "      dhcp4: no")?;
        writeln!(&mut c, "      dhcp6: no")?;
        writeln!(&mut c, "      addresses:")?;
        writeln!(&mut c, "        - {last_addr}")?;

        let mut label_idx: usize = 0;
        for (r, gen) in self.external_routers.iter() {
            for n in gen.neighbors() {
                writeln!(
                    &mut c,
                    "        - {}:\n            label: {}:{label_idx}",
                    self.addressor.iface_address_full(*r, *n)?,
                    CONFIG.server.exabgp_iface,
                )?;
                label_idx += 1;
            }
        }

        Ok(c)
    }

    /// Generate the python script to execute in exabgp
    pub fn generate_exabgp_runner(&mut self) -> Result<String, CiscoLabError> {
        let mut s = String::from(
            "#!/usr/bin/env python3\nimport sys\nimport time\nfrom os.path import expanduser as full\n\n",
        );

        let mut lines: BTreeMap<Duration, Vec<String>> = BTreeMap::new();
        self.external_routers
            .values()
            .map(|x| x.generate_lines(&mut self.addressor))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .for_each(|(l, time)| lines.entry(time).or_default().extend(l));

        // write the function to wait until
        let c = CONFIG.server.exabgp_runner_control_filename.as_str();
        writeln!(&mut s, "def wait_until(x):")?;
        writeln!(&mut s, "    while True:")?;
        writeln!(&mut s, "        try:")?;
        writeln!(&mut s, "            with open(full('{c}'), 'r') as f:")?;
        writeln!(&mut s, "                t = int(f.read())")?;
        writeln!(&mut s, "                if t >= x: return")?;
        writeln!(&mut s, "        except FileNotFoundError:")?;
        writeln!(&mut s, "            pass")?;
        writeln!(&mut s, "        except ValueError:")?;
        writeln!(&mut s, "            pass")?;
        writeln!(&mut s, "        time.sleep(0.1)")?;
        writeln!(&mut s)?;

        for (time, lines) in lines {
            // add the newline
            writeln!(&mut s)?;

            let time = time.as_secs_f64();
            writeln!(&mut s, "wait_until({time})")?;

            // write all lines
            for line in lines {
                writeln!(&mut s, "{line}")?;
            }
            writeln!(&mut s, "sys.stdout.flush()")?;
        }

        writeln!(&mut s, "\nwait_until(1_000_000)")?;

        Ok(s)
    }

    /// Advance all external router generators in time. This can be used to create a BGP event. The
    /// step will always be equal to 1.
    ///
    /// The generated python runner works as follows: Before sending the BGP updates of a next
    /// round, it waits until the contorl file has stored a number that is larger or equal to the
    /// current step.
    pub fn step_external_time(&mut self) {
        let step = Duration::from_secs(1);
        self.external_routers
            .values_mut()
            .for_each(|r| r.step_time(step));
    }

    /// Advertise an additional route. This will only change the python runner for exabgp that are
    /// generated in the future. If used together with [`CiscoLab::step_external_time`], you can
    /// create an exabgp runner that will change its avertisements over time.
    pub fn advertise_route(
        &mut self,
        router: RouterId,
        route: &BgpRoute<P>,
    ) -> Result<(), CiscoLabError> {
        self.external_routers
            .get_mut(&router)
            .ok_or_else(|| NetworkError::DeviceNotFound(router))?
            .advertise_route(self.net, &mut self.addressor, route)?;
        Ok(())
    }

    /// Withdraw a previously advertised route. This will only change the python runner for exabgp
    /// that are generated in the future. If used together with [`CiscoLab::step_external_time`],
    /// you can create an exabgp runner that will change its avertisements over time.
    ///
    /// *Warning*: Make sure that the route was advertised before.
    pub fn withdraw_route(&mut self, router: RouterId, prefix: P) -> Result<(), CiscoLabError> {
        self.external_routers
            .get_mut(&router)
            .ok_or_else(|| NetworkError::DeviceNotFound(router))?
            .withdraw_route(self.net, &mut self.addressor, prefix)?;
        Ok(())
    }
}

impl<'n, P: Prefix, Q> CiscoLab<'n, P, Q, Active> {
    /// Function to get a session handle for the server. This handle will use the pre-established
    /// SSH connection as long as it is still available, After that, it will re-establish a new
    /// connection for each command.
    ///
    /// See [`crate::ssh::SshSession`] for how to use the server session.
    pub fn get_server_session(&self) -> SshSession {
        self.state.server.0.clone()
    }

    /// Function to get the the exabgp handle which is running exabgp.
    pub fn get_exabgp_handle(&mut self) -> &mut ExaBgpHandle {
        &mut self.state.exabgp
    }

    /// Configure netplan. This requires the configuration file to be writable as the current user,
    /// and that the command `sudo netplan apply` can be executed without asking for the root
    /// password.
    pub(crate) async fn configure_netplan(&mut self) -> Result<(), CiscoLabError> {
        let cfg = self.generate_exabgp_netplan_config()?;
        self.state.server.configure_netplan(cfg).await?;
        Ok(())
    }

    /// Start an `iperf` client that will generate some basic data-plane traffic to a running
    /// `iperf` server instance.
    ///
    /// The `bitrate` specifies the amount of traffic to be generated, in 1 Gigabit/sec. The `udp`
    /// specifies whether to generate UDP traffic, or TCP traffic otherwise. Beware that `iperf`
    /// can achieve much higher bitrates for TCP than UDP.
    pub async fn start_iperf(
        &mut self,
        bitrate: u8,
        udp: bool,
    ) -> Result<CmdHandle, CiscoLabError> {
        let cmd = format!(
            "iperf3 --bind {} {} --bitrate {}G --time 0 --client {}",
            &CONFIG.server.iperf_client_ip,
            if udp { "--udp" } else { "" },
            bitrate,
            &CONFIG.server.iperf_server_ip,
        );
        let mut handle = CmdHandle::new("iperf client", cmd, self.state.server.0.clone()).await?;
        handle.start().await?;
        Ok(handle)
    }

    /// Stop the `iperf` client.
    pub async fn stop_iperf(&mut self, handle: CmdHandle) -> Result<(), CiscoLabError> {
        handle.stop().await?;
        Ok(())
    }

    /// Start a `tcpdump` process capturing all data-plane traffic on the `traffic_monitor_iface`.
    /// Requires that the config option `traffic_monitor_enable` is set to `true` and that the
    /// `traffic_monitor_tofino_port`, `traffic_monitor_iface`, and `traffic_monitor_pcap_path` are
    /// set correctly and exist. Pcap files will be called `{pcap_path}/{name}_{timestamp}.pcap`
    /// and stored on the server. Requires that the user can run `sudo tcpdump` without a password.
    ///
    /// `filter_iperf_traffic` controls whether to add an IP-based packet capture filter to omit
    /// traffic generated by using the `start_iperf` API. Useful for smaller-sized PCAPs.
    pub async fn start_traffic_monitor(
        &mut self,
        name: impl AsRef<str>,
        filter_iperf_traffic: bool,
    ) -> Result<(PathBuf, CmdHandle), CiscoLabError> {
        let cur_time = OffsetDateTime::now_local()
            .unwrap_or_else(|_| OffsetDateTime::now_utc())
            .format(
                &format_description::parse("[year]-[month]-[day]_[hour]-[minute]-[second]")
                    .unwrap(),
            )
            .unwrap();

        let mut pcap_path = PathBuf::from(&CONFIG.server.traffic_monitor_pcap_path);
        pcap_path.push(format!("{}_{cur_time}.pcap", name.as_ref()));

        let filter = if filter_iperf_traffic {
            format!(
                "not src {} and not src {}",
                &CONFIG.server.iperf_client_ip, &CONFIG.server.iperf_server_ip,
            )
        } else {
            "".to_string()
        };

        // prepare the `tcpdump` command
        let cmd = format!(
            "sudo tcpdump -i {} -w {} {}",
            &CONFIG.server.traffic_monitor_iface,
            pcap_path.to_string_lossy(),
            filter,
        );

        // create the persistent child process running `tcpdump`
        let mut handle =
            CmdHandle::new("traffic monitor", cmd, self.state.server.0.clone()).await?;
        handle.start().await?;

        Ok((pcap_path, handle))
    }

    /// Stop the `traffic_monitor`.
    pub async fn stop_traffic_monitor(
        &mut self,
        handle: CmdHandle,
    ) -> Result<SshSession, CiscoLabError> {
        Ok(handle.stop().await?)
    }

    /// Start a capture that will test all routers and all destinations in the network. See
    /// [`TrafficCaptureHandle`] for more information on how the capture is created.
    ///
    /// The `frequency` captures the number of ping packets sent per second for each flow in the
    /// network. A flow is a tuple consisting of a source router and a target prefix. For each
    /// router and for each prefix, one such flow is created.
    ///
    /// If there are more than 5 prefixes, the capture will probe the first, 25% median, median,
    /// 75% median and last prefix.
    pub async fn start_capture(
        &mut self,
        frequency: u64,
    ) -> Result<TrafficCaptureHandle, CiscoLabError> {
        let mut prefixes: Vec<_> = self.get_prefix_ip_lookup()?.into_keys().collect();
        prefixes.sort();

        // ensure that our prober is not being overloaded by choosing at most 5 destinations
        let selected_prefixes = Self::choose_k(5, prefixes);

        let flows: Vec<TrafficFlow> = self
            .prober_ifaces
            .values()
            .flat_map(|(_, mac, addr)| {
                selected_prefixes.iter().map(move |dst_ip| TrafficFlow {
                    src_mac: *mac,
                    src_ip: *addr,
                    dst_ip: *dst_ip,
                })
            })
            .collect();

        let mut handle = TrafficCaptureHandle::new(self.state.server.0.clone(), &flows).await?;
        handle.start(frequency).await?;
        Ok(handle)
    }

    /// Choose up to `k` elements from a `Vec<T>`. If there are more than `k` elements, the result
    /// will contain the first, last, and `k-2` equidistant elements from the `Vec<T>`.
    fn choose_k<T: Copy>(k: usize, xs: Vec<T>) -> Vec<T> {
        let l = xs.len() - 1;
        if l >= k {
            (0..k).map(|i| xs[(i * l) / (k - 1)]).collect()
        } else {
            xs
        }
    }

    /// Stop a packet capture and parse the results. The returned hashmap contains, for each
    /// `(source, prefix)` pair, a vector of samples, where each sample has the following fields:
    /// `(t_send, t_recv, ext, counter)`
    ///
    /// - `t_send`: Timestamp when the packet was sent by the prober.
    /// - `t_recv`: Timestamp when the packet was received by the collector.
    /// - `ext`: Router ID of the external router to whom the packet was sent.
    /// - `counter`: Index of the packet..
    ///
    /// This function returns a hash map that contains, as key, both the source router, the external
    /// prefix, and the actual destination IP address that was used. This allows you to distinguish
    /// multiple destinations for the same Prefix Equivalence Class.
    ///
    /// Samples that cannot be parsed are simply ignored.
    pub async fn stop_capture(
        &mut self,
        mut handle: TrafficCaptureHandle,
    ) -> Result<Capture<P>, CiscoLabError> {
        handle.stop().await?;

        let selected_dst_ips: Vec<_> = handle
            .get_prober_config()
            .flows
            .iter()
            .map(|f| f.dst_ip)
            .collect();

        let prefix_lookup = self.get_prefix_ip_lookup()?;
        let int_lookup: HashMap<Ipv4Addr, RouterId> = self
            .prober_ifaces
            .iter()
            .map(|(r, (_, _, x))| (*x, *r))
            .collect();
        let ext_lookup = self.get_external_router_mac_lookup()?;

        let mut destinations: HashMap<P, Vec<Ipv4Addr>> = HashMap::new();
        prefix_lookup.iter().for_each(|(addr, p)| {
            if selected_dst_ips.contains(addr) {
                destinations.entry(*p).or_default().push(*addr);
            }
        });

        let mut results = HashMap::new();
        self.net.get_routers().into_iter().for_each(|r| {
            destinations.iter().for_each(|(p, addrs)| {
                addrs.iter().for_each(|addr| {
                    results.insert((r, *p, *addr), Vec::new());
                })
            })
        });

        for sample in handle.take_samples().await? {
            if let (Some(int), Some(prefix), Some(ext)) = (
                int_lookup.get(&sample.src_ip),
                prefix_lookup.get(&sample.dst_ip),
                ext_lookup.get(&sample.mac),
            ) {
                results
                    .get_mut(&(*int, *prefix, sample.dst_ip))
                    .unwrap()
                    .push((sample.send_time, sample.time, *ext, sample.counter));
            }
        }

        Ok(results)
    }

    /// Perform a step in external advertisements at runtime. This causes ExaBGP to update the
    /// routing inputs.
    pub async fn step_exabgp(&mut self) -> Result<(), CiscoLabError> {
        Ok(ExaBgpHandle::step(&self.state.exabgp).await?)
    }

    /// Schedule a step in external advertisements at runtime. Once this triggers, ExaBGP will
    /// update the routing inptus. Do schedule two steps for the same time. The step value will
    /// remain consistent, even with multiple scheduled steps.
    pub fn step_exabgp_scheduled(&mut self, delay: Duration) -> Result<(), CiscoLabError> {
        let session = self.state.exabgp.session.clone();
        tokio::task::spawn(async move {
            tokio::time::sleep(delay).await;
            log::info!("Perform step in external inputs!");
            match exabgp::read_step(&session).await {
                Ok(step) => match exabgp::write_step(&session, step + 1).await {
                    Ok(_) => {}
                    Err(e) => {
                        log::error!("[{}] Cannot perform an exabgp step! {e}", session.name())
                    }
                },
                Err(e) => log::error!(
                    "[{}] Cannot read the current exabgp step! {e}",
                    session.name()
                ),
            }
        });
        Ok(())
    }

    /// Compute the prefix lookup. In case of a preifx equivalence class, this function will return
    /// the first prefix, the last, and one in between.
    fn get_prefix_ip_lookup(&mut self) -> Result<HashMap<Ipv4Addr, P>, ExportError> {
        let mut lookup = HashMap::new();
        for p in self.net.get_known_prefixes() {
            match self.addressor.prefix(*p)? {
                bgpsim::export::MaybePec::Single(net) => {
                    lookup.insert(
                        net.hosts().next().ok_or(ExportError::NotEnoughAddresses)?,
                        *p,
                    );
                }
                bgpsim::export::MaybePec::Pec(_, mut networks) => {
                    networks.sort_by_cached_key(|n| n.to_string());
                    let n = networks.len();
                    let networks = match n {
                        0 | 1 | 2 => networks,
                        _ => vec![networks[0], networks[n / 2], networks[n - 1]],
                    };
                    for net in networks {
                        lookup.insert(
                            net.hosts().next().ok_or(ExportError::NotEnoughAddresses)?,
                            *p,
                        );
                    }
                }
            }
        }
        Ok(lookup)
    }

    /// compute the router IP address lookup
    fn get_external_router_mac_lookup(
        &mut self,
    ) -> Result<HashMap<[u8; 6], RouterId>, ExportError> {
        self.net
            .get_external_routers()
            .into_iter()
            .flat_map(|ext| {
                self.addressor
                    .list_ifaces(ext)
                    .into_iter()
                    .map(move |(int, _, _, _)| (ext, int))
            })
            .collect_vec()
            .into_iter()
            .map(|(ext, int)| {
                let iface_idx = self.addressor.iface_index(int, ext)?;
                let iface = self.routers[&int]
                    .0
                    .ifaces
                    .get(iface_idx)
                    .ok_or(ExportError::NotEnoughInterfaces(int))?;
                Ok((iface.mac, ext))
            })
            .collect()
    }
}

/// Export the captured traffic to CSV files.
///
/// This function will first create a folder in `root` named `{root}/{name}_{timestamp}`. Then, it
/// will create files named `{src}-{prefix}-{addr}-{dst}.csv` that contains the timestamps and
/// sequence numbers of all packets that were captured. Here, `src` is the router name from where
/// the packets originate, `prefix` is the prefix using `P::From<Ipv4Net>`, `addr` is the `Ipv4Addr`
/// that was used in each packet as destination IP, and `dst` is the name of the external router
/// to which packets were forwarded.
///
/// Each file will contain comma-separated values. The first column stores the time when the packet
/// was received, and the second column stores the sequence number of that packet.
///
/// This function returns the path to the folder that was created.
pub fn export_capture_to_csv<P: Prefix, Q>(
    net: &Network<P, Q>,
    capture: &Capture<P>,
    root: impl AsRef<OsStr>,
    name: impl AsRef<str>,
) -> Result<PathBuf, std::io::Error> {
    let cur_time = OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(
            &format_description::parse("[year]-[month]-[day]_[hour]-[minute]-[second]").unwrap(),
        )
        .unwrap();
    let mut path = PathBuf::from(root.as_ref());
    path.push(format!("{}_{cur_time}", name.as_ref()));

    let mut idx = None;
    while path.exists() {
        let i = idx.unwrap_or(0) + 1;
        idx = Some(i);
        path.pop();
        path.push(format!("{}_{cur_time}_{i}", name.as_ref()));
    }
    std::fs::create_dir_all(&path)?;

    // write the measurement result to file
    for ((src, dst, addr), data) in capture {
        let prefix_str = dst.to_string().replace(['.', '/'], "_");
        let addr_str = addr.to_string().replace('.', "_");
        for ext in net.get_external_routers() {
            path.push(format!(
                "{}-{}-{}-{}.csv",
                src.fmt(net),
                prefix_str,
                addr_str,
                ext.fmt(net)
            ));
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(&path)?;
            file.write_all(b"send_time,recv_time,sequence_num\n")?;
            file.write_all(
                data.iter()
                    .filter(|(_, _, e, _)| *e == ext)
                    .map(|(t_send, t_recv, _, k)| format!("{t_send},{t_recv},{k}"))
                    .join("\n")
                    .as_bytes(),
            )?;
            path.pop();
        }
    }
    Ok(path)
}
