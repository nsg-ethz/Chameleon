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

//! Runtime for the simulated system (in [`bgpsim`]).

use bgpsim::{config::NetworkConfig, event::EventQueue, prelude::*};
use log::error;
use thiserror::Error;

use crate::{decomposition::Decomposition, specification::Specification, P};

use super::controller::Controller;

mod executor;

/// Probability that the controller is called to try making progress in this step of the
/// convergence.
const PROB_CONTROLLER_STEP: f64 = 0.5;

/// Perform the decomposed update on the network using the simulated environment (bgpsim). Here, we
/// check on each step in the simulation if (1) the policies are satisfied, and (2) if it is safe to
/// perform any update. The strategy is such that we try to make the update as fast as
/// possible. This is obviously not easy to do in practice.
pub fn run<Q>(
    mut net: Network<P, Q>,
    decomp: Decomposition,
    spec: &Specification,
) -> Result<(Network<P, Q>, SimStats), SimError>
where
    Q: Clone + EventQueue<P> + PartialEq + std::fmt::Debug,
{
    let mut exp_net = net.clone();
    exp_net.apply_modifier(&decomp.original_command)?;

    let trace = decomp.fw_state_trace.clone();
    let mut controller = Controller::new(decomp);

    let stats = controller.execute_sim(&mut net, spec, PROB_CONTROLLER_STEP, trace, true)?;

    // check if they are equal
    if net != exp_net {
        pretty_assertions_sorted::assert_eq!(net, exp_net);
        Err(SimError::WrongFinalState)
    } else {
        Ok((net, stats))
    }
}

/// Perform the decomposed update on the network using the simulated environment (bgpsim). This
/// function will not do any kind of checks.
pub fn run_no_checks<Q>(
    mut net: Network<P, Q>,
    decomp: Decomposition,
) -> Result<(Network<P, Q>, SimStats), SimError>
where
    Q: Clone + EventQueue<P> + PartialEq + std::fmt::Debug,
{
    let mut exp_net = net.clone();
    exp_net.apply_modifier(&decomp.original_command)?;

    let mut controller = Controller::new(decomp);

    let stats = controller.execute_sim(
        &mut net,
        &Default::default(),
        PROB_CONTROLLER_STEP,
        Default::default(),
        false,
    )?;

    // check if they are equal
    if net != exp_net {
        pretty_assertions_sorted::assert_eq!(net, exp_net);
        Err(SimError::WrongFinalState)
    } else {
        Ok((net, stats))
    }
}

/// Statistics collected during simulation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct SimStats {
    /// Number of routes within the BgpRibIn and BgpRib in the initial state.
    pub num_routes_before: usize,
    /// Number of routes within the BgpRibIn and BgpRib in the final state.
    pub num_routes_after: usize,
    /// Maximum number of routes stored within the BgpRibIn and the BgpRib at any given time.
    pub max_routes: usize,
    /// Sequence of forwarding deltas performed during the migration.
    pub fw_deltas: Vec<Vec<(RouterId, P, Vec<RouterId>)>>,
}

/// Error of the simulated runtime.
#[derive(Debug, Error)]
pub enum SimError {
    /// Network has thrown an unexpected error
    #[error("{0}")]
    NetworkError(#[from] NetworkError),
    /// A policy was not satisfied at some stage during the convergence.
    #[error("Specification Violation")]
    Violation,
    /// The resulting network is not equal to the expected network!
    #[error("The resulting network is not equal to the expected network!")]
    WrongFinalState,
    /// The controller cannot make any progress.
    #[error("The controller cannot make any progress")]
    CannotProgress,
    /// The simulated trace does not match the scheduled trace
    #[error("Simulated trace does not match the scheduled trace! {0}")]
    TraceMismatch(String),
}
