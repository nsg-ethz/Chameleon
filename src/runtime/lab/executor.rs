// Chameleon: Taming the transient while reconfiguring BGP
// Copyright (C) 2023 Tibor Schneider <sctibor@ethz.ch>
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

//! Parallel executor for the GNS3 model

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    net::Ipv4Addr,
    ops::DerefMut,
    sync::Arc,
    time::Duration,
};

use atomic_command::{AtomicCommand, AtomicCondition};
use bgpsim::{
    bgp::BgpRibEntry,
    config::ConfigModifier,
    export::{Addressor, DefaultAddressor, ExportError, InternalCfgGen, MaybePec},
    prelude::*,
};
use cisco_lab::{
    router::{BgpPathType, BgpRoute, CiscoSession, CiscoShell, CiscoShellError},
    Active, CiscoLab, CiscoLabError,
};
use ipnet::Ipv4Net;
use itertools::Itertools;
use lazy_static::lazy_static;
use log::info;
use rand::prelude::*;
#[cfg(feature = "serde")]
use serde::Serialize;
use time::OffsetDateTime;
use tokio::{
    select,
    sync::{
        broadcast::{self, error::RecvError},
        Mutex,
    },
    task::{spawn, JoinHandle},
    time::{sleep_until, Instant},
};

use crate::{
    runtime::controller::{Controller, ControllerStage, StateItem},
    P,
};

use super::LabError;

/// The interval by which to check for pre- or postconditions.
const CHECK_INTERVAL: Duration = Duration::from_millis(500);
/// Two minutes timeout, until we say we cannot progress.
const TIMEOUT: Duration = Duration::from_secs(60);
/// Number of networks to prove when checking for a condition on  a prefix equivalence class.
///
/// This module will always check the first and last network (in alphabetical order). Further, it
/// will check `PEC_NUM_CHECK - 2` random networks.
const PEC_NUM_CHECK: usize = 10;

lazy_static! {
    static ref EVENT_LOG: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
}

/// Event log entry
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Event {
    /// Job ID
    pub id: JobId,
    /// Time when the event occurred,
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub time: OffsetDateTime,
    /// Duration since the beginning
    pub elapsed_secs: f64,
    /// Command for which the event occurred,
    pub command: AtomicCommand<P>,
    /// Configuration command that is sent
    pub config: String,
    /// Target device where to send the command
    pub router: String,
    /// Kind of event.
    pub event: EventKind,
}

/// Event kind to create the log.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub enum EventKind {
    /// The command is scheduled.
    Scheduled,
    /// The precondition is satisfied,
    PreconditionSatisfied,
    /// The postcondition is satisfied.
    PostConditionSatisfied,
}

/// Kill channel type, to send kill commands or receive a kill command.
struct KillChannel {
    /// Sender of the kill command.
    tx: broadcast::Sender<()>,
    /// Receiver of the kill command
    rx: broadcast::Receiver<()>,
}

impl Clone for KillChannel {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            rx: self.rx.resubscribe(),
        }
    }
}

impl KillChannel {
    /// Create a new, empty kill channel.
    pub fn new(cap: usize) -> Self {
        let (tx, rx) = broadcast::channel::<()>(cap);
        Self { tx, rx }
    }

    /// Send the kill command to all other threads.
    pub fn send(&self) -> Result<(), RecvError> {
        self.tx.send(()).map_err(|_| RecvError::Closed).map(|_| ())
    }

    /// Wait for the kill command. This function is cancellable.
    pub async fn recv(&mut self) -> Result<(), RecvError> {
        self.rx.recv().await
    }
}

