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

//! Module for generating a pcap-file containing all ping packets, replaying that ping packet, and
//! capturing all these ping packets on the server.

use std::{collections::HashMap, net::Ipv4Addr, process::Stdio};

use hex::FromHex;
use serde::Serialize;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader, Lines},
    process::{Child, ChildStdout},
    select,
    sync::oneshot,
    task::JoinHandle,
};

use crate::{
    config::CONFIG,
    ssh::{SshError, SshSession},
};

/// This structure measures the current network reachability state.
///
/// # Idea
///
/// Ths basic idea is to send IP packets from the server to the tofino, one for each router. These
/// IP packets have a destination MAC address, such that the router will forward them to any port
/// of that VDC, such that the packet is then routed by that VDC. The ping packets have as
/// destination IP an IP of a prefix, for which routers think they can reach it over the exabgp
/// interface on the server. Therefore, we capture all ICMP packets on that interface on the server,
/// and look at the following four fields:
///
/// - **Timestamp**: to generate a time series
/// - **Source MAC address**: The source MAC address is set by the last VDC. The MAC address
///   corresponds to the link between an internal router and an external one. This MAC address
///   therefore can be mapped to a specific link in `bgpsim` that leaves the internal network, and
///   we can know where packets left the network.
/// - **Source IP Address**: We set the source IP address to be in the range of the router's local
///   network. This allows us to figure out which was the "source" router that routed the ping
///   packets first.
/// - **Destination IP Address**: This field corresponds to the destination prefix.
///
/// # Implementation
///
/// On initialization, we create a pcap file containing exactly one ping for each packet. Then, on
/// [`TrafficCaptureHandle::start`], we start two processes on the server. The first is `prober`
/// that sends all prepared IP packets at a requested frequency, including a payload containing an
/// incrementing counter. The second is `collector` that captures all packets on the server, filters
/// out all packets that were sent by the prober and writes all the interesting fields (timestamp,
/// source mac address, source IP address, destination IP address, and the counter) as a
/// comma-separated line.
///
/// Once the processes have been started, we also start a `tokio` task that reads from `stdout` of
/// `collector` and parses packets concurrently (and in parallel). This task as an associated kill
/// `oneshot` channel, that we can use to kill the thread. Once killed, this task will return the
/// samples, as well as the `stdout`, so we can resume reading later.
///
/// The function [`TrafficCaptureHandle::get_samples`] or [`TrafficCaptureHandle::take_samples`]
/// will stop the reader task for a short time, update local samples, and start it again. This way,
/// we can always get the current samples, while still parsing them concurrently.
pub struct TrafficCaptureHandle {
    /// SSH session to use
    session: SshSession,
    /// The according prober's config
    prober_config: ProberConfig,
    /// Child process for `prober` (if still running)
    prober_child: Option<Child>,
    /// Thread that reads the collector output concurrently
    #[allow(clippy::type_complexity)]
    prober_reader: Option<
        JoinHandle<
            Result<
                (
                    HashMap<(Ipv4Addr, Ipv4Addr, u64), f64>,
                    Lines<BufReader<ChildStdout>>,
                ),
                TrafficCaptureError,
            >,
        >,
    >,
    /// Buffered reader for the stdout of the prober.
    prober_stdout: Option<Lines<BufReader<ChildStdout>>>,
    /// kill channel for the prober reader.
    prober_reader_kill: Option<oneshot::Sender<()>>,
    /// All sent packets by the prober
    prober_samples: HashMap<(Ipv4Addr, Ipv4Addr, u64), f64>,
    /// Child process for `collector` (if still running)
    collector_child: Option<Child>,
    /// Thread that reads the collector output concurrently
    #[allow(clippy::type_complexity)]
    collector_reader: Option<
        JoinHandle<
            Result<(Vec<CollectorSample>, Lines<BufReader<ChildStdout>>), TrafficCaptureError>,
        >,
    >,
    /// Buffered reader for the stdout of collector.
    collector_stdout: Option<Lines<BufReader<ChildStdout>>>,
    /// kill channel for the collector reader.
    collector_reader_kill: Option<oneshot::Sender<()>>,
    /// Vector of all received packets.
    collector_samples: Vec<CollectorSample>,
    /// Vector of all processed samples.
    samples: Vec<CaptureSample>,
}

