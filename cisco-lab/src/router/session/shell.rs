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

//! Abstraction of the cisco shell.

use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
    string::FromUtf8Error,
    time::Duration,
};

use ipnet::Ipv4Net;
use itertools::Itertools;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStderr, ChildStdin, ChildStdout},
    sync::{broadcast, mpsc},
    time::{sleep, timeout},
};

use crate::{
    router::{session::invert_config, ConvergenceMessage, ConvergenceState},
    ssh::wait_prompt,
    CiscoLabError,
};

use super::{BgpNeighbor, BgpRoute, ParseError};
use super::{OspfNeighbor, OspfRoute};

/// The `CiscoShell` represents an SSH command that is established with the router and running the
/// Cisco NX OS shell. To create such a shell, use [`super::CiscoSession::shell`].
pub struct CiscoShell {
    name: String,
    _child: Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
    stdin: ChildStdin,
}

impl CiscoShell {
    /// Create a new shell from a CommandChild. See `CiscoSession::shell` on how to call it. Make
    /// sure that the `tokio::process::Command` used to call this function has set
    /// `cmd.stdout(piped())`, `cmd.stdin(piped())`, `cmd.stderr(piped())`, and
    /// `cmd.kill_on_drop(true)`.
    pub(super) async fn new(mut child: Child, name: String) -> Result<Self, CiscoLabError> {
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        // disable the session timeout
        stdin.write_all(b"terminal session-timeout 0\n").await?;

        // construct the shell
        let mut s = Self {
            name,
            _child: child,
            stdout,
            stdin,
            stderr,
        };

        // wait until initialization is done
        s.wait_done().await?;

        // clear stderr
        s.clear_stderr().await;

        Ok(s)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the running configuration
    pub async fn get_running_config(&mut self) -> Result<String, CiscoShellError> {
        self.show("running-config").await
    }

    /// Get the startup configuration
    pub async fn get_startup_config(&mut self) -> Result<String, CiscoShellError> {
        self.show("startup-config").await
    }

    /// Check if the router is in initial state, that is, if the router is running the
    /// startup-config.
    pub async fn in_initial_state(&mut self) -> Result<bool, CiscoShellError> {
        let startup_config = self.get_startup_config().await?;
        let running_config = self.get_running_config().await?;
        let start = startup_config
            .lines()
            .filter(|l| !l.is_empty())
            .filter(|l| !l.trim().starts_with('!'))
            .collect_vec();
        let run = running_config
            .lines()
            .filter(|l| !l.is_empty())
            .filter(|l| !l.trim().starts_with('!'))
            .collect_vec();
        Ok(start == run)
    }

    /// Reset the router to startup config using cisco's `configure-replace`. **Warning**: This
    /// function may break the VDC, casing it to be restarted. Further, this function does not check
    /// if the configuration is actually reset.
    ///
    /// This is done by first writing the startup-config to file, and then using `configure replace`
    /// to migrate to that new config. In other words, the following two commands are executed:
    ///
    /// ```text
    /// copy startup-config bootflash:///startup-config
    /// configure replace bootflash:///startup-config
    /// ```
    ///
    /// # Safety
    /// - This function may not reset the configuration. Check if the running-config was actually
    ///   reset to the startup config after calling this function!
    /// - Calling this function may break the router, causing it to restart. Use with care!
    pub async unsafe fn reset_configuration_hard(&mut self) -> Result<(), CiscoShellError> {
        self.send_cmd_expect("delete bootflash:///startup-config no-prompt", |stdout| {
            stdout.is_empty() || stdout == "No such file or directory"
        })
        .await?;
        self.send_cmd_expect(
            "copy startup-config bootflash:///startup-config",
            |stdout| stdout == "Copy complete, now saving to disk (please wait)...",
        )
        .await?;
        self.send_cmd_expect("configure replace bootflash:///startup-config", |stdout| {
            stdout.contains("Rolling back to previous configuration is successful")
                || stdout.contains("Rollback Patch is Empty")
                || stdout.contains("Configure replace completed successfully")
        })
        .await?;
        Ok(())
    }

    /// Reset the router configuration by parsing the current configuration and inverting
    /// configuraitons that were generated using `bgpsim`. For a detailed list on what configuration
    /// is reset, see [`crate::router::invert_config`] .
    pub async fn reset_configuration(&mut self) -> Result<(), CiscoShellError> {
        log::debug!("[{}] Manually reset configuration.", self.name);
        // first, get the configurati
        let cfg = self.get_running_config().await?;
        let invert_cmds = invert_config(cfg);
        self.configure(invert_cmds).await?;

        Ok(())
    }

    /// Write the configuration to the device.
    pub async fn configure(&mut self, conf: impl AsRef<str>) -> Result<(), CiscoShellError> {
        self.send_cmd_expect("configure terminal", str::is_empty)
            .await?;
        for line in conf.as_ref().lines() {
            let line = line.trim();
            // skip empty lines and lines with comments
            if line.is_empty() || line.starts_with('!') {
                continue;
            }
            self.send_cmd_expect(line, str::is_empty).await?;
        }
        self.send_cmd_expect("end", str::is_empty).await?;
        log::debug!("[{}] Configured", self.name());
        Ok(())
    }

    /// Reset the routing (RIB entries) without clearing the FIB table. This will send the command
    /// `clear routing ipv4 unicast x.x.x.x/x`.
    pub async fn clear_routing(&mut self, net: Ipv4Net) -> Result<(), CiscoShellError> {
        self.send_cmd_expect(&format!("clear routing ipv4 unicast {net}"), |s| {
            s.starts_with("Clearing") && s.contains(&net.to_string())
        })
        .await
    }

    /// Send the command `clear ip bgp {neighbor} soft` to request a route refresh of all routes
    /// from that neighbor. This must be executed when changing incoming route-maps. Otherwise, this
    /// effect has no affect. When `neighbor` is `None`, then this command will request a route
    /// refresh from all neighbors.
    pub async fn bgp_refresh_routes(
        &mut self,
        neighbor: Option<Ipv4Addr>,
    ) -> Result<(), CiscoShellError> {
        if let Some(addr) = neighbor {
            self.send_cmd_expect(&format!("clear ip bgp {addr} soft"), str::is_empty)
                .await
        } else {
            self.send_cmd_expect("clear ip bgp * soft", str::is_empty)
                .await
        }
    }

    /// Get all OSPF neighbors using `show ip ospf neighbors`
    pub async fn get_ospf_neighbors(&mut self) -> Result<Vec<OspfNeighbor>, CiscoShellError> {
        Ok(OspfNeighbor::from_table(
            &self.show("ip ospf neighbors").await?,
        )?)
    }

    /// Get the current OSPF state, that includes routes towards all destinations. This will execute
    /// `show ip ospf route`
    pub async fn get_ospf_state(&mut self) -> Result<HashMap<Ipv4Net, OspfRoute>, CiscoShellError> {
        Ok(OspfRoute::from_xml_output(
            &self.show("ip ospf route | xml\n").await?,
        )?)
    }

    /// Get a specific OSPF route.
    pub async fn get_ospf_route(
        &mut self,
        net: Ipv4Net,
    ) -> Result<Option<OspfRoute>, CiscoShellError> {
        let mut routes =
            OspfRoute::from_xml_output(&self.show(format!("ip ospf route {net} | xml")).await?)?;
        Ok(routes.remove(&net))
    }

    /// Get all BGP neighbors and their state, using `show ip bgp summary`.
    pub async fn get_bgp_neighbors(&mut self) -> Result<Vec<BgpNeighbor>, CiscoShellError> {
        Ok(BgpNeighbor::from_table(
            &self.show("ip bgp summary").await?,
        )?)
    }

    /// Get a detailed list of the BGP route for the given network
    pub async fn get_bgp_route(
        &mut self,
        net: Ipv4Net,
    ) -> Result<Option<Vec<BgpRoute>>, CiscoShellError> {
        Ok(
            BgpRoute::from_detail(self.show(format!("bgp ipv4 unicast {net} detail")).await?)?
                .remove(&net),
        )
    }

    /// Get a detailed list of all BGP routes using `show bgp ipv4 unicast detail`.
    pub async fn get_bgp_routes(
        &mut self,
    ) -> Result<HashMap<Ipv4Net, Vec<BgpRoute>>, CiscoShellError> {
        Ok(BgpRoute::from_detail(
            self.show("bgp ipv4 unicast detail").await?,
        )?)
    }

    /// get a list of bgp routes for the selected networks. This function will execute
    /// `Self::get_bgp_route` multiple times.
    async fn get_bgp_routes_for_networks(
        &mut self,
        networks: impl IntoIterator<Item = Ipv4Net>,
    ) -> Result<HashMap<Ipv4Net, Vec<BgpRoute>>, CiscoShellError> {
        let mut result = HashMap::new();
        for net in networks {
            if let Some(route) = self.get_bgp_route(net).await? {
                result.insert(net, route);
            }
        }
        Ok(result)
    }

    /// Get a detailed list of all BGP route received by a specific neighbor, using `show bgp ipv4
    /// unicast neighbors x.x.x.x routes detail`.
    pub async fn get_bgp_routes_from_neighbor(
        &mut self,
        neighbor: Ipv4Addr,
    ) -> Result<HashMap<Ipv4Net, Vec<BgpRoute>>, CiscoShellError> {
        Ok(BgpRoute::from_detail(
            self.show(format!(
                "bgp ipv4 unicast neighbors {neighbor} routes detail"
            ))
            .await?,
        )?)
    }

    /// Get a detailed list of all BGP route that have a specific community value set, using `show
    /// bgp ipv4 unicast community "{as}:{community}" summary`.
    pub async fn get_bgp_routes_with_community(
        &mut self,
        community: (u16, u16),
    ) -> Result<HashMap<Ipv4Net, Vec<BgpRoute>>, CiscoShellError> {
        Ok(BgpRoute::from_detail(
            self.show(format!(
                "bgp ipv4 unicast community \"{}:{}\" detail",
                community.0, community.1
            ))
            .await?,
        )?)
    }

    /// Get a detailed list of all BGP that match a specific route-map. The route-map must be given
    /// as a string.
    pub async fn get_bgp_routes_matching_route_map(
        &mut self,
        rm: impl AsRef<str>,
    ) -> Result<HashMap<Ipv4Net, Vec<BgpRoute>>, CiscoShellError> {
        Ok(BgpRoute::from_detail(
            self.show(format!("bgp ipv4 unicast route-map {} detail", rm.as_ref()))
                .await?,
        )?)
    }

    /// Execute a show command, and return the stdout while expecting empty stderr. Only provide the
    /// arguments to `show`, as `show` will be added by this command.
    async fn show(&mut self, cmd: impl AsRef<str>) -> Result<String, CiscoShellError> {
        let cmd = format!("show {}\n", cmd.as_ref().trim());
        log::trace!("[{}] {}", self.name, cmd.trim());
        self.stdin.write_all(cmd.as_bytes()).await?;
        let output = self.wait_done().await?;
        self.expect_empty_stderr().await?;
        Ok(String::from_utf8(output)?)
    }

    /// Wait unil the command is finished by writing `echo #DONE#` to stdin, and waiting until we
    /// receive the `#DONE#` on stdout.
    async fn wait_done(&mut self) -> Result<Vec<u8>, CiscoShellError> {
        self.stdin.write_all(b"echo #DONE#\n").await?;
        Ok(wait_prompt(&mut self.stdout, Duration::from_secs(120), b"#DONE#\n").await?)
    }

    /// Read the entire stderr. If nothing is written to stdout within 1ms, this function returns.
    async fn clear_stderr(&mut self) -> Vec<u8> {
        let mut buffer = Vec::new();
        let stderr = &mut self.stderr;
        loop {
            if timeout(Duration::from_micros(100), stderr.read_buf(&mut buffer))
                .await
                .is_err()
            {
                break;
            }
        }
        buffer
    }

    /// Expect that stderr is empty. Otherwise, return an error.
    async fn expect_empty_stderr(&mut self) -> Result<(), CiscoShellError> {
        let stderr = self.clear_stderr().await;
        if stderr.is_empty() {
            Ok(())
        } else {
            let stderr = String::from_utf8(stderr)?;
            log::warn!("Non-empty stderr:\n{}", stderr);
            Err(CiscoShellError::UnexpectedStderr(stderr))
        }
    }

    /// Send a command and expect the given text to be printed out to stdout. At the same time,
    /// expect stderr to be empty.
    async fn send_cmd_expect<'a, F>(
        &mut self,
        cmd: impl AsRef<str>,
        exp: F,
    ) -> Result<(), CiscoShellError>
    where
        F: FnOnce(&str) -> bool,
    {
        let cmd = cmd.as_ref().trim();
        log::trace!("[{}] {}", self.name, cmd);
        self.stdin.write_all(cmd.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        let stdout = String::from_utf8(self.wait_done().await?)?;
        self.expect_empty_stderr().await?;
        if exp(&stdout) {
            Ok(())
        } else {
            log::warn!(
                "[{}] Unexpected stdout:{}",
                self.name,
                if stdout.is_empty() {
                    String::new()
                } else {
                    format!("\n{stdout}")
                }
            );
            Err(CiscoShellError::UnexpectedStdout(stdout))
        }
    }

    /// Wait unitl we have reached convergence.
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) async fn wait_convergence_task(
        mut self,
        id: usize,
        num: usize,
        exp_ospf_neighbors: HashSet<OspfNeighbor>,
        exp_bgp_routes: HashMap<Ipv4Net, Option<Ipv4Addr>>,
        message_tx: mpsc::Sender<ConvergenceMessage>,
        mut state_rx: broadcast::Receiver<ConvergenceState>,
        mut state: ConvergenceState,
    ) -> Result<(), CiscoShellError> {
        let mut last_ospf_state = None;
        let mut last_bgp_state = None;

        log::trace!("[{}] Expected BGP state: {exp_bgp_routes:?}", self.name);

        // sleep a specific time to spread the jobs out
        sleep(Duration::from_secs_f64(id as f64 / num as f64)).await;

        loop {
            // do a single tick
            match state {
                ConvergenceState::OspfNeighbors => {
                    // check if the ospf neighbors are the same
                    let ospf_neighbors = self.get_ospf_neighbors().await?;
                    if ospf_neighbors.into_iter().collect::<HashSet<_>>() == exp_ospf_neighbors {
                        // send the message to the controller
                        message_tx
                            .send(ConvergenceMessage(state, id))
                            .await
                            .map_err(|_| CiscoShellError::Synchronization)?;
                        // transition
                        state = ConvergenceState::OspfNeighborsDone;
                    }
                }
                ConvergenceState::OspfNeighborsDone => {
                    // nothing to do
                }
                ConvergenceState::OspfState => {
                    let new_ospf_state = self.get_ospf_state().await?;
                    if Some(&new_ospf_state) != last_ospf_state.as_ref() {
                        // update the last ospf state
                        last_ospf_state = Some(new_ospf_state);
                        // send the message
                        message_tx
                            .send(ConvergenceMessage(state, id))
                            .await
                            .map_err(|_| CiscoShellError::Synchronization)?;
                    }
                }
                ConvergenceState::BgpNeighbors => {
                    let bgp_neighbors = self.get_bgp_neighbors().await?;
                    if bgp_neighbors.into_iter().all(|s| s.connected) {
                        // send the message to the controller
                        message_tx
                            .send(ConvergenceMessage(state, id))
                            .await
                            .map_err(|_| CiscoShellError::Synchronization)?;
                        // transition
                        state = ConvergenceState::BgpNeighborsDone;
                    }
                }
                ConvergenceState::BgpNeighborsDone => {
                    // nothing to do, simply wait for the next state
                }
                ConvergenceState::BgpNextHop => {
                    // check all bgp routes and their next-hops
                    if self.check_bgp_next_hop(&exp_bgp_routes).await? {
                        // all routes have the expected next-hop
                        message_tx
                            .send(ConvergenceMessage(state, id))
                            .await
                            .map_err(|_| CiscoShellError::Synchronization)?;
                        state = ConvergenceState::BgpNextHopDone;
                    }
                }
                ConvergenceState::BgpNextHopDone => {
                    // Nothing to do, simply wait for the next state.
                }
                ConvergenceState::BgpState => {
                    let new_bgp_state = self
                        .get_bgp_routes_for_networks(exp_bgp_routes.keys().copied())
                        .await?;
                    if Some(&new_bgp_state) != last_bgp_state.as_ref() {
                        // update the last bgp state
                        last_bgp_state = Some(new_bgp_state);
                        // send the message
                        message_tx
                            .send(ConvergenceMessage(state, id))
                            .await
                            .map_err(|_| CiscoShellError::Synchronization)?;
                    }
                }
                ConvergenceState::Done => {
                    // we are done, break out of this loop
                    return Ok(());
                }
            }

            // Once the update are complete, wait either for one second, or wait until we get a new
            // update from the controller

            match timeout(Duration::from_secs(1), state_rx.recv()).await {
                // we got a new state! update the current state
                Ok(Ok(new_state)) => {
                    state = new_state;
                    sleep(Duration::from_secs_f64(id as f64 / num as f64)).await;
                }
                // this case is a synchronization error
                Ok(Err(_)) => return Err(CiscoShellError::Synchronization),
                // simply go back to the loop and heck the state.
                Err(_) => {}
            }
        }
    }

