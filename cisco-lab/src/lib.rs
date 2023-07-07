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

//! This library contains methods for generating configurations for the cisco-setup at NSG.
//!
//! # Configuration
//!
//! Setting up the lab requires a lot of configurations. You can find sample configuration in the
//! `cisco-lab/config` folder. The most important aspect is changing the SSH host names to match
//! your `~/.ssh/config` file, and to create the router configuration files (see [getting the lab
//! setup](#getting-the-lab-setup)).
//!
//! This library will establish SSH sessions with the server, the Tofino and with all routers
//! (VDCs). For this to work, make sure that the SSH host name matches the SSH
//! configuration. Further, make sure that you have configured SSH such that the command `ssh
//! $hostname` will automatically establish the session without the need for a username or
//! password.
//!
//! # Getting the Labsetup
//!
//! This library must know which routers are available under which address, and how many interfaces
//! are available, and how they are connected. To do that, you first need to edit
//! `config/routers.toml`. Write all router ssh hostnames there (see
//! [configuration](#configuration)). Then, generate the configurations as follows:
//!
//! ```bash
//! git submodule update --init --remote
//! cd config
//! ./generate_interfaces.sh
//! export LAB_SETUP_CONFIG=$(pwd)
//! ```
//!
//! Also, make sure to export the path to the configuration into the environment variable
//! `LAB_SETUP_CONFIG`.
//!
//! # Locking Mechanism
//!
//! This library is interacting with a physical lab. Therefore, you cannot run multiple experiments
//! at once. Thus, the main structure `CiscoLab` contains a type parameter `S` which is either
//! `Inactive` or `Active`. in `Inactive` state, you can only generate confiugration strings. In
//! `Active` state, it will connect itself to the physical lab and reconfigure stuff.
//!
//! To enforce that only a single instance can actively use the lab, the `CiscoLab` will create a
//! lock file on the server called `/tmp/cisco-lab.lock`. When creating the lock, it will write the
//! user into the file (for debugging purpose). When the lock already exists, the `CiscoLab` cannot
//! be turned from `Inactive` to `Active`. See [`CiscoLab`] for more details.
//!
//! # Experiment Setup
//!
//! ## The Physical Setup
//!
//! The lab is set-up as follows: Three Cisco Nexus 7000 series routers are all connected to a
//! single Tofino, and so is a server with 6 ports. The way the routers are connected is documented
//! in the [labsetup](https://tools.nsg.ee.ethz.ch/labsetup/). Each router is further divided into
//! four VDCs.
//!
//! ```text
//!           Router 1                         Router 2                         Router 3
//! ┌──────┬──────┬──────┬──────┐    ┌──────┬──────┬──────┬──────┐    ┌──────┬──────┬──────┬──────┐
//! │ VDC1 │ VDC2 │ VDC3 │ VDC4 │    │ VDC1 │ VDC2 │ VDC3 │ VDC4 │    │ VDC1 │ VDC2 │ VDC3 │ VDC4 │
//! │ 111  │ 112  │ 113  │ 113  │    │ 121  │ 122  │ 123  │ 123  │    │ 131  │ 132  │ 133  │ 133  │
//! └──────┴─────┬┴┬─────┴──────┘    └──────┴─────┬┴┬─────┴──────┘    └──────┴─────┬┴┬─────┴──────┘
//!              │ │                              │ │                              │ │
//!              │ │                              │ │                              │ │
//!              │ │                              │ │                              │ │
//!              │ │                  ┌───────────┴─┴───────────┐                  │ │
//!              │ └──────────────────┤         Tofino          ├──────────────────┘ │
//!              └────────────────────┤ - emulating the network ├────────────────────┘
//!                                   └───────────┬─┬───────────┘
//!                                               │ │
//!                                               │ │
//!                                               │ │
//!                                   ┌───────────┴─┴───────────┐
//!                                   │         Server          │
//!                                   │ - exabgp                │
//!                                   │ - traffig generator     │
//!                                   │ - traffic collector     │
//!                                   │ - delay mechanism       │
//!                                   │   (not yet implemented) │
//!                                   └─────────────────────────┘
//! ```
//!
//! ## Tofino Setup
//!
//! The tofino is responsible to emulate the physical network connections. It is configured such
//! that wires are connected properly. In addition, it can be used to mirror BGP packets towards the
//! server and attach a timestamp to the packets.
//!
//! ## Server Setup
//!
//! The server is responsible for emulating the external routers using ExaBGP. ExaBGP will run as a
//! single instance and connect to all routers at once. Netplan is configured such that each
//! instance has its own IP address, according to the addressing scheme.
//!
//! To emulate events that are changing, you can configure `CiscoLab` to change the advertisements
//! during the experiment. To do this, you need to call [`CiscoLab::step_external_time`] to
//! increment a counter, followed by either calling [`CiscoLab::advertise_route`] or
//! [`CiscoLab::withdraw_route`]. The generated python script to interact with ExaBGP will then
//! advertise all routes of a single step, and then wait while reading from a specific file. As soon
//! as this file contains a number that is larger or equal to the current counter value, the program
//! will contineue and advertise routes of the next step.
//!
//! **Important Notes**
//! - Make sure that you can execute the command `sudo netplan apply` without requiring a
//!   password. To do this, edit the `/etc/sudoers` file accordingly.
//! - Make sure that the netplan configuraiton file, as specified in `config/config.toml` as the key
//!   `server.netplan_config_filename` is writable by regular users. To do so, set the permissions
//!   to `666`.
//!
//! ## Delay Mechanism
//!
//! In order to emulate delays on the system, the tofino modifies the packets to include the delay
//! time and sends them to the server. The server will then cache the packets for the given duration
//! and send them back to the tofino.
//!
//! **This functionaly is not yet implemented.**

