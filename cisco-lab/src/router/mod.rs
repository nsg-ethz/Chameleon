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

//! This module is responsible to manage routers (VDCs). It contains methods to generate the
//! configuration. In addition, it contains a client handle over SSH that can push new
//! configuratoin, change configuration and get the current forwarding or routing state of the
//! device.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap, HashSet},
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use bgpsim::{
    config::ConfigModifier,
    export::{
        cisco_frr_generators::{Interface, Target::CiscoNexus7000},
        Addressor, CiscoFrrCfgGen, DefaultAddressor, ExportError, InternalCfgGen,
    },
    prelude::*,
};
use ipnet::Ipv4Net;
use itertools::Itertools;
use serde::Deserialize;
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
    time::timeout,
};

use crate::{
    config::{RouterProperties, ROUTERS, VDCS},
    ssh::SshSession,
    Active, CiscoLab, CiscoLabError, Inactive,
};

mod session;
pub use session::{
    invert_config, BgpNeighbor, BgpPathType, BgpRoute, BgpRoutesDetailError, CiscoSession,
    CiscoShell, CiscoShellError, OspfNeighbor, OspfRoute, ParseError, TableParseError,
};

const OSPF_CONVERGENCE_THRESHOLD_SECS: u64 = 10;
const BGP_CONVERGENCE_THRESHOLD_SECS: u64 = 10;
const BGP_PEC_CHECK: usize = 10;

impl<'n, P: Prefix, Q> CiscoLab<'n, P, Q, Inactive> {
    /// Prepare all internal routers (used in the constructor of `CiscoLab`).
    pub(super) fn prepare_internal_routers(
        net: &'n Network<P, Q>,
    ) -> Result<BTreeMap<RouterId, (&'static RouterProperties, CiscoFrrCfgGen<P>)>, CiscoLabError>
    {
        // get all internal routers
        let mut internal_routers = net.get_routers();
        // sort by their degree
        internal_routers
            .sort_by_key(|r| (Reverse(net.get_topology().neighbors(*r).count()), r.index()));
        let n = internal_routers.len();

        // assign routers
        internal_routers
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                let mut gen = CiscoFrrCfgGen::new(
                    net,
                    r,
                    CiscoNexus7000,
                    VDCS[i].ifaces.iter().map(|x| x.iface.clone()).collect(),
                )?;
                gen.set_ospf_parameters(None, None);
                for iface in VDCS[i].ifaces.iter() {
                    gen.set_mac_address(&iface.iface, iface.mac);
                }
                Ok((
                    r,
                    (
                        VDCS.get(i)
                            .ok_or_else(|| CiscoLabError::TooManyRouters(n))?,
                        gen,
                    ),
                ))
            })
            .collect()
    }

    /// Connect to all routers in parallel, and return a HashMap with all sessions. If any
    /// connection fails, the function will return an error.
    pub(crate) async fn connect_all_routers(
        &self,
    ) -> Result<HashMap<RouterId, CiscoSession>, CiscoLabError> {
        log::debug!("Connect to all routers");

        let mut sessions: HashMap<String, CiscoSession> = HashMap::new();
        for job in VDCS
            .iter()
            .map(|r| r.ssh_name.as_str())
            .map(|name| tokio::spawn(CiscoSession::new_with_reset(name)))
            .collect::<Vec<_>>()
        {
            let session = job.await??;
            sessions.insert(session.name().to_string(), session);
        }

        // now, get those sessions that we need
        Ok(self
            .routers
            .iter()
            .map(|(r, (c, _))| (*r, sessions.remove(&c.ssh_name).unwrap()))
            .collect())
    }
}

