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

//! Module for managing SSH sessions.

use std::{
    ffi::{OsStr, OsString},
    io::ErrorKind,
    process::{Command as StdCommand, ExitStatus, Output, Stdio},
    str::FromStr,
    string::FromUtf8Error,
    time::Duration,
};

use itertools::Itertools;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStdout, Command},
    time::timeout,
};

pub const EMPTY: &[&str] = &[];

/// This is the main SSH session with a remote host.
///
/// This session is configured to automatically manage a control master using thye following
/// arguments:
///
/// - `ControlMaster auto`
/// - `ControlPath /tmp/.ssh-%r@%h:%p`
/// - `ControlPersist 10m`
/// - `BatchMode yes`
///
/// **Warning** Make sure that the destination is properly configured in `~/.ssh/config`, such that
/// no password is required when logging in.
#[derive(Debug, Clone)]
pub struct SshSession {
    /// SSH destination host
    destination: String,
}

impl SshSession {
    /// Create a new SSH Session with the destination.
    pub async fn new(destination: impl Into<String>) -> Result<Self, SshError> {
        let destination = destination.into();

        log::trace!("[{}] connecting...", destination);

        let this = Self { destination };

        // wait for 10 seconds until the connection is established
        match timeout(Duration::from_secs(10), this.execute_cmd(&["echo", "test"])).await {
            Ok(Ok((stdout, stderr))) => {
                let stdout = String::from_utf8_lossy(&stdout);
                let stderr = String::from_utf8_lossy(&stderr);
                if stderr.is_empty() || stderr.trim() == "User Access Verification" {
                    if stdout.trim() == "test" {
                        log::trace!("[{}] connection established!", this.name());
                        Ok(this)
                    } else {
                        log::error!(
                            "[{}] Unexpected stdout! expected `test`, but got:\n{stdout}",
                            this.name()
                        );
                        Err(SshError::Setup(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("Expected `test`, but got {stdout}"),
                        )))
                    }
                } else {
                    log::error!("[{}] Unexpected stderr:\n{stderr}", this.name());
                    Err(SshError::Setup(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("Expected empty stderr, but got {stderr}"),
                    )))
                }
            }
            Ok(Err(e)) => {
                log::error!(
                    "[{}] Error while connecting to the target: {e}",
                    this.name()
                );
                Err(e)
            }
            Err(_) => {
                log::error!("[{}] connection timeout!", this.name());
                Err(SshError::Timeout)
            }
        }
    }

    /// Get the hostname for the session.
    pub fn name(&self) -> &str {
        &self.destination
    }

    /// Create a raw `ssh` command with the following attributes set:
    ///
    /// - `oControlMaster=auto`
    /// - `oControlPath=/tmp/.ssh-%r@%h:%p`
    /// - `oControlPersist=30m`
    /// - `oBatchMode=yes`
    /// - `args` as given by the other arguments.
    /// - `destination` to connect to the given destination (or `none` if the path must exist).
    /// - `kill_on_drop = true` to kill the thread once it is dropped.
    pub(crate) fn raw_command(&self, args: &[impl AsRef<OsStr>]) -> Command {
        let mut cmd = Command::from(self.std_command(args));
        log::trace!("[tokio::process::Command] {:?}", cmd);
        cmd.kill_on_drop(true);
        cmd
    }

    /// Get a new command that executes the given program on the remote machine
    ///
    /// The following example will execute the command `echo hi`:
    /// ```rust,no_run
    /// use cisco_lab::ssh::SshSession;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///
    /// let s = SshSession::new("host.domain.ch").await?;
    /// let _ = s.command("echo").arg("hi").output().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = self.raw_command(EMPTY);
        cmd.arg(program);
        cmd
    }

    /// Copy a local file to the remote host.
    pub async fn scp_loc2rem(
        &self,
        src: impl AsRef<OsStr> + Send + Sync,
        dst: impl AsRef<OsStr> + Send + Sync,
    ) -> Result<(), SshError> {
        let cmd_str = || {
            format!(
                "scp {:?} {}:{:?}",
                src.as_ref().to_string_lossy(),
                self.name(),
                dst.as_ref().to_string_lossy()
            )
        };

        log::trace!("[{}] {}", self.name(), cmd_str());

        // generate the command
        let mut cmd = self.scp_cmd();
        let mut dst_arg = OsString::from_str(self.name()).unwrap();
        dst_arg.push(":");
        dst_arg.push(dst.as_ref());
        cmd.arg(src.as_ref()).arg(dst_arg);

        // turn the command into a tokio command
        let mut cmd = Command::from(cmd);
        cmd.kill_on_drop(true);

        // execute and check the output
        let output = match cmd.output().await {
            Ok(out) => out,
            Err(e) => {
                log::error!("[{}] {} failed: {}", self.name(), cmd_str(), e,);
                Err(e)?
            }
        };
        check_output(self.name(), output, cmd_str).map(|_| ())
    }

    /// Copy a remote file to the local host.
    pub async fn scp_rem2loc(
        &self,
        src: impl AsRef<OsStr> + Send + Sync,
        dst: impl AsRef<OsStr> + Send + Sync,
    ) -> Result<(), SshError> {
        let cmd_str = || {
            format!(
                "scp {}:{:?} {:?}",
                self.name(),
                src.as_ref().to_string_lossy(),
                dst.as_ref().to_string_lossy()
            )
        };

        log::trace!("[{}] {}", self.name(), cmd_str());

        // generate the command
        let mut cmd = self.scp_cmd();
        let mut src_arg = OsString::from_str(self.name()).unwrap();
        src_arg.push(":");
        src_arg.push(src.as_ref());
        cmd.arg(src_arg).arg(dst.as_ref());

        // turn the command into a tokio command
        let mut cmd = Command::from(cmd);
        cmd.kill_on_drop(true);

        // execute and check the output
        let output = match cmd.output().await {
            Ok(out) => out,
            Err(e) => {
                log::error!("[{}] {} failed: {}", self.name(), cmd_str(), e,);
                Err(e)?
            }
        };
        check_output(self.name(), output, cmd_str).map(|_| ())
    }

    /// Execute a command and return the bytes of both `STDOUT` and `STDERR`. This funciton call
    /// will check that the returned exit code is 0.
    ///
    /// The following example will execute the command `echo hi`:
    /// ```rust,no_run
    /// use cisco_lab::ssh::SshSession;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///
    /// let s = SshSession::new("host.domain.ch").await?;
    /// let (stdout, stderr) = s.execute_cmd(&["echo", "hi"]).await?;
    /// assert_eq!(stdout, b"hi\n");
    /// assert!(stderr.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn execute_cmd(
        &self,
        args: &[impl AsRef<str> + Sync],
    ) -> Result<(Vec<u8>, Vec<u8>), SshError> {
        let cmd_str = || args.iter().map(AsRef::as_ref).join(" ");

        log::trace!("[{}] `{}`", self.name(), cmd_str());
        let mut cmd = self.raw_command(EMPTY);
        for arg in args {
            cmd.arg(arg.as_ref());
        }
        let output = match cmd.output().await {
            Ok(out) => out,
            Err(e) => {
                log::error!("[{}] {} failed: {}", self.name(), cmd_str(), e);
                Err(e)?
            }
        };

        check_output(self.name(), output, cmd_str)
    }

    /// Execute a command. Then, check that the status is successful, and that STDERR is
    /// empty. Finally, return the parsed STDOUT.
    ///
    /// The following example will execute the command `echo hi`:
    /// ```rust,no_run
    /// use cisco_lab::ssh::SshSession;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///
    /// let s = SshSession::new("host.domain.ch").await?;
    /// let stdout = s.execute_cmd_stdout(&["echo", "hi"]).await?;
    /// assert_eq!(stdout, "hi\n");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn execute_cmd_stdout(
        &self,
        args: &[impl AsRef<str> + Sync],
    ) -> Result<String, SshError> {
        let (stdout, stderr) = self.execute_cmd(args).await?;

        if !stderr.is_empty()
            && String::from_utf8_lossy(&stderr).trim() != "User Access Verification"
        {
            log::trace!(
                "[{}] {} returned non-empty stderr:{}",
                self.name(),
                args.iter().map(AsRef::as_ref).join(" "),
                format!("\nSTDERR:\n{}", String::from_utf8_lossy(&stderr))
            );
            Err(SshError::CommandError(
                self.name().to_string(),
                args.iter().map(AsRef::as_ref).join(" "),
                255,
            ))
        } else {
            Ok(String::from_utf8(stdout)?)
        }
    }

    /// Execute a command and return the status. This function will **not** check for the exit code,
    /// but simply return it.
    ///
    /// The following example will check if the file "/tmp/file" exists:
    /// ```rust,no_run
    /// use cisco_lab::ssh::SshSession;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///
    /// let s = SshSession::new("host.domain.ch").await?;
    /// if s.execute_cmd_status(&["test", "-e", "/tmp/file"]).await?.success() {
    ///     println!("File exists!");
    /// } else {
    ///     println!("File does not exist!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn execute_cmd_status(
        &self,
        args: &[impl AsRef<str> + Sync],
    ) -> Result<ExitStatus, SshError> {
        log::trace!(
            "[{}] `{}`",
            self.name(),
            args.iter().map(AsRef::as_ref).join(" ")
        );
        let mut cmd = self.raw_command(EMPTY);
        for arg in args {
            cmd.arg(arg.as_ref());
        }
        match cmd.output().await {
            Ok(out) => Ok(out.status),
            Err(e) => {
                log::error!(
                    "[{}] {} failed: {}",
                    self.name(),
                    args.iter().map(AsRef::as_ref).join(" "),
                    e
                );
                Err(e)?
            }
        }
    }

    /// Write a file to the destination using `tee`. This command will not overwrite
    ///
    /// The following example will write "test" into the file "/tmp/file".
    /// ```rust,no_run
    /// use cisco_lab::ssh::SshSession;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///
    /// let s = SshSession::new("host.domain.ch").await?;
    /// // make sure that the file does not exist yet.
    /// assert!(!s.execute_cmd_status(&["test", "-e", "/tmp/file"]).await?.success());
    /// // write the file
    /// s.write_file_tee("/tmp/file", b"test\n").await?;
    /// // make sure that the file contains the correct content
    /// assert_eq!(s.execute_cmd_stdout(&["cat", "/tmp/file"]).await?, "test\n");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn write_file_tee(
        &self,
        path: impl AsRef<str> + Send + Sync,
        content: impl AsRef<[u8]> + Send + Sync,
    ) -> Result<(), SshError> {
        log::trace!("[{}] Write file {}", self.name(), path.as_ref());
        let mut tee = self
            .command("tee")
            .arg(path.as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stderr = tee.stderr.take().unwrap();
        tee.stdin
            .take()
            .unwrap()
            .write_all(content.as_ref())
            .await?;
        let result = tee.wait().await?;
        if result.success() {
            // now, check that the file exists
            if self
                .execute_cmd_status(&["test", "-e", path.as_ref()])
                .await?
                .success()
            {
                Ok(())
            } else {
                log::error!("[{}] File {} was not written!", self.name(), path.as_ref());
                Err(SshError::CommandError(
                    self.name().to_string(),
                    format!("tee {}", path.as_ref()),
                    255,
                ))
            }
        } else {
            let mut s_error = String::new();
            stderr.read_to_string(&mut s_error).await?;
            log::error!(
                "[{}] Cannot write {}!{}",
                self.name(),
                path.as_ref(),
                if !s_error.is_empty() {
                    format!("\nSTDERR:\n{s_error}")
                } else {
                    String::new()
                }
            );
            Err(SshError::CommandError(
                self.name().to_string(),
                format!("tee {}", path.as_ref()),
                result.code().unwrap_or_default(),
            ))
        }
    }

    /// Write a file to the destination using `scp`.
    ///
    /// The following example will write "test" into the file "/tmp/file".
    /// ```rust,no_run
    /// use cisco_lab::ssh::SshSession;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///
    /// let s = SshSession::new("host.domain.ch").await?;
    /// // make sure that the file does not exist yet.
    /// assert!(!s.execute_cmd_status(&["test", "-e", "/tmp/file"]).await?.success());
    /// // write the file
    /// s.write_file("/tmp/file", b"test\n").await?;
    /// // make sure that the file contains the correct content
    /// assert_eq!(s.execute_cmd_stdout(&["cat", "/tmp/file"]).await?, "test\n");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn write_file(
        &self,
        path: impl AsRef<str> + Send + Sync,
        content: impl AsRef<[u8]> + Send + Sync,
    ) -> Result<(), SshError> {
        let tempdir = tempfile::tempdir()?;
        let mut filepath = tempdir.path().to_path_buf();
        filepath.push("scp_file");
        let mut file = tokio::fs::File::create(&filepath).await?;
        file.write_all(content.as_ref()).await?;

        // drop the file
        let _ = file;

        // copy the file
        self.scp_loc2rem(&filepath, path.as_ref()).await?;

        // remote the temporary directory
        let _ = tempdir;

        Ok(())
    }

    /// Create a raw `ssh` command with the following attributes set:
    /// - `oControlMaster=auto`
    /// - `oControlPath=/tmp/.ssh-%r@%h:%p`
    /// - `oControlPersist=30m`
    /// - `oBatchMode=yes`
    /// - `args` as given by the other arguments.
    /// - `destination` to connect to the given destination (or `none` if the path must exist).
    pub fn std_command(&self, args: &[impl AsRef<OsStr>]) -> StdCommand {
        let mut cmd = StdCommand::new("ssh");
        cmd.arg("-oControlMaster=auto")
            .arg("-oControlPath=/tmp/.ssh-%r@%h:%p")
            .arg("-oControlPersist=30m")
            .arg("-oBatchMode=yes")
            .args(args)
            .arg(self.name());
        cmd
    }

    /// Create a raw `scp` command with the following attributes set:
    /// - `oControlMaster=auto`
    /// - `oControlPath=/tmp/.ssh-%r@%h:%p`
    /// - `oControlPersist=30m`
    /// - `oBatchMode=yes`
    fn scp_cmd(&self) -> StdCommand {
        let mut cmd = StdCommand::new("scp");
        cmd.arg("-oControlMaster=auto")
            .arg("-oControlPath=/tmp/.ssh-%r@%h:%p")
            .arg("-oControlPersist=30m")
            .arg("-oBatchMode=yes");
        cmd
    }
}
/// Wait on the stdout until we get the next prompt. If the timeout triggers before we read the
/// prompt, return `None`.
pub(crate) async fn wait_prompt(
    stdout: &mut ChildStdout,
    duration: Duration,
    prompt: impl AsRef<[u8]>,
) -> Result<Vec<u8>, std::io::Error> {
    timeout(duration, wait_prompt_no_timeout(stdout, prompt))
        .await
        .map_err(|_| {
            log::warn!("Timeout occurred!");
            std::io::Error::new(
                ErrorKind::TimedOut,
                "Timeout occurred while waiting for a prompt!",
            )
        })?
}

