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

//! This module is responsible for decomposing a command into atomic commands, as well as finding an
//! ordering in which to apply them.

use std::collections::{HashMap, HashSet};

use bgpsim::{
    bgp::BgpState,
    config::{ConfigModifier, NetworkConfig},
    event::EventQueue,
    forwarding_state::ForwardingState,
    prelude::Network,
    types::{NetworkError, RouterId},
};
use good_lp::ResolutionError;
use log::info;
use thiserror::Error;

use crate::{
    decomposition::ilp_scheduler::{FwStateTrace, NodeSchedule, Schedule},
    specification::Specification,
    P,
};

use self::bgp_dependencies::BgpDependencies;

#[cfg(feature = "explicit-loop-checker")]
pub(self) mod all_loops;
// pub mod atomic;
pub mod bgp_dependencies;
pub mod compiler;
pub mod ilp_scheduler;

use atomic_command::{AtomicCommand, AtomicCondition, AtomicModifier};

/// Decomposition of an individual command into multiple commands, including the order in which to
/// apply those commands.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Decomposition {
    /// Original command which has been decomposed
    pub original_command: ConfigModifier<P>,
    /// BGP Dependencies for each prefix
    pub bgp_deps: HashMap<P, BgpDependencies>,
    /// The computed schedule for each router and each prefix.
    pub schedule: HashMap<P, HashMap<RouterId, NodeSchedule>>,
    /// The expected forwarding state trace
    pub fw_state_trace: HashMap<P, FwStateTrace>,
    /// Commands used to prepare the update. These commands will not change anything in the
    /// forwarding, and they are used for all prefixes together!
    pub setup_commands: Vec<Vec<AtomicCommand<P>>>,
    /// Commands used to clean up the update. These commands will also not change anything in the
    /// forwarding, and they are used for all prefixes together!
    pub cleanup_commands: Vec<Vec<AtomicCommand<P>>>,
    /// Atomic commands and their ordering, which need to be applied *before* the main command is
    /// applied. The outer vector represents the order in which to apply the commands, and the inner
    /// vector stores several config modifiers that can be executed simultaneously.
    pub atomic_before: HashMap<P, Vec<Vec<AtomicCommand<P>>>>,
    /// The main commands to apply. These typically only involve applying the original
    /// command. However, this also involves adding a special tag to the specific session that is
    /// traversed.
    pub main_commands: Vec<Vec<AtomicCommand<P>>>,
    /// Atomic commands and their ordering, which need to be applied *after* the main command is
    /// applied. The outer vector represents the order in which to apply the commands, and the inner
    /// vector stores several config modifiers that can be executed simultaneously.
    pub atomic_after: HashMap<P, Vec<Vec<AtomicCommand<P>>>>,
}

impl Decomposition {
    /// Generate the baseline decomposition that only contains a single command without any
    /// conditions.
    pub fn baseline(command: ConfigModifier<P>) -> Self {
        Self {
            original_command: command.clone(),
            bgp_deps: Default::default(),
            schedule: Default::default(),
            fw_state_trace: Default::default(),
            setup_commands: Default::default(),
            cleanup_commands: Default::default(),
            atomic_before: Default::default(),
            main_commands: vec![vec![AtomicCommand {
                command: AtomicModifier::Raw(command),
                precondition: AtomicCondition::None,
                postcondition: AtomicCondition::None,
            }]],
            atomic_after: Default::default(),
        }
    }
}

/// Decompose the command and return a [`Decomposition`].
pub fn decompose<Q>(
    net: &Network<P, Q>,
    command: ConfigModifier<P>,
    spec: &Specification,
) -> Result<Decomposition, DecompositionError>
where
    Q: EventQueue<P> + Clone,
{
    let info = CommandInfo::new(net, command, spec)?;
    let bgp_deps = bgp_dependencies::find_dependencies(&info);

    let schedules: HashMap<P, (Schedule, FwStateTrace)> = info
        .prefixes
        .iter()
        .map(|p| Ok((*p, ilp_scheduler::schedule(&info, &bgp_deps, *p)?)))
        .collect::<Result<HashMap<_, _>, DecompositionError>>()?;

    compiler::build(&info, bgp_deps, schedules)
}

/// A single forwarding delta, storing the old and the new next-hop
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FwDiff {
    /// Old next-hop
    old: Option<RouterId>,
    /// New next-hop
    new: Option<RouterId>,
}