#![doc(html_logo_url = "https://iospf.tibors.ch/images/bgpsim/dark_only.svg")]

use bgpsim::{
    export::{Addressor, DefaultAddressorBuilder, ExportError},
    types::{NetworkError, NonOverlappingPrefix},
};
use ipnet::Ipv4Net;
use router::{CiscoSession, CiscoShellError};
use server::{CmdError, ExaBgpHandle, ServerSession, TrafficCaptureError};
use ssh::SshError;
use thiserror::Error;
use tofino::TofinoSession;

pub mod config;
pub mod router;
pub mod server;
pub mod ssh;
mod tofino;

pub use server::export_capture_to_csv;

#[cfg(test)]
mod test;

use std::{
    collections::{BTreeMap, HashMap},
    net::Ipv4Addr,
};

use bgpsim::{
    export::{CiscoFrrCfgGen, DefaultAddressor, ExaBgpCfgGen},
    prelude::*,
};

use config::{RouterProperties, CONFIG};

/// The CiscoLab is in offline mode. This means that it will not do anything on the physical
/// hardware, but you can still generate the configuration strings.
pub struct Inactive;

/// The `CiscoLab` is connected to the physical hardware, and actively managing it. The structure
/// contains the established sessions. There can always be at most one `CiscoLab<'n, Q, Active>`
/// instance. This is enforced by creating a lock file on the server.
pub struct Active {
    pub(crate) server: ServerSession,
    pub(crate) exabgp: ExaBgpHandle,
    pub(crate) tofino: TofinoSession,
    pub(crate) routers: HashMap<RouterId, CiscoSession>,
}

/// This structure represents an instance of a real network. The type parameter `S` is used to
/// indicate the current state of the lab. This can either be [`Inactive`] or [`Active`]. There can
/// always be at most one `CiscoLab<'n, Q, Active>` instance. This is enforced by creating a lock
/// file on the server. A `CiscoLab<'n, Q, Inactive>` instance will not connect itself to any of the
/// physical devices in the lab, and can only be used to generate the configuration strings. The
/// lock file is located on the server at `/tmp/cisco-lab.lock`, and it will contain the username of
/// the user that has created the lock.
///
/// Calling [`CiscoLab::new`] will create an inactive instance. This inactive instance can be used
/// to generate the configuration strings, but it will not connect itself to any device via
/// SSH. Calling [`CiscoLab::connect`] on a `CiscoLab<'n, Q, Inactive>` will try to connect itself
/// to all devices, returning a `CiscoLab<'n, Q, Active>` instance.
pub struct CiscoLab<'n, P: Prefix, Q, S = Inactive> {
    net: &'n Network<P, Q>,
    addressor: DefaultAddressor<'n, P, Q>,
    routers: BTreeMap<RouterId, (&'static RouterProperties, CiscoFrrCfgGen<P>)>,
    prober_ifaces: HashMap<RouterId, (usize, [u8; 6], Ipv4Addr)>,
    external_routers: BTreeMap<RouterId, ExaBgpCfgGen<P>>,
    link_delays: HashMap<(RouterId, RouterId), u32>,
    state: S,
}