impl Controller {
    /// Perform the complete migration (all stages) in parallel using the parallel executor.
    pub async fn execute_lab<'a, 'n: 'a, Q>(
        self,
        lab: &'a mut CiscoLab<'n, P, Q, Active>,
        net: &Network<P, Q>,
    ) -> Result<Vec<Event>, LabError> {
        // clear the event log.
        EVENT_LOG.lock().await.clear();

        let stages = self.into_remaining_states();

        let num_routers = net.get_routers().len();
        let queue_size = stages.iter().map(|s| s.count_commands()).sum();
        let mut idx = 0;

        let c_kill = KillChannel::new(num_routers);
        let (c_jobs_tx, c_jobs_rx) = broadcast::channel(queue_size);
        let (c_done_tx, c_done_rx) = broadcast::channel(queue_size);

        // prepare the pec addresses
        let mut rng = thread_rng();
        let pec_addresses = lab
            .addressor()
            .get_pecs()
            .iter()
            .map(|(p, vs)| {
                (
                    *p,
                    MaybePec::Pec((*p).into(), vs.clone())
                        .sample_random_n(&mut rng, PEC_NUM_CHECK)
                        .into_iter()
                        .copied()
                        .collect(),
                )
            })
            .collect();

        // start all runners
        info!("Connecting to all routers...");
        let runners = start_runners(
            net,
            lab,
            c_jobs_rx.resubscribe(),
            c_done_tx.clone(),
            c_kill.clone(),
        )?;

        for stage in stages {
            info!("Executing stage {} in parallel...", stage.name());
            match stage {
                ControllerStage::Setup(s)
                | ControllerStage::Main(s)
                | ControllerStage::Cleanup(s) => {
                    execute_stage(
                        net,
                        lab,
                        s,
                        None,
                        &pec_addresses,
                        &mut idx,
                        c_jobs_tx.clone(),
                        c_done_rx.resubscribe(),
                        c_kill.clone(),
                    )?
                    .await
                    .map_err(|e| LabErrorToKill(LabError::ThreadError(e), c_kill.tx.clone()))??;
                }
                ControllerStage::UpdateBefore(s) | ControllerStage::UpdateAfter(s) => {
                    execute_prefix_stage(
                        net,
                        lab,
                        s,
                        &pec_addresses,
                        &mut idx,
                        c_jobs_tx.clone(),
                        c_done_rx.resubscribe(),
                        c_kill.clone(),
                    )
                    .await?;
                }
                ControllerStage::Finished => {}
            }
        }
        info!("Migration complete!");

        // send the kill command
        let _ = c_kill.send();
        // await all runners
        let mut result = Ok(std::mem::take(EVENT_LOG.lock().await.deref_mut()));
        for runner in runners {
            match runner.await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => result = Err(e),
                Err(e) => result = Err(LabError::ThreadError(e)),
            }
        }
        result
    }
}

/// Start all shells and return a vector of join handles.
fn start_runners<Q>(
    net: &Network<P, Q>,
    lab: &CiscoLab<'_, P, Q, Active>,
    c_jobs: broadcast::Receiver<Job>,
    c_done: broadcast::Sender<JobId>,
    c_kill: KillChannel,
) -> Result<Vec<JoinHandle<Result<(), LabError>>>, LabErrorToKill> {
    let mut jobs = Vec::new();
    for r in net.get_routers() {
        let handle = lab.get_router_session(r).map_err(|e| (e, &c_kill))?;
        let c_jobs = c_jobs.resubscribe();
        let c_done = c_done.clone();
        let c_kill = c_kill.clone();
        jobs.push(spawn(async move {
            runner(handle, r, c_jobs, c_done, c_kill).await
        }));
    }

    Ok(jobs)
}

/// Execute a stage that is parallelized per prefix.
#[allow(clippy::too_many_arguments)]
async fn execute_prefix_stage<'a, 'n: 'a, Q>(
    net: &Network<P, Q>,
    lab: &'a mut CiscoLab<'n, P, Q, Active>,
    stage: HashMap<P, StateItem>,
    pec_addresses: &HashMap<P, Vec<Ipv4Net>>,
    idx: &mut usize,
    c_jobs: broadcast::Sender<Job>,
    c_done: broadcast::Receiver<JobId>,
    c_kill: KillChannel,
) -> Result<(), LabError> {
    let mut jobs = Vec::new();
    for (p, stage) in stage {
        jobs.push(execute_stage(
            net,
            lab,
            stage,
            Some(p),
            pec_addresses,
            idx,
            c_jobs.clone(),
            c_done.resubscribe(),
            c_kill.clone(),
        )?);
    }
    for job in jobs {
        job.await
            .map_err(|e| LabErrorToKill(LabError::ThreadError(e), c_kill.tx.clone()))??;
    }

    Ok(())
}