impl TrafficCaptureHandle {
    /// Create a new `TrafficCaptureHandle` which will create pings for each source in `sources`,
    /// and target prefix in `targets`.
    pub async fn new(
        handle: SshSession,
        flows: &[TrafficFlow],
    ) -> Result<Self, TrafficCaptureError> {
        // create the config file
        let prober_config = ProberConfig {
            iface: CONFIG.server.prober_iface.to_string(),
            freq: 1_000_000,
            flows: flows.to_vec(),
        };

        // write the pcap file to the server
        handle
            .write_file(
                &CONFIG.server.prober_config_filename,
                toml::to_string(&prober_config).unwrap(),
            )
            .await?;

        Ok(Self {
            session: handle,
            prober_config,
            prober_child: None,
            prober_reader: None,
            prober_stdout: None,
            prober_reader_kill: None,
            prober_samples: HashMap::new(),
            collector_child: None,
            collector_reader: None,
            collector_stdout: None,
            collector_reader_kill: None,
            collector_samples: Vec::new(),
            samples: Vec::new(),
        })
    }

    /// Start both the tcpreplay and tcpdump process. If the capture is already running, this
    /// function will do nothing.
    ///
    /// The `frequency` specifies the number of ping packets per flow per second that should be
    /// sent. This will set the `--pps` options in tcpreplay appropriately. For instance, if there
    /// are 10 flows, and `frequency` is set to 10, then `--pps=100` will be set.
    pub async fn start(&mut self, frequency: u64) -> Result<(), TrafficCaptureError> {
        if self.collector_child.is_some()
            || self.collector_reader.is_some()
            || self.collector_reader_kill.is_some()
            || self.collector_stdout.is_some()
            || self.prober_child.is_some()
        {
            return Ok(());
        }

        let cmd = format!("sudo collector -b 256 {}", &CONFIG.server.exabgp_iface);

        log::trace!("[{}] {}", self.session.name(), cmd);
        let mut collector_child = self
            .session
            .raw_command(&["-tt"])
            .arg(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let collector_stdout = BufReader::new(collector_child.stdout.take().unwrap()).lines();

        // compute packets per second
        let cmd = format!(
            "sudo prober -f {} {}",
            1_000_000 / frequency,
            &CONFIG.server.prober_config_filename
        );

        log::trace!("[{}] {}", self.session.name(), cmd);
        let mut prober_child = self
            .session
            .raw_command(&["-tt"])
            .arg(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let prober_stdout = BufReader::new(prober_child.stdout.take().unwrap()).lines();

        self.prober_child = Some(prober_child);
        self.prober_stdout = Some(prober_stdout);
        self.prober_reader = None;
        self.prober_reader_kill = None;
        self.collector_child = Some(collector_child);
        self.collector_stdout = Some(collector_stdout);
        self.collector_reader = None;
        self.collector_reader_kill = None;

        self.spawn_prober_reader();
        self.spawn_collector_reader();

        Ok(())
    }

    /// Stop the capture. If it is not yet started, this function will do nothing.
    pub async fn stop(&mut self) -> Result<(), TrafficCaptureError> {
        log::debug!("[server] stop packet capture!");
        // send the kill command to both collector and to tcpreplay
        if let Some(mut child) = self.prober_child.take() {
            child.kill().await.map_err(SshError::Client)?;
            if log::log_enabled!(log::Level::Trace) {
                let mut stderr = String::new();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .await?;
                log::trace!(
                    "[{}] Killed prober.{}",
                    self.session.name(),
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!("\nSTDERR:\n{stderr}")
                    }
                );
            }
        }
        if let Some(mut child) = self.collector_child.take() {
            child.kill().await.map_err(SshError::Client)?;
            if log::log_enabled!(log::Level::Trace) {
                let mut stderr = String::new();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .await?;
                log::trace!(
                    "[{}] Killed collector.{}",
                    self.session.name(),
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!("\nSTDERR:\n{stderr}")
                    }
                );
            }
        }

        // join the prober and collector reader
        self.join_prober_reader().await?;
        self.join_collector_reader().await?;

        log::trace!(
            "[{}] Received {} samples from the prober",
            self.session.name(),
            self.prober_samples.len()
        );
        log::trace!(
            "[{}] Received {} samples from the collector",
            self.session.name(),
            self.collector_samples.len()
        );

        Ok(())
    }

    /// Get a reference to all samples stored in the traffic capture. To also get the prober
    /// timestamp, use `take_samples` instead.
    pub async fn get_samples(&mut self) -> Result<&[CaptureSample], TrafficCaptureError> {
        // join the current readers.
        self.join_prober_reader().await?;
        self.join_collector_reader().await?;
        // spawn a new collector reader (if necessary)
        self.spawn_prober_reader();
        self.spawn_collector_reader();
        // process samples
        self.update_samples();
        Ok(&self.samples)
    }

