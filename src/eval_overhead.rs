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

use std::{
    fs::create_dir,
    io::Write,
    iter::repeat,
    path::PathBuf,
    time::{Duration, Instant},
};

use atomic_command::{AtomicCondition, AtomicModifier};
use bgpsim::{
    forwarding_state::ForwardingState, prelude::BasicEventQueue, topology_zoo::TopologyZoo,
};
use chameleon::{
    decomposition::{
        bgp_dependencies::find_dependencies,
        compiler::build,
        ilp_scheduler::{schedule_smart, NodeSchedule},
        CommandInfo,
    },
    experiment::{Experiment, Scenario, _TopologyZoo},
    runtime,
    specification::SpecificationBuilder,
    Decomposition, P,
};
use clap::{Parser, ValueEnum};
use good_lp::ResolutionError;
use maplit::hashmap;
use serde::Serialize;
use time::{format_description, OffsetDateTime};

/// Evaluate the ILP scheduler.
#[derive(Debug, Parser)]
struct Cli {
    /// Kind of scenario to simulate
    #[clap(short = 'e', long = "event", default_value = "del-best-route")]
    scenario: Scenario,
    /// Kind of invariants to generate
    #[clap(short = 's', long = "spec", default_value = "old-until-new-egress")]
    spec_kind: SpecificationKind,
    /// Fraction of routers allowed to use a temporary bgp sessions allowed.
    ///
    /// The actual number is computed by multiplying this factor by the number of internal
    /// routers. The default value is 2, such that the solver will accept any number of temporary
    /// bgp sessions.
    #[clap(short = 'A', long = "temp-sessions", default_value = "2.0")]
    num_allowed_temp_sessions: f64,
    /// Timeout for the ILP solver in seconds
    #[clap(short = 'T', long, default_value = "1000")]
    timeout: u64,
    /// Minimum size of networks to test (number of nodes).
    #[clap(short = 'm', long = "min", default_value = "1")]
    min_size: usize,
    /// Maximum size of networks to test (number of nodes).
    #[clap(short = 'M', long = "max", default_value = "1000")]
    max_size: usize,
    /// Topologies to use. If nothing was given, then use all topologies. You can give multiple topologies.
    #[clap(short = 'g', long = "topo")]
    topologies: Vec<_TopologyZoo>,
    /// Ignore topology
    #[clap(short, long)]
    ignore: Vec<TopologyZoo>,
    /// Number of repetitions for each execution
    #[clap(short = 'n', long = "num-repetitions", default_value = "1")]
    num_repetitions: usize,
    /// Randomize configuration
    #[clap(short, long)]
    rand: bool,
}

/// What kind of invariants should be generated
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, ValueEnum)]
enum SpecificationKind {
    /// Require reachability for all sources to reach the destination.
    Reachability,
    /// Require both reachability for all sources to reach the destination, but also that each
    /// source can only use either its old or its new egress.
    EgressWaypoint,
    /// Require that all routers switch once from their old egress to the new egress, while also
    /// maintaining reachability in the process. Once a router has used the new egress, it cannot go
    /// back.
    OldUntilNewEgress,
    /// Iterate over specification that is becoming more and more complex.
    IterSpec,
    /// Iterate over all kinds.
    All,
}