impl<'n, P: Prefix, Q, S> CiscoLab<'n, P, Q, S> {
    /// Get the router SSH host name that is associated with the given router ID. If the router ID
    /// is not an internal router, this function will return a `NetworkError`.
    pub fn get_router_device(&self, router: RouterId) -> Result<&'static str, CiscoLabError> {
        Ok(self
            .routers
            .get(&router)
            .map(|(i, _)| i.ssh_name.as_str())
            .ok_or_else(|| NetworkError::DeviceNotFound(router))?)
    }

    /// Get the `RouterProperties` corresponding to the `router`. Contains the `ssh_name`,
    /// `mgnt_addr` and a list of connected `RouterIface`. If the router ID is not an internal
    /// router, this function will return a `NetworkError`.
    pub fn get_router_properties(
        &self,
        router: RouterId,
    ) -> Result<&'static RouterProperties, CiscoLabError> {
        Ok(self
            .routers
            .get(&router)
            .map(|(i, _)| i)
            .ok_or_else(|| NetworkError::DeviceNotFound(router))?)
    }

    /// Get the interface configurations used by the prober, e.g., to automatically search through
    /// captured monitoring traffic and detect changes in the forwarding state precisely.
    ///
    /// For each `RouterId`, contains the tofino port used, the router-side MAC address of the
    /// prober interface, and the source IP for prober traffic to be expected on this interface.
    pub fn get_prober_ifaces(&self) -> &HashMap<RouterId, (usize, [u8; 6], Ipv4Addr)> {
        &self.prober_ifaces
    }

    /// Generate the configuration for all internal routers in the network. This function will
    /// return the configuration as a string.
    pub fn generate_router_config(&mut self, router: RouterId) -> Result<String, CiscoLabError> {
        let (vdc, gen) = self
            .routers
            .get_mut(&router)
            .ok_or_else(|| NetworkError::DeviceNotFound(router))?;

        // first, generate the string
        let mut config = gen.generate_config(self.net, &mut self.addressor)?;

        // check if an interface of that router is not yet used.
        let ifaces = self.addressor.list_ifaces(router);

        // get the first interface that is not yet used
        if let Some(prober_iface) =
            (0..vdc.ifaces.len()).find(|iface| ifaces.iter().all(|(_, _, _, i)| i != iface))
        {
            let network = self.addressor.router_network(router)?;
            let src_addr = network
                .hosts()
                .nth(5)
                .ok_or(ExportError::NotEnoughAddresses)?;
            let iface_addr = network.hosts().nth(4).expect("already checked");
            let name = vdc.ifaces[prober_iface].iface.as_str();
            let mac = vdc.ifaces[prober_iface].mac;

            // store the interface
            if let Some((stored_iface, stored_mac, stored_addr)) = self.prober_ifaces.get(&router) {
                if stored_iface != &prober_iface || stored_mac != &mac || stored_addr != &src_addr {
                    log::warn!(
                        "[{}] Computed prober interface does not match the value previously computed!",
                        vdc.ssh_name
                    );
                }
            } else {
                log::debug!(
                    "[{}] Using interface {name} with IP {iface_addr} for prober packets on {}, set source IP to {src_addr}",
                    vdc.ssh_name,
                    router.fmt(self.net),
                );
            }
            self.prober_ifaces
                .insert(router, (prober_iface, mac, src_addr));

            // generate the configuration
            config.push_str("!\n! Interface for the prober\n!\n");
            config.push_str(
                &Interface::new(name)
                    .ip_address(Ipv4Net::new(iface_addr, 30).unwrap())
                    .mac_address(mac)
                    .no_shutdown()
                    .build(CiscoNexus7000),
            );
        } else {
            let (neighbor, addr, _, iface) = *ifaces.first().unwrap();
            let mac = vdc.ifaces[iface].mac;

            // store the interface
            if let Some((stored_iface, stored_mac, stored_addr)) = self.prober_ifaces.get(&router) {
                if stored_iface != &iface || stored_mac != &mac || stored_addr != &addr {
                    log::warn!(
                        "[{}] Computed prober interface does not match the value previously computed!",
                        vdc.ssh_name
                    );
                }
            } else {
                log::warn!(
                    "[{}] not enough addresses for dedicated prober interface on {}, using interface towards {} instead!",
                    vdc.ssh_name,
                    router.fmt(self.net),
                    neighbor.fmt(self.net),
                );
            }
            self.prober_ifaces.insert(router, (iface, mac, addr));
        }
        Ok(config)
    }

    /// Get the configuration of all internal routers, including their associated SSH host name.
    pub fn generate_router_config_all(
        &mut self,
    ) -> Result<BTreeMap<RouterId, (&'static str, String)>, CiscoLabError> {
        self.routers
            .iter()
            .map(|(r, (vdc, _))| (*r, vdc.ssh_name.as_str()))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(r, vdc)| Ok((r, (vdc, self.generate_router_config(r)?))))
            .collect()
    }

    /// Get a mutable reference to the configuration generator of a specific router. If the router
    /// does not exist or is not an internal router, this function will return a
    /// `NetworkError`. This function will also return a mutable reference to the addressor.
    pub fn get_router_cfg_gen(
        &mut self,
        router: RouterId,
    ) -> Result<(&mut CiscoFrrCfgGen<P>, &mut DefaultAddressor<'n, P, Q>), CiscoLabError> {
        let cfg_gen = self
            .routers
            .get_mut(&router)
            .map(|(_, x)| x)
            .ok_or_else(|| NetworkError::DeviceNotFound(router))?;
        Ok((cfg_gen, &mut self.addressor))
    }
}

