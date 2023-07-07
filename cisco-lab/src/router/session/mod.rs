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

//! This module implements an SSH session for Cisco IOS devices.

use std::{net::Ipv4Addr, process::Stdio};

use thiserror::Error;

use crate::{
    ssh::{SshError, SshSession, EMPTY},
    CiscoLabError,
};

mod bgp;
mod ospf;
mod reset_config;
mod shell;
pub(self) mod table_parser;
pub use bgp::{BgpNeighbor, BgpPathType, BgpRoute, BgpRoutesDetailError};
pub use ospf::{OspfNeighbor, OspfRoute};
pub use reset_config::invert_config;
pub use shell::{CiscoShell, CiscoShellError};
pub use table_parser::TableParseError;

/// An SSH session that can be used to trigger multiple commands at the same time while reusing the
/// same session.
///
/// **Warning** Make sure that the destination is properly configured in `~/.ssh/config`, such that
/// no password is required when logging in.
#[derive(Debug, Clone)]
pub struct CiscoSession(SshSession);

impl CiscoSession {
    pub async fn new(destination: impl Into<String>) -> Result<Self, SshError> {
        let destination = destination.into();
        let mut i = 0;
        let session = loop {
            i += 1;
            match SshSession::new(destination.clone()).await {
                Ok(s) => break s,
                Err(e) if i >= 5 => return Err(e),
                Err(_) => {
                    log::warn!(
                        "[{}] Cannot establish connection, trying again!",
                        destination,
                    )
                }
            }
        };

        Ok(Self(session))
    }

    /// Create a new session and load startup configuration without restarting the router.
    pub async fn new_with_reset(destination: impl Into<String>) -> Result<Self, CiscoLabError> {
        // First, create the session
        let s = Self::new(destination).await?;

        // then, reset the configuration
        let mut shell = s.shell().await?;
        shell.reset_configuration().await?;

        // finally, return
        Ok(s)
    }

    /// Execute a command on the router. Expect an empty output (both STDERR and STDOUT).
    pub async fn execute_cmd(&self, cmd: impl AsRef<str> + Send + Sync) -> Result<(), SshError> {
        let (stdout, stderr) = self.0.execute_cmd(&[cmd.as_ref()]).await?;

        if !stdout.is_empty() || !stderr.is_empty() {
            log::trace!(
                "[{}] {} returned non-empty answer:{}{}",
                self.name(),
                cmd.as_ref(),
                if stdout.is_empty() {
                    String::new()
                } else {
                    format!("\nSTDOUT:\n{}", String::from_utf8_lossy(&stdout))
                },
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!("\nSTDERR:\n{}", String::from_utf8_lossy(&stderr))
                },
            );
            Err(SshError::CommandError(
                self.name().to_string(),
                cmd.as_ref().to_string(),
                255,
            ))
        } else {
            Ok(())
        }
    }

    /// Execute the show command with the provided arguments.
    ///
    /// ```rust,no_run
    /// use cisco_lab::router::{CiscoSession, CiscoSsh};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///
    /// let s = CiscoSession::new("router1.domain.com").await?;
    /// let config = s.show("running-config").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn show(&self, cmd: impl AsRef<str> + Send + Sync) -> Result<String, SshError> {
        self.0.execute_cmd_stdout(&["show", cmd.as_ref()]).await
    }

    /// Send the command `clear ip arp force-delete` to clear all entries in the ARP cache. This
    /// helps to avoid connectivity problems when reloading different topology configurations on
    /// the testbed that use the same IP prefixes.
    pub async fn clear_arp_cache(&self) -> Result<(), SshError> {
        log::debug!("[{}] Flushing ARP cache.", self.name());

        // execute the command to clear the ARP cache
        self.execute_cmd("clear ip arp force-delete").await?;

        Ok(())
    }

    /// Send the command `clear ip bgp {neighbor} soft` to request a route refresh of all routes
    /// from that neighbor. This must be executed when changing incoming route-maps. Otherwise, this
    /// effect has no affect. When `neighbor` is `None`, then this command will request a route
    /// refresh from all neighbors.
    pub async fn refresh_routes(&self, neighbor: Option<Ipv4Addr>) -> Result<(), SshError> {
        if let Some(addr) = neighbor {
            self.execute_cmd(format!("clear ip bgp {addr} soft")).await
        } else {
            self.execute_cmd("clear ip bgp {} soft").await
        }
    }

    /// Create a new Cisco shell.
    ///
    /// ```rust,no_run
    /// use cisco_lab::router::{CiscoSession, CiscoSsh};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///
    /// let s = CiscoSession::new("router1.domain.com").await?;
    /// let sh = s.shell().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn shell(&self) -> Result<CiscoShell, CiscoLabError> {
        log::trace!("[{}] Create remote shell", self.name());
        let shell_process = self
            .0
            .raw_command(EMPTY)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped())
            .spawn()
            .map_err(SshError::Client)?;
        CiscoShell::new(shell_process, self.name().to_string()).await
    }

    /// Get the SSH hostname of the target.
    pub fn name(&self) -> &str {
        self.0.name()
    }
}

/// Error while parsing output from a cisco router.
#[derive(Debug, Error)]
pub enum ParseError {
    /// Cannot parse a table
    #[error("Table parse error: {0}")]
    TableParse(#[from] TableParseError),
    /// XML Error
    #[error("XML: {0}")]
    Xml(#[from] roxmltree::Error),
    /// Unexpected XML Structure
    #[error("Missing a specific XML tag")]
    MissingXmlTag(&'static str),
    /// Tag is not a text node
    #[error("Tag is not a text node!")]
    NoText,
    /// Cannot parse IP network
    #[error("Cannot parse IP network: {0}")]
    IpNetParse(#[from] ipnet::AddrParseError),
    /// Cannot parse IP address
    #[error("Cannot parse IP address: {0}")]
    IpAddrParse(#[from] std::net::AddrParseError),
    /// Cannot parse int
    #[error("Cannot parse integer: {0}")]
    IntParse(#[from] std::num::ParseIntError),
    /// Wrong prefix length
    #[error("Wrong prefix length: {0}")]
    PrefixLen(#[from] ipnet::PrefixLenError),
    /// Invalid preamble before the table starts.
    #[error("Invalid Preamble before the table:\n{0}")]
    InvalidPreamble(String),
    /// Read an unknown flag
    #[error("Unknown flag: {0}")]
    UnknownFlag(char),
    /// Error when parsing the bgp routes detail table.
    #[error("Cannot parse BGP routes detail table: {0}")]
    BgpRoutesDetail(#[from] BgpRoutesDetailError),
}