impl<'n, P: Prefix, Q> CiscoLab<'n, P, Q, Inactive> {
    /// Generate a new instance to manage the network. This will only allocate strucutres, but not
    /// change anything on the network itself. This function will not yet connect to any router.
    pub fn new(net: &'n Network<P, Q>) -> Result<Self, CiscoLabError> {
        let routers = Self::prepare_internal_routers(net)?;
        let external_routers = Self::prepare_external_routers(net)?;
        let addressor = DefaultAddressorBuilder {
            internal_ip_range: CONFIG.addresses.internal_ip_range,
            external_ip_range: CONFIG.addresses.external_ip_range,
            local_prefix_len: CONFIG.addresses.local_prefix_len,
            link_prefix_len: CONFIG.addresses.link_prefix_len,
            external_prefix_len: CONFIG.addresses.external_prefix_len,
        }
        .build(net)?;

        Ok(Self {
            net,
            addressor,
            routers,
            prober_ifaces: Default::default(),
            external_routers,
            link_delays: Default::default(),
            state: Inactive,
        })
    }

    /// Setup the environment. This function will connect to all devices in the lab and configure
    /// them properly. This function will create the lock on the server. Dropping `CiscoLab<'n, Q,
    /// Active>` will also drop the [`server::ServerSession`], which will automatically release the
    /// lock.
    pub async fn connect(mut self) -> Result<CiscoLab<'n, P, Q, Active>, CiscoLabError> {
        // Connect to the server and create the lock file!
        let server = ServerSession::new().await?;

        // before we change anything on the server, the tofino or any of the VDCs, check the module
        // status on all routers
        router::check_router_ha_status().await?;

        let exabgp = server
            .setup_exabgp(
                self.generate_exabgp_config()?,
                self.generate_exabgp_runner()?,
            )
            .await?;
        let tofino = TofinoSession::new().await?;
        let routers = if cfg!(feature = "ignore-routers") {
            Default::default()
        } else {
            self.connect_all_routers().await?
        };

        let mut lab = CiscoLab {
            net: self.net,
            addressor: self.addressor,
            routers: self.routers,
            external_routers: self.external_routers,
            prober_ifaces: self.prober_ifaces,
            link_delays: self.link_delays,
            state: Active {
                server,
                exabgp,
                tofino,
                routers,
            },
        };

        lab.configure_netplan().await?;
        lab.configure_tofino().await?;
        if !cfg!(feature = "ignore-routers") {
            lab.configure_routers().await?;
            lab.clear_router_arp_caches().await?;
        }

        lab.state.server.clear_arp_cache().await?;

        // wait for the configurations to be processed everywhere
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        lab.state.exabgp.start().await?;

        log::debug!("[CiscoLab] Hardware mapping:");
        lab.routers.iter().for_each(|(r, (vdc, _))| {
            log::debug!("- router {} mapped to {}", r.fmt(lab.net), vdc.ssh_name);
            let ifaces = lab.addressor.list_ifaces(*r);
            ifaces.iter().for_each(|(neighbor, ipv4, _, iface_idx)| {
                log::debug!(
                    "        {iface_idx}: MAC({}) IP({ipv4}) connecting to {}",
                    vdc.ifaces[*iface_idx]
                        .mac
                        .map(|b: u8| format!("{b:02x}"))
                        .join(":"),
                    neighbor.fmt(lab.net),
                );
            })
        });

        Ok(lab)
    }
}