impl<'n, P: Prefix, Q> CiscoLab<'n, P, Q, Active> {
    pub(crate) async fn configure_routers(&mut self) -> Result<(), CiscoLabError> {
        log::info!("Configure all routers");

        let mut config = self.generate_router_config_all()?;

        for job in self
            .state
            .routers
            .iter()
            .map(|(r, s)| (s.clone(), config.remove(r).unwrap()))
            .map(|(handle, (_, config))| {
                tokio::spawn(async move {
                    let mut sh = handle.shell().await?;
                    sh.configure(config).await?;
                    Ok(())
                })
            })
            .collect::<Vec<JoinHandle<Result<(), CiscoLabError>>>>()
        {
            job.await??;
        }

        Ok(())
    }

    /// Clear all the routers' ARP caches.
    pub(crate) async fn clear_router_arp_caches(&self) -> Result<(), CiscoLabError> {
        log::info!("Clear ARP cache on all routers");

        for job in self
            .state
            .routers
            .values()
            .cloned()
            .map(|h| tokio::spawn(async move { h.clear_arp_cache().await }))
        {
            job.await??;
        }

        Ok(())
    }

    /// Get a SessionHandle of a router SSH session.
    pub fn get_router_session(&self, router: RouterId) -> Result<CiscoSession, CiscoLabError> {
        Ok(self
            .state
            .routers
            .get(&router)
            .ok_or(NetworkError::DeviceNotFound(router))?
            .clone())
    }

    /// Apply a command to the network.
    pub async fn apply_command(&mut self, expr: ConfigModifier<P>) -> Result<(), CiscoLabError> {
        log::info!("Apply {}", expr.fmt(self.net));

        for router in expr.routers() {
            if self.net.get_device(router).is_external() {
                log::warn!("Skipping reconfiguration on external router!");
                continue;
            }

            let cmd = self.routers.get_mut(&router).unwrap().1.generate_command(
                self.net,
                &mut self.addressor,
                expr.clone(),
            )?;

            // get a shell
            let mut shell = self.state.routers[&router].shell().await?;

            // execute the command
            shell.configure(cmd).await?;
        }

        Ok(())
    }

    /// Schedule a command to be applied to the network at a later time.
    pub fn apply_command_schedule(
        &mut self,
        expr: ConfigModifier<P>,
        delay: Duration,
    ) -> Result<(), CiscoLabError> {
        let cmd_fmt = expr.fmt(self.net);
        let mut plan = HashMap::new();

        for router in expr.routers() {
            if self.net.get_device(router).is_external() {
                log::warn!("Skipping reconfiguration on external router!");
                continue;
            }

            let cmd = self.routers.get_mut(&router).unwrap().1.generate_command(
                self.net,
                &mut self.addressor,
                expr.clone(),
            )?;
            let handle = self.state.routers[&router].clone();

            plan.insert(router, (cmd, handle));
        }

        tokio::task::spawn(async move {
            tokio::time::sleep(delay).await;
            log::info!("Apply {cmd_fmt}");
            for (cmd, handle) in plan.into_values() {
                match handle.shell().await {
                    Ok(mut shell) => match shell.configure(cmd).await {
                        Ok(_) => {}
                        Err(e) => log::error!("[{}] Cannot apply the command: {e}", handle.name()),
                    },
                    Err(e) => log::error!("[{}] Cannot get the shell: {e}", handle.name()),
                }
            }
        });

        Ok(())
    }