impl SpecificationKind {
    fn as_vec(&self, num_internals: usize) -> Vec<SpecificationBuilder> {
        match self {
            SpecificationKind::Reachability => vec![SpecificationBuilder::Reachability],
            SpecificationKind::EgressWaypoint => vec![SpecificationBuilder::EgressWaypoint],
            SpecificationKind::OldUntilNewEgress => vec![SpecificationBuilder::OldUntilNewEgress],
            SpecificationKind::IterSpec => {
                let step_size = 10;
                let steps: Vec<usize> = (0..)
                    .map(|x| x * step_size)
                    .take_while(|x| *x < num_internals)
                    .chain(Some(num_internals))
                    .collect();
                steps
                    .iter()
                    .copied()
                    .map(SpecificationBuilder::Scalable)
                    .chain(
                        steps
                            .iter()
                            .copied()
                            .map(SpecificationBuilder::ScalableNonTemporal),
                    )
                    .collect()
            }
            SpecificationKind::All => vec![
                SpecificationBuilder::Reachability,
                SpecificationBuilder::EgressWaypoint,
                SpecificationBuilder::OldUntilNewEgress,
            ],
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init_timed();

    let args = Cli::parse();

    let mut path = generate_folder()?;

    let topos: Vec<_> = if args.topologies.is_empty() {
        TopologyZoo::topologies_increasing_nodes()
            .iter()
            .filter(|t| !args.ignore.contains(t))
            .filter(|t| {
                t.num_externals() == 0
                    && (args.min_size..=args.max_size).contains(&t.num_internals())
            })
            .copied()
            .collect()
    } else {
        args.topologies.into_iter().map(|x| x.0).collect()
    };
    let total: usize = topos
        .iter()
        .map(|x| args.spec_kind.as_vec(x.num_internals()).len())
        .sum::<usize>()
        * args.num_repetitions;
    let mut i = 0;

    // for each topology
    for topo in topos {
        // build the network and the spec
        let spec_kinds = args.spec_kind.as_vec(topo.num_internals());
        let Ok((net, p, c)) = args.scenario.build(topo, BasicEventQueue::new(), args.rand) else {
            println!("Skipping {topo}");
            i += spec_kinds.len() * args.num_repetitions;
            continue
        };

        // for each spec
        for spec_kind in spec_kinds {

            // for each repetition
            for _ in 0..args.num_repetitions {
                i += 1;
                let mut topo_s = format!("{topo:?}");
                if topo_s.chars().count() > 20 {
                    topo_s = topo_s.chars().take(18).chain(repeat('.').take(2)).collect();
                }
                let case_s = format!("{topo_s}, {spec_kind:?}");
                print!(
                    "{i: >3}/{total}: {: >3} nodes, {case_s: <40} ",
                    topo.num_internals(),
                );
                std::io::stdout().flush()?;

                let spec = spec_kind.build_all(&net, Some(&c), [p]);

                // prepare the scheduler
                let info = CommandInfo::new(&net, c.clone(), &spec)?;
                let bgp_deps = find_dependencies(&info);

                let start_time = Instant::now();
                let (result, size) = schedule_smart(
                    &info,
                    &bgp_deps,
                    p,
                    Duration::from_secs(args.timeout),
                    (args.num_allowed_temp_sessions * net.num_devices() as f64).round() as usize,
                );

                let path_len = compute_avg_path_length(&info);
                let time = start_time.elapsed().as_secs_f64();
                print!("{time: >8.3}, result: ");
                let (exp_result, decomp) = match result {
                    Ok(schedule) => {
                        let cost: usize = schedule.0.values().map(NodeSchedule::cost).sum();
                        let steps = schedule.0.values().map(|x| x.new_route).max().unwrap_or(0);
                        let schedules = hashmap! {p => schedule};
                        match build(&info, bgp_deps, schedules) {
                            Ok(decomp) => {
                                let steps = decomp
                                    .atomic_before
                                    .get(&p)
                                    .into_iter()
                                    .flatten()
                                    .chain(decomp.atomic_after.get(&p).into_iter().flatten())
                                    .count();
                                let slow_steps = decomp
                                    .atomic_before
                                    .get(&p)
                                    .into_iter()
                                    .flatten()
                                    .chain(decomp.atomic_after.get(&p).into_iter().flatten())
                                    .filter(|step| {
                                        step.iter().any(|x| {
                                            matches!(
                                                x.command,
                                                AtomicModifier::ChangePreference { .. }
                                                    | AtomicModifier::UseTempSession { .. }
                                            ) && matches!(
                                                x.postcondition,
                                                AtomicCondition::SelectedRoute { .. }
                                            )
                                        })
                                    })
                                    .count();
                                // perform the update and get the statistics
                                let (_, stats) =
                                    runtime::sim::run(net.clone(), decomp.clone(), &spec)?;
                                let bs_decomp =
                                    Decomposition::baseline(decomp.original_command.clone());
                                let (_, bs_stats) =
                                    runtime::sim::run_no_checks(net.clone(), bs_decomp)?;
                                println!(
                                "success, steps {slow_steps: >2}/{steps: <2}, cost {cost: >2}, routes {}/{}/{}, paths {path_len: >4.1} ILP {size}",
                                bs_stats.max_routes,
                                stats.max_routes,
                                stats.num_routes_before + stats.num_routes_after
                            );
                                (
                                    ExperimentResult::Success {
                                        cost,
                                        steps,
                                        slow_steps,
                                        max_routes: stats.max_routes,
                                        routes_before: stats.num_routes_before,
                                        routes_after: stats.num_routes_after,
                                        max_routes_baseline: bs_stats.max_routes,
                                    },
                                    Some(decomp),
                                )
                            }
                            Err(_) => {
                                println!("Synth failed, steps {steps}, cost {cost}, paths {path_len: >4.1} ILP {size}");
                                (ExperimentResult::SynthesisFailed { cost, steps }, None)
                            }
                        }
                    }
                    Err(ResolutionError::Infeasible) => {
                        println!("infeasible, paths {path_len: >4.1} ILP {size}");
                        (ExperimentResult::Infeasible, None)
                    }
                    Err(_) => {
                        println!("timeout, paths {path_len: >4.1} ILP {size}");
                        (ExperimentResult::Timeout, None)
                    }
                };

                path.push(format!("{i}_{topo:?}_{spec_kind:?}"));

                // store the experiment
                Experiment {
                    net: &net,
                    topo: Some(topo),
                    scenario: Some(args.scenario),
                    spec_builder: Some(spec_kind),
                    spec: &spec,
                    decomp: decomp.as_ref(),
                    rand: args.rand,
                    data: ExperimentData {
                        time,
                        result: exp_result,
                        num_variables: size.cols,
                        num_equations: size.rows,
                        model_steps: size.steps,
                        avg_path_length: path_len,
                        fw_state_before: &info.fw_before,
                        fw_state_after: &info.fw_after,
                    },
                }
                .write_json(&path)?;

                path.pop();
            } // for each repetition
        } // for each scenario
    } // for each topology

    Ok(())
}

/// Generate the new results folder and return its path.
fn generate_folder() -> Result<PathBuf, std::io::Error> {
    let mut path = PathBuf::from("results");
    if !path.exists() {
        create_dir(&path)?;
    }
    let cur_time = OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(
            &format_description::parse("[year]-[month]-[day]_[hour]-[minute]-[second]").unwrap(),
        )
        .unwrap();
    path.push(format!("overhead_{cur_time}"));
    create_dir(&path)?;
    Ok(path)
}

#[derive(Debug, Serialize)]
enum ExperimentResult {
    Success {
        cost: usize,
        steps: usize,
        slow_steps: usize,
        max_routes: usize,
        routes_before: usize,
        routes_after: usize,
        max_routes_baseline: usize,
    },
    SynthesisFailed {
        cost: usize,
        steps: usize,
    },
    Timeout,
    Infeasible,
}

#[derive(Debug, Serialize)]
struct ExperimentData<'a> {
    time: f64,
    result: ExperimentResult,
    num_variables: usize,
    num_equations: usize,
    model_steps: usize,
    avg_path_length: f64,
    fw_state_before: &'a ForwardingState<P>,
    fw_state_after: &'a ForwardingState<P>,
}

fn compute_avg_path_length<Q>(info: &CommandInfo<'_, Q>) -> f64 {
    let mut num_paths = 0usize;
    let mut acc = 0.0;
    let mut fw_before = info.fw_before.clone();
    let mut fw_after = info.fw_after.clone();
    for r in info.net_before.get_routers() {
        for p in info.net_before.get_known_prefixes() {
            for path in fw_before.get_paths(r, *p).into_iter().flatten() {
                num_paths += 1;
                acc += path.len() as f64;
            }
            for path in fw_after.get_paths(r, *p).into_iter().flatten() {
                num_paths += 1;
                acc += path.len() as f64;
            }
        }
    }
    acc / num_paths as f64
}
