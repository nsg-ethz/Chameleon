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

//! Formatting things.

use std::cmp::Ordering;

use atomic_command::AtomicCommand;
use bgpsim::prelude::*;
use itertools::Itertools;

use crate::{
    decomposition::{bgp_dependencies::BgpDependency, Decomposition},
    runtime::controller::{AtomicCommandState, Controller, ControllerStage, StateItem},
    specification::{Invariant, Property, SpecExpr, Violation},
    P,
};

/// Trait to format things using appropriate indentation.
pub trait IndentedNetworkFormatter<'a, 'n, Q> {
    /// Format something using the network and some specific indent.
    fn fmt(&'a self, net: &'n Network<P, Q>, indent: usize) -> String;
}

impl<'a, 'n, Q> IndentedNetworkFormatter<'a, 'n, Q> for AtomicCommand<P> {
    fn fmt(&'a self, net: &'n Network<P, Q>, indent: usize) -> String {
        let tab: String = " ".repeat(indent);
        format!(
            "{tab}{}\n{tab}  pre condition:  {}\n{tab}  post condition: {}\n{tab}  raw:\n{}",
            self.command.fmt(net),
            self.precondition.fmt(net),
            self.postcondition.fmt(net),
            self.command
                .clone()
                .into_raw()
                .into_iter()
                .map(|c| format!("{}    {}", tab, c.fmt(net)))
                .join("\n"),
            tab = tab
        )
    }
}

impl<'a, 'n, Q> IndentedNetworkFormatter<'a, 'n, Q> for [AtomicCommand<P>] {
    fn fmt(&'a self, net: &'n Network<P, Q>, indent: usize) -> String {
        let tab: String = " ".repeat(indent);
        format!(
            "{tab}{{\n{}\n{tab}}}",
            self.iter().map(|x| x.fmt(net, indent + 2)).join(",\n"),
            tab = tab,
        )
    }
}