    /// Check that the BGP state is equal to the provided network. Equality is checked by making
    /// sure every router selects the correct BGP next-hop for every destination prefix. Make sure
    /// that `net` has the same routers as `self.net`.
    pub async fn equal_bgp_state(&mut self, net: &Network<P, Q>) -> Result<bool, CiscoLabError> {
        let mut all_correct = true;
        for (router, exp_bgp_routes) in self.expected_bgp_state(Some(net))? {
            let mut shell = self.state.routers[&router].shell().await?;
            if !shell.check_bgp_next_hop(&exp_bgp_routes).await? {
                log::warn!(
                    "{} ({}) has wrong BGP state!",
                    router.fmt(net),
                    self.get_router_device(router)?,
                );
                log::debug!("Expected state:\n{:#?}", exp_bgp_routes);
                log::debug!(
                    "Acquired state:\n{:#?}",
                    shell
                        .get_bgp_routes()
                        .await?
                        .into_iter()
                        .filter(|(n, _)| exp_bgp_routes.contains_key(n))
                        .filter_map(|(n, r)| Some((n, r.into_iter().find(|r| r.selected)?)))
                        .collect::<HashMap<_, _>>()
                );
                all_correct = false;
            }
        }

        Ok(all_correct)
    }

    /// Wait for OSPF and BGP to converge. This function will wait until the following has occurred:
    ///
    /// 1. All OSPF neighbors are established
    /// 2. OSPF table does not change for 10 seconds
    /// 3. ALL BGP sessions are established
    /// 4. BGP table does not change for 10 seconds.
    ///
    /// This is done by using two channels. The first one is an MPSC channel that sends the updates
    /// from the router threads to the controller thread. The second one is a Broadcast channel used
    /// by the controller thread to trigger the next state of the workers.
    pub async fn wait_for_convergence(&mut self) -> Result<(), CiscoLabError> {
        if cfg!(feature = "ignore-routers") {
            log::warn!("Skip convergence! (Feature `ignore-routers` is enabled)");
            return Ok(());
        }
        let (message_tx, message_rx) = mpsc::channel::<ConvergenceMessage>(1024);
        let (state_tx, state_rx) = broadcast::channel::<ConvergenceState>(1024);

        // compute the expected bgp state
        let mut exp_bgp_state = self.expected_bgp_state(None)?;

        log::info!("[convergence] Wait for convergence");
        let num_workers = self.routers.len();

        let mut workers = Vec::new();
        for (worker_id, (router, (cfg, _))) in self.routers.iter().enumerate() {
            // compute the expected OSPF state
            let exp_ospf_neighbors: HashSet<OspfNeighbor> = self
                .addressor
                // get all interfaces
                .list_ifaces(*router)
                .into_iter()
                // only care about the neighbor and the interface idx
                .map(|(n, _, _, iface)| (n, iface))
                // only care about internal routers
                .filter(|(n, _)| self.net.get_device(*n).is_internal())
                // get the router-id of the neighbor, and the address of its connected interface
                .map(|(n, iface)| {
                    let id = self.addressor.router_address(n)?;
                    let address = self.addressor.iface_address(n, *router)?;
                    Ok(OspfNeighbor {
                        id,
                        address,
                        iface: cfg.ifaces[iface].iface.clone(),
                    })
                })
                .collect::<Result<_, CiscoLabError>>()?;

            let exp_bgp_routes = exp_bgp_state.remove(router).unwrap_or_default();

            // spawn the threads
            let child_message_tx = message_tx.clone();
            let child_state_rx = state_rx.resubscribe();

            let shell = self.state.routers[router].shell().await?;
            // start the task
            workers.push(tokio::task::spawn(async move {
                shell
                    .wait_convergence_task(
                        worker_id,
                        num_workers,
                        exp_ospf_neighbors,
                        exp_bgp_routes,
                        child_message_tx,
                        child_state_rx,
                        ConvergenceState::OspfNeighbors,
                    )
                    .await
            }))
        }

        std::mem::drop(message_tx);
        std::mem::drop(state_rx);

        // call the controller
        let result = self.wait_convergence_controller(message_rx, state_tx).await;

        // join all workers
        for worker in workers {
            worker.await??;
        }

        result
    }