/// Wait on the stdout until we get the next prompt.
pub(crate) async fn wait_prompt_no_timeout(
    stdout: &mut ChildStdout,
    prompt: impl AsRef<[u8]>,
) -> Result<Vec<u8>, std::io::Error> {
    let mut buffer = Vec::new();
    let mut counter_zero = 0;
    let prompt = prompt.as_ref();
    while !buffer.ends_with(prompt) {
        let num = stdout.read_buf(&mut buffer).await?;
        if num == 0 {
            counter_zero += 1;
            if counter_zero >= 10 {
                return Err(std::io::Error::new(
                    ErrorKind::ConnectionRefused,
                    "Connection refused while expecting a prompt!",
                ));
            }
        }
    }

    // remove the prompt from the buffer
    buffer.truncate(buffer.len() - prompt.len());

    let last_newline_pos = buffer
        .iter()
        .enumerate()
        .rev()
        .find(|(_, c)| **c == b'\n')
        .map(|(p, _)| p)
        .unwrap_or(0);

    buffer.truncate(last_newline_pos);
    Ok(buffer)
}

/// Check the output for successful exit code
pub fn check_output<F, S>(
    host: &str,
    output: Output,
    cmd: F,
) -> Result<(Vec<u8>, Vec<u8>), SshError>
where
    F: FnOnce() -> S,
    S: std::fmt::Display,
{
    if output.status.success() {
        Ok((output.stdout, output.stderr))
    } else {
        let cmd = cmd().to_string();
        log::error!(
            "[{}] {} exited with exit code {}{}{}",
            host,
            cmd,
            output.status.code().unwrap_or_default(),
            if !output.stdout.is_empty() {
                format!("\nSTDOUT:\n{}", String::from_utf8_lossy(&output.stdout))
            } else {
                String::new()
            },
            if !output.stderr.is_empty() {
                format!("\nSTDERR:\n{}", String::from_utf8_lossy(&output.stderr))
            } else {
                String::new()
            }
        );
        Err(SshError::CommandError(
            host.to_string(),
            cmd,
            output.status.code().unwrap_or_default(),
        ))
    }
}

/// Error kind returned by [`SshSession`].
#[derive(Debug, Error)]
pub enum SshError {
    /// Error while establishing the main connection
    #[error("Error while establishing the connection: {0}")]
    Setup(std::io::Error),
    /// Timeout while establishing the session
    #[error("Timeout while establishing the session.")]
    Timeout,
    /// Error while interacting with the main connection
    #[error("SSH Client error: {0}")]
    Client(#[from] std::io::Error),
    /// Error while executing a command.
    #[error("Non-zero exit code of command {1} on {0}: {2}")]
    CommandError(String, String, i32),
    /// Cannot parse output as utf8
    #[error("Cannot parse output as UTF-8: {0}")]
    FromUtf8(#[from] FromUtf8Error),
}

impl SshError {
    /// Return the status code if the error was a [`SshError::CommandError`]. Otherwise, return `None`.
    pub fn status(&self) -> Option<i32> {
        if let SshError::CommandError(_, _, status) = self {
            Some(*status)
        } else {
            None
        }
    }
}
