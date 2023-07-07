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

//! This module is the executor, that applies the actual atomic commands to the network (in `bgpsim`).

use std::{collections::HashMap, mem::take};

use bgpsim::{event::EventQueue, forwarding_state::ForwardingState, prelude::*};
use itertools::Itertools;
use log::{error, info, warn};
use rand::prelude::*;

use crate::{
    decomposition::ilp_scheduler::FwStateTrace,
    runtime::controller::{AtomicCommandState, Controller, ControllerStage, StateItem},
    specification::{Checker, Specification},
    P,
};

use super::{SimError, SimStats};

impl Controller {
    /// Perform the complete migration on the simulated network. During the migration, this function
    /// will check for policy violations at every state during convergence.
    ///
    /// At every step during the migration, we perform a step on the controller with probability
    /// `prob_controller_step`. If the RNG requires no update to be executed, the next network event
    /// will be called without calling [`Controller::step_sim`].
    ///
    /// If `check` is set to `false`, then do not perform any kind of checks..
    pub fn execute_sim<Q>(
        &mut self,
        net: &mut Network<P, Q>,
        spec: &Specification,
        prob_controller_step: f64,
        mut expected_fw_trace: HashMap<P, FwStateTrace>,
        check: bool,
    ) -> Result<SimStats, SimError>
    where
        Q: EventQueue<P>,
    {
        // set the net into manual simulation
        let auto_simulation = net.auto_simulation_enabled();
        net.manual_simulation();

        let mut checker = Checker::new(spec);
        let mut fw_state = net.get_forwarding_state();
        let mut stats = SimStats {
            num_routes_before: usize::MAX,
            num_routes_after: 0,
            max_routes: 0,
            fw_deltas: Vec::new(),
        };

        loop {
            // check for properties and update stats
            check_and_update_stats(
                check,
                net,
                &mut fw_state,
                &mut checker,
                &mut expected_fw_trace,
                &mut stats,
            )?;
            // simulate a step on the network
            net.simulate_step()?;
            // check for properties and update stats
            check_and_update_stats(
                check,
                net,
                &mut fw_state,
                &mut checker,
                &mut expected_fw_trace,
                &mut stats,
            )?;

            // skip the controller if the queue is not empty and with a certain probability
            if net.queue().is_empty() || thread_rng().gen_bool(prob_controller_step) {
                // do a step on the controller
                let change = self.step_sim(net)?;
                // check if we are done here.
                if self.is_finished() && net.queue().is_empty() {
                    // controler has finished, and the network has converged
                    break;
                } else if !change && net.queue().is_empty() {
                    error!(
                        "Controller cannot progress! state:\n{}\n",
                        self.state().fmt(net)
                    );
                    warn!(
                        "Current conditions:\n{}",
                        self.state().fmt_current_conditions(net)
                    );
                    // The controller did not make any progress, but the queue is currently empty, meaning
                    // that we are essentially stuck.
                    return Err(SimError::CannotProgress);
                }
            }
        }

        // reset the manual simulation
        if auto_simulation {
            net.auto_simulation();
        }

        Ok(stats)
    }

    /// Perform a single step (which may trigger multiple updates at the same time) on the simulated
    /// network and `true` if something has changed in the state or in the network.
    pub fn step_sim<Q>(&mut self, net: &mut Network<P, Q>) -> Result<bool, SimError>
    where
        Q: EventQueue<P>,
    {
        let (update, proceed) = self.state.step_sim(net)?;

        if proceed {
            if self.is_finished() {
                return Ok(false);
            }
            // proceed to the next step
            self.state = match self.state {
                ControllerStage::Setup(_) => {
                    ControllerStage::update_before(take(&mut self.decomp.atomic_before))
                }
                ControllerStage::UpdateBefore(_) => {
                    ControllerStage::main(take(&mut self.decomp.main_commands))
                }
                ControllerStage::Main(_) => {
                    ControllerStage::update_after(take(&mut self.decomp.atomic_after))
                }
                ControllerStage::UpdateAfter(_) => {
                    ControllerStage::cleanup(take(&mut self.decomp.cleanup_commands))
                }
                ControllerStage::Cleanup(_) => ControllerStage::Finished,
                ControllerStage::Finished => ControllerStage::Finished,
            };
            info!("Proceed to the next stage: {}.", self.state.name());
            // return true, meaning that there was some change.
            Ok(true)
        } else {
            Ok(update)
        }
    }
}

/// macro rules to make error hanling easier
macro_rules! err {
    () => {
        || SimError::TraceMismatch(String::from("Unexpected FW delta"))
    };
    ($($arg:expr),+) => {
        || SimError::TraceMismatch(format!($($arg),*))
    };
}