impl<'a, 'n, Q> IndentedNetworkFormatter<'a, 'n, Q> for [Vec<AtomicCommand<P>>] {
    fn fmt(&'a self, net: &'n Network<P, Q>, indent: usize) -> String {
        let tab: String = " ".repeat(indent);
        format!(
            "{tab}[\n{}\n{tab}]",
            self.iter().map(|x| x.fmt(net, indent + 2)).join(",\n"),
            tab = tab,
        )
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for Decomposition {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        let setup = self.setup_commands.fmt(net, 4);
        let cleanup = self.cleanup_commands.fmt(net, 4);
        let main = self.main_commands.fmt(net, 4);
        let atomics_before = self
            .atomic_before
            .iter()
            .map(|(p, cmdss)| format!("prefix: {} {{\n{}\n    }}", p, cmdss.fmt(net, 6)))
            .join("\n    ");
        let atomics_after = self
            .atomic_after
            .iter()
            .map(|(p, cmdss)| format!("prefix: {} {{\n{}\n    }}", p, cmdss.fmt(net, 6)))
            .join("\n    ");

        let prefixes = self
            .schedule
            .keys()
            .chain(self.fw_state_trace.keys())
            .chain(self.bgp_deps.keys())
            .unique()
            .sorted();

        let prefix_schedules = prefixes
            .map(|p| {
                (
                    p,
                    self.bgp_deps.get(p).unwrap(),
                    self.schedule.get(p),
                    self.fw_state_trace.get(p),
                )
            })
            .map(|(p, bgp_deps, schedule, fw_state_trace)| {
                format!(
                    "    {p}: {{\n      BGP schedule:\n{}\n      FW State trace:\n{}}}",
                    schedule
                        .into_iter()
                        .flatten()
                        .sorted_by_key(|(_, s)| (s.fw_state, s.old_route, s.new_route))
                        .map(|(r, s)| format!(
                            "        {}: ({}) {} <= {} <= {} ({})",
                            r.fmt(net),
                            bgp_deps
                                .get(r)
                                .map(|d| d.old_from.fmt(net))
                                .unwrap_or_default(),
                            s.old_route,
                            s.fw_state,
                            s.new_route,
                            bgp_deps
                                .get(r)
                                .map(|d| d.new_from.fmt(net))
                                .unwrap_or_default(),
                        ))
                        .join("\n"),
                    fw_state_trace
                        .into_iter()
                        .flatten()
                        .map(|step| format!(
                            "        - {}",
                            step.iter()
                                .map(|(r, nh)| format!("{}: {}", r.fmt(net), nh.fmt(net)))
                                .join("\n          ")
                        ))
                        .join("\n")
                )
            })
            .join("\n");

        let schedule = format!("  schedule: {{\n{prefix_schedules}\n  }}");
        let setup = format!("  setup: {{\n{setup}\n  }}");
        let before = format!("  atomic commands before: {{\n    {atomics_before}\n  }}");
        let main = format!("  main: {{\n{main}\n  }}");
        let after = format!("  atomic commands after: {{\n    {atomics_after}\n  }}");
        let cleanup = format!("  cleanup: {{\n{cleanup}\n  }}");

        format!(
            "Decomposition of command: {} {{\n{}\n{}\n{}\n{}\n{}\n{}\n}}",
            self.original_command.fmt(net),
            schedule,
            setup,
            before,
            main,
            after,
            cleanup,
        )
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for Controller {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        format!(
            "{}\n\n{}",
            self.decomposition().fmt(net),
            self.state().fmt(net),
        )
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for ControllerStage {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        match self {
            ControllerStage::Setup(s) => format!("Setup:\n{}", s.fmt(net)),
            ControllerStage::UpdateBefore(s) => format!(
                "Main Update before the command:\n{}",
                s.iter()
                    .map(|(p, s)| format!("*** {}:\n{}", p, s.fmt(net)))
                    .join("\n")
            ),
            ControllerStage::Main(s) => format!("Main Command:\n{}", s.fmt(net)),
            ControllerStage::UpdateAfter(s) => format!(
                "Main Update after the command:\n{}",
                s.iter()
                    .map(|(p, s)| format!("*** {}:\n{}", p, s.fmt(net)))
                    .join("\n")
            ),
            ControllerStage::Cleanup(s) => format!("Cleanup:\n{}", s.fmt(net)),
            ControllerStage::Finished => String::from("Finished"),
        }
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for StateItem {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        self.commands
            .iter()
            .enumerate()
            .map(|(i, cmds)| match i.cmp(&self.round) {
                Ordering::Less => cmds.iter().map(|c| c.fmt(net, 7)).join("\n"),
                Ordering::Greater => cmds.iter().map(|c| c.fmt(net, 7)).join("\n"),
                Ordering::Equal => cmds
                    .iter()
                    .zip(self.entries.iter())
                    .map(|(c, e)| {
                        let s = match e {
                            AtomicCommandState::Precondition => "pre ",
                            AtomicCommandState::Postcondition => "post",
                            AtomicCommandState::Done => "done",
                        };
                        format!("[{}] {}", s, c.fmt(net, 7).trim_start())
                    })
                    .join("\n"),
            })
            .join("\n")
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for BgpDependency {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        format!(
            "before: {}, after: {}",
            self.old_from.fmt(net),
            self.new_from.fmt(net)
        )
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for SpecExpr {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        match self {
            SpecExpr::All(x) => format!("({})", x.iter().map(|p| p.fmt(net)).join(" && ")),
            SpecExpr::Any(x) => format!("({})", x.iter().map(|p| p.fmt(net)).join(" || ")),
            SpecExpr::Not(x) => format!("!{}", x.fmt(net)),
            SpecExpr::True => String::from('t'),
            SpecExpr::Next(x) => format!("N {}", x.fmt(net)),
            SpecExpr::Finally(x) => format!("F {}", x.fmt(net)),
            SpecExpr::Globally(x) => format!("G {}", x.fmt(net)),
            SpecExpr::Until(a, b) => format!("({} U {})", a.fmt(net), b.fmt(net)),
            SpecExpr::WeakUntil(a, b) => format!("({} W {})", a.fmt(net), b.fmt(net)),
            SpecExpr::Invariant(x) => x.fmt(net),
        }
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for Invariant {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        format!("[{}, {}]", self.router.fmt(net), self.prop.fmt(net))
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for Property {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        match self {
            Property::All(x) => format!("({})", x.iter().map(|p| p.fmt(net)).join(" && ")),
            Property::Any(x) => format!("({})", x.iter().map(|p| p.fmt(net)).join(" || ")),
            Property::Not(x) => format!("!{}", x.fmt(net)),
            Property::Waypoint(wp) => wp.fmt(net).to_string(),
            Property::Reachability => String::from("reach"),
            Property::True => String::from('t'),
        }
    }
}

impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for Violation {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        match self {
            Violation::Path(prefix, prop, path, valid) => {
                format!(
                    "Violation for {prefix}: {} violated on {} ({})",
                    prop.fmt(net),
                    path.fmt(net),
                    if *valid { "valid" } else { "invalid" },
                )
            }
        }
    }
}

#[cfg(feature = "cisco-lab")]
impl<'a, 'n, Q> NetworkFormatter<'a, 'n, P, Q> for crate::runtime::lab::Event {
    type Formatter = String;

    fn fmt(&'a self, net: &'n Network<P, Q>) -> Self::Formatter {
        format!(
            "{: >8.3} {} | {}",
            self.elapsed_secs,
            match self.event {
                crate::runtime::lab::EventKind::Scheduled => "INIT",
                crate::runtime::lab::EventKind::PreconditionSatisfied => "PRE ",
                crate::runtime::lab::EventKind::PostConditionSatisfied => "POST",
            },
            self.command.fmt(net, 16).trim()
        )
    }
}