/// Datastructure for storing all information about the command that can be directly observed from
/// the simulator result.
#[derive(Debug)]
pub struct CommandInfo<'n, Q> {
    /// Reconfiguration command to decompose
    pub command: ConfigModifier<P>,
    /// Network before the reconfiguration command
    pub net_before: &'n Network<P, Q>,
    /// Network after the reconfiguration command
    pub net_after: Network<P, Q>,
    /// Initial forwarding state
    pub fw_before: ForwardingState<P>,
    /// Final forwarding state
    pub fw_after: ForwardingState<P>,
    /// Difference of the forwarding state for each individual prefix
    pub fw_diff: HashMap<P, HashMap<RouterId, FwDiff>>,
    /// Set of prefixes
    pub prefixes: HashSet<P>,
    /// Initial BGP state
    pub bgp_before: HashMap<P, BgpState<P>>,
    /// Final BGP state
    pub bgp_after: HashMap<P, BgpState<P>>,
    /// Invariants during the migration.
    pub spec: &'n Specification,
}

impl<'n, Q> CommandInfo<'n, Q>
where
    Q: EventQueue<P> + Clone,
{
    /// Create a new decomposition structure that keeps all information about the reconfiguration
    /// command that can be directly observed from the simulator.
    pub fn new(
        net_before: &'n Network<P, Q>,
        command: ConfigModifier<P>,
        spec: &'n Specification,
    ) -> Result<Self, DecompositionError> {
        info!("Extract the network state before and after the update.");
        let fw_before = net_before.get_forwarding_state();
        let bgp_before = net_before
            .get_known_prefixes()
            .map(|p| (*p, net_before.get_bgp_state_owned(*p)))
            .collect();
        let mut net_after = net_before.clone();
        net_after.apply_modifier(&command)?;
        let fw_after = net_after.get_forwarding_state();
        let bgp_after = net_after
            .get_known_prefixes()
            .map(|p| (*p, net_after.get_bgp_state_owned(*p)))
            .collect();

        let prefixes: HashSet<P> = net_before
            .get_known_prefixes()
            .chain(net_after.get_known_prefixes())
            .copied()
            .collect();

        // check if every next-hop is unique
        for r in net_before.get_topology().node_indices() {
            for p in prefixes.iter() {
                if fw_before.get_next_hops(r, *p).len() > 1
                    || fw_after.get_next_hops(r, *p).len() > 1
                {
                    return Err(DecompositionError::LoadBalancingEnabled);
                }
            }
        }

        let fw_diff = fw_before
            .diff(&fw_after)
            .into_iter()
            .map(|(p, diff)| {
                (
                    p,
                    diff.into_iter()
                        .map(|(r, mut old, mut new)| {
                            (
                                r,
                                FwDiff {
                                    old: old.pop(),
                                    new: new.pop(),
                                },
                            )
                        })
                        .collect(),
                )
            })
            .collect();

        Ok(Self {
            command,
            net_before,
            net_after,
            fw_before,
            fw_after,
            fw_diff,
            prefixes,
            bgp_before,
            bgp_after,
            spec,
        })
    }
}

impl<'n, Q> CommandInfo<'n, Q> {
    /// Get an iterator over all routers in the network.
    pub fn routers(&self) -> Vec<RouterId> {
        self.net_before.get_topology().node_indices().collect()
    }

    /// Get a vector of all internal routers
    pub fn internal_routers(&self) -> Vec<RouterId> {
        self.net_before.get_routers()
    }
}

/// Error when decomposing a command
#[derive(Debug, Error)]
pub enum DecompositionError {
    /// Error while operating with the Network.
    #[error("Network Error: {0}")]
    NetworkError(#[from] NetworkError),
    /// Could not compute the schedule
    #[error("Could not compute the schedule: {0}")]
    SchedulerError(#[from] ResolutionError),
    /// Load balancing is not yet supported
    #[error("Load balancing is enabled, but it is not yet supported!")]
    LoadBalancingEnabled,
    /// Cannot add a temporary BGP session if it already exists.
    #[error("Cannot add a temporary BGP session between {0:?} and {1:?} that already exists.")]
    TemporaryBgpSession(RouterId, RouterId),
    /// The round at which to apply the main command could not be determined
    #[error("Illdefined round at which to apply the main command for prefix {0}: {1}")]
    InconsistentMainCommandRound(P, &'static str),
}