/// Update the forwarding state and log all deltas. Then, check compare the diff with the expected
/// trace.
fn check_and_update_stats<Q>(
    check: bool,
    net: &Network<P, Q>,
    fw_state: &mut ForwardingState<P>,
    checker: &mut Checker<'_>,
    expected_fw_trace: &mut HashMap<P, FwStateTrace>,
    stats: &mut SimStats,
) -> Result<(), SimError> {
    // handle the forwarding state
    let new = net.get_forwarding_state();
    let diff = fw_state.diff(&new);
    if !diff.is_empty() {
        log::info!("Forwarding delta!");
    }
    let mut delta = Vec::new();
    for (p, diff) in diff {
        for (r, nh) in diff.into_iter().map(|(r, _, nh)| (r, nh)).unique() {
            log::info!("FW delta: {} => {p}: {}", r.fmt(net), nh.fmt(net));
            // remove the diff from the expected trace
            if check {
                let prefix_trace = expected_fw_trace
                    .get_mut(&p)
                    .ok_or_else(err!("FW delta for prefix {p} that should not be affected!"))?;
                let step = prefix_trace
                    .first_mut()
                    .ok_or_else(err!("FW delta after prefix {p} should be fully migrated!"))?;
                step.remove(&(r, nh.clone()))
                    .then_some(())
                    .ok_or_else(err!(
                        "FW delta not expected: {} => {p}: {}",
                        r.fmt(net),
                        nh.fmt(net)
                    ))?;
                if step.is_empty() {
                    prefix_trace.remove(0);
                }
            }
            delta.push((r, p, nh))
        }
    }
    if !delta.is_empty() {
        stats.fw_deltas.push(delta);
    }
    *fw_state = new;

    // check specificatoin
    if check && !checker.step(fw_state) {
        error!("Policy violation during simulation!\n");
        return Err(SimError::Violation);
    }

    // read bgp state
    let mut sum_routes = 0;
    for p in net.get_known_prefixes() {
        let bgp_state = net.get_bgp_state(*p);
        for r in net.get_routers() {
            if bgp_state.get(r).is_some() {
                sum_routes += 1;
            }
            sum_routes += bgp_state
                .outgoing(r)
                .filter(|(n, _)| net.get_device(*n).is_internal())
                .count();
        }
    }
    if stats.num_routes_before == usize::MAX {
        stats.num_routes_before = sum_routes;
    }
    stats.max_routes = stats.max_routes.max(sum_routes);
    stats.num_routes_after = sum_routes;

    Ok(())
}

impl ControllerStage {
    /// Perform an individual step on the state. The first returned boolean tells if there was
    /// something that has changed, and the second one tells if the current state is done, and we
    /// can move to the next state.
    pub fn step_sim<Q>(&mut self, net: &mut Network<P, Q>) -> Result<(bool, bool), SimError>
    where
        Q: EventQueue<P>,
    {
        match self {
            ControllerStage::Setup(s) | ControllerStage::Main(s) | ControllerStage::Cleanup(s) => {
                s.step_sim(net)
            }
            ControllerStage::UpdateBefore(s) | Self::UpdateAfter(s) => s
                .values_mut()
                .map(|s| s.step_sim(net))
                .fold(Ok((false, true)), |acc, x| {
                    let (a_change, a_done) = acc?;
                    let (change, done) = x?;
                    Ok((a_change || change, a_done && done))
                }),
            ControllerStage::Finished => Ok((false, false)),
        }
    }
}

impl StateItem {
    /// Perform an individual step on the state. The first returned boolean tells if there was
    /// something that has changed, and the second one tells if the current state is done, and we
    /// can move to the next state.
    fn step_sim<Q>(&mut self, net: &mut Network<P, Q>) -> Result<(bool, bool), SimError>
    where
        Q: EventQueue<P>,
    {
        if let Some(cmds) = self.commands.get(self.round) {
            let mut has_changed = false;

            // check if anything can make progress.
            for (i, state) in self.entries.iter_mut().enumerate() {
                let cmd = &cmds[i];
                match state {
                    AtomicCommandState::Precondition => {
                        // check the precondition
                        if cmd.precondition.check(net)? {
                            // precondition can be executed
                            info!("Precondition satisfied: {}", cmd.precondition.fmt(net));
                            info!("Execute {}", cmd.command.fmt(net));
                            has_changed = true;
                            cmd.command.apply(net)?;
                            *state = AtomicCommandState::Postcondition;
                            // check for postcondition
                            if cmd.postcondition.check(net)? {
                                *state = AtomicCommandState::Done;
                                info!("Postcondition satisfied: {}", cmd.postcondition.fmt(net));
                            }
                        }
                    }
                    AtomicCommandState::Postcondition => {
                        // check the postcondition
                        if cmd.postcondition.check(net)? {
                            has_changed = true;
                            *state = AtomicCommandState::Done;
                            info!("Postcondition satisfied: {}", cmd.postcondition.fmt(net));
                        }
                    }
                    AtomicCommandState::Done => {}
                }
            }

            // check if we are done.
            if self.entries.iter().all(|s| s.is_done()) {
                // proceed to the next round
                loop {
                    self.round += 1;
                    self.entries = self
                        .commands
                        .get(self.round)
                        .iter()
                        .flat_map(|x| x.iter())
                        .map(|_| AtomicCommandState::Precondition)
                        .collect_vec();
                    if self.round >= self.commands.len() {
                        break Ok((true, true));
                    } else if !self.commands[self.round].is_empty() {
                        break Ok((true, false));
                    }
                }
            } else {
                Ok((has_changed, false))
            }
        } else {
            Ok((false, true))
        }
    }
}