impl<'n, P: Prefix + NonOverlappingPrefix, Q> CiscoLab<'n, P, Q, Inactive> {
    /// Register a prefix equivalence class. The given `prefix` will be replaced with a set of
    /// actual IP networks. This will change the generated configuration, the advertised routes, as
    /// well as the behavior on checking wether a configuration has converged.
    pub fn register_pec(&mut self, prefix: P, networks: Vec<Ipv4Net>) {
        self.addressor.register_pec(prefix, networks);
    }
}

impl<'n, P: Prefix, Q> CiscoLab<'n, P, Q, Inactive> {
    /// Get a mutable reference to the addressor. Any modification on that addressor will have an
    /// affect on the network that is generated when calling `self.connect`.
    pub fn addressor_mut(&mut self) -> &mut DefaultAddressor<'n, P, Q> {
        &mut self.addressor
    }
}

impl<'n, P: Prefix, Q> CiscoLab<'n, P, Q, Active> {
    /// Disconnect the instance from the lab, removing the lock file and killing exabgp
    pub async fn disconnect(self) -> Result<CiscoLab<'n, P, Q, Inactive>, CiscoLabError> {
        self.state.exabgp.kill().await?;
        Ok(CiscoLab {
            net: self.net,
            addressor: self.addressor,
            routers: self.routers,
            prober_ifaces: self.prober_ifaces,
            external_routers: self.external_routers,
            link_delays: self.link_delays,
            state: Inactive,
        })
    }
}

impl<'n, P: Prefix, Q, S> CiscoLab<'n, P, Q, S> {
    /// Get the addressor. Since modifying the running addressor is invalid and could lead to
    /// problems during execution of the network, only the immutable addressor is accessible in
    /// both the status `Active` and `Inactive`.
    ///
    /// In case you need the addressor for looking up addresses, use the `try_get_...` functions
    /// provided.
    pub fn addressor(&self) -> &DefaultAddressor<'n, P, Q> {
        &self.addressor
    }
}

/// Error type thrown while managing the lab network.
#[derive(Debug, Error)]
pub enum CiscoLabError {
    /// Error from `bgpsim::Network`
    #[error("{0}")]
    Network(#[from] NetworkError),
    /// Error while exporting configurations
    #[error("{0}")]
    Export(#[from] ExportError),
    /// I/O Error
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Session error
    #[error("Session error: {0}")]
    Ssh(#[from] SshError),
    /// Cisco Shell error
    #[error("Cisco shell error: {0}")]
    CiscoShell(#[from] CiscoShellError),
    /// Formatting Error
    #[error("{0}")]
    Fmt(#[from] std::fmt::Error),
    /// Network contains too many routers to simulate it in the lab network.
    #[error("Cannot fit {0} routers in the lab network")]
    TooManyRouters(usize),
    /// Network contains too many nodes with too high degree.
    #[error("Cannot fit {0} links in the lab network on router {1}.")]
    TooManyLinks(usize, &'static str),
    /// Cannot obtain the lock.
    #[error("Cannot obtain the lock! {0} owns the lock tho the lab.")]
    CannotObtainLock(String),
    /// Cannot join a parallel job
    #[error("Cannot join thread: {0}")]
    Join(#[from] tokio::task::JoinError),
    /// Error when generating basic data-plane traffic
    #[error("Cmd error: {0}")]
    Cmd(#[from] CmdError),
    /// Error when doing traffic capture
    #[error("Capture error: {0}")]
    TrafficCapture(#[from] TrafficCaptureError),
    /// Timeout occurred while waiting for convergence
    #[error("Timeout occurred while waiting for convergence!")]
    ConvergenceTimeout,
    /// Synchronization error during convergence
    #[error("Synchronization error during convergence!")]
    ConvergenceError,
    /// Cannot parse the output of `show module` on the main router.
    #[error("Cannot parse `show module` command output! {0}")]
    CannotParseShowModule(String),
    /// The supervisor status on a router is suboptimal. Reboot the router
    #[error("Supervisor on {0} is in a bad state! Maybe `reload` the router?")]
    WrongSupervisorStatus(&'static str),
}