/// Create a single task that executes the entire stage.
#[allow(clippy::too_many_arguments)]
fn execute_stage<'a, 'n: 'a, Q>(
    net: &Network<P, Q>,
    lab: &'a mut CiscoLab<'n, P, Q, Active>,
    stage: StateItem,
    prefix: Option<P>,
    pec_addresses: &HashMap<P, Vec<Ipv4Net>>,
    idx: &mut usize,
    c_jobs: broadcast::Sender<Job>,
    mut c_done: broadcast::Receiver<JobId>,
    mut c_kill: KillChannel,
) -> Result<JoinHandle<Result<(), LabError>>, LabErrorToKill> {
    let mut steps_jobs = Vec::new();
    // iterate over all steps in the stage
    for step in stage.commands.iter() {
        let mut jobs = Vec::new();
        // iterate ovewr all commands of that step
        for cmd in step {
            // iterate over all routers for that command
            for r in cmd.command.routers() {
                if net.get_device(r).is_external() {
                    continue;
                }
                *idx += 1;

                // get the generator and addressor to create the command.
                let (gen, addressor) = lab.get_router_cfg_gen(r).map_err(|e| (e, &c_kill))?;

                jobs.push(Job {
                    id: (r, prefix, *idx),
                    cmd: Vec::<ConfigModifier<P>>::from(cmd.command.clone())
                        .into_iter()
                        .filter(|c| c.routers().contains(&r))
                        .map(|c| gen.generate_command(net, addressor, c))
                        .collect::<Result<_, _>>()
                        .map_err(|e| (e, &c_kill))?,
                    cmd_repr: cmd.command.fmt(net),
                    pre: LabCondition::translate(
                        &cmd.precondition,
                        r,
                        net,
                        addressor,
                        pec_addresses,
                    )
                    .map_err(|e| (e, &c_kill))?,
                    post: LabCondition::translate(
                        &cmd.postcondition,
                        r,
                        net,
                        addressor,
                        pec_addresses,
                    )
                    .map_err(|e| (e, &c_kill))?,
                    state: JobState::Pre,
                    command: cmd.clone(),
                });
            }
        }
        steps_jobs.push(jobs);
    }

    // now, create a task to execute the stage
    Ok(spawn(async move {
        for (i, jobs) in steps_jobs.into_iter().enumerate() {
            info!(
                "Executing step {}{}",
                i,
                prefix.map(|p| format!(" for {p}")).unwrap_or_default()
            );
            execute_jobs(jobs, &c_jobs, &mut c_done, &mut c_kill).await?;
        }
        Ok(())
    }))
}

/// Execute a set of jobs concurrently.
async fn execute_jobs(
    jobs: Vec<Job>,
    c_jobs: &broadcast::Sender<Job>,
    c_done: &mut broadcast::Receiver<JobId>,
    c_kill: &mut KillChannel,
) -> Result<(), LabErrorToKill> {
    // spawn all threads and wait for all of them to complete.
    let mut ids = HashSet::new();

    // spawn all jobs
    for job in jobs {
        ids.insert(job.id);
        c_jobs
            .send(job)
            .map_err(|_| (RecvError::Closed, c_kill.tx.clone()))?;
    }

    // receive all signals and wait until we have them all
    let deadline = Instant::now() + TIMEOUT;

    while !ids.is_empty() {
        // wait until we get something from either c_done or c_kill
        select! {
            biased;
            _ = c_kill.recv() => {
                return Ok(())
            }
            r = c_done.recv() => {
                let id = r.map_err(|e| (e, c_kill.tx.clone()))?;
                ids.remove(&id);
            }
            _ = sleep_until(deadline) => {
                // send the kill command
                log::warn!("Timeout occurred!");
                return Err((LabError::CannotProgress, c_kill).into())
            }
        }
    }

    Ok(())
}

