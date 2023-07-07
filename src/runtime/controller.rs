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

//! Controller and State Machine for the migration

use std::{collections::HashMap, mem::take};

use atomic_command::{AtomicCommand, AtomicCondition};
use bgpsim::prelude::*;
use itertools::Itertools;

use crate::{decomposition::Decomposition, P};

/// The controller structure keeps track of the current step of the update, and checks if it is safe
/// to perform the next change. If so, it will perform it.
#[derive(Debug)]
pub struct Controller {
    /// The command decomposition
    pub decomp: Decomposition,
    /// The current state of the update
    pub state: ControllerStage,
}

impl Controller {
    /// Create a new controller in the initial state
    pub fn new(mut decomp: Decomposition) -> Self {
        let state = ControllerStage::setup(take(&mut decomp.setup_commands));
        Self { decomp, state }
    }

    /// Get the decomposition of the command
    pub fn decomposition(&self) -> &Decomposition {
        &self.decomp
    }

    /// Get a reference to the current state.
    pub fn state(&self) -> &ControllerStage {
        &self.state
    }

    /// Returns `true` if the controller has finished processing the update.
    pub fn is_finished(&self) -> bool {
        matches!(self.state, ControllerStage::Finished)
    }

    /// Turn the current controller into a list of stages that still need to be performed.
    #[cfg(feature = "cisco-lab")]
    pub(crate) fn into_remaining_states(self) -> Vec<ControllerStage> {
        let mut stages = Vec::new();
        let mut state = self.state;
        let Decomposition {
            cleanup_commands,
            atomic_before,
            main_commands,
            atomic_after,
            ..
        } = self.decomp;
        if matches!(state, ControllerStage::Setup(_)) {
            stages.push(state);
            state = ControllerStage::update_before(atomic_before);
        }
        if matches!(state, ControllerStage::UpdateBefore(_)) {
            stages.push(state);
            state = ControllerStage::main(main_commands);
        }
        if matches!(state, ControllerStage::Main(_)) {
            stages.push(state);
            state = ControllerStage::update_after(atomic_after);
        }
        if matches!(state, ControllerStage::UpdateAfter(_)) {
            stages.push(state);
            state = ControllerStage::cleanup(cleanup_commands);
        }
        if matches!(state, ControllerStage::Cleanup(_)) {
            stages.push(state);
        }

        stages
    }
}

/// In which state is the controller currently in.
#[derive(Debug)]
pub enum ControllerStage {
    /// The controller is currently setting up the network
    Setup(StateItem),
    /// The controller is currently performing the all updates before the main command
    UpdateBefore(HashMap<P, StateItem>),
    /// Performing the main update
    Main(StateItem),
    /// The controller is currently performing the all updates after the main command
    UpdateAfter(HashMap<P, StateItem>),
    /// The controller is currently cleaning up the update.
    Cleanup(StateItem),
    /// the controller has finished performing the update
    Finished,
}

impl ControllerStage {
    /// Create a new setup stage.
    pub fn setup(commands: Vec<Vec<AtomicCommand<P>>>) -> Self {
        Self::Setup(StateItem {
            round: 0,
            entries: commands
                .first()
                .iter()
                .flat_map(|x| x.iter())
                .map(|_| AtomicCommandState::Precondition)
                .collect_vec(),
            commands,
        })
    }

    /// Create a new update stage before applying the main command.
    pub fn update_before(commands: HashMap<P, Vec<Vec<AtomicCommand<P>>>>) -> Self {
        Self::UpdateBefore(
            commands
                .into_iter()
                .map(|(p, commands)| {
                    (
                        p,
                        StateItem {
                            round: 0,
                            entries: commands
                                .first()
                                .iter()
                                .flat_map(|x| x.iter())
                                .map(|_| AtomicCommandState::Precondition)
                                .collect_vec(),
                            commands,
                        },
                    )
                })
                .collect(),
        )
    }

    /// Create a new stage when applying the main command
    pub fn main(commands: Vec<Vec<AtomicCommand<P>>>) -> Self {
        Self::Main(StateItem {
            round: 0,
            entries: commands
                .first()
                .iter()
                .flat_map(|x| x.iter())
                .map(|_| AtomicCommandState::Precondition)
                .collect_vec(),
            commands,
        })
    }

    /// Create a new update stage after applying the main command.
    pub fn update_after(commands: HashMap<P, Vec<Vec<AtomicCommand<P>>>>) -> Self {
        Self::UpdateAfter(
            commands
                .into_iter()
                .map(|(p, commands)| {
                    (
                        p,
                        StateItem {
                            round: 0,
                            entries: commands
                                .first()
                                .iter()
                                .flat_map(|x| x.iter())
                                .map(|_| AtomicCommandState::Precondition)
                                .collect_vec(),
                            commands,
                        },
                    )
                })
                .collect(),
        )
    }