    /// Check if all bgp next-hops equal to `exp_bgp_routes`.
    pub(crate) async fn check_bgp_next_hop(
        &mut self,
        exp_bgp_routes: &HashMap<Ipv4Net, Option<Ipv4Addr>>,
    ) -> Result<bool, CiscoShellError> {
        let mut all_correct = true;
        for (net, next_hop) in exp_bgp_routes.iter() {
            let routes = self.get_bgp_route(*net).await?;
            if *next_hop
                != routes
                    .into_iter()
                    .flatten()
                    .find(|r| r.selected)
                    .map(|r| r.next_hop)
            {
                all_correct = false;
            }
        }
        Ok(all_correct)
    }
}

/// Error type thrown by the Cisco Shell
#[derive(Debug, Error)]
pub enum CiscoShellError {
    /// Expected no answer, but got an answer
    #[error("Expected no answer, but got the following:\n{0}")]
    UnexpectedStdout(String),
    /// Unexpected message on stderr
    #[error("Unexpected message on stderr: {0}")]
    UnexpectedStderr(String),
    /// IO Error occurred, most likely because the session broke down.
    #[error("I/O Error: {0}")]
    IoError(#[from] std::io::Error),
    /// Cannot parse the result as an UTF8 string
    #[error("Cannot parse the output as UTF8: {0}")]
    Utf8Error(#[from] FromUtf8Error),
    /// Parse error
    #[error("Cannot parse the result form the router! {0}")]
    Parser(#[from] ParseError),
    /// Synchronization error occurred
    #[error("Synchronization Error")]
    Synchronization,
}