/// Job runner on a single router.
async fn runner(
    session: CiscoSession,
    router: RouterId,
    c_jobs: broadcast::Receiver<Job>,
    c_done: broadcast::Sender<JobId>,
    c_kill: KillChannel,
) -> Result<(), LabError> {
    Ok(_runner(session, router, c_jobs, c_done, c_kill).await?)
}

/// Job runner on a single router, where each error must be unwrapped to send the kill command.
async fn _runner(
    session: CiscoSession,
    router: RouterId,
    mut c_jobs: broadcast::Receiver<Job>,
    c_done: broadcast::Sender<JobId>,
    mut c_kill: KillChannel,
) -> Result<(), LabErrorToKill> {
    let mut shell = session.shell().await.map_err(|e| (e, &c_kill))?;
    let mut running_jobs: Vec<Job> = Vec::new();

    let mut deadline = Instant::now() + CHECK_INTERVAL;

    /// Process all jobs. This means getting the current set of routes, processing all jobs,
    /// removing those that are finished, and sending the ID of finished jobs back over the channel.
    async fn process_jobs(
        shell: &mut CiscoShell,
        jobs: &mut Vec<Job>,
        c_done: &broadcast::Sender<JobId>,
        c_kill: &KillChannel,
    ) -> Result<(), LabErrorToKill> {
        // early exit if jobs is empty
        if jobs.is_empty() {
            return Ok(());
        }

        // create the cache for all routes
        let mut cache = HashMap::new();

        // process all jobs
        let mut to_del = Vec::new();
        for (i, j) in jobs.iter_mut().enumerate() {
            // process the job and check if it is finished.
            if j.process(shell, &mut cache)
                .await
                .map_err(|e| (e, c_kill))?
            {
                // if so, send a message and remove it from the current jobs list.
                c_done
                    .send(j.id)
                    .map_err(|_| (broadcast::error::RecvError::Closed, c_kill))?;
                to_del.push(i);
            }
        }
        // update the jobs list
        while let Some(i) = to_del.pop() {
            jobs.remove(i);
        }
        Ok(())
    }

    loop {
        select! {
            biased;
            _ = c_kill.recv() => {
                break Ok(())
            }
            r = c_jobs.recv() => {
                let j = r.map_err(|e| (e, &c_kill))?;
                if j.id.0 == router {
                    j.log_sched(shell.name()).await;
                    // push the job
                    running_jobs.push(j);
                    // process all jobs
                    process_jobs(&mut shell, &mut running_jobs, &c_done, &c_kill).await?;
                    // update the deadline
                    deadline = Instant::now() + CHECK_INTERVAL;
                }
            }
            _ = sleep_until(deadline) => {
                // process all jobs
                process_jobs(&mut shell, &mut running_jobs, &c_done, &c_kill).await?;
                // update the deadline
                deadline = Instant::now() + CHECK_INTERVAL;
            }
        }
    }
}

/// Job Identification
type JobId = (RouterId, Option<P>, usize);

/// Arguments to the job
#[derive(Clone, Debug)]
struct Job {
    /// The job identification
    id: JobId,
    /// Command to apply (as a string)
    cmd: String,
    /// Representation of the command as a string, for logging
    cmd_repr: String,
    /// Precondition before applying the command
    pre: LabCondition,
    /// Postcondition after applying the command.
    post: LabCondition,
    /// State of the job.
    state: JobState,
    /// The original command
    command: AtomicCommand<P>,
}

impl Job {
    /// Process the job. The function returns if the job is complete.
    async fn process(
        &mut self,
        shell: &mut CiscoShell,
        cache: &mut HashMap<Ipv4Net, Vec<BgpRoute>>,
    ) -> Result<bool, LabError> {
        // check precondition
        if self.state == JobState::Pre
            && self
                .pre
                .check(shell, cache)
                .await
                .map_err(CiscoLabError::CiscoShell)?
        {
            self.log_precond(shell.name()).await;
            self.state = JobState::Post;
            shell
                .configure(&self.cmd)
                .await
                .map_err(CiscoLabError::CiscoShell)?;
        } else {
            log::trace!("[{}] Waiting for precondition {}", shell.name(), self.pre);
        }
        // check postcondition
        if self.state == JobState::Post
            && self
                .post
                .check(shell, cache)
                .await
                .map_err(CiscoLabError::CiscoShell)?
        {
            self.log_postcond(shell.name()).await;
            self.state = JobState::Done;
        } else {
            log::trace!("[{}] Waiting for postcondition {}", shell.name(), self.post);
        }

        Ok(self.state == JobState::Done)
    }
}