    /// Wait until we don't see any new bgp updates within the given duration.
    pub async fn wait_for_no_bgp_messages(
        &mut self,
        duration: Duration,
    ) -> Result<(), CiscoLabError> {
        if cfg!(feature = "ignore-routers") {
            log::warn!("Skip convergence! (Feature `ignore-routers` is enabled)");
            return Ok(());
        }
        let (message_tx, message_rx) = mpsc::channel::<ConvergenceMessage>(1024);
        let (state_tx, state_rx) = broadcast::channel::<ConvergenceState>(1024);

        // compute the expected bgp state
        let mut exp_bgp_state = self.expected_bgp_state(None)?;

        let num_workers = self.routers.len();
        let mut workers = Vec::new();
        for (worker_id, router) in self.routers.keys().enumerate() {
            let child_message_tx = message_tx.clone();
            let child_state_rx = state_rx.resubscribe();
            let exp_bgp_routes = exp_bgp_state.remove(router).unwrap_or_default();
            let shell = self.state.routers[router].shell().await?;
            workers.push(tokio::task::spawn(async move {
                shell
                    .wait_convergence_task(
                        worker_id,
                        num_workers,
                        Default::default(),
                        exp_bgp_routes,
                        child_message_tx,
                        child_state_rx,
                        ConvergenceState::BgpState,
                    )
                    .await
            }))
        }

        std::mem::drop(message_tx);
        std::mem::drop(state_rx);

        // call the controller
        let result = self
            .wait_no_bgp_messages(duration, message_rx, state_tx)
            .await;

        // join all workers
        for worker in workers {
            worker.await??;
        }

        result
    }

    /// Main controller for waiting for convergence
    async fn wait_no_bgp_messages(
        &self,
        delay: Duration,
        mut message_rx: mpsc::Receiver<ConvergenceMessage>,
        state_tx: broadcast::Sender<ConvergenceState>,
    ) -> Result<(), CiscoLabError> {
        let deadline = Duration::from_secs(300);
        let start_time = Instant::now();

        log::info!("[convergence] Wait for BGP to stop sending messages.");

        self.wait_convergence_no_message(
            &mut message_rx,
            ConvergenceState::BgpState,
            deadline,
            start_time,
            delay,
        )
        .await?;
        state_tx
            .send(ConvergenceState::Done)
            .map_err(|_| CiscoLabError::ConvergenceError)?;

        log::info!(
            "[convergence] Network has converged after {} seconds",
            start_time.elapsed().as_secs()
        );

        Ok(())
    }

    /// Main controller for waiting for convergence
    async fn wait_convergence_controller(
        &self,
        mut message_rx: mpsc::Receiver<ConvergenceMessage>,
        state_tx: broadcast::Sender<ConvergenceState>,
    ) -> Result<(), CiscoLabError> {
        let deadline = Duration::from_secs(300);
        let start_time = Instant::now();

        log::info!("[convergence] Wait for OSPF to establish neighbors");

        // first, wati for done messages
        self.wait_convergence_done_messages(
            &mut message_rx,
            ConvergenceState::OspfNeighbors,
            deadline,
            start_time,
        )
        .await?;
        state_tx
            .send(ConvergenceState::OspfState)
            .map_err(|_| CiscoLabError::ConvergenceError)?;

        log::info!("[convergence] Wait for OSPF to converge");

        // then, wait for no update message in ospf state
        self.wait_convergence_no_message(
            &mut message_rx,
            ConvergenceState::OspfState,
            deadline,
            start_time,
            Duration::from_secs(OSPF_CONVERGENCE_THRESHOLD_SECS),
        )
        .await?;
        state_tx
            .send(ConvergenceState::BgpNeighbors)
            .map_err(|_| CiscoLabError::ConvergenceError)?;

        log::info!("[convergence] Wait for BGP to establish neighbors");

        // Then, wait for all BGP sessions to connect
        self.wait_convergence_done_messages(
            &mut message_rx,
            ConvergenceState::BgpNeighbors,
            deadline,
            start_time,
        )
        .await?;
        state_tx
            .send(ConvergenceState::BgpNextHop)
            .map_err(|_| CiscoLabError::ConvergenceError)?;

        log::info!("[convergence] Wait for BGP to reach the desired state");

        // Then, wait for all BGP sessions to connect
        self.wait_convergence_done_messages(
            &mut message_rx,
            ConvergenceState::BgpNextHop,
            deadline,
            start_time,
        )
        .await?;
        state_tx
            .send(ConvergenceState::BgpState)
            .map_err(|_| CiscoLabError::ConvergenceError)?;

        log::info!("[convergence] Wait for BGP to converge");

        // Finally, wait for BGP to converge
        self.wait_convergence_no_message(
            &mut message_rx,
            ConvergenceState::BgpState,
            deadline,
            start_time,
            Duration::from_secs(BGP_CONVERGENCE_THRESHOLD_SECS),
        )
        .await?;
        state_tx
            .send(ConvergenceState::Done)
            .map_err(|_| CiscoLabError::ConvergenceError)?;

        log::info!(
            "[convergence] Network has converged after {} seconds",
            start_time.elapsed().as_secs()
        );

        for (rid, cisco_session) in self.state.routers.iter() {
            log::trace!(
                "[convergence] BGP state of router {} after convergence:\n{}",
                rid.fmt(self.net),
                cisco_session.show("ip bgp all").await?
            );
        }

        Ok(())
    }

