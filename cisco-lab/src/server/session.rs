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

//! Implementation of the SSH session with the server

use std::{path::PathBuf, process::Command};

use itertools::Itertools;

use crate::{
    config::CONFIG,
    ssh::{SshError, SshSession},
    CiscoLabError,
};

use super::ExaBgpHandle;

const LOCK_FILE_PATH: &str = "/tmp/cisco-lab.lock";

/// An active session to the server. This instance is used to manage the server.
pub struct ServerSession(pub(super) SshSession);

impl ServerSession {
    /// Create a new server-side session. This function will try to obtain the lock by creating the
    /// lock file. If a different instance owns the lock, this function will return an error.
    ///
    /// This function is private, since it will also try to lock the entire lab.
    pub(crate) async fn new() -> Result<Self, CiscoLabError> {
        let s = SshSession::new(&CONFIG.server.ssh_name).await?;

        log::trace!("[{}] Obtaining the lock", s.name());
        if s.execute_cmd_status(&["test", "-e", LOCK_FILE_PATH])
            .await?
            .success()
        {
            // the file exists!
            let user = s.execute_cmd_stdout(&["cat", LOCK_FILE_PATH]).await?;
            log::error!(
                "[{}] Cannot obtain the lock! User {} is already running experiments!",
                s.name(),
                user
            );
            return Err(CiscoLabError::CannotObtainLock(user));
        }

        log::trace!("[{}] Create the lock file", s.name());

        // create the lock
        let whoami = s.execute_cmd_stdout(&["whoami"]).await?;
        s.write_file(LOCK_FILE_PATH, whoami).await?;

        let s = ServerSession(s);
        s.create_all_folders().await?;

        Ok(s)
    }

    /// Send the netplan configuration to the server and reload netplan. Make sure that the sudoers
    /// file allows executing `sudo netplan apply` without passwords!
    pub(crate) async fn configure_netplan(
        &self,
        config: impl AsRef<[u8]> + Send + Sync,
    ) -> Result<(), SshError> {
        log::debug!("[{}] Configuring netplan.", self.0.name());

        // send the file
        self.0
            .write_file(&CONFIG.server.netplan_config_filename, config)
            .await?;

        // execute the command to refresh netplan
        self.0.execute_cmd(&["sudo", "netplan", "apply"]).await?;
        Ok(())
    }

    /// Clear the server's ARP cache for all relevant interfaces. Make sure that the sudoers file
    /// allows executing `sudo ip -s -s neigh flush dev <iface>` for the configured interfaces.
    pub(crate) async fn clear_arp_cache(&self) -> Result<(), SshError> {
        log::debug!("[{}] Flushing ARP cache.", self.0.name());

        // flush the ARP cache of the exabgp interface
        self.flush_arp_cache_iface(&CONFIG.server.exabgp_iface)
            .await?;
        // flush the ARP cache of the prober interface
        self.flush_arp_cache_iface(&CONFIG.server.prober_iface)
            .await?;
        Ok(())
    }

    /// Flushes the ARP cache for an interface by executing `sudo ip -s -s neigh flush dev <iface>`.
    async fn flush_arp_cache_iface(&self, iface: &str) -> Result<(), SshError> {
        // execute the command to flush the ARP cache
        self.0
            .execute_cmd(&["sudo", "ip", "-s", "-s", "neigh", "flush", "dev", iface])
            .await?;
        Ok(())
    }

    /// Create an ExaBGP Handle. This will prepare configurations for exabgp, but it will not yet
    /// start the exabgp process.
    pub async fn setup_exabgp<C>(&self, config: C, runner: C) -> Result<ExaBgpHandle, SshError>
    where
        C: AsRef<str> + AsRef<[u8]> + Send + Sync,
    {
        ExaBgpHandle::new(self.0.clone(), config, runner).await
    }

    /// Create all required folders on the server.
    async fn create_all_folders(&self) -> Result<(), SshError> {
        // create all necessary folders
        for folder in [
            &CONFIG.server.exabgp_runner_filename,
            &CONFIG.server.exabgp_config_filename,
            &CONFIG.server.exabgp_runner_control_filename,
            &CONFIG.server.prober_config_filename,
        ]
        .into_iter()
        .map(|p| {
            let mut folder = PathBuf::from(p);
            folder.pop();
            folder.to_str().unwrap().to_string()
        })
        .unique()
        {
            self.0.execute_cmd(&["mkdir", "-p", &folder]).await?;
        }

        Ok(())
    }
}

impl Drop for ServerSession {
    fn drop(&mut self) {
        log::debug!("[{}] Releasing lock (drop)", self.0.name());
        let _ = Command::new("ssh")
            .arg(&CONFIG.server.ssh_name)
            .arg("rm")
            .arg(LOCK_FILE_PATH)
            .output();
    }
}