    /// Get all samples stored in the traffic capture, removing them from the caputre.
    pub async fn take_samples(&mut self) -> Result<Vec<CaptureSample>, TrafficCaptureError> {
        // join the current collector reader (if it exists).
        self.join_prober_reader().await?;
        self.join_collector_reader().await?;
        // spawn a new collector reader (if necessary)
        self.spawn_prober_reader();
        self.spawn_collector_reader();
        // process samples
        self.update_samples();
        Ok(std::mem::take(&mut self.samples))
    }

    /// Take all samples from the collector, lookup their sending time, and store them in
    /// `self.samples`.
    fn update_samples(&mut self) {
        let collector_samples = std::mem::take(&mut self.collector_samples);
        self.samples
            .extend(collector_samples.into_iter().filter_map(|s| {
                Some(CaptureSample {
                    time: s.time,
                    send_time: *self.prober_samples.get(&(s.src_ip, s.dst_ip, s.counter))?,
                    mac: s.mac,
                    src_ip: s.src_ip,
                    dst_ip: s.dst_ip,
                    counter: s.counter,
                })
            }));
    }

    /// Spawn a prober reader that asynchronously reads all samples and puts them into a vector. It
    /// will also create the kill channel. If the reader is killed, then it will return both the
    /// read stdout, but also the buf reader
    ///
    /// If `self.collector_stdout` is empty (which means that collector is not running), this function
    /// does nothing.
    fn spawn_prober_reader(&mut self) {
        // get the standardout
        if let Some(mut reader) = self.prober_stdout.take() {
            // create a new channel
            let (kill_tx, mut kill_rx) = oneshot::channel();
            self.prober_reader_kill = Some(kill_tx);

            let prober_reader = tokio::task::spawn(async move {
                let mut result = HashMap::new();
                'reader_loop: loop {
                    select! {
                        biased;
                        _ = (&mut kill_rx) => break 'reader_loop Ok((result, reader)),
                        x = reader.next_line() => match x {
                            Err(e) => break 'reader_loop Err(TrafficCaptureError::Io(e)),
                            Ok(None) => break 'reader_loop Ok((result, reader)),
                            Ok(Some(l)) => if let Some(sample) = ProberSample::from_line(l.trim()) {
                                result.insert((sample.src_ip, sample.dst_ip, sample.counter), sample.time);
                            } else {
                                log::trace!("Cannot parse line: {l}");
                            }
                        },
                    }
                }
            });

            self.prober_reader = Some(prober_reader);
        }
    }

    /// Spawn a collector reader that asynchronously reads all samples and puts them into a vector. It
    /// will also create the kill channel. If the reader is killed, then it will return both the
    /// read stdout, but also the buf reader
    ///
    /// If `self.collector_stdout` is empty (which means that collector is not running), this function
    /// does nothing.
    fn spawn_collector_reader(&mut self) {
        // get the standardout
        if let Some(mut reader) = self.collector_stdout.take() {
            // create a new channel
            let (kill_tx, mut kill_rx) = oneshot::channel();
            self.collector_reader_kill = Some(kill_tx);

            let collector_reader = tokio::task::spawn(async move {
                let mut result = Vec::new();
                'reader_loop: loop {
                    select! {
                        biased;
                        _ = (&mut kill_rx) => break 'reader_loop Ok((result, reader)),
                        x = reader.next_line() => match x {
                            Err(e) => break 'reader_loop Err(TrafficCaptureError::Io(e)),
                            Ok(None) => break 'reader_loop Ok((result, reader)),
                            Ok(Some(l)) => if let Some(sample) = CollectorSample::from_line(l) {
                                result.push(sample)
                            }
                        },
                    }
                }
            });

            self.collector_reader = Some(collector_reader);
        }
    }

    /// join the prober reader, and update all samples
    ///
    /// If the prober reader is empty, this function does nothing.
    async fn join_prober_reader(&mut self) -> Result<(), TrafficCaptureError> {
        // only continue if the reader is something, and the kill channel is something
        if let Some(kill_tx) = self.prober_reader_kill.take() {
            if let Some(reader_job) = self.prober_reader.take() {
                // send the kill signal
                let _ = kill_tx.send(());
                // wait for the thread to finish
                let (samples, prober_stdout) = reader_job.await??;
                self.prober_samples.extend(samples);
                self.prober_stdout = Some(prober_stdout);
            }
        }
        Ok(())
    }

    /// join the collector reader, and update all samples
    ///
    /// If the collector reader is empty, this function does nothing.
    async fn join_collector_reader(&mut self) -> Result<(), TrafficCaptureError> {
        // only continue if the reader is something, and the kill channel is something
        if let Some(kill_tx) = self.collector_reader_kill.take() {
            if let Some(reader_job) = self.collector_reader.take() {
                // send the kill signal
                let _ = kill_tx.send(());
                // wait for the thread to finish
                let (samples, collector_stdout) = reader_job.await??;
                self.collector_samples.extend(samples);
                self.collector_stdout = Some(collector_stdout);
            }
        }
        Ok(())
    }

    /// Return the prober config used for this capture.
    pub fn get_prober_config(&self) -> &ProberConfig {
        &self.prober_config
    }
}