/// logging helpe rfunctions
impl Job {
    /// Create a log entry
    async fn log(&self, event: EventKind, name: &str) {
        let time = OffsetDateTime::now_local()
            .ok()
            .unwrap_or_else(OffsetDateTime::now_utc);
        let mut logs = EVENT_LOG.lock().await;
        let elapsed_secs = logs
            .first()
            .map(|l| (time - l.time).as_seconds_f64())
            .unwrap_or_default();
        let event = Event {
            id: self.id,
            time,
            elapsed_secs,
            command: self.command.clone(),
            config: self.cmd.clone(),
            router: name.to_string(),
            event,
        };
        logs.push(event);
    }

    /// Log a command to be scheduled.
    async fn log_sched(&self, name: impl AsRef<str>) {
        let name = name.as_ref();
        log::debug!("[{name}] Command scheduled: {self}");
        self.log(EventKind::Scheduled, name).await
    }

    /// Log a command to be scheduled.
    async fn log_precond(&self, name: impl AsRef<str>) {
        let name = name.as_ref();
        if !self.pre.is_none() {
            log::debug!("[{name}] Precondition satisfied! {self}");
        } else {
            log::debug!("[{name}] Execute command! {self}");
        }
        self.log(EventKind::PreconditionSatisfied, name).await
    }

    /// Log a command to be scheduled.
    async fn log_postcond(&self, name: impl AsRef<str>) {
        let name = name.as_ref();
        if !self.post.is_none() {
            log::debug!("[{name}] Postcondition satisfied! {self}");
        }
        self.log(EventKind::PostConditionSatisfied, name).await
    }
}

impl std::fmt::Display for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}",
            self.cmd_repr,
            if self.pre.is_none() {
                String::new()
            } else {
                format!(", PRE: {}", self.pre)
            },
            if self.post.is_none() {
                String::new()
            } else {
                format!(", POST: {}", self.post)
            }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Enumeration of states for a job. It can either wait for the precondition, or wait for the
/// postcondition.
enum JobState {
    /// Waiting for the precondition to be satisfied. Once satisfied, send the reconfiguration
    /// command.
    Pre,
    /// Waiting for the postcondition to be satisfied. Once satisfied, send a signal on the `done`
    /// broadcast channel.
    Post,
    /// State when the job was executed successfully, and the postcondition is satisfied.
    Done,
}

impl Default for JobState {
    fn default() -> Self {
        Self::Pre
    }
}

/// The atomic Condition, translated to a form that can be checked using a [`CiscoShell`];
#[derive(Debug, Clone)]
enum LabCondition {
    /// No condition necessary. This condition is satisfied automatically.
    None,
    /// Condition on the current RIB entry (selected route) of a router and a prefix. This condition
    /// requires that a route for this prefix is available. In addition, one can choose to check
    /// that the route is coming from a speicfic neighbor, or that the route has a specific
    /// community value set.
    SelectedRoute {
        /// Which prefixes should be checked
        prefixes: MaybePec<Ipv4Net>,
        /// The selected route was learned from the given neighbors. If this is set to `None`, then
        /// the neighbor will not be checked.
        neighbor: Option<Ipv4Addr>,
        /// The selected route has a given (local) weight. If `None`, then the weight is ignored.
        weight: Option<u32>,
        /// The selected route has a given next-hop. If `None`, then the next-hop is ignored.
        next_hop: Option<Ipv4Addr>,
    },
    /// Condition on the availability of a given route. It implies that there exists at least one
    /// route that is from either one of the given neighbors, and that contains all given community
    /// values. If both options are `None`, then it just asserts that a route for this prefix is
    /// available.
    AvailableRoute {
        /// Which prefixes should be checked
        prefixes: MaybePec<Ipv4Net>,
        /// The selected route was learned from the given neighbors. If this is set to `None`, then
        /// the neighbor will not be checked.
        neighbor: Option<Ipv4Addr>,
        /// The selected route has a given (local) weight. If `None`, then the weight is ignored.
        weight: Option<u32>,
        /// The selected route has a given next-hop. If `None`, then the next-hop is ignored.
        next_hop: Option<Ipv4Addr>,
    },
    /// The BGP Session to a neighbor is established
    BgpSessionEstablished {
        /// IP address of the neighbor with which the router should have established a session.
        neighbor: Ipv4Addr,
    },
    /// Condition that checks if all routes received by a node are less preferred than the provided
    /// ones, before actually removing the weight rewrite.
    RoutesLessPreferred {
        /// Which prefixes should be checked
        prefixes: MaybePec<Ipv4Net>,
        /// Which neighbors are supposed to be preferred and can be ignored
        good_neighbors: BTreeSet<Ipv4Addr>,
        /// the route that we will receive from the good neighbors. Any other route must be less
        /// preferred than this one.
        route: BgpRibEntry<P>,
        /// The next hop that all routes from good neighbors must have
        next_hop: Ipv4Addr,
    },
}

