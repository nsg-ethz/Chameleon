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

//! Implementation of the SSH session with the tofino

use std::path::PathBuf;

use itertools::Itertools;

use crate::{
    config::CONFIG,
    ssh::{check_output, SshError, SshSession},
};

/// The SSH session to talk to the tofino.
#[derive(Clone, Debug)]
pub struct TofinoSession(pub(crate) SshSession);

impl TofinoSession {
    /// Create the SSH session with th etofino.
    pub(crate) async fn new() -> Result<TofinoSession, SshError> {
        Ok(TofinoSession(
            SshSession::new(&CONFIG.tofino.ssh_name).await?,
        ))
    }

    /// Get the hostname of the tofino
    pub fn name(&self) -> &str {
        self.0.name()
    }

    /// Enable the selected ports on the tofino. The ports must be given as `X/X`.
    pub(crate) async fn enable_ports(&self, ports: &[&str]) -> Result<(), SshError> {
        log::debug!(
            "[{}] Enable ports: {}",
            self.name(),
            ports.iter().join(" + ")
        );

        let script = format!(
            "ucli\n\n{}\n\nexit\nexit\n",
            ports.iter().map(|p| format!("pm port-enb {p}")).join("\n")
        );

        self.execute_ucli_script(script).await
    }

    /// Disable the selected ports on the tofino. The ports must be given as `X/X`.
    pub(crate) async fn disable_ports(&self, ports: &[&str]) -> Result<(), SshError> {
        log::debug!(
            "[{}] Disable ports: {}",
            self.name(),
            ports.iter().join(" + ")
        );

        let script = format!(
            "ucli\n\n{}\n\nexit\nexit\n",
            ports.iter().map(|p| format!("pm port-dis {p}")).join("\n")
        );

        self.execute_ucli_script(script).await
    }

    /// Setup all ports on the tofino, and make sure they are enabled.
    pub(crate) async fn setup_ports(&self) -> Result<(), SshError> {
        log::debug!("[{}] configure ports...", self.0.name());

        // send the command
        let cmd = format!(
            "bash -c \"source {}; {} -f {}\"",
            &CONFIG.tofino.bf_sde_path,
            &CONFIG.tofino.bf_sde_shell,
            &CONFIG.tofino.ports_setup_filename,
        );

        let output = self.0.raw_command(&["-T"]).arg(&cmd).output().await?;
        check_output(self.name(), output, || cmd)?;

        Ok(())
    }

    /// Send the configuration file to the tofino and re-program the tables on the data-plane.
    pub(crate) async fn configure(
        &self,
        controller: impl AsRef<[u8]> + Send + Sync,
    ) -> Result<(), SshError> {
        log::debug!("[{}] configuring...", self.name());

        // first, make sure that the folder exists
        let mut foldername = PathBuf::from(&CONFIG.tofino.controller_filename);
        foldername.pop();
        let foldername = foldername.to_str().unwrap();

        self.0.execute_cmd(&["mkdir", "-p", foldername]).await?;

        // then, write the file
        self.0
            .write_file(&CONFIG.tofino.controller_filename, controller)
            .await?;

        // finally, load up the controller
        let cmd = format!(
            "bash -c \"source {}; {} -b {}\"",
            &CONFIG.tofino.bf_sde_path,
            &CONFIG.tofino.bf_sde_shell,
            &CONFIG.tofino.controller_filename,
        );

        let output = self.0.raw_command(&["-T"]).arg(&cmd).output().await?;
        let (stdout, stderr) = check_output(self.name(), output, || cmd)?;
        log::trace!(
            "Configured the tofino! \nSTDOUT:\n\n{}\n\n\nSTDERR:\n\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        );

        Ok(())
    }

    /// Execute a UCLI script
    async fn execute_ucli_script(&self, script: String) -> Result<(), SshError> {
        // send the script
        self.0
            .write_file(&CONFIG.tofino.ucli_script_filename, script)
            .await?;

        // execute the script
        let cmd = format!(
            "bash -c \"source {}; {} -f {}\"",
            &CONFIG.tofino.bf_sde_path,
            &CONFIG.tofino.bf_sde_shell,
            &CONFIG.tofino.ucli_script_filename,
        );

        let output = self.0.raw_command(&["-T"]).arg(&cmd).output().await?;
        check_output(self.name(), output, || cmd)?;

        // remove the script
        self.0
            .execute_cmd_stdout(&["rm", &CONFIG.tofino.ucli_script_filename])
            .await?;

        Ok(())
    }
}