    async fn wait_convergence_done_messages(
        &self,
        message_rx: &mut mpsc::Receiver<ConvergenceMessage>,
        state: ConvergenceState,
        deadline: Duration,
        start_time: Instant,
    ) -> Result<(), CiscoLabError> {
        let mut seen_messages = HashSet::new();

        while seen_messages.len() < self.routers.len() {
            let until_deadline = deadline.saturating_sub(start_time.elapsed());
            match timeout(until_deadline, message_rx.recv()).await {
                // timeout occurred
                Err(_) => {
                    log::warn!(
                        "[convergence] Timeout occurred while waiting for convergence in state {:?}",
                        state
                    );
                    return Err(CiscoLabError::ConvergenceTimeout);
                }
                // Channels closed
                Ok(None) => {
                    log::warn!(
                        "[convergence] MPSC channel for receiving messages has no senders left!",
                    );
                    return Err(CiscoLabError::ConvergenceError);
                }
                // received message from correct state
                Ok(Some(ConvergenceMessage(s, i))) if s == state => {
                    log::debug!("[convergence] Received message from {}", i);
                    seen_messages.insert(i);
                }
                // received message from wrong state
                Ok(Some(ConvergenceMessage(s, i))) => {
                    log::debug!(
                        "[convergence] Received message from {} in old state {:?}. Ignore the message",
                        i,
                        s
                    );
                }
            }
        }

        Ok(())
    }

    async fn wait_convergence_no_message(
        &self,
        message_rx: &mut mpsc::Receiver<ConvergenceMessage>,
        state: ConvergenceState,
        deadline: Duration,
        start_time: Instant,
        threshold: Duration,
    ) -> Result<(), CiscoLabError> {
        let mut last_update = Instant::now();
        while start_time.elapsed() < deadline {
            let until_threshold = threshold.saturating_sub(last_update.elapsed());
            match timeout(until_threshold, message_rx.recv()).await {
                // If the timeout was reached, we can proceed
                Err(_) => {
                    log::debug!("[convergence] No update from workers received! Transition to the next state");
                    return Ok(());
                }
                // channels broke down.
                Ok(None) => {
                    log::warn!(
                        "[convergence] MPSC channel for receiving messages has no senders left!",
                    );
                    return Err(CiscoLabError::ConvergenceError);
                }
                // received message from correct state
                Ok(Some(ConvergenceMessage(s, i))) if s == state => {
                    log::debug!("[convergence] Received message from {}", i);
                    last_update = Instant::now();
                }
                // received message from wrong state
                Ok(Some(ConvergenceMessage(s, i))) => {
                    log::debug!(
                        "[convergence] Received message from {} in old state {:?}. Ignore the message",
                        i,
                        s
                    );
                }
            }
        }

        log::warn!(
            "[convergence] Timeout occurred while waiting for convergence in state {:?}",
            state
        );
        Err(CiscoLabError::ConvergenceTimeout)
    }