/// Describing a single traffic flow to monitor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct TrafficFlow {
    /// MAC Address of a port of the source router
    pub src_mac: [u8; 6],
    /// Source IP address for the ping packet
    pub src_ip: Ipv4Addr,
    /// Destination IP Address for the ping packet
    pub dst_ip: Ipv4Addr,
}

/// Configuration file for the prober (use `toml` to deserialize it).
#[derive(Debug, Clone, Serialize)]
pub struct ProberConfig {
    /// Interface name to delay packets on.
    pub iface: String,
    /// number of microseconds between two packets of each flow
    pub freq: u64,
    /// The flows to generate traffic for
    pub flows: Vec<TrafficFlow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaptureSample {
    /// Timestamp when the message was received by the collector.
    pub time: f64,
    /// Timestamp when the message was sent by the prober
    pub send_time: f64,
    /// ethernet MAC source address, which can be used to determine which router has sent the
    /// message, and to which external router it is destined.
    pub mac: [u8; 6],
    /// Source IP address, which encodes the router that first routed the ping packet.
    pub src_ip: Ipv4Addr,
    /// Destination IP address, which encodes the destination prefix
    pub dst_ip: Ipv4Addr,
    /// The counter index
    pub counter: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectorSample {
    /// Time relative to the first arrived packet
    pub time: f64,
    /// ethernet MAC source address, which can be used to determine which router has sent the
    /// message, and to which external router it is destined.
    pub mac: [u8; 6],
    /// Source IP address, which encodes the router that first routed the ping packet.
    pub src_ip: Ipv4Addr,
    /// Destination IP address, which encodes the destination prefix
    pub dst_ip: Ipv4Addr,
    /// The counter index
    pub counter: u64,
}

impl CollectorSample {
    /// Try to read a single line and generate a sample. The data must be comma-separated, and in
    /// the order: `{time},{mac},{src_ip},{dst_ip},{idx}`.
    pub(crate) fn from_line(line: impl AsRef<str>) -> Option<Self> {
        let line = line.as_ref();
        let mut iter = line.trim().split(',').peekable();
        let time: f64 = iter.next()?.parse().ok()?;
        let mac: [u8; 6] = FromHex::from_hex(iter.next()?.split(':').collect::<String>()).ok()?;
        let src_ip = iter.next()?.parse().ok()?;
        let dst_ip = iter.next()?.parse().ok()?;
        let counter = iter.next()?.parse().ok()?;
        if iter.next().is_some() {
            return None;
        }
        Some(Self {
            time,
            mac,
            src_ip,
            dst_ip,
            counter,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Packet sent by the prober.
pub struct ProberSample {
    /// Time relative to the first arrived packet
    pub time: f64,
    /// Source IP address, which encodes the router that first routed the ping packet.
    pub src_ip: Ipv4Addr,
    /// Destination IP address, which encodes the destination prefix
    pub dst_ip: Ipv4Addr,
    /// The counter index
    pub counter: u64,
}

impl ProberSample {
    /// Try to read a single line and generate a sample. The data must be comma-separated, and in
    /// the order: `{time},{src_ip},{dst_ip},{idx}`.
    pub(crate) fn from_line(line: impl AsRef<str>) -> Option<Self> {
        let line = line.as_ref();
        let mut iter = line.trim().split(',').peekable();
        let time: f64 = iter.next()?.parse().ok()?;
        let src_ip = iter.next()?.parse().ok()?;
        let dst_ip = iter.next()?.parse().ok()?;
        let counter = iter.next()?.parse().ok()?;
        if iter.next().is_some() {
            return None;
        }
        Some(Self {
            time,
            src_ip,
            dst_ip,
            counter,
        })
    }
}

/// Errors thrown by the traffic capture
#[derive(Debug, Error)]
pub enum TrafficCaptureError {
    /// I/O Error
    #[error("I/O Error: {0}")]
    Io(#[from] std::io::Error),
    /// Ssh Session error
    #[error("{0}")]
    Ssh(#[from] SshError),
    /// Cannot join a parallel job
    #[error("Cannot join thread: {0}")]
    Join(#[from] tokio::task::JoinError),
}
