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

//! Implementation for starting the exabgp process.

use std::{
    process::Stdio,
    time::{Duration, Instant},
};

use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStderr, ChildStdout},
    time::timeout,
};

use crate::{
    config::CONFIG,
    ssh::{SshError, SshSession, EMPTY},
};

const START_TIMEOUT: Duration = Duration::from_secs(10);
const NUM_RETRY: usize = 6;

/// Handle to an exabgp instance
pub struct ExaBgpHandle {
    /// Handle to the SSH session.
    pub(super) session: SshSession,
    /// Number of neighbors configured
    num_sessions: usize,
    /// child process if the process is already running
    child: Option<Child>,
}

impl ExaBgpHandle {
    /// Create a new ExaBGP Handle. This will not yet start the process, but it will configure
    /// exabgp properly.
    pub(crate) async fn new<C>(handle: SshSession, config: C, runner: C) -> Result<Self, SshError>
    where
        C: AsRef<str> + AsRef<[u8]> + Send + Sync,
    {
        let num_sessions = str::lines(config.as_ref())
            .filter(|l| l.starts_with("neighbor"))
            .count();

        log::debug!(
            "[{}] Configuring exabgp with {} sessions",
            handle.name(),
            num_sessions
        );

        handle
            .write_file(&CONFIG.server.exabgp_config_filename, config)
            .await?;
        handle
            .write_file(&CONFIG.server.exabgp_runner_filename, runner)
            .await?;

        log::debug!("exabgp configured!");

        // create self
        let s = Self {
            session: handle,
            num_sessions,
            child: None,
        };
        // create the file
        write_step(&s.session, -1).await?;

        Ok(s)
    }

    /// Get the current step.
    pub async fn current_step(&self) -> Result<isize, SshError> {
        read_step(&self.session).await
    }

    /// Go to the next step in the exabgp execution
    pub async fn step(&self) -> Result<(), SshError> {
        let step = read_step(&self.session).await?;
        write_step(&self.session, step + 1).await
    }

    /// Start the ExaBGP Process. This will fail if you attemp to start the process multiple times!
    pub(crate) async fn start(&mut self) -> Result<(), SshError> {
        if self.child.is_some() {
            log::warn!("[{}] Skip starting exabgp twice.", self.session.name());
            return Ok(());
        }

        let mut iter = 0;
        'retry: loop {
            if iter >= NUM_RETRY {
                log::error!("[{}] Could not start exabgp!", self.session.name());
                return Err(SshError::CommandError(
                    self.session.name().to_string(),
                    format!("exabgp {}", &CONFIG.server.exabgp_config_filename),
                    255,
                ));
            }

            iter += 1;

            // first, kill all previous exabgp processes
            let _ = self
                .session
                .execute_cmd_status(&["killall", "exabgp"])
                .await?;

            // execute the command
            log::debug!(
                "[{}] exabgp {}",
                self.session.name(),
                &CONFIG.server.exabgp_config_filename
            );

            let mut child = self
                .session
                .command("exabgp")
                .arg(&CONFIG.server.exabgp_config_filename)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;

            let start = Instant::now();

            // wait for the appropriate output
            log::trace!(
                "[{}] waiting for exabgp to setup all its sessions",
                self.session.name()
            );

            let mut stdout = child.stdout.take().unwrap();
            let mut stderr = child.stderr.take().unwrap();
            let mut buffer: Vec<u8> = Vec::new();

            // wait until we ses `loaded new configuraiton successfully
            self.expect_output(
                &mut buffer,
                &mut stdout,
                &mut stderr,
                "| loaded new configuration successfully",
                start,
            )
            .await?;

            // wait until we are connected with all peers
            for i in 1..=self.num_sessions {
                let exp = format!("| connected to peer-{i}");
                match self
                    .expect_output(&mut buffer, &mut stdout, &mut stderr, exp, start)
                    .await
                {
                    Ok(_) => {}
                    Err(_) => {
                        child.kill().await?;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue 'retry;
                    }
                }
            }

            // exabgp started successfully
            log::trace!("[{}] exabgp started successfully", self.session.name());
            child.stdout = Some(stdout);
            child.stderr = Some(stderr);

            // we have seen all connections! continue to the first step.
            self.step().await?;

            self.child = Some(child);

            return Ok(());
        }
    }

    /// Gracefully kill the exabgp process by removing the control file.
    pub(crate) async fn kill(mut self) -> Result<(), SshError> {
        if let Some(mut child) = self.child.take() {
            child.kill().await?;
            if log::max_level() == log::Level::Trace {
                let mut stdout = String::new();
                child
                    .stdout
                    .take()
                    .unwrap()
                    .read_to_string(&mut stdout)
                    .await
                    .unwrap();
                log::trace!("[{}] exabgp output was:\n{}", self.session.name(), stdout);
            }
        }

        // kill all exabgp sessions
        self.session
            .execute_cmd_status(&["killall", "exabgp"])
            .await?;

        Ok(())
    }

    /// Wait until a specific output was received.
    async fn expect_output(
        &mut self,
        buffer: &mut Vec<u8>,
        stdout: &mut ChildStdout,
        stderr: &mut ChildStderr,
        target: impl AsRef<str>,
        start_time: Instant,
    ) -> Result<(), SshError> {
        while !String::from_utf8_lossy(buffer).contains(target.as_ref()) {
            let elapsed = start_time.elapsed();
            let to_deadline = if let Some(x) = START_TIMEOUT.checked_sub(elapsed) {
                x
            } else {
                log::warn!(
                    "[{}] exabgp could not load configuration successfully!\nStdout:\n{}",
                    self.session.name(),
                    String::from_utf8_lossy(buffer)
                );
                return Err(SshError::CommandError(
                    self.session.name().to_string(),
                    format!("exabgp {}", &CONFIG.server.exabgp_config_filename),
                    255,
                ));
            };

            match timeout(to_deadline, stdout.read_buf(buffer)).await {
                Ok(Ok(0)) => {
                    let mut s_stderr = String::new();
                    stderr.read_to_string(&mut s_stderr).await?;
                    log::error!(
                        "[{}] exabgp: Unexpected EOF!\nSTDOUT:\n{}STDERR:\n{}",
                        self.session.name(),
                        String::from_utf8_lossy(buffer),
                        s_stderr
                    );
                    return Err(SshError::CommandError(
                        self.session.name().to_string(),
                        format!("exabgp {}", &CONFIG.server.exabgp_config_filename),
                        255,
                    ));
                }
                Ok(Err(e)) => Err(e)?,
                // timeout occurred
                _ => {}
            }
        }

        Ok(())
    }
}

/// Write `self.step` into the control file.
pub(super) async fn write_step(session: &SshSession, step: isize) -> Result<(), SshError> {
    session
        .write_file(
            &CONFIG.server.exabgp_runner_control_filename,
            step.to_string() + "\n",
        )
        .await
}

/// Read `self.step` from the control file.
pub(super) async fn read_step(session: &SshSession) -> Result<isize, SshError> {
    let current_step = session
        .execute_cmd_stdout(&["cat", &CONFIG.server.exabgp_runner_control_filename])
        .await?;
    Ok(current_step.trim().parse().unwrap_or_default())
}

impl Drop for ExaBgpHandle {
    fn drop(&mut self) {
        // send the killall command
        log::trace!("[{}] killall exabgp (drop)", self.session.name());
        let _ = self
            .session
            .std_command(EMPTY)
            .arg("killall")
            .arg("exabgp")
            .output();
    }
}