impl LabCondition {
    /// translate an `AtomicCondition` to an `LabCondition`.
    ///
    /// The `pec_addrsses` is a lookup for prefix equivalence classes, and which networks to
    /// actually check for those.
    fn translate<Q>(
        from: &AtomicCondition<P>,
        r: RouterId,
        net: &Network<P, Q>,
        addressor: &mut DefaultAddressor<'_, P, Q>,
        pec_addresses: &HashMap<P, Vec<Ipv4Net>>,
    ) -> Result<Self, ExportError> {
        /// compute the prefix from the addressor
        fn get_prefixes<Q>(
            prefix: &P,
            addressor: &mut DefaultAddressor<'_, P, Q>,
            pec_addresses: &HashMap<P, Vec<Ipv4Net>>,
        ) -> Result<MaybePec<Ipv4Net>, ExportError> {
            if let Some(addrs) = pec_addresses.get(prefix) {
                Ok(MaybePec::Pec((*prefix).into(), addrs.clone()))
            } else {
                addressor.prefix(*prefix)
            }
        }

        /// transform all neighbors using `get_router_addr`.
        fn get_neighbors<Q>(
            router: RouterId,
            neighbors: &BTreeSet<RouterId>,
            net: &Network<P, Q>,
            addressor: &mut DefaultAddressor<'_, P, Q>,
        ) -> Result<BTreeSet<Ipv4Addr>, ExportError> {
            neighbors
                .iter()
                .map(|n| get_router_addr(router, Some(*n), net, addressor).map(|x| x.unwrap()))
                .collect()
        }

        /// Compute the Ip address of a neighbor (or another router). If it is an external router,
        /// then use the interface address when talking between both.
        fn get_router_addr<Q>(
            router: RouterId,
            neighbor: Option<RouterId>,
            net: &Network<P, Q>,
            addressor: &mut DefaultAddressor<'_, P, Q>,
        ) -> Result<Option<Ipv4Addr>, ExportError> {
            if let Some(n) = neighbor {
                Ok(Some(if net.get_device(n).is_internal() {
                    addressor.router_address(n)?
                } else {
                    addressor.iface_address(n, router)?
                }))
            } else {
                Ok(None)
            }
        }

        Ok(match from {
            AtomicCondition::None => LabCondition::None,
            AtomicCondition::SelectedRoute {
                router,
                prefix,
                neighbor,
                weight,
                next_hop,
            } if r == *router => LabCondition::SelectedRoute {
                prefixes: get_prefixes(prefix, addressor, pec_addresses)?,
                neighbor: get_router_addr(r, *neighbor, net, addressor)?,
                weight: *weight,
                next_hop: get_router_addr(r, *next_hop, net, addressor)?,
            },
            AtomicCondition::AvailableRoute {
                router,
                prefix,
                neighbor,
                weight,
                next_hop,
            } if r == *router => LabCondition::AvailableRoute {
                prefixes: get_prefixes(prefix, addressor, pec_addresses)?,
                neighbor: get_router_addr(r, *neighbor, net, addressor)?,
                weight: *weight,
                next_hop: get_router_addr(r, *next_hop, net, addressor)?,
            },
            AtomicCondition::BgpSessionEstablished {
                router: a,
                neighbor: b,
            }
            | AtomicCondition::BgpSessionEstablished {
                router: b,
                neighbor: a,
            } if r == *a => {
                let neighbor = if net.get_device(*b).is_internal() {
                    addressor.router_address(*b)?
                } else {
                    addressor.iface_address(*b, *a)?
                };
                LabCondition::BgpSessionEstablished { neighbor }
            }
            AtomicCondition::RoutesLessPreferred {
                router,
                prefix,
                good_neighbors,
                route,
            } if r == *router => LabCondition::RoutesLessPreferred {
                prefixes: get_prefixes(prefix, addressor, pec_addresses)?,
                good_neighbors: get_neighbors(r, good_neighbors, net, addressor)?,
                route: route.clone(),
                next_hop: get_router_addr(r, Some(route.route.next_hop), net, addressor)?.unwrap(),
            },
            _ => unreachable!("Condition is on a different device!"),
        })
    }

