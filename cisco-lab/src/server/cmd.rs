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

//! Module for running a persistent cmd command on the server.

use std::process::Stdio;

use thiserror::Error;
use tokio::{io::AsyncReadExt, process::Child};

use crate::ssh::{SshError, SshSession};

/// This structure allows to generate basic data-plane traffic for more realistic measurements
pub struct CmdHandle {
    /// Name of the cmd child process to use for logging
    process_name: String,
    /// Command to run on the server
    cmd: String,
    /// SSH session to use
    session: SshSession,
    /// Child process for `prober` (if still running)
    cmd_child: Option<Child>,
}

impl CmdHandle {
    /// Create a new `CmdHandle`
    pub async fn new(
        process_name: impl AsRef<str>,
        cmd: impl AsRef<str>,
        session: SshSession,
    ) -> Result<Self, CmdError> {
        Ok(Self {
            process_name: process_name.as_ref().to_owned(),
            cmd: cmd.as_ref().to_owned(),
            session,
            cmd_child: None,
        })
    }

    /// Run the `cmd` given in a child process. If the process is already running, this function
    /// will do nothing.
    pub async fn start(&mut self) -> Result<(), CmdError> {
        log::debug!("[server] start {}!", self.process_name());
        if self.cmd_child.is_some() {
            return Ok(());
        }

        log::trace!("[{}] {}", self.session.name(), self.cmd);
        self.cmd_child = Some(
            self.session
                .raw_command(&["-tt"])
                .arg(&self.cmd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );

        Ok(())
    }

    /// Stop the traffic generator. If it is not yet started, this function will do nothing.
    /// Return a reference to the `SshHandle` in use so that users can perform post-processing
    /// tasks on the same machine.
    pub async fn stop(mut self) -> Result<SshSession, CmdError> {
        log::debug!("[server] stop {}!", self.process_name());
        // send the kill command to the cmd child
        if let Some(mut child) = self.cmd_child.take() {
            // send SIGTERM
            child.kill().await.map_err(SshError::Client)?;
            // print stdout
            if log::max_level() == log::Level::Trace {
                let mut stdout = String::new();
                let mut stderr = String::new();
                child
                    .stdout
                    .take()
                    .unwrap()
                    .read_to_string(&mut stdout)
                    .await?;
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .await?;
                log::trace!(
                    "[{}] Killed {}.{}{}",
                    self.session.name(),
                    self.process_name(),
                    if stdout.is_empty() {
                        String::new()
                    } else {
                        format!("\nSTDOUT:\n{stdout}")
                    },
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!("\nSTDERR:\n{stderr}")
                    }
                );
            }
        }
        Ok(self.session)
    }

    /// Return the name of the cmd child process used for logging
    pub fn process_name(&self) -> &str {
        &self.process_name
    }
}

/// Errors thrown by the traffic capture
#[derive(Debug, Error)]
pub enum CmdError {
    /// I/O Error
    #[error("I/O Error: {0}")]
    Io(#[from] std::io::Error),
    /// Ssh Session error
    #[error("{0}")]
    Ssh(#[from] SshError),
}
