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

use chameleon::{
    decompose,
    decomposition::DecompositionError,
    experiment::{Experiment, Scenario, _TopologyZoo},
    runtime::sim,
    specification::{Specification, SpecificationBuilder},
    Decomposition, P,
};
use bgpsim::{
    config::{ConfigModifier, NetworkConfig},
    event::{EventQueue, GeoTimingModel, ModelParams},
    prelude::*,
    topology_zoo::TopologyZoo,
};
use clap::{Parser, ValueEnum};
use itertools::iproduct;
use rayon::prelude::*;

type Net = Network<P, GeoTimingModel<P>>;
type Cmd = ConfigModifier<P>;

/// Evaluate the ILP scheduler.
#[derive(Debug, Parser)]
struct Cli {
    /// Topology name from Topology Zoo
    topo: _TopologyZoo,
    /// output path, where to store the generated JSON. The generated files will have the following
    /// filenames: `{OUTPUT}/{TOPO}_{SCENARIO}_{SPECIFICATION}_{TIME}.json`
    output: String,
    /// Kind of scenario to simulate
    #[clap(short = 'e', long = "event", default_value = "all")]
    scenario: ScenarioIter,
    /// Kind of invariants to generate
    #[clap(short = 's', long = "spec", default_value = "all")]
    spec_kind: SpecificationKind,
    /// Number of iterations per miration scenario
    #[clap(short, long = "iter", default_value = "1000")]
    n_iter: usize,
    /// Number of times a scenario should be generated (randomly) and tested.
    #[clap(short, long, default_value = "1")]
    repeat: usize,
    /// Number of workers to use in parallel. If not specified, it will use all available workers.
    #[clap(short, long)]
    threads: Option<usize>,
    /// Enable verbose output.
    #[clap(short, long)]
    verbose: bool,
    /// Randomize configuration
    #[clap(short, long)]
    rand: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init_timed();

    let args = Cli::parse();

    let threads = args.threads.unwrap_or_else(num_cpus::get);
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()?;

    let topo = args.topo.0;
    for (scenario, spec_kind) in iproduct!(args.scenario, args.spec_kind) {
        let mut i = 0;
        while i < args.repeat {
            let (net, p, c) = scenario.build(topo, queue(topo), args.rand)?;
            let spec = spec_kind.build_all(&net, Some(&c), [p]);

            let decomp = match decompose(&net, c.clone(), &spec) {
                Ok(d) => d,
                Err(DecompositionError::SchedulerError(_)) => {
                    log::warn!("Problem is inveasible!");
                    continue;
                }
                Err(e) => {
                    log::error!("Error during decomposition: {e}");
                    panic!("Error during decomposition: {e}");
                }
            };
            i += 1;

            let mut success: Vec<Result<(), sim::SimError>> = Vec::new();
            (0..args.n_iter)
                .into_par_iter()
                .map(|_| assert_schedule_correct(&net, &decomp, &spec))
                .collect_into_vec(&mut success);
            success.into_iter().collect::<Result<Vec<()>, _>>()?;

            let mut naive_times: Vec<Result<f64, NetworkError>> = Vec::new();
            (0..args.n_iter)
                .into_par_iter()
                .map(|_| measure_naive(&net, &c, &spec, p))
                .collect_into_vec(&mut naive_times);

            let violation_times = naive_times.into_iter().collect::<Result<Vec<_>, _>>()?;

            let n = violation_times.len() as f64;
            let mean = violation_times.iter().copied().sum::<f64>() / n;
            let sigma = (violation_times
                .iter()
                .copied()
                .map(|x| (x - mean) * (x - mean))
                .sum::<f64>()
                / n)
                .sqrt();

            if args.verbose {
                println!(
                    "topo {topo:?}, scenario {scenario:?}, spec {spec_kind:?} [{i}]: {mean} +- {sigma}",
                )
            }

            Experiment {
                net: &net,
                topo: Some(topo),
                scenario: Some(scenario),
                spec_builder: Some(spec_kind),
                spec: &spec,
                decomp: Some(&decomp),
                rand: args.rand,
                data: violation_times,
            }
            .write_json_with_timestamp(format!(
                "{}/{topo:?}_{scenario:?}_{spec_kind:?}",
                args.output
            ))?;
        }
    }

    Ok(())
}

fn measure_naive(net: &Net, c: &Cmd, spec: &Specification, prefix: P) -> Result<f64, NetworkError> {
    let invariants = spec
        .get(&prefix)
        .map(|x| x.clone().as_global_invariants(net))
        .unwrap_or_default();
    let mut net = net.clone();

    let mut fw_state = net.get_forwarding_state();
    let mut sat = invariants
        .iter()
        .all(|i| i.check(&mut fw_state, prefix).is_ok());
    let mut last_time = net.queue().get_time().unwrap();

    net.manual_simulation();
    net.apply_modifier(c)?;

    let mut result = 0.0;

    while net.simulate_step()?.is_some() {
        // update the time
        let current_time = net.queue().get_time().unwrap();
        let delta = current_time - last_time;
        last_time = current_time;

        if !sat {
            result += delta;
        }

        // check all policies
        let mut fw_state = net.get_forwarding_state();
        sat = invariants
            .iter()
            .all(|i| i.check(&mut fw_state, prefix).is_ok());
    }

    Ok(result)
}

fn assert_schedule_correct(
    net: &Net,
    decomp: &Decomposition,
    spec: &Specification,
) -> Result<(), sim::SimError> {
    sim::run(net.clone(), decomp.clone(), spec).map(|_| ())
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
    /// Iterate over all kinds.
    All,
}

impl IntoIterator for SpecificationKind {
    type Item = SpecificationBuilder;
    type IntoIter = std::vec::IntoIter<SpecificationBuilder>;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            SpecificationKind::Reachability => vec![SpecificationBuilder::Reachability],
            SpecificationKind::EgressWaypoint => vec![SpecificationBuilder::EgressWaypoint],
            SpecificationKind::OldUntilNewEgress => vec![SpecificationBuilder::OldUntilNewEgress],
            SpecificationKind::All => vec![
                SpecificationBuilder::Reachability,
                SpecificationBuilder::EgressWaypoint,
                SpecificationBuilder::OldUntilNewEgress,
            ],
        }
        .into_iter()
    }
}

/// What is the kind of reconfiguration that should be done?
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, ValueEnum)]
enum ScenarioIter {
    /// Advertise a new route that is better than all others
    NewBestRoute,
    /// Withdraw the old best route.
    DelBestRoute,
    /// Iterate over all scenarios
    All,
}

impl IntoIterator for ScenarioIter {
    type Item = Scenario;
    type IntoIter = std::vec::IntoIter<Scenario>;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            ScenarioIter::NewBestRoute => vec![Scenario::NewBestRoute],
            ScenarioIter::DelBestRoute => vec![Scenario::DelBestRoute],
            ScenarioIter::All => vec![Scenario::NewBestRoute, Scenario::DelBestRoute],
        }
        .into_iter()
    }
}

/// generate the event queue
fn queue(topo: TopologyZoo) -> GeoTimingModel<P> {
    GeoTimingModel::new(
        // queuing_params for the control plane, including computation and data plane update delay
        ModelParams::new(
            0.003, // offset
            0.004, // scale
            2.0,   // alpha
            5.0,   // beta
            0.001, // collision
        ),
        // queuing_params for transporting packets through the network's data plane (usually fast!)
        ModelParams::new(
            0.000_000_1, // offset
            0.000_000_1, // scale
            2.0,         // alpha
            5.0,         // beta
            0.0,         // collision
        ),
        &topo.geo_location(),
    )
}