    /// Return `true` if `self == LabCondition::None`
    fn is_none(&self) -> bool {
        matches!(self, LabCondition::None)
    }

    /// Check if the condition is satisfied by issuing commands to the cisco shell.
    async fn check(
        &self,
        shell: &mut CiscoShell,
        cache: &mut HashMap<Ipv4Net, Vec<BgpRoute>>,
    ) -> Result<bool, CiscoShellError> {
        /// Get the BGP roues from either the cache or from the router shell.
        async fn get<'a>(
            shell: &mut CiscoShell,
            net: &Ipv4Net,
            cache: &'a mut HashMap<Ipv4Net, Vec<BgpRoute>>,
        ) -> Result<&'a Vec<BgpRoute>, CiscoShellError> {
            if !cache.contains_key(net) {
                let r = shell.get_bgp_route(*net).await?.unwrap_or_default();
                cache.insert(*net, r);
            }
            Ok(cache.get(net).unwrap())
        }

        Ok(match self {
            LabCondition::None => true,
            LabCondition::SelectedRoute {
                prefixes,
                neighbor,
                weight,
                next_hop,
            } => {
                for p in prefixes.iter() {
                    if !get(shell, p, cache)
                        .await?
                        .iter()
                        .any(|r| r.selected && check_route(r, *weight, *next_hop, *neighbor))
                    {
                        return Ok(false);
                    }
                }
                true
            }
            LabCondition::AvailableRoute {
                prefixes,
                neighbor,
                weight,
                next_hop,
            } => {
                for p in prefixes.iter() {
                    if !get(shell, p, cache)
                        .await?
                        .iter()
                        .any(|r| check_route(r, *weight, *next_hop, *neighbor))
                    {
                        return Ok(false);
                    }
                }
                true
            }
            LabCondition::BgpSessionEstablished { neighbor } => shell
                .get_bgp_neighbors()
                .await?
                .iter()
                .any(|n| n.connected && n.id == *neighbor),
            LabCondition::RoutesLessPreferred {
                prefixes,
                good_neighbors,
                route,
                next_hop,
            } => {
                for p in prefixes.iter() {
                    if get(shell, p, cache)
                        .await?
                        .iter()
                        .any(|r| !check_route_preference(r, route, good_neighbors, *next_hop))
                    {
                        return Ok(false);
                    }
                }
                true
            }
        })
    }
}

/// Check if a route is coming from one of the given neighbors, and has all of the given
/// communities.
fn check_route(
    route: &BgpRoute,
    weight: Option<u32>,
    next_hop: Option<Ipv4Addr>,
    neighbor: Option<Ipv4Addr>,
) -> bool {
    if let Some(w) = weight {
        if w != route.weight {
            return false;
        }
    }
    if let Some(nh) = next_hop {
        if nh != route.next_hop {
            return false;
        }
    }
    if let Some(n) = neighbor {
        if !(n == route.neighbor || n == route.neighbor_id) {
            return false;
        }
    }
    true
}