    /// Create a new cleanup stage
    pub fn cleanup(commands: Vec<Vec<AtomicCommand<P>>>) -> Self {
        Self::Cleanup(StateItem {
            round: 0,
            entries: commands
                .first()
                .iter()
                .flat_map(|x| x.iter())
                .map(|_| AtomicCommandState::Precondition)
                .collect_vec(),
            commands,
        })
    }

    /// Print a log of the Atomic Conditions, which consists of information needed to check if we
    /// can make any progress
    pub fn fmt_current_conditions<Q>(&self, net: &Network<P, Q>) -> String {
        match self {
            ControllerStage::Setup(s) => s.fmt_current_conditions(net),
            ControllerStage::UpdateBefore(s) | ControllerStage::UpdateAfter(s) => s
                .iter()
                .map(|(p, s)| format!("{}:\n{}", p, s.fmt_current_conditions(net)))
                .join("\n\n"),
            ControllerStage::Main(s) => s.fmt_current_conditions(net),
            ControllerStage::Cleanup(s) => s.fmt_current_conditions(net),
            ControllerStage::Finished => "Done".to_string(),
        }
    }

    /// Return the name of the current stage.
    pub fn name(&self) -> &'static str {
        match self {
            ControllerStage::Setup(_) => "Setup",
            ControllerStage::UpdateBefore(_) => "UpdateBefore",
            ControllerStage::Main(_) => "Main",
            ControllerStage::UpdateAfter(_) => "UpdateAfter",
            ControllerStage::Cleanup(_) => "Cleanup",
            ControllerStage::Finished => "Finished",
        }
    }

    /// Count the number of commands stored within this stage..
    #[cfg(feature = "cisco-lab")]
    pub(crate) fn count_commands(&self) -> usize {
        match self {
            ControllerStage::Setup(s) | ControllerStage::Main(s) | ControllerStage::Cleanup(s) => {
                s.commands.iter().map(|x| x.len()).sum()
            }
            ControllerStage::UpdateAfter(ss) | ControllerStage::UpdateBefore(ss) => ss
                .values()
                .map(|s| s.commands.iter().map(|x| x.len()).sum::<usize>())
                .sum(),
            ControllerStage::Finished => 0,
        }
    }
}

/// The state of a `Vec<Vec<AtomicCommand>>`
#[derive(Debug)]
pub struct StateItem {
    /// The current round, as an index into the first array
    pub round: usize,
    /// A vector storing the state for each atomic command in the round.
    pub entries: Vec<AtomicCommandState>,
    /// All atomic commands to be executed in that state.
    pub commands: Vec<Vec<AtomicCommand<P>>>,
}

impl StateItem {
    /// Print a log of the Atomic Conditions, which consists of information needed to check if we
    /// can make any progress
    pub fn fmt_current_conditions<Q>(&self, net: &Network<P, Q>) -> String {
        if let Some(cmds) = self.commands.get(self.round) {
            let mut result = Vec::new();

            // check if anything can make progress.
            for (i, state) in self.entries.iter().enumerate() {
                let cmd = &cmds[i];
                let (state, cond) = match state {
                    AtomicCommandState::Precondition => ("pre ", cmd.precondition.clone()),
                    AtomicCommandState::Postcondition => ("post", cmd.postcondition.clone()),
                    AtomicCommandState::Done => continue,
                };

                let net_state = match cond {
                    AtomicCondition::None => "()".to_string(),
                    AtomicCondition::SelectedRoute { router, prefix, .. } => {
                        if let Some(rib) = net
                            .get_device(router)
                            .unwrap_internal()
                            .get_selected_bgp_route(prefix)
                        {
                            rib.fmt(net)
                        } else {
                            String::from("no route selected")
                        }
                    }
                    AtomicCondition::AvailableRoute { router, prefix, .. }
                    | AtomicCondition::RoutesLessPreferred { router, prefix, .. } => net
                        .get_device(router)
                        .unwrap_internal()
                        .get_bgp_rib_in()
                        .get(&prefix)
                        .into_iter()
                        .flat_map(|t| t.values())
                        .map(|e| e.fmt(net))
                        .join("\n                  "),
                    AtomicCondition::BgpSessionEstablished { .. } => {
                        String::from("not established")
                    }
                };

                result.push(format!(
                    "[{}] condition: {}\n           state: {}",
                    state,
                    cond.fmt(net),
                    net_state
                ))
            }

            if result.is_empty() {
                "Done".to_string()
            } else {
                result.into_iter().join("\n")
            }
        } else {
            "Done".to_string()
        }
    }
}

/// The state of a single atomic command. It can either be waiting for the precondition, waiting for
/// the postcondition, or be executed successfully.
#[derive(Debug)]
pub enum AtomicCommandState {
    /// Waiting for the preconditions to be satisfied
    Precondition,
    /// Waiting for the postcondition to ber satisfied
    Postcondition,
    /// Command is complete.
    Done,
}

impl AtomicCommandState {
    /// Check if the command has finished executing
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }
}