    /// Compute the expected BGP state, which is a list of routes and their expected BGP next-hop
    /// for each router in the network.
    ///
    /// If the argument `net` is `Some(net)`, then use this network as reference for what next-hop
    /// we expect. Otherwise, use `self.net`.
    fn expected_bgp_state(
        &mut self,
        net: Option<&Network<P, Q>>,
    ) -> Result<HashMap<RouterId, HashMap<Ipv4Net, Option<Ipv4Addr>>>, ExportError> {
        let mut result = HashMap::new();

        let known_prefixes = self
            .net
            .get_known_prefixes()
            .chain(net.iter().flat_map(|n| n.get_known_prefixes()))
            .copied()
            .collect_vec();

        let net = net.unwrap_or(self.net);

        for router in self.routers.keys().copied() {
            if let Some(r) = net.get_device(router).internal() {
                let mut exp_bgp_routes = HashMap::new();
                for p in known_prefixes.iter().copied() {
                    let nh = r
                        .get_selected_bgp_route(p)
                        .map(|x| {
                            let nh = x.route.next_hop;
                            // check if nh is internal or external
                            if self.state.routers.contains_key(&nh) {
                                // router is internal. use router ip
                                self.addressor.router_address(nh)
                            } else {
                                self.addressor.iface_address(nh, router)
                            }
                        })
                        .transpose()?;
                    for net in self.addressor.prefix(p)?.sample_uniform_n(BGP_PEC_CHECK) {
                        exp_bgp_routes.insert(*net, nh);
                    }
                }
                result.insert(router, exp_bgp_routes);
            }
        }

        Ok(result)
    }
}

/// Run `show module` on all routers (not on the vdcs) and make sure that the first supervisor
/// module status is set to `active *`, while the second one is set to `ha-standby`.
pub(crate) async fn check_router_ha_status() -> Result<(), CiscoLabError> {
    for job in ROUTERS
        .iter()
        .map(String::as_str)
        .map(|x| tokio::spawn(_check_router_ha_status(x)))
        .collect::<Vec<_>>()
    {
        job.await??;
    }
    Ok(())
}

/// Run `show module` on `router` and make sure that the first supervisor module status is set to
/// `active *`, while the second one is set to `ha-standby`.
pub(crate) async fn _check_router_ha_status(router: &'static str) -> Result<(), CiscoLabError> {
    log::debug!("[{router}] checking supervisor status.");

    #[derive(Deserialize)]
    struct ModInfo {
        #[serde(alias = "TABLE_modinfo")]
        table: ModInfoTable,
    }
    #[derive(Deserialize)]
    struct ModInfoTable {
        #[serde(alias = "ROW_modinfo")]
        rows: Vec<ModInfoRow>,
    }
    #[derive(Deserialize)]
    struct ModInfoRow {
        #[serde(alias = "mod")]
        module: u32,
        modtype: String,
        status: String,
    }
    let ssh = SshSession::new(router).await?;
    let mod_info_json = ssh.execute_cmd_stdout(&["show module | json"]).await?;
    let mod_info: ModInfo = serde_json::from_str(&mod_info_json).map_err(|e| {
        CiscoLabError::CannotParseShowModule({
            let mut error_msg = format!("[{router}] ");
            error_msg.push_str(&e.to_string());
            error_msg
        })
    })?;

    if let (Some(row_1), Some(row_2)) = (mod_info.table.rows.get(0), mod_info.table.rows.get(1)) {
        if row_1.module != 1 || row_2.module != 2 {
            log::error!("[{router}] Unexpected numbering of modules in `show modules`!");
        } else if row_1.modtype != "Supervisor Module-2" || row_2.modtype != "Supervisor Module-2" {
            log::error!("[{router}] Module 1 and 2 on the device are not supervisors!");
        } else if row_1.status != "active *" || row_2.status != "ha-standby" {
            log::error!(
                "[{router}] Module 1 is in status `{}`, while module 2 is in status `{}`!",
                row_1.status,
                row_2.status
            )
        } else {
            log::trace!("[{router}] Supervisor status is correct!");
            return Ok(());
        }
    } else {
        log::error!("[{router}] Router contains less than two supervisors!")
    }

    log::error!(
        "[{router}] Supervisor (high-availability) status is bad! Maybe restart the router?"
    );
    log::info!("[{router}] Hint: `ssh {router} reload`");

    Err(CiscoLabError::WrongSupervisorStatus(router))
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(self) enum ConvergenceState {
    OspfNeighbors,
    OspfNeighborsDone,
    OspfState,
    BgpNeighbors,
    BgpNeighborsDone,
    BgpNextHop,
    BgpNextHopDone,
    BgpState,
    Done,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(self) struct ConvergenceMessage(ConvergenceState, usize);