/// Check that a route is less preferred than the provided one from the simulation.
fn check_route_preference(
    route: &BgpRoute,
    better: &BgpRibEntry<P>,
    good_neighbors: &BTreeSet<Ipv4Addr>,
    next_hop: Ipv4Addr,
) -> bool {
    // We ignore any route with the AsId 666 in the path. This is a route that we use for emulating
    // an unforeseen external event.
    // TODO: This should be done properly.
    if route.path.contains(&AsId(666)) {
        true
    } else if good_neighbors.contains(&route.neighbor)
        || good_neighbors.contains(&route.neighbor_id)
    {
        next_hop == route.next_hop
    } else {
        let rib = BgpRibEntry {
            route: bgpsim::prelude::BgpRoute {
                prefix: better.route.prefix,
                as_path: route.path.clone(),
                next_hop: 0.into(),
                local_pref: route.local_pref,
                med: route.med,
                community: Default::default(),
                originator_id: None,
                cluster_list: Default::default(),
            },
            from_type: if route.path_type == BgpPathType::External {
                BgpSessionType::EBgp
            } else {
                BgpSessionType::IBgpPeer
            },
            from_id: 0.into(),
            to_id: None,
            igp_cost: Some((route.igp_cost as f64).try_into().unwrap()),
            weight: route.weight,
        };
        &rib < better
    }
}

impl std::fmt::Display for LabCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::SelectedRoute {
                prefixes,
                neighbor,
                weight,
                next_hop,
            } => {
                let from = neighbor
                    .as_ref()
                    .map(|n| format!(" from {n}"))
                    .unwrap_or_default();
                let weight = weight
                    .map(|w| format!(" with weight {w}"))
                    .unwrap_or_default();
                let nh = next_hop.map(|nh| format!(" via {nh}")).unwrap_or_default();
                write!(f, "select route for {prefixes}{from}{nh}{weight}")
            }
            Self::AvailableRoute {
                prefixes,
                neighbor,
                weight,
                next_hop,
            } => {
                let from = neighbor
                    .as_ref()
                    .map(|n| format!(" from {n}"))
                    .unwrap_or_default();
                let weight = weight
                    .map(|w| format!(" with weight {w}"))
                    .unwrap_or_default();
                let nh = next_hop.map(|nh| format!(" via {nh}")).unwrap_or_default();
                write!(f, "know a route for {prefixes}{from}{nh}{weight}")
            }
            LabCondition::BgpSessionEstablished { neighbor } => {
                write!(f, "BGP Session with {neighbor} established")
            }
            LabCondition::RoutesLessPreferred {
                prefixes,
                good_neighbors,
                ..
            } => {
                let from = good_neighbors.iter().join(" & ");
                write!(f, "routes for {prefixes} from {from} are most preferred")
            }
        }
    }
}

/// Error that must be unwrapped
struct LabErrorToKill(LabError, broadcast::Sender<()>);

impl std::fmt::Debug for LabErrorToKill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LabErrorToKill").field(&self.0).finish()
    }
}

impl<E: Into<LabError>> From<(E, broadcast::Sender<()>)> for LabErrorToKill {
    fn from(value: (E, broadcast::Sender<()>)) -> Self {
        Self(value.0.into(), value.1)
    }
}

impl<E: Into<LabError>> From<(E, &KillChannel)> for LabErrorToKill {
    fn from(value: (E, &KillChannel)) -> Self {
        Self(value.0.into(), value.1.tx.clone())
    }
}

impl<E: Into<LabError>> From<(E, &mut KillChannel)> for LabErrorToKill {
    fn from(value: (E, &mut KillChannel)) -> Self {
        Self(value.0.into(), value.1.tx.clone())
    }
}

impl<E: Into<LabError>> From<(E, KillChannel)> for LabErrorToKill {
    fn from(value: (E, KillChannel)) -> Self {
        Self(value.0.into(), value.1.tx)
    }
}

impl From<LabErrorToKill> for LabError {
    fn from(value: LabErrorToKill) -> Self {
        let _ = value.1.send(());
        value.0
    }
}
